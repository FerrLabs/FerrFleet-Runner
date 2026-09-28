use std::time::{Duration, Instant};

use chrono::Utc;
use ferrfleet_shared::ExecutorEvent;

use super::{Claim, EventSender};
use crate::fake_api::{FAST, FakeApi, RUN_ID, Reply, dead_api_url, sender};
use crate::retry::Backoff;

fn event() -> ExecutorEvent {
    ExecutorEvent::AssistantMessage {
        content: "hello".to_owned(),
        timestamp: Utc::now(),
    }
}

fn run_config() -> serde_json::Value {
    serde_json::json!({
        "run_id": RUN_ID,
        "agent_id": "pr-agent",
        "prompt": "review it",
        "working_dir": "/workdir",
    })
}

fn reqwest_error(err: &anyhow::Error) -> &reqwest::Error {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<reqwest::Error>())
        .expect("a reqwest error in the chain")
}

#[tokio::test]
async fn a_502_or_503_is_retried_until_the_api_answers() {
    for status in [502, 503] {
        let api = FakeApi::start(move |n| Reply::status(if n < 3 { status } else { 202 })).await;

        sender(&api.url)
            .send(&event())
            .await
            .unwrap_or_else(|err| panic!("{status} then 202 should deliver: {err:#}"));

        assert_eq!(api.received().len(), 4, "{status} was not retried");
    }
}

#[tokio::test]
async fn a_refused_connection_is_retried_until_the_api_listens() {
    let api = FakeApi::start_later(Duration::from_millis(150), |_| {
        Reply::json(200, &run_config())
    });

    let cfg = sender(&api.url)
        .fetch_config()
        .await
        .expect("the config once the api listens");

    assert_eq!(cfg.run_id.to_string(), RUN_ID);
    assert_eq!(api.received().len(), 1);
}

#[tokio::test]
async fn ambiguous_failures_and_client_errors_are_sent_once() {
    for status in [400, 401, 404, 422, 500, 504] {
        let api = FakeApi::start(move |_| Reply::status(status)).await;

        let err = sender(&api.url)
            .send(&event())
            .await
            .expect_err("a failure status is an error");

        assert_eq!(api.received().len(), 1, "{status} was retried");
        assert_eq!(
            reqwest_error(&err).status().map(|s| s.as_u16()),
            Some(status)
        );
    }
}

#[tokio::test]
async fn a_client_error_still_reads_as_one_in_the_error_chain() {
    let api = FakeApi::start(|_| Reply::status(404)).await;

    let err = sender(&api.url)
        .fetch_config()
        .await
        .expect_err("404 is an error");

    assert!(
        format!("{err:?}").contains("HTTP status client error"),
        "the image TLS check greps for this: {err:?}"
    );
}

#[tokio::test]
async fn a_request_that_timed_out_is_not_sent_again() {
    let api = FakeApi::start(|_| Reply::status(202).after(Duration::from_secs(2))).await;
    let sender = EventSender::for_tests(&api.url, FAST, Duration::from_millis(200));

    let err = sender
        .send(&event())
        .await
        .expect_err("the reply comes after the timeout");
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert!(reqwest_error(&err).is_timeout(), "{err:#}");
    assert_eq!(
        api.received().len(),
        1,
        "a possibly applied event was resent"
    );
}

#[tokio::test]
async fn a_dead_api_is_given_up_on_once_the_budget_is_spent() {
    let backoff = Backoff {
        first_delay: Duration::from_millis(10),
        max_delay: Duration::from_millis(40),
        budget: Duration::from_millis(400),
    };
    let sender = EventSender::for_tests(&dead_api_url(), backoff, Duration::from_secs(2));

    let started = Instant::now();
    let err = sender
        .fetch_config()
        .await
        .expect_err("nothing ever listens");
    let elapsed = started.elapsed();

    assert!(reqwest_error(&err).is_connect(), "{err:#}");
    assert!(
        elapsed + backoff.max_delay >= backoff.budget,
        "gave up after {elapsed:?}, before the budget was spent"
    );
    assert!(
        elapsed < backoff.budget + Duration::from_secs(1),
        "still retrying {elapsed:?} into a {:?} budget",
        backoff.budget
    );
}

#[tokio::test]
async fn a_conflict_behind_a_503_is_still_already_taken() {
    let api = FakeApi::start(|n| Reply::status(if n == 0 { 503 } else { 409 })).await;

    let claim = sender(&api.url)
        .claim_run()
        .await
        .expect("a conflict is an answer");

    assert!(matches!(claim, Claim::AlreadyTaken));
    let received = api.received();
    assert_eq!(received.len(), 2);
    assert!(
        received
            .iter()
            .all(|r| r.method == "POST" && r.path == format!("/runs/{RUN_ID}/claim"))
    );
}
