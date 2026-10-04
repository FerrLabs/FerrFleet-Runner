use std::time::Duration;

use reqwest::StatusCode;
use serde::Deserialize;
use uuid::Uuid;

use crate::config::{AgentConfig, PoolToken};

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
    token: PoolToken,
    runner: String,
    client: reqwest::Client,
}

impl PoolClient {
    pub fn new(config: &AgentConfig, timeout: Duration) -> Self {
        Self {
            url: format!("{}/runner-pools/lease", config.api_url),
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
}
