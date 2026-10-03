use std::fmt;
use std::sync::Arc;

use reqwest::StatusCode;
use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lost {
    RunOver,
    Superseded,
}

impl fmt::Display for Lost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::RunOver => {
                "the run is over on FerrFleet's side (finished, cancelled or superseded)"
            }
            Self::Superseded => "this runner no longer holds the run's lease",
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[error("run stopped: {0}")]
pub struct Stopped(pub Lost);

#[derive(Clone)]
pub struct LeaseState(Arc<watch::Sender<Option<Lost>>>);

impl Default for LeaseState {
    fn default() -> Self {
        Self(Arc::new(watch::channel(None).0))
    }
}

impl LeaseState {
    pub fn observe(&self, status: StatusCode) {
        let lost = match status {
            StatusCode::CONFLICT => Lost::Superseded,
            StatusCode::GONE => Lost::RunOver,
            _ => return,
        };
        self.0.send_if_modified(|state| {
            if state.is_some() {
                return false;
            }
            *state = Some(lost);
            true
        });
    }

    pub fn lost(&self) -> Option<Lost> {
        *self.0.borrow()
    }

    pub async fn wait(&self) -> Lost {
        let mut rx = self.0.subscribe();
        if let Ok(state) = rx.wait_for(Option::is_some).await
            && let Some(lost) = *state
        {
            return lost;
        }
        std::future::pending().await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn the_first_answer_that_ends_the_lease_is_the_one_kept() {
        let lease = LeaseState::default();
        let waiting = tokio::spawn({
            let lease = lease.clone();
            async move { lease.wait().await }
        });

        lease.observe(StatusCode::OK);
        lease.observe(StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(lease.lost(), None, "only 409 and 410 end a lease");

        lease.observe(StatusCode::GONE);
        lease.observe(StatusCode::CONFLICT);

        let woken = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("the waiter is woken")
            .expect("the waiter did not panic");
        assert_eq!(woken, Lost::RunOver);
        assert_eq!(lease.lost(), Some(Lost::RunOver));
    }
}
