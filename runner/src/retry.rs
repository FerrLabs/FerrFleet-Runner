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

pub async fn with_backoff<T, F, Fut>(
    backoff: Backoff,
    mut attempt: F,
    should_retry: impl Fn(&T) -> bool,
    on_retry: impl Fn(&T, Duration),
) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = T>,
{
    let deadline = Instant::now() + backoff.budget;
    let mut delay = backoff.first_delay;
    loop {
        let outcome = attempt().await;
        if !should_retry(&outcome) || Instant::now() + delay > deadline {
            return outcome;
        }
        on_retry(&outcome, delay);
        sleep(delay).await;
        delay = (delay * 2).min(backoff.max_delay);
    }
}

pub async fn while_unprocessed<F, Fut>(backoff: Backoff, attempt: F) -> reqwest::Result<Response>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = reqwest::Result<Response>>,
{
    with_backoff(backoff, attempt, surely_unprocessed, |outcome, delay| {
        match outcome {
            Ok(resp) => {
                warn!(status = %resp.status(), url = %resp.url(), ?delay, "api unavailable; retrying");
            }
            Err(err) => warn!(?err, ?delay, "api unreachable; retrying"),
        }
    })
    .await
}

fn surely_unprocessed(outcome: &reqwest::Result<Response>) -> bool {
    match outcome {
        Ok(resp) => resp.status() == StatusCode::SERVICE_UNAVAILABLE,
        Err(err) => err.is_connect(),
    }
}
