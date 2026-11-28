use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use ferrfleet_shared::ExecutorEvent;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval_at};
use tracing::{info, warn};

use crate::delta_buffer::DeltaBuffer;
use crate::redactor::{Secret, SharedRedactor};
use crate::sender::EventSender;

const DELTA_INTERVAL: Duration = Duration::from_millis(150);
const MAX_PENDING_DELTAS: usize = 4;

enum Item {
    Event(ExecutorEvent),
    Delta(ExecutorEvent),
}

#[derive(Clone)]
struct DeltaLane {
    tx: mpsc::UnboundedSender<Item>,
    buffer: Arc<Mutex<DeltaBuffer>>,
    pending: Arc<AtomicUsize>,
    redactor: SharedRedactor,
}

impl DeltaLane {
    fn buffer(&self) -> MutexGuard<'_, DeltaBuffer> {
        self.buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn release(&self, buffer: &mut DeltaBuffer) {
        let redactor = self.redactor.current();
        for delta in buffer.drain() {
            if self.pending.load(Ordering::Acquire) >= MAX_PENDING_DELTAS {
                continue;
            }
            let delta = redactor.event(delta);
            self.pending.fetch_add(1, Ordering::AcqRel);
            if self.tx.send(Item::Delta(delta)).is_err() {
                self.pending.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }

    fn push(&self, event: ExecutorEvent) {
        if let ExecutorEvent::Delta {
            kind,
            content,
            timestamp,
        } = &event
        {
            self.buffer().append(*kind, content, *timestamp);
            return;
        }
        let event = self.redactor.current().event(event);
        let mut buffer = self.buffer();
        self.release(&mut buffer);
        if self.tx.send(Item::Event(event)).is_err() {
            warn!("event queue is no longer drained; event dropped");
        }
    }

    async fn release_every_interval(self) {
        let mut ticks = interval_at(tokio::time::Instant::now() + DELTA_INTERVAL, DELTA_INTERVAL);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            self.release(&mut self.buffer());
        }
    }
}

pub struct EventQueue {
    lane: DeltaLane,
    releaser: JoinHandle<()>,
    drain: JoinHandle<()>,
    flush_budget: Duration,
}

impl EventQueue {
    pub fn start(sender: EventSender, redactor: SharedRedactor) -> Self {
        let flush_budget = sender.retry_budget();
        let (tx, mut rx) = mpsc::unbounded_channel::<Item>();
        let lane = DeltaLane {
            tx,
            buffer: Arc::default(),
            pending: Arc::default(),
            redactor,
        };
        let pending = lane.pending.clone();
        let drain = tokio::spawn(async move {
            while let Some(item) = rx.recv().await {
                if matches!(item, Item::Delta(_)) {
                    pending.fetch_sub(1, Ordering::AcqRel);
                }
                if sender.lost().is_some() {
                    continue;
                }
                match item {
                    Item::Event(event) => {
                        if let Err(err) = sender.send(&event).await {
                            if let Some(lost) = sender.lost() {
                                info!(%lost, "run stopped; queued events are dropped");
                            } else {
                                warn!(?err, "failed to push event to api (continuing)");
                            }
                        }
                    }
                    Item::Delta(delta) => {
                        if let Err(err) = sender.send_delta(&delta).await {
                            tracing::debug!(?err, "delta dropped");
                        }
                    }
                }
            }
        });
        let releaser = tokio::spawn(lane.clone().release_every_interval());
        Self {
            lane,
            releaser,
            drain,
            flush_budget,
        }
    }

    pub fn push(&self, event: ExecutorEvent) {
        self.lane.push(event);
    }

    pub fn protect(&self, secrets: Vec<Secret>) {
        self.lane.redactor.add(secrets);
    }

