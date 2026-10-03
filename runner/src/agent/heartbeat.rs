use std::time::Duration;

use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

use crate::sender::EventSender;

pub struct Heartbeat(JoinHandle<()>);

impl Heartbeat {
    pub fn start(sender: EventSender, every: Duration) -> Self {
        Self(tokio::spawn(async move {
            let mut beats = interval(every);
            beats.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                beats.tick().await;
                let Err(err) = sender.heartbeat().await else {
                    continue;
                };
                if let Some(lost) = sender.lost() {
                    info!(%lost, "heartbeat refused; stopping the run");
                    return;
                }
                warn!(?err, "heartbeat failed; trying again on the next beat");
            }
        }))
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.0.abort();
    }
}
