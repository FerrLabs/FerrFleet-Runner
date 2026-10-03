use std::time::Duration;

use ferrfleet_shared::ExecutorEvent;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::sender::EventSender;

pub struct EventQueue {
    tx: mpsc::UnboundedSender<ExecutorEvent>,
    drain: JoinHandle<()>,
    flush_budget: Duration,
}

impl EventQueue {
    pub fn start(sender: EventSender) -> Self {
        let flush_budget = sender.retry_budget();
        let (tx, mut rx) = mpsc::unbounded_channel::<ExecutorEvent>();
        let drain = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if sender.lost().is_some() {
                    continue;
                }
                if let Err(err) = sender.send(&event).await {
                    if let Some(lost) = sender.lost() {
                        info!(%lost, "run stopped; queued events are dropped");
                    } else {
                        warn!(?err, "failed to push event to api (continuing)");
                    }
                }
            }
        });
        Self {
            tx,
            drain,
            flush_budget,
        }
    }

    pub fn push(&self, event: ExecutorEvent) {
        if self.tx.send(event).is_err() {
            warn!("event queue is no longer drained; event dropped");
        }
    }

    pub async fn flush(self) {
        let Self {
            tx,
            mut drain,
            flush_budget,
        } = self;
        drop(tx);
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

    use chrono::Utc;
    use ferrfleet_shared::ExecutorEvent;

    use super::EventQueue;
    use crate::fake_api::{FakeApi, RUN_ID, Received, Reply, dead_api_url, sender};
    use crate::retry::Backoff;
    use crate::sender::EventSender;

    fn message(n: usize) -> ExecutorEvent {
        ExecutorEvent::AssistantMessage {
            content: n.to_string(),
            timestamp: Utc::now(),
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
        let events = EventQueue::start(sender(&api.url));

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
    async fn an_event_the_api_refused_does_not_stop_the_ones_behind_it() {
        let api = FakeApi::start(|n| Reply::status(if n == 0 { 500 } else { 202 })).await;
        let events = EventQueue::start(sender(&api.url));

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
        let events = EventQueue::start(EventSender::for_tests(
            &dead_api_url(),
            backoff,
            Duration::from_secs(2),
        ));
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
}
