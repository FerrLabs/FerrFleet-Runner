use anyhow::{Context, Result, bail};
use chrono::Utc;
use ferrfleet_shared::{ExecutorEvent, RunConfig, pricing};
use tracing::{error, info, warn};

mod agent;
mod claude;
mod claude_stream;
mod claude_version;
mod config;
mod event_queue;
#[cfg(test)]
mod fake_api;
mod lease;
mod mcp_config;
mod private_file;
mod repo_context;
mod retry;
mod review_threads;
mod sender;
mod subcommands;
mod usage;
mod workspace;

use claude::{INSIDE_AGENT_VAR, Streamed, TokenTotals, spawn_and_stream};
use config::Env;
use event_queue::EventQueue;
use lease::Lost;
use sender::EventSender;

const RUNNER_USAGE: &str = "usage: ferrfleet-runner result '<json>' | @<file>
       ferrfleet-runner resolve-thread <thread_id>
       ferrfleet-runner pull-request <url>";

fn check_supervisor_invocation(args: &[String], inside_agent: bool) -> Result<()> {
    if !args.is_empty() {
        bail!("unknown arguments {args:?}\n{RUNNER_USAGE}");
    }
    if inside_agent {
        bail!(
            "ferrfleet-runner without a subcommand starts a run; an agent cannot start one\n{RUNNER_USAGE}"
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    install_crypto_provider()?;

    // Sous-commandes dediees, appelees par l'agent lui-meme (pas par le
    // superviseur de run). Verifiees avant tout le reste: elles ne doivent
    // jamais changer le comportement de l'invocation sans argument, qui reste
    // le chemin normal d'execution d'un run.
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("resolve-thread") => return subcommands::resolve_thread(&args).await,
        Some("pull-request") => return subcommands::pull_request(&args).await,
        Some("result") => return subcommands::result(&args).await,
        Some("agent") => return agent::run_subcommand(&args[2..]).await,
        Some("dispatch") => return agent::dispatch_subcommand(&args[2..]).await,
        _ => {}
    }
    check_supervisor_invocation(&args[1..], std::env::var_os(INSIDE_AGENT_VAR).is_some())?;

    let env = Env::from_env().context("loading environment")?;
    info!(run_id = %env.run_id, "starting runner");

    let sender = EventSender::new(env.clone());
    supervise(&env, &sender).await?;
    info!(run_id = %env.run_id, "runner exited cleanly");
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Completed { exit_code: i32 },
    TakenElsewhere,
    Stopped(Lost),
}

async fn supervise(env: &Env, sender: &EventSender) -> Result<RunOutcome> {
    let queue = EventQueue::start(sender.clone());

    let outcome = match (run(env, sender, &queue).await, sender.lost()) {
        (_, Some(lost)) => {
            info!(run_id = %env.run_id, %lost, "run stopped; nothing more is reported");
            Ok(RunOutcome::Stopped(lost))
        }
        (Ok(outcome), None) => Ok(outcome),
        (Err(err), None) => {
            error!(?err, "runner failed");
            queue.push(ExecutorEvent::Error {
                message: err.to_string(),
                provider_signal: false,
                timestamp: Utc::now(),
            });
            queue.push(ExecutorEvent::Completed {
                exit_code: 1,
                session_id: None,
                timestamp: Utc::now(),
            });
            Err(err)
        }
    };
    queue.flush().await;
    outcome
}

async fn run(env: &Env, sender: &EventSender, queue: &EventQueue) -> Result<RunOutcome> {
    let mut cfg = sender.fetch_config().await?;
    if cfg.run_id.to_string() != env.run_id {
        bail!(
            "config run_id ({}) does not match env FERRFLEET_RUN_ID ({})",
            cfg.run_id,
            env.run_id
        );
    }

    // Before anything observable happens, and before any credential is
    // fetched. An external run has no Job standing behind it, so this is where
    // "exactly once" comes from: a second runner started by a re-run of the
    // same workflow loses the claim and stops here, having touched nothing.
    if cfg.runner_mode.is_external() {
        match sender.claim_run().await? {
            sender::Claim::Granted => info!(run_id = %cfg.run_id, "run claimed"),
            sender::Claim::AlreadyTaken => {
                info!(run_id = %cfg.run_id, "run already claimed by another runner; nothing to do");
                return Ok(RunOutcome::TakenElsewhere);
            }
        }
    }

    apply_working_dir_override(&mut cfg, env.working_dir.as_deref())?;

    // Garde le canal d'authentification git vivant pour toute la duree du
    // run: c'est l'agent qui pousse sa branche, pas le runner, et sans
    // `GIT_ASKPASS` le push echoue systematiquement. Le script est supprime
    // quand cette variable sort de portee, a la fin du run.
    let git_credentials = if let Some(checkout) = cfg.checkout.as_ref() {
        Some(
            workspace::prepare(checkout, std::path::Path::new(&cfg.working_dir), env)
                .await
                .context("preparation du depot de travail")?,
        )
    } else {
        None
    };

    let started = std::time::Instant::now();
    let mut totals = TokenTotals::default();
    let mut session_id = cfg.session_id.clone();

    let exit_code = match spawn_and_stream(
        &cfg,
        env,
        queue,
        &mut totals,
        &mut session_id,
        git_credentials.as_ref(),
        sender.wait_lost(),
    )
    .await
    {
        Ok(Streamed::Exited(code)) => code,
        Ok(Streamed::Stopped(lost)) => return Ok(RunOutcome::Stopped(lost)),
        Err(err) => {
            warn!(?err, "claude execution failed");
            queue.push(ExecutorEvent::Error {
                message: format!("claude execution failed: {err:#}"),
                provider_signal: false,
                timestamp: Utc::now(),
            });
            -1
        }
    };

    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    // The runner has no DB access to look up per-model rates, so it keeps the
    // fixed rate table for this display-only figure; the API recomputes the
    // real charge from `credits::pricing` at settlement.
    #[allow(deprecated, reason = "runner has no DB access to per-model rates")]
    let cost_usd = pricing::cost_usd_for_sonnet(
        totals.input,
        totals.output,
        totals.cache_creation,
        totals.cache_read,
    );

    info!(
        exit_code,
        tokens_in = totals.input,
        tokens_out = totals.output,
        cost_usd,
        duration_ms,
        "claude run finished"
    );

    // A clean exit that produced nothing is not a success. It happens when
    // claude cannot start work at all — no credentials, an unreachable MCP
    // server, a prompt it refuses — and it exits 0 within a second having
    // opened no session and spent no tokens. Reporting that as `completed`
    // hides the failure: the run looks fine in the UI while nothing was
    // reviewed. Both real symptoms cost a full turn each before being noticed.
    let produced_nothing = session_id.is_none() && totals.input == 0 && totals.output == 0;
    let exit_code = if exit_code == 0 && produced_nothing {
        warn!(
            duration_ms,
            "claude exited cleanly without starting a session or spending tokens — reporting as failed"
        );
        queue.push(ExecutorEvent::Error {
            message: "claude exited 0 without starting a session or spending any tokens; \
                      the run produced no output (check credentials and MCP servers)"
                .to_owned(),
            provider_signal: false,
            timestamp: Utc::now(),
        });
        1
    } else {
        exit_code
    };

    queue.push(ExecutorEvent::Completed {
        exit_code,
        session_id,
        timestamp: Utc::now(),
    });
    Ok(RunOutcome::Completed { exit_code })
}

fn apply_working_dir_override(cfg: &mut RunConfig, working_dir: Option<&str>) -> Result<()> {
    let Some(dir) = working_dir else {
        return Ok(());
    };
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creation du repertoire de travail {dir}"))?;
    dir.clone_into(&mut cfg.working_dir);
    Ok(())
}

#[cfg(test)]
mod working_dir_tests {
    use super::*;

    fn config(working_dir: &str) -> RunConfig {
        serde_json::from_value(serde_json::json!({
            "run_id": "01999999-9999-7999-9999-999999999999",
            "agent_id": "pr-agent",
            "prompt": "review it",
            "working_dir": working_dir,
        }))
        .expect("building the fixture")
    }

    #[test]
    fn an_override_replaces_the_api_path_and_creates_it() {
        let dir = std::env::temp_dir().join(format!("ferrfleet-wd-{}", uuid::Uuid::new_v4()));
        let dir = dir.to_string_lossy().into_owned();
        let mut cfg = config("/workdir");

        apply_working_dir_override(&mut cfg, Some(&dir)).expect("applying the override");

        assert_eq!(cfg.working_dir, dir);
        assert!(std::path::Path::new(&dir).is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_override_leaves_the_api_path_alone() {
        let mut cfg = config("/workdir");

        apply_working_dir_override(&mut cfg, None).expect("applying no override");

        assert_eq!(cfg.working_dir, "/workdir");
    }

    #[test]
    fn an_override_pointing_at_a_file_is_an_error_rather_than_a_silent_fallback() {
        let file = std::env::temp_dir().join(format!("ferrfleet-wd-{}", uuid::Uuid::new_v4()));
        std::fs::write(&file, b"not a directory").expect("writing the fixture");
        let mut cfg = config("/workdir");

        let err = apply_working_dir_override(&mut cfg, Some(&file.to_string_lossy()));

        assert!(err.is_err());
        assert_eq!(cfg.working_dir, "/workdir");
        let _ = std::fs::remove_file(&file);
    }
}

fn install_crypto_provider() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("a rustls crypto provider was already installed"))
}

fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().json())
        .init();
}

#[cfg(test)]
mod supervisor_invocation_tests {
    use super::*;

    #[test]
    fn only_a_bare_invocation_outside_an_agent_starts_a_run() {
        assert!(check_supervisor_invocation(&[], false).is_ok());

        let inside = check_supervisor_invocation(&[], true)
            .unwrap_err()
            .to_string();
        assert!(inside.contains("an agent cannot start one"), "{inside}");

        for args in [["--help"], ["help"], ["results"]] {
            let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
            for inside_agent in [false, true] {
                let err = check_supervisor_invocation(&args, inside_agent)
                    .unwrap_err()
                    .to_string();
                assert!(err.contains("usage: ferrfleet-runner result"), "{err}");
            }
        }
    }
}
