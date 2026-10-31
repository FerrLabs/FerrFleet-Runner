use std::time::Duration;

use reqwest::StatusCode;
use serde::Deserialize;
use tokio::time::{Instant, sleep};
use tracing::warn;
use uuid::Uuid;

use crate::config::{AgentConfig, PoolToken};
use crate::retry::Backoff;

#[derive(Deserialize)]
pub struct Lease {
    pub run_id: Uuid,
    pub run_token: String,
    heartbeat_every_secs: u64,
}

impl Lease {
    pub fn heartbeat_every(&self, second: Duration) -> Duration {
        let secs = u32::try_from(self.heartbeat_every_secs.max(1)).unwrap_or(u32::MAX);
        second.saturating_mul(secs)
    }
}

pub enum Poll {
    Leased(Lease),
    Empty,
    Throttled,
    Unavailable(anyhow::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum Refused {
    #[error(
        "FerrFleet refused the pool token (401): it is wrong, was rotated away, or its pool was revoked"
    )]
    Token,
    #[error("FerrFleet answered {0} to the lease request; check FERRFLEET_API_URL")]
    Status(StatusCode),
}

pub struct PoolClient {
    url: String,
    dispatch_url: String,
    dispatch_backoff: Backoff,
    token: PoolToken,
    runner: String,
    client: reqwest::Client,
}

impl PoolClient {
    pub fn new(config: &AgentConfig, timeout: Duration, dispatch_backoff: Backoff) -> Self {
        Self {
            url: format!("{}/runner-pools/lease", config.api_url),
            dispatch_url: format!("{}/runner-pools/dispatch", config.api_url),
            dispatch_backoff,
            token: config.pool_token.clone(),
            runner: config.runner_name.clone(),
            client: reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .expect("building reqwest client"),
        }
    }

    pub async fn lease(&self) -> Result<Poll, Refused> {
        let sent = self
            .client
            .post(&self.url)
            .bearer_auth(self.token.expose())
            .json(&serde_json::json!({ "runner": self.runner }))
            .send()
            .await;
        let resp = match sent {
            Ok(resp) => resp,
            Err(err) => return Ok(Poll::Unavailable(err.into())),
        };
        match resp.status() {
            StatusCode::OK => Ok(resp.json::<Lease>().await.map_or_else(
                |err| Poll::Unavailable(anyhow::Error::from(err).context("decoding the lease")),
                Poll::Leased,
            )),
            StatusCode::NO_CONTENT => Ok(Poll::Empty),
            StatusCode::TOO_MANY_REQUESTS => Ok(Poll::Throttled),
            StatusCode::UNAUTHORIZED => Err(Refused::Token),
            status if status.is_client_error() => Err(Refused::Status(status)),
            status => Ok(Poll::Unavailable(anyhow::anyhow!(
                "FerrFleet answered {status} to the lease request"
            ))),
        }
    }

    pub async fn dispatch(
        &self,
        agent_id: &str,
        context: &serde_json::Value,
    ) -> anyhow::Result<Lease> {
        let pinned = pins_a_commit(context);
        let deadline = Instant::now() + self.dispatch_backoff.budget;
        let mut delay = self.dispatch_backoff.first_delay;
        loop {
            let err = match self.dispatch_once(agent_id, context, pinned).await {
                Attempt::Done(lease) => return Ok(lease),
                Attempt::Failed(err) => return Err(err),
                Attempt::Retry(err) => err,
            };
            if Instant::now() + delay > deadline {
                return Err(err.context("giving up on dispatching the run"));
            }
            warn!(?err, ?delay, "dispatch not accepted; retrying");
            sleep(delay).await;
            delay = (delay * 2).min(self.dispatch_backoff.max_delay);
        }
    }

    async fn dispatch_once(
        &self,
        agent_id: &str,
        context: &serde_json::Value,
        pinned: bool,
    ) -> Attempt {
        let sent = self
            .client
            .post(&self.dispatch_url)
            .bearer_auth(self.token.expose())
            .json(&serde_json::json!({ "agent_id": agent_id, "context": context }))
            .send()
            .await;
        let resp = match sent {
            Ok(resp) => resp,
            Err(err) => {
                let err =
                    anyhow::Error::from(err).context("reaching FerrFleet to dispatch the run");
                return if pinned || is_connect_failure(&err) {
                    Attempt::Retry(err)
                } else {
                    Attempt::Failed(err)
                };
            }
        };
        let status = resp.status();
        if status == StatusCode::OK {
            return match resp.json().await {
                Ok(lease) => Attempt::Done(lease),
                Err(err) => {
                    Attempt::Failed(anyhow::Error::from(err).context("decoding the dispatched run"))
                }
            };
        }
        let body = resp.text().await.unwrap_or_default();
        let message = api_message(&body);
        if status == StatusCode::CONFLICT {
            return Attempt::Failed(anyhow::anyhow!(
                "FerrFleet already has a run in flight for this commit (409): {message}; not dispatching a second one"
            ));
        }
        let err = anyhow::anyhow!("FerrFleet refused to dispatch a run ({status}): {message}");
        if retryable(status, pinned) {
            Attempt::Retry(err)
        } else {
            Attempt::Failed(err)
        }
    }
}

enum Attempt {
    Done(Lease),
    Retry(anyhow::Error),
    Failed(anyhow::Error),
}

fn pins_a_commit(context: &serde_json::Value) -> bool {
    context
        .get("head_sha")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|sha| !sha.trim().is_empty())
}

fn retryable(status: StatusCode, pinned: bool) -> bool {
    match status {
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => true,
        status => pinned && status.is_server_error(),
    }
}

fn is_connect_failure(err: &anyhow::Error) -> bool {
    err.downcast_ref::<reqwest::Error>()
        .is_some_and(reqwest::Error::is_connect)
}

fn api_message(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|json| {
            ["message", "error"]
                .into_iter()
                .find_map(|key| json.get(key)?.as_str().map(str::to_owned))
        })
        .unwrap_or_else(|| body.trim().to_owned())
}
