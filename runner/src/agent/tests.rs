use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use ferrfleet_shared::ExecutorEvent;
use tokio::sync::watch;

use super::workroot::RootLock;
use super::{Agent, Ended, Mode, Timings};
use crate::RunOutcome;
use crate::config::{AgentConfig, Env, PoolToken};
use crate::event_queue::EventQueue;
use crate::fake_api::{FAST, FakeApi, RUN_ID, Received, Reply};
use crate::lease::Lost;
use crate::sender::EventSender;

const POOL_TOKEN: &str = "ffrp_test_pool_token";
const RUN_TOKEN: &str = "run-token-from-the-lease";
const LEASE_PATH: &str = "/runner-pools/lease";

const TIMINGS: Timings = Timings {
    api: FAST,
    api_timeout: Duration::from_secs(2),
    lease_timeout: Duration::from_secs(2),
    poll_retry_first: Duration::from_millis(50),
    poll_retry_max: Duration::from_millis(200),
    dispatch_budget: Duration::from_millis(600),
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

fn stale_run_dir(root: &std::path::Path) -> PathBuf {
    let stale = root.join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(stale.join("target/debug")).unwrap();
    std::fs::write(stale.join("target/debug/big.o"), b"leftover").unwrap();
    stale
}

async fn idle_pool_agent(config: AgentConfig) {
    let agent = Agent::new(config, TIMINGS, |_, _| async {
        Ok(RunOutcome::Completed { exit_code: 0 })
    });
    let ended = agent
        .serve(Mode::Loop, watch::channel(true).1)
        .await
        .unwrap();
    assert_eq!(ended, Ended::Shutdown);
}

#[tokio::test]
async fn a_pool_agent_clears_checkouts_left_by_a_killed_run_before_taking_any() {
    let api = FakeApi::routed(|_, _| nothing_yet()).await;
    let config = config(&api);
    let root = config.work_root.clone();
    let stale = stale_run_dir(&root);
    std::fs::write(root.join("stray-file"), b"x").unwrap();

    idle_pool_agent(config).await;

    assert!(
        !stale.exists(),
        "a dead run's checkout would fill the volume"
    );
    assert!(root.join("stray-file").exists());
    assert_eq!(count(&api, is_lease), 0);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn clearing_stale_checkouts_keeps_directories_that_are_not_run_ids() {
    let api = FakeApi::routed(|_, _| nothing_yet()).await;
    let config = config(&api);
    let root = config.work_root.clone();
    let foreign = root.join("workspace");
    std::fs::create_dir_all(foreign.join("src")).unwrap();
    let stale = stale_run_dir(&root);

    idle_pool_agent(config).await;

    assert!(foreign.join("src").is_dir());
    assert!(!stale.exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_pool_agent_skips_the_sweep_while_another_agent_holds_the_root() {
    let api = FakeApi::routed(|_, _| nothing_yet()).await;
    let config = config(&api);
    let root = config.work_root.clone();
    let live = stale_run_dir(&root);
    let first = RootLock::exclusive(&root).expect("the first agent takes the root");

    idle_pool_agent(config).await;

    assert!(live.exists(), "a second agent wiped the first one's run");
    drop(first);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_pool_agent_skips_the_sweep_while_a_dispatch_run_is_live() {
    let api = FakeApi::routed(|_, _| nothing_yet()).await;
    let config = config(&api);
    let root = config.work_root.clone();
    let live = stale_run_dir(&root);
    let dispatch = RootLock::shared(&root).expect("a dispatch run takes a shared lock");

    idle_pool_agent(config).await;

    assert!(live.exists(), "the sweep wiped a live dispatch run");
    drop(dispatch);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_pool_agent_sweeps_once_the_previous_holder_is_gone() {
    let api = FakeApi::routed(|_, _| nothing_yet()).await;
    let config = config(&api);
    let root = config.work_root.clone();
    let stale = stale_run_dir(&root);
    drop(RootLock::exclusive(&root));

    idle_pool_agent(config).await;

    assert!(!stale.exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_dispatch_run_holds_the_root_until_it_ends() {
    let api = FakeApi::routed(|r, _| {
        if r.path.contains("dispatch") {
            leased(30)
        } else {
            Reply::status(200)
        }
    })
    .await;
    let config = config(&api);
    let root = config.work_root.clone();
    let observed = Arc::new(Mutex::new(None::<bool>));
    let agent = Agent::new(config, TIMINGS, {
        let observed = observed.clone();
        let root = root.clone();
        move |_, _| {
            *observed.lock().unwrap() = Some(RootLock::exclusive(&root).is_none());
            async { Ok(RunOutcome::Completed { exit_code: 0 }) }
        }
    });

    agent
        .dispatch("agent-1", &serde_json::json!({}), None)
        .await
        .unwrap();

    assert_eq!(*observed.lock().unwrap(), Some(true));
    assert!(
        RootLock::exclusive(&root).is_some(),
        "the lock outlived the run"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn clearing_stale_checkouts_keeps_the_work_root_itself() {
    let api = FakeApi::routed(|_, _| nothing_yet()).await;
    let config = config(&api);
    let root = config.work_root.clone();
    stale_run_dir(&root);

    idle_pool_agent(config).await;

    assert!(root.is_dir(), "the root is the volume mount point");
    let left: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(left, [std::ffi::OsString::from(".ferrfleet.lock")]);
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_missing_work_root_does_not_stop_a_pool_agent() {
    let api = FakeApi::routed(|_, _| nothing_yet()).await;
    let config = config(&api);
    assert!(!config.work_root.exists());

    idle_pool_agent(config).await;
}

#[cfg(unix)]
#[tokio::test]
async fn clearing_stale_checkouts_never_follows_a_symlink_out_of_the_root() {
    let api = FakeApi::routed(|_, _| nothing_yet()).await;
    let config = config(&api);
    let root = config.work_root.clone();
    let outside = std::env::temp_dir().join(format!("ferrfleet-outside-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep"), b"x").unwrap();
    std::fs::create_dir_all(&root).unwrap();
    let link = root.join(uuid::Uuid::new_v4().to_string());
    std::os::unix::fs::symlink(&outside, &link).unwrap();

    idle_pool_agent(config).await;

    assert!(outside.join("keep").exists());
    assert!(link.is_symlink());
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&outside);
}

#[tokio::test]
async fn an_ephemeral_agent_leaves_the_work_root_alone() {
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
    let stale = stale_run_dir(&root);
    let agent = Agent::new(config, TIMINGS, |_, _| async {
        Ok(RunOutcome::Completed { exit_code: 0 })
    });

    agent
        .serve(Mode::Ephemeral, watch::channel(false).1)
        .await
        .unwrap();

    assert!(stale.exists());
    let _ = std::fs::remove_dir_all(&root);
}

const DISPATCH_PATH: &str = "/runner-pools/dispatch";

fn is_dispatch(r: &Received) -> bool {
    r.path == DISPATCH_PATH
}

fn pr_context() -> serde_json::Value {
    serde_json::json!({ "repo": "acme/api", "pr_number": 42 })
}

#[tokio::test]
async fn a_dispatch_creates_one_run_and_executes_it_like_a_lease() {
    let api = FakeApi::routed(|r, _| {
        if is_dispatch(r) {
            leased(30)
        } else {
            Reply::status(200)
        }
    })
    .await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(config(&api), TIMINGS, {
        let seen = seen.clone();
        move |env, _| {
            seen.lock().unwrap().push((env.run_id, env.run_token));
            async { Ok(RunOutcome::Completed { exit_code: 0 }) }
        }
    });

    let ended = agent
        .dispatch("pr-agent", &pr_context(), None)
        .await
        .unwrap();

    assert_eq!(ended, Ended::RunDone);
    assert_eq!(
        *seen.lock().unwrap(),
        [(RUN_ID.to_owned(), RUN_TOKEN.to_owned())]
    );
    let received = api.received();
    let dispatch = received.iter().find(|r| is_dispatch(r)).unwrap();
    assert_eq!(dispatch.method, "POST");
    assert_eq!(
        dispatch.authorization.as_deref(),
        Some(&format!("Bearer {POOL_TOKEN}") as &str)
    );
    let body: serde_json::Value = serde_json::from_str(&dispatch.body).unwrap();
    assert_eq!(
        body,
        serde_json::json!({ "agent_id": "pr-agent", "context": pr_context() })
    );
    assert_eq!(count(&api, is_lease), 0, "a dispatch must not poll");
    assert_eq!(count(&api, is_dispatch), 1);
}

#[tokio::test]
async fn a_failed_run_ends_the_dispatch_as_failed() {
    let api = FakeApi::routed(|r, _| {
        if is_dispatch(r) {
            leased(30)
        } else {
            Reply::status(200)
        }
    })
    .await;
    let agent = Agent::new(config(&api), TIMINGS, |_, _| async {
        Ok(RunOutcome::Completed { exit_code: 3 })
    });

    let ended = agent
        .dispatch("pr-agent", &pr_context(), None)
        .await
        .unwrap();

    assert_eq!(ended, Ended::RunFailed);
}

#[tokio::test]
async fn a_dispatch_records_the_run_id_for_the_caller() {
    let api = FakeApi::routed(|r, _| {
        if is_dispatch(r) {
            leased(30)
        } else {
            Reply::status(200)
        }
    })
    .await;
    let agent = Agent::new(config(&api), TIMINGS, |_, _| async {
        Ok(RunOutcome::Completed { exit_code: 0 })
    });
    let file = std::env::temp_dir().join(format!("run-id-{}", uuid::Uuid::new_v4()));

    agent
        .dispatch("pr-agent", &pr_context(), Some(&file))
        .await
        .unwrap();

    assert_eq!(std::fs::read_to_string(&file).unwrap(), RUN_ID);
    let _ = std::fs::remove_file(&file);
}

async fn a_refused_dispatch_says_what_the_api_said(status: u16, message: &'static str) {
    let api = FakeApi::routed(move |r, _| {
        if is_dispatch(r) {
            Reply::json(status, &serde_json::json!({ "message": message }))
        } else {
            Reply::status(200)
        }
    })
    .await;
    let ran = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(config(&api), TIMINGS, {
        let ran = ran.clone();
        move |_, _| {
            ran.fetch_add(1, Ordering::SeqCst);
            async { Ok(RunOutcome::Completed { exit_code: 0 }) }
        }
    });

    let err = agent
        .dispatch("pr-agent", &pr_context(), None)
        .await
        .expect_err("a refusal is an error");

    let text = format!("{err:#}");
    assert!(text.contains(message), "{text}");
    assert!(text.contains(&status.to_string()), "{text}");
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    assert_eq!(count(&api, is_dispatch), 1, "a refusal was retried");
}

#[tokio::test]
async fn a_dispatch_for_an_agent_outside_the_pool_is_refused_with_the_message() {
    a_refused_dispatch_says_what_the_api_said(404, "no such agent in this pool").await;
}

#[tokio::test]
async fn a_dispatch_with_a_bad_token_is_refused_with_the_message() {
    a_refused_dispatch_says_what_the_api_said(401, "pool token revoked").await;
}

#[tokio::test]
async fn a_refusal_without_json_still_carries_the_body() {
    let api = FakeApi::routed(|r, _| {
        if is_dispatch(r) {
            Reply::text(404, "no route for this pool")
        } else {
            Reply::status(200)
        }
    })
    .await;

    let err = finishing_agent(&api)
        .dispatch("pr-agent", &pr_context(), None)
        .await
        .expect_err("a 404 is an error");

    let text = format!("{err:#}");
    assert!(
        text.contains("404") && text.contains("no route for this pool"),
        "{text}"
    );
}

#[tokio::test]
async fn a_dispatch_rejected_as_malformed_is_not_retried() {
    a_refused_dispatch_says_what_the_api_said(400, "head_sha does not match").await;
}

fn sha_context() -> serde_json::Value {
    serde_json::json!({ "repo": "acme/api", "pr_number": 42, "head_sha": "0123abc" })
}

fn finishing_agent(
    api: &FakeApi,
) -> Agent<impl Fn(Env, EventSender) -> std::future::Ready<anyhow::Result<RunOutcome>>> {
    Agent::new(config(api), TIMINGS, |_, _| {
        std::future::ready(Ok(RunOutcome::Completed { exit_code: 0 }))
    })
}

async fn dispatch_fails_then_leases(failure: u16, failures: usize) -> FakeApi {
    let seen = AtomicUsize::new(0);
    FakeApi::routed(move |r, _| match is_dispatch(r) {
        true if seen.fetch_add(1, Ordering::SeqCst) < failures => Reply::status(failure),
        true => leased(30),
        false => Reply::status(200),
    })
    .await
}

#[tokio::test]
async fn a_dispatch_retries_a_503_until_the_api_accepts_it() {
    let api = dispatch_fails_then_leases(503, 2).await;

    let ended = finishing_agent(&api)
        .dispatch("pr-agent", &pr_context(), None)
        .await
        .unwrap();

    assert_eq!(ended, Ended::RunDone);
    assert_eq!(count(&api, is_dispatch), 3);
}

#[tokio::test]
async fn a_dispatch_retries_a_429_until_the_api_accepts_it() {
    let api = dispatch_fails_then_leases(429, 1).await;

    let ended = finishing_agent(&api)
        .dispatch("pr-agent", &pr_context(), None)
        .await
        .unwrap();

    assert_eq!(ended, Ended::RunDone);
    assert_eq!(count(&api, is_dispatch), 2);
}

#[tokio::test]
async fn a_dispatch_retries_while_the_api_is_unreachable() {
    let api = FakeApi::start_later(Duration::from_millis(150), |_| leased(30));

    let ended = finishing_agent(&api)
        .dispatch("pr-agent", &pr_context(), None)
        .await
        .unwrap();

    assert_eq!(ended, Ended::RunDone);
    assert_eq!(count(&api, is_dispatch), 1);
}

#[tokio::test]
async fn a_dispatch_to_an_api_that_stays_down_gives_up_within_the_budget() {
    let url = crate::fake_api::dead_api_url();
    let mut cfg = config(&FakeApi::start(|_| Reply::status(200)).await);
    cfg.api_url = url;
    let agent = Agent::new(cfg, TIMINGS, |_, _| async {
        Ok(RunOutcome::Completed { exit_code: 0 })
    });

    let started = Instant::now();
    let err = agent
        .dispatch("pr-agent", &pr_context(), None)
        .await
        .expect_err("a dead api cannot dispatch");

    assert!(format!("{err:#}").contains("giving up"), "{err:#}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_dispatch_retried_on_a_503_is_bounded_by_the_budget() {
    let api = FakeApi::start(|_| Reply::status(503)).await;

    let started = Instant::now();
    let err = finishing_agent(&api)
        .dispatch("pr-agent", &pr_context(), None)
        .await
        .expect_err("a 503 that never ends is an error");

    let text = format!("{err:#}");
    assert!(text.contains("503") && text.contains("giving up"), "{text}");
    let attempts = count(&api, is_dispatch);
    assert!((3..=8).contains(&attempts), "{attempts} attempts");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_conflict_stops_the_dispatch_with_a_clear_error() {
    let api = dispatch_fails_then_leases(409, usize::MAX).await;

    let err = finishing_agent(&api)
        .dispatch("pr-agent", &sha_context(), None)
        .await
        .expect_err("a run is already in flight");

    let text = format!("{err:#}");
    assert!(text.contains("409") && text.contains("in flight"), "{text}");
    assert_eq!(count(&api, is_dispatch), 1, "a 409 was retried");
}

#[tokio::test]
async fn a_conflict_after_a_retry_points_at_the_run_our_first_attempt_created() {
    let seen = AtomicUsize::new(0);
    let api = FakeApi::routed(move |r, _| match is_dispatch(r) {
        true if seen.fetch_add(1, Ordering::SeqCst) == 0 => Reply::status(500),
        true => Reply::status(409),
        false => Reply::status(200),
    })
    .await;

    let err = finishing_agent(&api)
        .dispatch("pr-agent", &sha_context(), None)
        .await
        .expect_err("the run exists but its lease was lost");

    let text = format!("{err:#}");
    assert!(text.contains("earlier attempt"), "{text}");
    assert_eq!(count(&api, is_dispatch), 2);
}

#[tokio::test]
async fn a_conflict_without_a_commit_does_not_claim_a_commit_is_in_flight() {
    let api = dispatch_fails_then_leases(409, usize::MAX).await;

    let err = finishing_agent(&api)
        .dispatch("pr-agent", &pr_context(), None)
        .await
        .expect_err("the API refused the dispatch");

    let text = format!("{err:#}");
    assert!(
        text.contains("409") && !text.contains("this commit"),
        "{text}"
    );
    assert_eq!(count(&api, is_dispatch), 1, "a 409 was retried");
}

#[tokio::test]
async fn a_dispatch_without_a_commit_does_not_retry_a_500() {
    let api = dispatch_fails_then_leases(500, 1).await;

    let err = finishing_agent(&api)
        .dispatch("pr-agent", &pr_context(), None)
        .await
        .expect_err("a 500 may have created the run");

    assert!(format!("{err:#}").contains("500"), "{err:#}");
    assert_eq!(count(&api, is_dispatch), 1);
}

#[tokio::test]
async fn a_dispatch_pinned_to_a_commit_retries_a_500() {
    let api = dispatch_fails_then_leases(500, 1).await;

    let ended = finishing_agent(&api)
        .dispatch("pr-agent", &sha_context(), None)
        .await
        .unwrap();

    assert_eq!(ended, Ended::RunDone);
    assert_eq!(count(&api, is_dispatch), 2);
}
