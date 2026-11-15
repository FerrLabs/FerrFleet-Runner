use std::time::Duration;

use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

use crate::resources::{SAMPLE_EVERY, Sampler};
use crate::sender::EventSender;

pub struct Heartbeat(JoinHandle<()>);

impl Heartbeat {
    pub fn start(sender: EventSender, every: Duration) -> Self {
        Self(tokio::spawn(async move {
            let mut sampler = Sampler::start();
            let mut beats = interval(every);
            beats.set_missed_tick_behavior(MissedTickBehavior::Delay);
            let mut sample_ticks = interval(SAMPLE_EVERY);
            sample_ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = sample_ticks.tick() => {
                        if let Some(sampler) = sampler.as_mut() {
                            sampler.sample();
                        }
                        continue;
                    }
                    _ = beats.tick() => {}
                }
                let resources = sampler.as_ref().and_then(Sampler::resources);
                let Err(err) = sender.heartbeat(resources.as_ref()).await else {
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
