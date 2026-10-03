use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use ferrfleet_shared::ExecutorEvent;
use tokio::sync::watch;

use super::{Agent, Ended, Mode, Timings};
use crate::RunOutcome;
use crate::config::{AgentConfig, PoolToken};
use crate::event_queue::EventQueue;
use crate::fake_api::{FAST, FakeApi, RUN_ID, Received, Reply};
use crate::lease::Lost;

const POOL_TOKEN: &str = "ffrp_test_pool_token";
const RUN_TOKEN: &str = "run-token-from-the-lease";
const LEASE_PATH: &str = "/runner-pools/lease";

const TIMINGS: Timings = Timings {
    api: FAST,
    api_timeout: Duration::from_secs(2),
    lease_timeout: Duration::from_secs(2),
    poll_retry_first: Duration::from_millis(50),
    poll_retry_max: Duration::from_millis(200),
    second: Duration::from_millis(20),
};

fn config(api: &FakeApi) -> AgentConfig {
    let _ = rustls::crypto::ring::default_provider().install_default();
    AgentConfig {
        api_url: api.url.clone(),
        pool_token: PoolToken::parse(POOL_TOKEN.to_owned()).expect("a pool token"),
        runner_name: "build-farm-07".to_owned(),
        work_root: std::env::temp_dir().join(format!("ferrfleet-agent-{}", uuid::Uuid::new_v4())),
    }
}

fn leased(heartbeat_every_secs: u64) -> Reply {
    Reply::json(
        200,
        &serde_json::json!({
            "run_id": RUN_ID,
            "run_token": RUN_TOKEN,
            "lease_expires_at": "2026-10-03T09:14:14Z",
            "heartbeat_every_secs": heartbeat_every_secs,
        }),
    )
}

fn nothing_yet() -> Reply {
    Reply::status(204).after(Duration::from_millis(30))
}

fn is_lease(r: &Received) -> bool {
    r.path == LEASE_PATH
}

fn is_heartbeat(r: &Received) -> bool {
    r.path == format!("/runs/{RUN_ID}/heartbeat")
}

fn is_event(r: &Received) -> bool {
    r.path == format!("/runs/{RUN_ID}/events")
}

fn count(api: &FakeApi, which: fn(&Received) -> bool) -> usize {
    api.received().iter().filter(|r| which(r)).count()
}

fn one_lease_then_nothing(
    heartbeat_every_secs: u64,
) -> impl Fn(&Received) -> Option<Reply> + Send + Sync {
    let leases = AtomicUsize::new(0);
    move |r| {
        is_lease(r).then(|| {
            if leases.fetch_add(1, Ordering::SeqCst) == 0 {
                leased(heartbeat_every_secs)
            } else {
                nothing_yet()
            }
        })
    }
}

