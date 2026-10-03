use std::future::Future;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::RunOutcome;
use crate::config::{AgentConfig, Env};
use crate::lease::LeaseState;
use crate::retry::Backoff;
use crate::sender::EventSender;

mod heartbeat;
mod pool;
#[cfg(test)]
mod tests;

use heartbeat::Heartbeat;
use pool::{Lease, Poll, PoolClient};

const USAGE: &str = "usage: ferrfleet-runner agent [--ephemeral]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Loop,
    Ephemeral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Shutdown,
    RunDone,
    RunFailed,
}

#[derive(Debug, Clone, Copy)]
pub struct Timings {
    pub api: Backoff,
    pub api_timeout: Duration,
    pub lease_timeout: Duration,
    pub poll_retry_first: Duration,
    pub poll_retry_max: Duration,
    pub second: Duration,
}

impl Timings {
    pub const PRODUCTION: Self = Self {
        api: Backoff::API_REDEPLOY,
        api_timeout: EventSender::TIMEOUT,
        lease_timeout: Duration::from_secs(60),
        poll_retry_first: Duration::from_secs(1),
        poll_retry_max: Duration::from_secs(30),
        second: Duration::from_secs(1),
    };
}

pub async fn run_subcommand(args: &[String]) -> Result<()> {
    let mode = match args {
        [] => Mode::Loop,
        [flag] if flag == "--ephemeral" => Mode::Ephemeral,
        _ => bail!(USAGE),
    };
    let config = AgentConfig::from_env().context("loading the agent environment")?;
    info!(runner = %config.runner_name, ?mode, "taking runs from the pool");

    let agent = Agent::new(config, Timings::PRODUCTION, |env, sender| async move {
        crate::supervise(&env, &sender).await
    });
    match agent.serve(mode, shutdown_on_signal()?).await? {
        Ended::Shutdown | Ended::RunDone => Ok(()),
        Ended::RunFailed => bail!("the run failed"),
    }
}

fn shutdown_on_signal() -> Result<watch::Receiver<bool>> {
    let mut term = signal(SignalKind::terminate()).context("listening for SIGTERM")?;
    let mut int = signal(SignalKind::interrupt()).context("listening for SIGINT")?;
    let (tx, rx) = watch::channel(false);
    tokio::spawn(async move {
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
        info!("shutdown requested; finishing the current run, if any, and taking no other");
        let _ = tx.send(true);
        tx.closed().await;
    });
    Ok(rx)
}

async fn requested(shutdown: &mut watch::Receiver<bool>) {
    if shutdown.wait_for(|stop| *stop).await.is_err() {
        std::future::pending::<()>().await;
    }
}

pub struct Agent<X> {
    config: AgentConfig,
    pool: PoolClient,
    timings: Timings,
    execute: X,
}

impl<X, F> Agent<X>
where
    X: Fn(Env, EventSender) -> F,
    F: Future<Output = Result<RunOutcome>>,
{
    pub fn new(config: AgentConfig, timings: Timings, execute: X) -> Self {
        Self {
            pool: PoolClient::new(&config, timings.lease_timeout),
            config,
            timings,
            execute,
        }
    }

    pub async fn serve(&self, mode: Mode, mut shutdown: watch::Receiver<bool>) -> Result<Ended> {
        let mut retry_delay = self.timings.poll_retry_first;
        loop {
            let poll = tokio::select! {
                biased;
                () = requested(&mut shutdown) => return Ok(Ended::Shutdown),
                poll = self.pool.lease() => poll?,
            };
            match poll {
                Poll::Leased(lease) => {
                    retry_delay = self.timings.poll_retry_first;
                    let ended = self.take(lease).await;
                    if mode == Mode::Ephemeral {
                        return Ok(ended);
                    }
                    continue;
                }
                Poll::Empty => {
                    retry_delay = self.timings.poll_retry_first;
                    continue;
                }
                Poll::Throttled => warn!(?retry_delay, "rate limited by FerrFleet; backing off"),
                Poll::Unavailable(err) => {
                    warn!(?err, ?retry_delay, "could not reach FerrFleet; backing off");
                }
            }
            tokio::select! {
                biased;
                () = requested(&mut shutdown) => return Ok(Ended::Shutdown),
                () = tokio::time::sleep(retry_delay) => {}
            }
            retry_delay = (retry_delay * 2).min(self.timings.poll_retry_max);
        }
    }

    async fn take(&self, lease: Lease) -> Ended {
        let run_id = lease.run_id.to_string();
        let every = lease.heartbeat_every(self.timings.second);
        info!(%run_id, "run leased");
        let workdir = self.config.work_root.join(&run_id);
        let env = Env {
            api_url: self.config.api_url.clone(),
            run_id,
            working_dir: Some(workdir.to_string_lossy().into_owned()),
            runner_name: Some(self.config.runner_name.clone()),
            run_token: lease.run_token,
        };
        let sender =
            EventSender::configured(env.clone(), self.timings.api, self.timings.api_timeout)
                .with_lease(LeaseState::default());

        let heartbeat = Heartbeat::start(sender.clone(), every);
        let outcome = (self.execute)(env, sender).await;
        drop(heartbeat);

        remove_workdir(&workdir);
        verdict(outcome)
    }
}

fn verdict(outcome: Result<RunOutcome>) -> Ended {
    match outcome {
        Ok(RunOutcome::Completed { exit_code: 0 }) => {
            info!("run completed");
            Ended::RunDone
        }
        Ok(RunOutcome::Completed { exit_code }) => {
            info!(exit_code, "run completed with a failure");
            Ended::RunFailed
        }
        Ok(RunOutcome::TakenElsewhere) => Ended::RunDone,
        Ok(RunOutcome::Stopped(lost)) => {
            info!(%lost, "run stopped by FerrFleet");
            Ended::RunDone
        }
        Err(err) => {
            error!(?err, "run failed in the runner");
            Ended::RunFailed
        }
    }
}

fn remove_workdir(workdir: &Path) {
    match std::fs::remove_dir_all(workdir) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            warn!(?err, path = %workdir.display(), "could not remove the run's working directory");
        }
    }
}
