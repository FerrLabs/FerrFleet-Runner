use std::future::Future;
use std::time::Duration;

use reqwest::{Response, StatusCode};
use tokio::time::{Instant, sleep};
use tracing::warn;

#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub first_delay: Duration,
    pub max_delay: Duration,
    pub budget: Duration,
}

impl Backoff {
    pub const API_REDEPLOY: Self = Self {
        first_delay: Duration::from_secs(1),
        max_delay: Duration::from_secs(10),
        budget: Duration::from_secs(5 * 60),
    };
}

pub async fn while_unprocessed<F, Fut>(
    backoff: Backoff,
    mut attempt: F,
) -> reqwest::Result<Response>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = reqwest::Result<Response>>,
{
    let deadline = Instant::now() + backoff.budget;
    let mut delay = backoff.first_delay;
    loop {
        let outcome = attempt().await;
        if !surely_unprocessed(&outcome) || Instant::now() + delay > deadline {
            return outcome;
        }
        match &outcome {
            Ok(resp) => {
                warn!(status = %resp.status(), url = %resp.url(), ?delay, "api unavailable; retrying");
            }
            Err(err) => warn!(?err, ?delay, "api unreachable; retrying"),
        }
        sleep(delay).await;
        delay = (delay * 2).min(backoff.max_delay);
    }
}

fn surely_unprocessed(outcome: &reqwest::Result<Response>) -> bool {
    match outcome {
        Ok(resp) => matches!(
            resp.status(),
            StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE
        ),
        Err(err) => err.is_connect(),
    }
}