async fn eventually(api: &FakeApi, what: &str, done: impl Fn(&[Received]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(&api.received()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn event() -> ExecutorEvent {
    ExecutorEvent::AssistantMessage {
        content: "working".to_owned(),
        timestamp: Utc::now(),
    }
}

#[tokio::test]
async fn an_empty_poll_is_followed_by_another_and_the_run_it_brings() {
    let leases = AtomicUsize::new(0);
    let api = FakeApi::routed(move |r, _| {
        if !is_lease(r) {
            return Reply::status(200);
        }
        match leases.fetch_add(1, Ordering::SeqCst) {
            0 => Reply::status(204),
            1 => leased(30),
            _ => nothing_yet(),
        }
    })
    .await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(config(&api), TIMINGS, {
        let seen = seen.clone();
        move |env, _| {
            seen.lock().unwrap().push(env.run_id);
            async { Ok(RunOutcome::Completed { exit_code: 0 }) }
        }
    });
    let (stop, shutdown) = watch::channel(false);
    let serving = tokio::spawn(async move { agent.serve(Mode::Loop, shutdown).await });

    eventually(&api, "a third poll", |r| {
        r.iter().filter(|r| is_lease(r)).count() >= 3
    })
    .await;
    stop.send(true).unwrap();

    assert_eq!(serving.await.unwrap().unwrap(), Ended::Shutdown);
    assert_eq!(*seen.lock().unwrap(), [RUN_ID]);
}

#[tokio::test]
async fn a_refused_pool_token_ends_the_agent_with_an_error_that_says_so() {
    let api = FakeApi::start(|_| Reply::status(401)).await;
    let ran = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(config(&api), TIMINGS, {
        let ran = ran.clone();
        move |_, _| {
            ran.fetch_add(1, Ordering::SeqCst);
            async { Ok(RunOutcome::Completed { exit_code: 0 }) }
        }
    });

    let err = agent
        .serve(Mode::Loop, watch::channel(false).1)
        .await
        .expect_err("a revoked pool cannot be served");

    assert!(err.to_string().contains("pool token"), "{err:#}");
    assert_eq!(
        api.received().len(),
        1,
        "a refused token was presented again"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_rate_limited_poll_waits_longer_each_time_before_asking_again() {
    let api = FakeApi::routed(|r, n| match n {
        _ if !is_lease(r) => Reply::status(200),
        0..=2 => Reply::status(429),
        _ => leased(30),
    })
    .await;
    let agent = Agent::new(config(&api), TIMINGS, |_, _| async {
        Ok(RunOutcome::Completed { exit_code: 0 })
    });

    let started = Instant::now();
    let ended = agent
        .serve(Mode::Ephemeral, watch::channel(false).1)
        .await
        .unwrap();

    assert_eq!(ended, Ended::RunDone);
    assert_eq!(count(&api, is_lease), 4);
    assert!(
        started.elapsed() >= Duration::from_millis(50 + 100 + 200),
        "three 429s were answered within {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn heartbeats_start_with_the_lease_and_stop_with_the_run() {
    let api = FakeApi::routed(|r, n| match n {
        0 => leased(2),
        _ if is_heartbeat(r) => Reply::status(200),
        _ => nothing_yet(),
    })
    .await;
    let agent = Agent::new(config(&api), TIMINGS, |_, _| async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        Ok(RunOutcome::Completed { exit_code: 0 })
    });

    agent
        .serve(Mode::Ephemeral, watch::channel(false).1)
        .await
        .unwrap();
    let beats = count(&api, is_heartbeat);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let received = api.received();
    assert!(is_lease(&received[0]));
    assert!(
        is_heartbeat(&received[1]),
        "the first heartbeat waited for something"
    );
    assert!(
        beats >= 4,
        "{beats} heartbeats over a 300 ms run beating every 40 ms"
    );
    assert_eq!(
        count(&api, is_heartbeat),
        beats,
        "heartbeats outlived the run"
    );
    assert!(
        received[1..]
            .iter()
            .all(|r| r.authorization.as_deref() == Some(&format!("Bearer {RUN_TOKEN}"))),
        "a heartbeat went out without the run token"
    );
}

async fn a_refused_heartbeat_stops_the_run_and_polling_resumes(status: u16, expected: Lost) {
    let lease = one_lease_then_nothing(30);
    let api = FakeApi::routed(move |r, _| lease(r).unwrap_or_else(|| Reply::status(status))).await;
    let stopped = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(config(&api), TIMINGS, {
        let stopped = stopped.clone();
        move |_, sender| {
            let stopped = stopped.clone();
            async move {
                let lost = tokio::time::timeout(Duration::from_secs(3), sender.wait_lost())
                    .await
                    .expect("the run is told to stop");
                stopped.lock().unwrap().push(lost);
                Ok(RunOutcome::Stopped(lost))
            }
        }
    });
    let (stop, shutdown) = watch::channel(false);
    let serving = tokio::spawn(async move { agent.serve(Mode::Loop, shutdown).await });

    eventually(&api, "the next poll", |r| {
        r.iter().filter(|r| is_lease(r)).count() >= 2
    })
    .await;
    stop.send(true).unwrap();

    assert_eq!(serving.await.unwrap().unwrap(), Ended::Shutdown);
    assert_eq!(*stopped.lock().unwrap(), [expected]);
    assert_eq!(
        count(&api, is_heartbeat),
        1,
        "a refused heartbeat was repeated"
    );
}

#[tokio::test]
async fn a_gone_heartbeat_stops_the_run_and_polling_resumes() {
    a_refused_heartbeat_stops_the_run_and_polling_resumes(410, Lost::RunOver).await;
}

#[tokio::test]
async fn a_conflicting_heartbeat_stops_the_run_and_polling_resumes() {
    a_refused_heartbeat_stops_the_run_and_polling_resumes(409, Lost::Superseded).await;
}

#[tokio::test]
async fn a_conflict_on_events_stops_the_run_and_drops_what_is_still_queued() {
    let lease = one_lease_then_nothing(30);
    let api = FakeApi::routed(move |r, _| {
        lease(r).unwrap_or_else(|| Reply::status(if is_event(r) { 409 } else { 200 }))
    })
    .await;
    let stopped = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(config(&api), TIMINGS, {
        let stopped = stopped.clone();
        move |_, sender| {
            let stopped = stopped.clone();
            async move {
                let queue = EventQueue::start(sender.clone());
                for _ in 0..3 {
                    queue.push(event());
                }
                let lost = tokio::time::timeout(Duration::from_secs(3), sender.wait_lost())
                    .await
                    .expect("the run is told to stop");
                queue.push(event());
                queue.flush().await;
                stopped.lock().unwrap().push(lost);
                Ok(RunOutcome::Stopped(lost))
            }
        }
    });
    let (stop, shutdown) = watch::channel(false);
    let serving = tokio::spawn(async move { agent.serve(Mode::Loop, shutdown).await });

    eventually(&api, "the next poll", |r| {
        r.iter().filter(|r| is_lease(r)).count() >= 2
    })
    .await;
    stop.send(true).unwrap();

    assert_eq!(serving.await.unwrap().unwrap(), Ended::Shutdown);
    assert_eq!(*stopped.lock().unwrap(), [Lost::Superseded]);
    assert_eq!(
        count(&api, is_event),
        1,
        "events went out after the API said the run is no longer ours"
    );
}

#[tokio::test]
async fn an_ephemeral_agent_takes_one_run_and_exits_with_its_verdict() {
    for (outcome, expected) in [
        (RunOutcome::Completed { exit_code: 0 }, Ended::RunDone),
        (RunOutcome::Completed { exit_code: 2 }, Ended::RunFailed),
        (RunOutcome::Stopped(Lost::RunOver), Ended::RunDone),
    ] {
        let api = FakeApi::routed(|r, _| {
            if is_lease(r) {
                leased(30)
            } else {
                Reply::status(200)
            }
        })
        .await;
        let agent = Agent::new(
            config(&api),
            TIMINGS,
            move |_, _| async move { Ok(outcome) },
        );

        let ended = agent
            .serve(Mode::Ephemeral, watch::channel(false).1)
            .await
            .unwrap();

        assert_eq!(ended, expected, "{outcome:?}");
        assert_eq!(
            count(&api, is_lease),
            1,
            "an ephemeral agent asked for a second run"
        );
    }
}

#[tokio::test]
async fn a_shutdown_during_a_run_lets_it_finish_and_takes_no_other() {
    let api = FakeApi::routed(|r, _| {
        if is_lease(r) {
            leased(30)
        } else {
            Reply::status(200)
        }
    })
    .await;
    let (stop, shutdown) = watch::channel(false);
    let stop = Arc::new(stop);
    let finished = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(config(&api), TIMINGS, {
        let (stop, finished) = (stop.clone(), finished.clone());
        move |_, _| {
            let (stop, finished) = (stop.clone(), finished.clone());
            async move {
                stop.send(true).unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
                finished.fetch_add(1, Ordering::SeqCst);
                Ok(RunOutcome::Completed { exit_code: 0 })
            }
        }
    });

    let ended = agent.serve(Mode::Loop, shutdown).await.unwrap();

    assert_eq!(ended, Ended::Shutdown);
    assert_eq!(finished.load(Ordering::SeqCst), 1, "the run was cut short");
    assert_eq!(count(&api, is_lease), 1);
}

#[tokio::test]
async fn the_pool_token_only_reaches_the_lease_and_the_run_token_only_the_run() {
    let api = FakeApi::routed(|r, _| {
        if is_lease(r) {
            leased(30)
        } else {
            Reply::status(200)
        }
    })
    .await;
    let agent = Agent::new(config(&api), TIMINGS, |_, sender| async move {
        sender.send(&event()).await?;
        Ok(RunOutcome::Completed { exit_code: 0 })
    });

    agent
        .serve(Mode::Ephemeral, watch::channel(false).1)
        .await
        .unwrap();

    let received = api.received();
    assert!(received.iter().any(is_event));
    for r in &received {
        let expected = if is_lease(r) { POOL_TOKEN } else { RUN_TOKEN };
        assert_eq!(
            r.authorization.as_deref(),
            Some(format!("Bearer {expected}").as_str()),
            "{} {}",
            r.method,
            r.path
        );
    }
    let lease_body: serde_json::Value = serde_json::from_str(&received[0].body).unwrap();
    assert_eq!(lease_body["runner"], "build-farm-07");
}

#[tokio::test]
async fn each_run_works_in_a_fresh_directory_removed_after_it() {
    let api = FakeApi::routed(|r, _| {
        if is_lease(r) {
            leased(30)
        } else {
            Reply::status(200)
        }
    })
    .await;
    let config = config(&api);
    let root = config.work_root.clone();
    let used = Arc::new(Mutex::new(None::<PathBuf>));
    let agent = Agent::new(config, TIMINGS, {
        let used = used.clone();
        move |env, _| {
            let dir = PathBuf::from(env.working_dir.expect("a per-run directory"));
            std::fs::create_dir_all(dir.join("repo")).unwrap();
            *used.lock().unwrap() = Some(dir);
            async { Ok(RunOutcome::Completed { exit_code: 0 }) }
        }
    });

    agent
        .serve(Mode::Ephemeral, watch::channel(false).1)
        .await
        .unwrap();

    let used = used
        .lock()
        .unwrap()
        .clone()
        .expect("the run was given a directory");
    assert_eq!(used, root.join(RUN_ID));
    assert!(
        !used.exists(),
        "the next run would clone into a used checkout"
    );
    let _ = std::fs::remove_dir_all(&root);
}