    pub async fn flush(self) {
        let Self {
            lane,
            releaser,
            mut drain,
            flush_budget,
        } = self;
        releaser.abort();
        let _ = releaser.await;
        drop(lane);
        match tokio::time::timeout(flush_budget, &mut drain).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => warn!(?err, "event queue drain task failed"),
            Err(_) => {
                drain.abort();
                warn!(
                    ?flush_budget,
                    "api still unreachable; events left in the queue are lost"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use std::sync::Arc;

    use ferrfleet_shared::{DeltaKind, ExecutorEvent};
    use time::OffsetDateTime;
    use tokio::sync::mpsc;

    use super::{DELTA_INTERVAL, DeltaLane, EventQueue, Item, MAX_PENDING_DELTAS};
    use crate::fake_api::{FakeApi, RUN_ID, Received, Reply, dead_api_url, sender};
    use crate::redactor::{Secret, SharedRedactor};
    use crate::retry::Backoff;
    use crate::sender::EventSender;
    use crate::test_support::fake_value;

    fn message(n: usize) -> ExecutorEvent {
        ExecutorEvent::AssistantMessage {
            content: n.to_string(),
            timestamp: OffsetDateTime::now_utc(),
        }
    }

    fn contents(received: &[Received]) -> Vec<String> {
        received
            .iter()
            .inspect(|r| assert_eq!(r.path, format!("/runs/{RUN_ID}/events")))
            .map(|r| {
                let body: serde_json::Value = serde_json::from_str(&r.body).expect("an event body");
                body["content"].as_str().expect("a message").to_owned()
            })
            .collect()
    }

    #[tokio::test]
    async fn events_queued_while_the_api_is_away_arrive_once_and_in_order() {
        let api = FakeApi::start_later(Duration::from_millis(150), |n| {
            Reply::status(if n < 2 { 503 } else { 202 })
        });
        let events = EventQueue::start(sender(&api.url), SharedRedactor::default());

        for n in 0..20 {
            events.push(message(n));
        }
        events.flush().await;

        let expected: Vec<String> = ["0", "0"]
            .into_iter()
            .map(str::to_owned)
            .chain((0..20).map(|n| n.to_string()))
            .collect();
        assert_eq!(contents(&api.received()), expected);
    }

    #[tokio::test]
    async fn an_error_event_reaches_the_api_without_credentials() {
        let api = FakeApi::start(|_| Reply::status(202)).await;
        let events = EventQueue::start(sender(&api.url), SharedRedactor::default());
        let sample = crate::test_support::fake_token("ghs", 1);

        events.push(ExecutorEvent::Error {
            message: format!("git clone failed: https://{sample}@github.com/o/r"),
            provider_signal: false,
            timestamp: OffsetDateTime::now_utc(),
        });
        events.flush().await;

        let received = api.received();
        assert_eq!(received.len(), 1);
        assert!(!received[0].body.contains(&sample), "{}", received[0].body);
        assert!(
            received[0].body.contains("<redacted>@github.com"),
            "{}",
            received[0].body
        );
    }

    #[tokio::test]
    async fn an_event_the_api_refused_does_not_stop_the_ones_behind_it() {
        let api = FakeApi::start(|n| Reply::status(if n == 0 { 500 } else { 202 })).await;
        let events = EventQueue::start(sender(&api.url), SharedRedactor::default());

        for n in 0..3 {
            events.push(message(n));
        }
        events.flush().await;

        assert_eq!(contents(&api.received()), ["0", "1", "2"]);
    }

    #[tokio::test]
    async fn flushing_against_a_dead_api_ends_within_one_budget() {
        let backoff = Backoff {
            first_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(40),
            budget: Duration::from_millis(300),
        };
        let events = EventQueue::start(
            EventSender::for_tests(&dead_api_url(), backoff, Duration::from_secs(2)),
            SharedRedactor::default(),
        );
        for n in 0..5 {
            events.push(message(n));
        }

        let started = Instant::now();
        events.flush().await;

        assert!(
            started.elapsed() < backoff.budget + Duration::from_millis(500),
            "flush took {:?}, one budget per queued event instead of one in total",
            started.elapsed()
        );
    }

    fn delta(kind: DeltaKind, content: &str) -> ExecutorEvent {
        ExecutorEvent::Delta {
            kind,
            content: content.to_owned(),
            timestamp: OffsetDateTime::now_utc(),
        }
    }

    fn lane() -> (DeltaLane, mpsc::UnboundedReceiver<Item>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let lane = DeltaLane {
            tx,
            buffer: Arc::default(),
            pending: Arc::default(),
            redactor: SharedRedactor::default(),
        };
        (lane, rx)
    }

    fn shape(rx: &mut mpsc::UnboundedReceiver<Item>) -> Vec<String> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .map(|item| match item {
                Item::Delta(ExecutorEvent::Delta { kind, content, .. }) => {
                    format!("delta:{kind:?}:{content}")
                }
                Item::Event(ExecutorEvent::AssistantMessage { content, .. }) => {
                    format!("event:{content}")
                }
                _ => "other".to_owned(),
            })
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn deltas_inside_one_interval_leave_as_a_single_concatenated_delta() {
        let (lane, mut rx) = lane();
        tokio::spawn(lane.clone().release_every_interval());

        for chunk in ["a", "b", "c", "d"] {
            lane.push(delta(DeltaKind::Text, chunk));
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        assert!(shape(&mut rx).is_empty(), "released before the interval");

        tokio::time::sleep(DELTA_INTERVAL).await;
        assert_eq!(shape(&mut rx), ["delta:Text:abcd"]);
    }

    #[tokio::test(start_paused = true)]
    async fn thinking_and_text_deltas_never_mix() {
        let (lane, mut rx) = lane();
        tokio::spawn(lane.clone().release_every_interval());

        lane.push(delta(DeltaKind::Thinking, "hm"));
        lane.push(delta(DeltaKind::Text, "Hi"));
        lane.push(delta(DeltaKind::Thinking, "mm"));
        tokio::time::sleep(DELTA_INTERVAL + Duration::from_millis(1)).await;

        assert_eq!(shape(&mut rx), ["delta:Thinking:hmmm", "delta:Text:Hi"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_pending_delta_is_released_before_the_event_queued_after_it() {
        let (lane, mut rx) = lane();
        tokio::spawn(lane.clone().release_every_interval());

        lane.push(delta(DeltaKind::Text, "par"));
        lane.push(delta(DeltaKind::Text, "tial"));
        lane.push(message(7));
        tokio::time::sleep(DELTA_INTERVAL * 3).await;

        assert_eq!(shape(&mut rx), ["delta:Text:partial", "event:7"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_delta_after_the_event_comes_after_it() {
        let (lane, mut rx) = lane();
        tokio::spawn(lane.clone().release_every_interval());

        lane.push(message(1));
        lane.push(delta(DeltaKind::Text, "next"));
        tokio::time::sleep(DELTA_INTERVAL + Duration::from_millis(1)).await;

        assert_eq!(shape(&mut rx), ["event:1", "delta:Text:next"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_drain_never_holds_more_than_the_bound() {
        let (lane, mut rx) = lane();
        tokio::spawn(lane.clone().release_every_interval());

        for n in 0..50 {
            lane.push(delta(DeltaKind::Text, &n.to_string()));
            tokio::time::sleep(DELTA_INTERVAL + Duration::from_millis(1)).await;
        }

        let held = shape(&mut rx);
        assert_eq!(held.len(), MAX_PENDING_DELTAS);
        assert_eq!(
            held,
            [
                "delta:Text:0",
                "delta:Text:1",
                "delta:Text:2",
                "delta:Text:3"
            ],
            "the oldest deltas are the ones kept"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_bound_does_not_apply_to_events() {
        let (lane, mut rx) = lane();
        for n in 0..50 {
            lane.push(message(n));
        }
        assert_eq!(shape(&mut rx).len(), 50);
    }

    #[tokio::test]
    async fn a_rejected_delta_is_dropped_once_and_the_run_goes_on() {
        let api = FakeApi::routed(|request, _| {
            if request.body.contains(r#""type":"delta""#) {
                Reply::status(422)
            } else {
                Reply::status(202)
            }
        })
        .await;
        let queue = EventQueue::start(sender(&api.url), SharedRedactor::default());

        queue.push(delta(DeltaKind::Text, "live"));
        tokio::time::sleep(DELTA_INTERVAL * 2).await;
        queue.push(message(1));
        queue.push(message(2));
        queue.flush().await;

        let bodies: Vec<String> = api.received().into_iter().map(|r| r.body).collect();
        assert_eq!(
            bodies
                .iter()
                .filter(|b| b.contains(r#""type":"delta""#))
                .count(),
            1,
            "a refused delta was retried: {bodies:?}"
        );
        assert_eq!(
            bodies
                .iter()
                .filter(|b| b.contains("assistant_message"))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn the_api_receives_a_delta_before_the_event_it_belongs_to() {
        let api = FakeApi::start(|_| Reply::status(202)).await;
        let queue = EventQueue::start(sender(&api.url), SharedRedactor::default());

        queue.push(delta(DeltaKind::Text, "full "));
        queue.push(delta(DeltaKind::Text, "text"));
        queue.push(message(9));
        queue.flush().await;

        let types: Vec<(String, String)> = api
            .received()
            .into_iter()
            .map(|r| {
                let body: serde_json::Value = serde_json::from_str(&r.body).unwrap();
                (
                    body["type"].as_str().unwrap().to_owned(),
                    body["content"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        assert_eq!(
            types,
            [
                ("delta".to_owned(), "full text".to_owned()),
                ("assistant_message".to_owned(), "9".to_owned())
            ]
        );
    }

    fn guarded_lane(secret: &str) -> (DeltaLane, mpsc::UnboundedReceiver<Item>) {
        let (lane, rx) = lane();
        lane.redactor
            .reset(vec![Secret::new("FERRFLEET_RUN_TOKEN", secret).unwrap()]);
        (lane, rx)
    }

    #[tokio::test(start_paused = true)]
    async fn a_token_split_across_two_chunks_of_one_interval_is_redacted() {
        let token = fake_value(108);
        let (lane, mut rx) = guarded_lane(&token);
        tokio::spawn(lane.clone().release_every_interval());

        let (head, tail) = token.split_at(token.len() / 2);
        lane.push(delta(DeltaKind::Text, &format!("env: TOKEN={head}")));
        lane.push(delta(DeltaKind::Text, &format!("{tail} done")));
        tokio::time::sleep(DELTA_INTERVAL + Duration::from_millis(1)).await;

        assert_eq!(shape(&mut rx), ["delta:Text:env: TOKEN=<redacted> done"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_token_split_across_two_intervals_is_never_whole_in_a_delta_and_the_message_is_redacted()
     {
        let token = fake_value(108);
        let (lane, mut rx) = guarded_lane(&token);
        tokio::spawn(lane.clone().release_every_interval());

        let (head, tail) = token.split_at(token.len() / 2);
        lane.push(delta(DeltaKind::Text, head));
        tokio::time::sleep(DELTA_INTERVAL + Duration::from_millis(1)).await;
        lane.push(delta(DeltaKind::Text, tail));
        lane.push(ExecutorEvent::AssistantMessage {
            content: token.clone(),
            timestamp: OffsetDateTime::now_utc(),
        });

        let sent = shape(&mut rx);
        assert_eq!(
            sent,
            [
                format!("delta:Text:{head}"),
                format!("delta:Text:{tail}"),
                "event:<redacted>".to_owned()
            ]
        );
    }

    #[tokio::test]
    async fn every_event_kind_reaches_the_api_without_the_run_token() {
        let api = FakeApi::start(|_| Reply::status(202)).await;
        let redactor = SharedRedactor::default();
        let queue = EventQueue::start(sender(&api.url), redactor.clone());
        let token = fake_value(109);
        let at = OffsetDateTime::now_utc();
        queue.protect(vec![Secret::new("FERRFLEET_RUN_TOKEN", &token).unwrap()]);

        let leak = format!("FERRFLEET_RUN_TOKEN={token}");
        queue.push(delta(DeltaKind::Thinking, &leak));
        queue.push(ExecutorEvent::Thinking {
            content: leak.clone(),
            timestamp: at,
        });
        queue.push(ExecutorEvent::ToolUse {
            tool_name: "Bash".to_owned(),
            tool_use_id: "toolu_1".to_owned(),
            input: serde_json::json!({"command": format!("echo {token}")}),
            timestamp: at,
        });
        queue.push(ExecutorEvent::ToolResult {
            tool_use_id: "toolu_1".to_owned(),
            output: serde_json::json!([{"type": "text", "text": leak}]),
            is_error: false,
            timestamp: at,
        });
        queue.push(message(1));
        queue.flush().await;

        let bodies: Vec<String> = api.received().into_iter().map(|r| r.body).collect();
        assert_eq!(bodies.len(), 5, "{bodies:?}");
        for body in &bodies {
            assert!(!body.contains(&token), "{body}");
        }
        assert!(
            bodies[3].contains("FERRFLEET_RUN_TOKEN=<redacted>"),
            "{}",
            bodies[3]
        );
    }
}
