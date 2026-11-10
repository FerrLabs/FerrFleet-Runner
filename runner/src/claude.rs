use crate::claude_stream::{reopens_current_session, translate};
use crate::config::Env;
use crate::event_queue::EventQueue;
use crate::lease::Lost;
use crate::private_file::PrivateFile;
use crate::repo_context::prompt_with_repo_context;
use crate::{claude_version, mcp_config, usage, workspace};
use anyhow::{Context, Result};
use ferrfleet_shared::{ExecutorEvent, RunConfig};
use std::future::Future;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tracing::{info, warn};

pub(crate) const INSIDE_AGENT_VAR: &str = "FERRFLEET_INSIDE_AGENT";

pub(crate) enum Streamed {
    Exited(i32),
    Stopped(Lost),
}

/// Appended to every run's system prompt, so an agent does not spend a turn
/// discovering its environment.
const RUNNER_ENVIRONMENT_NOTE: &str = "Environment: you are executing a FerrFleet agent run. \
Use the MCP GitHub tools for every GitHub operation. Do NOT use the GitHub CLI (`gh`) \
even where it is installed: it routes around the connector's denied-tool \
list. If an MCP GitHub tool is refused, that refusal is deliberate; do not \
look for another way around it. Available CLI tools are `git`, `curl`, `jq`, \
`python3` and `ferrfleet-runner`. Work in the current working directory: you \
may write there and in /tmp, nowhere else. When a repository is checked \
out, `git push` is already authenticated: push with plain `git push -u origin \
HEAD`, never with a token in the remote URL, and never with --force. \
`ferrfleet-runner pull-request <url>` records the pull request you opened, \
`ferrfleet-runner resolve-thread <thread_id>` resolves one review thread, and \
`ferrfleet-runner result '<json>'` records what you concluded, as a JSON \
object, for whatever reads this run next. Record one when the task has an \
answer worth acting on.";

fn appended_system_prompt(stable: Option<&str>) -> String {
    match stable {
        Some(stable) => format!("{stable}\n\n{RUNNER_ENVIRONMENT_NOTE}"),
        None => RUNNER_ENVIRONMENT_NOTE.to_owned(),
    }
}

/// Loads the KEY=VALUE lines of a credentials file into the claude subprocess,
/// for a host that provisions `CLAUDE_CODE_OAUTH_TOKEN` as a file rather than
/// in the process environment. The file is `CLAUDE_ENV_FILE`, defaulting to
/// `/vault/secrets/claude.env`.
///
/// None of our own deployments use it any more: managed Jobs and the pool
/// runner read the token from a Secret, and the GitHub Action passes it as an
/// environment variable, which the child inherits. A missing file is therefore
/// the normal case and not worth a warning: warning about it would train
/// operators to ignore the one message that means claude really will run
/// unauthenticated, with zero tokens and no session, which looks like a silent
/// no-op run.
fn load_claude_credentials(cmd: &mut Command) {
    let claude_env_path = std::env::var("CLAUDE_ENV_FILE")
        .unwrap_or_else(|_| "/vault/secrets/claude.env".to_string());
    match std::fs::read_to_string(&claude_env_path) {
        Ok(contents) => {
            let mut loaded = 0u32;
            for line in contents.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((key, value)) = line.split_once('=') {
                    cmd.env(key.trim(), value.trim());
                    loaded += 1;
                }
            }
            info!(path = %claude_env_path, vars = loaded, "loaded claude credentials from vault file");
        }
        Err(err) if inherits_a_claude_credential() => {
            info!(path = %claude_env_path, ?err, "no vault credentials file; using the credential already in the environment");
        }
        Err(err) => {
            warn!(path = %claude_env_path, ?err, "no claude credentials file; claude will run unauthenticated");
        }
    }
}

/// Whether the process already carries a credential `claude` will pick up.
///
/// Only the presence is read, never the value: this decides which log line to
/// write, and a secret has no business in that decision's inputs beyond
/// existing.
fn inherits_a_claude_credential() -> bool {
    ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
}

const HEADLESS_DISALLOWED_TOOLS: [&str; 3] = ["Monitor", "ScheduleWakeup", "CronCreate"];

/// Construit la commande `claude` du run : arguments, identifiants, canal
/// d'authentification git et configuration MCP.
fn build_claude_command(
    cfg: &RunConfig,
    env: &Env,
    git_credentials: Option<&workspace::GitCredentials>,
) -> Result<ClaudeInvocation> {
    let prompt = cfg.prompt_parts();
    let mut cmd = Command::new("claude");
    cmd.arg("-p")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose")
        .current_dir(&cfg.working_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    load_claude_credentials(&mut cmd);
    env.apply_to(&mut cmd);
    cmd.env(INSIDE_AGENT_VAR, "1");

    // Le push est fait par l'agent: `GIT_ASKPASS` va donc dans l'environnement
    // de CE sous-processus, pour que `git push` lance par l'agent
    // s'authentifie. Seul le chemin (non secret) du script y va : le script
    // recupere lui-meme un token aupres de l'API a chaque invocation, il
    // n'est jamais pose dans une variable heritee par ce process ni par ses
    // enfants. Ca ferme la fuite passive (un `env` accidentel ou declenche
    // par une injection de prompt ne recrache plus de token), pas l'acces
    // delibere : `FERRFLEET_RUN_TOKEN` reste herite par ce process (il sert
    // a d'autres appels API du runner), et un agent qui l'utiliserait
    // lui-meme pour appeler l'endpoint peut toujours minter un token, borne
    // au seul depot de ce run.
    if let Some(credentials) = git_credentials {
        credentials.apply(&mut cmd);
    }

    if let Some(model) = &cfg.model {
        cmd.arg("--model").arg(model);
    }
    if let Some(sid) = &cfg.session_id {
        cmd.arg("-r").arg(sid);
    }
    for tool in &cfg.allowed_tools {
        cmd.arg("--allowed-tools").arg(tool);
    }

    // The list that actually constrains. Verified against the CLI:
    // `--allowed-tools` only pre-approves — a tool absent from it still runs —
    // whereas a disallowed tool is removed from the session entirely and stays
    // refused even under `bypassPermissions` below.
    for tool in &cfg.disallowed_tools {
        cmd.arg("--disallowed-tools").arg(tool);
    }
    for tool in HEADLESS_DISALLOWED_TOOLS {
        if !cfg.disallowed_tools.iter().any(|t| t == tool) {
            cmd.arg("--disallowed-tools").arg(tool);
        }
    }

    // Without this the CLI falls back to interactive approval, and nobody is
    // here to approve: the run spends its budget on "This command requires
    // approval" and then finishes green having done nothing.
    cmd.arg("--permission-mode").arg(&cfg.permission_mode);

    // Agents reach for `gh` unprompted — it accounted for the most frequent
    // error across the fleet. Our image leaves it out on purpose, because
    // authenticating it would route around the connector's denied-tool list,
    // but a runner we do not build may have it, so the note forbids it rather
    // than claiming it is absent.
    let system_prompt_file = PrivateFile::write(
        "system-prompt",
        appended_system_prompt(prompt.stable).as_bytes(),
    )?;
    cmd.arg("--append-system-prompt-file")
        .arg(system_prompt_file.path())
        .arg("--exclude-dynamic-system-prompt-sections");

    let mcp_config = cfg.mcp_config.as_ref().map(mcp_config::write).transpose()?;
    if let Some(file) = &mcp_config {
        cmd.arg("--mcp-config").arg(file.path());
    }

    // A run without a checkout has no repository, so there is nothing for
    // one to have stated. Announcing an absent file there would answer a
    // question nobody asked, about a repository that does not exist.
    let stdin_prompt = if cfg.checkout.is_some() {
        prompt_with_repo_context(prompt.per_run, &cfg.agent_id, &cfg.working_dir)
    } else {
        prompt.per_run.to_owned()
    };

    Ok(ClaudeInvocation {
        command: cmd,
        stdin_prompt,
        system_prompt_file,
        mcp_config,
    })
}

struct ClaudeInvocation {
    command: Command,
    stdin_prompt: String,
    system_prompt_file: PrivateFile,
    mcp_config: Option<PrivateFile>,
}

fn spawn_with_prompt(mut command: Command, prompt: String) -> Result<Child> {
    let mut child = command.spawn().context("spawning claude CLI")?;
    let mut stdin = child.stdin.take().context("claude stdin missing")?;
    tokio::spawn(async move {
        if let Err(err) = stdin.write_all(prompt.as_bytes()).await {
            warn!(%err, "could not write the prompt to claude");
        }
    });
    Ok(child)
}

pub(crate) async fn spawn_and_stream(
    cfg: &RunConfig,
    env: &Env,
    queue: &EventQueue,
    totals: &mut TokenTotals,
    session_id: &mut Option<String>,
    git_credentials: Option<&workspace::GitCredentials>,
    stop: impl Future<Output = Lost>,
) -> Result<Streamed> {
    claude_version::ensure_supported().await?;
    let ClaudeInvocation {
        command,
        stdin_prompt,
        system_prompt_file: _system_prompt_file,
        mcp_config: _mcp_config,
    } = build_claude_command(cfg, env, git_credentials)?;
    let child = spawn_with_prompt(command, stdin_prompt)?;
    let timeout = Duration::from_secs(cfg.timeout_seconds);
    stream_child(child, queue, totals, session_id, timeout, stop).await
}

async fn stream_child(
    mut child: Child,
    queue: &EventQueue,
    totals: &mut TokenTotals,
    session_id: &mut Option<String>,
    timeout: Duration,
    stop: impl Future<Output = Lost>,
) -> Result<Streamed> {
    let stdout = child.stdout.take().context("claude stdout missing")?;
    let stderr = child.stderr.take().context("claude stderr missing")?;
    let mut reader = BufReader::new(stdout).lines();

    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if !line.trim().is_empty() {
                warn!(target: "claude.stderr", "{line}");
            }
        }
    });

    let mut usage = usage::UsageLedger::default();
    let streaming = tokio::time::timeout(timeout, async {
        while let Some(line) = reader.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            match translate(&line, &mut usage) {
                Ok(events) => {
                    for ev in events {
                        if let ExecutorEvent::Usage {
                            input_tokens,
                            output_tokens,
                            cache_creation_input_tokens,
                            cache_read_input_tokens,
                            ..
                        } = &ev
                        {
                            totals.input = totals.input.saturating_add(*input_tokens);
                            totals.output = totals.output.saturating_add(*output_tokens);
                            totals.cache_creation = totals
                                .cache_creation
                                .saturating_add(*cache_creation_input_tokens);
                            totals.cache_read =
                                totals.cache_read.saturating_add(*cache_read_input_tokens);
                        }
                        if reopens_current_session(&ev, session_id.as_deref()) {
                            continue;
                        }
                        if let ExecutorEvent::SessionStarted {
                            session_id: sid, ..
                        } = &ev
                        {
                            *session_id = Some(sid.clone());
                        }
                        queue.push(ev);
                    }
                }
                Err(err) => {
                    warn!(%err, raw = %line, "could not parse claude output line");
                }
            }
        }
        anyhow::Ok(())
    });

    let result = tokio::select! {
        result = streaming => result,
        lost = stop => {
            warn!(%lost, "run stopped by FerrFleet; killing claude");
            let _ = child.kill().await;
            stderr_task.abort();
            return Ok(Streamed::Stopped(lost));
        }
    };

    let exit_code = match result {
        Ok(Ok(())) => child
            .wait()
            .await
            .context("waiting on claude")?
            .code()
            .unwrap_or(-1),
        Ok(Err(err)) => {
            let _ = stderr_task.await;
            return Err(err);
        }
        Err(_) => {
            warn!("timeout exceeded; killing claude");
            let _ = child.kill().await;
            -1
        }
    };

    let _ = stderr_task.await;
    Ok(Streamed::Exited(exit_code))
}

#[derive(Default)]
pub(crate) struct TokenTotals {
    pub(crate) input: u32,
    pub(crate) output: u32,
    pub(crate) cache_creation: u32,
    pub(crate) cache_read: u32,
}

#[cfg(test)]
mod environment_note_tests {
    use super::*;
    use crate::config;

    const STABLE: &str = "## How to write\n\nplain\n\nreview the pull request";

    fn config(run_id: &str, working_dir: &str, head_sha: &str, stable: Option<&str>) -> RunConfig {
        let per_run = format!("## Event context\n```json\n{{\"head_sha\": \"{head_sha}\"}}\n```");
        let mut cfg: RunConfig = serde_json::from_value(serde_json::json!({
            "run_id": run_id,
            "agent_id": "pr-agent",
            "prompt": format!("{STABLE}\n\n{per_run}"),
            "working_dir": working_dir,
        }))
        .expect("a minimal config");
        cfg.stable_prefix = stable.map(str::to_owned);
        cfg
    }

    fn env(working_dir: &str) -> Env {
        Env {
            api_url: "http://api.invalid".to_owned(),
            run_id: "run".to_owned(),
            run_token: "token".to_owned(),
            working_dir: Some(working_dir.to_owned()),
            runner_name: None,
        }
    }

    #[test]
    fn the_agent_process_is_marked_so_it_cannot_start_a_run() {
        let cfg = config(
            "00000000-0000-0000-0000-000000000001",
            "/workdir",
            "abc",
            None,
        );
        let invocation =
            build_claude_command(&cfg, &env(&cfg.working_dir), None).expect("building");
        let marked = invocation
            .command
            .as_std()
            .get_envs()
            .any(|(key, value)| key == INSIDE_AGENT_VAR && value.is_some());
        assert!(marked);
    }

    fn stdin_prompt(cfg: &RunConfig) -> String {
        build_claude_command(cfg, &env(&cfg.working_dir), None)
            .expect("building")
            .stdin_prompt
    }

    fn system_prompt(cfg: &RunConfig) -> String {
        let invocation = build_claude_command(cfg, &env(&cfg.working_dir), None).expect("building");
        let args: Vec<_> = invocation.command.as_std().get_args().collect();
        assert!(!args.iter().any(|a| *a == "--append-system-prompt"));
        let at = args
            .iter()
            .position(|a| *a == "--append-system-prompt-file")
            .expect("the system prompt is passed as a file");
        std::fs::read_to_string(args[at + 1]).expect("reading the system prompt file")
    }

    fn disallowed(args: &[&std::ffi::OsStr]) -> Vec<String> {
        args.windows(2)
            .filter(|pair| pair[0] == "--disallowed-tools")
            .map(|pair| pair[1].to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn tools_that_wait_for_a_later_turn_are_never_offered() {
        let mut cfg = config(
            "01999999-9999-7999-9999-999999999989",
            "/workdir",
            "abc",
            None,
        );
        cfg.disallowed_tools = vec!["WebFetch".to_string(), "Monitor".to_string()];
        let invocation =
            build_claude_command(&cfg, &env(&cfg.working_dir), None).expect("building");
        let args: Vec<_> = invocation.command.as_std().get_args().collect();

        assert_eq!(
            disallowed(&args),
            ["WebFetch", "Monitor", "ScheduleWakeup", "CronCreate"]
        );
    }

    #[test]
    fn the_prompt_is_not_an_argument() {
        let cfg = config(
            "01999999-9999-7999-9999-999999999993",
            "/workdir",
            "abc",
            Some(STABLE),
        );
        let invocation =
            build_claude_command(&cfg, &env(&cfg.working_dir), None).expect("building");
        let args: Vec<_> = invocation.command.as_std().get_args().collect();

        assert!(args.contains(&std::ffi::OsStr::new("-p")));
        assert!(args.iter().all(|a| a.len() < 512));
    }

    #[test]
    fn the_mcp_config_lives_outside_the_working_directory() {
        let mut cfg = config(
            "01999999-9999-7999-9999-999999999990",
            "/workdir",
            "abc",
            None,
        );
        cfg.mcp_config = Some(serde_json::json!({"mcpServers": {}}));

        let invocation =
            build_claude_command(&cfg, &env(&cfg.working_dir), None).expect("building");

        let args: Vec<_> = invocation.command.as_std().get_args().collect();
        let at = args
            .iter()
            .position(|a| *a == "--mcp-config")
            .expect("passed");
        let path = std::path::Path::new(args[at + 1]);
        assert!(!path.starts_with(&cfg.working_dir));
        assert_eq!(path, invocation.mcp_config.expect("kept alive").path());
    }

    #[test]
    fn the_claude_command_drops_the_pool_token_it_would_inherit() {
        let cfg = config(
            "01999999-9999-7999-9999-999999999999",
            "/workdir",
            "abc",
            None,
        );
        let invocation =
            build_claude_command(&cfg, &env(&cfg.working_dir), None).expect("building");
        let cmd = invocation.command;

        let removed = cmd
            .as_std()
            .get_envs()
            .find(|(key, _)| *key == config::POOL_TOKEN_VAR)
            .expect("the pool token variable is addressed explicitly");
        assert_eq!(removed.1, None);
    }

    #[test]
    fn two_runs_of_one_agent_send_a_byte_identical_system_prompt() {
        let first = config(
            "01999999-9999-7999-9999-999999999991",
            "/home/runner/work/_temp/ferrfleet-workdir-a1B2c3",
            "1111111",
            Some(STABLE),
        );
        let second = config(
            "01999999-9999-7999-9999-999999999992",
            "/home/runner/work/_temp/ferrfleet-workdir-Z9y8X7",
            "2222222",
            Some(STABLE),
        );

        let system = system_prompt(&first);

        assert_eq!(system, system_prompt(&second));
        assert!(system.starts_with(STABLE));
        for per_run in ["1111111", "ferrfleet-workdir", "999999991", "Event context"] {
            assert!(
                !system.contains(per_run),
                "{per_run} leaked into the system prompt"
            );
        }
    }

    #[test]
    fn the_stable_part_is_not_repeated_in_the_user_turn() {
        let cfg = config(
            "01999999-9999-7999-9999-999999999991",
            "/workdir",
            "1111111",
            Some(STABLE),
        );

        let user = stdin_prompt(&cfg);

        assert!(user.starts_with("## Event context"));
        assert!(user.contains("1111111"));
        assert!(!user.contains("review the pull request"));
    }

    #[test]
    fn an_api_that_sends_no_stable_prefix_still_gets_its_whole_prompt_run() {
        let cfg = config(
            "01999999-9999-7999-9999-999999999991",
            "/workdir",
            "1111111",
            None,
        );

        assert_eq!(stdin_prompt(&cfg), cfg.prompt);
        assert_eq!(system_prompt(&cfg), RUNNER_ENVIRONMENT_NOTE);
    }

    #[test]
    fn the_note_forbids_gh_without_claiming_it_is_absent() {
        assert!(RUNNER_ENVIRONMENT_NOTE.contains("Do NOT use the GitHub CLI (`gh`)"));
        assert!(!RUNNER_ENVIRONMENT_NOTE.contains("is NOT installed"));
    }
}

#[cfg(test)]
mod stdin_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const OVER_ARG_MAX_STRLEN: usize = 300 * 1024;

    fn stub(script: &str) -> (Command, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("ferrfleet-stub-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("writing the stub");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("making the stub executable");
        let mut command = Command::new(&path);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        (command, path)
    }

    #[tokio::test]
    async fn a_prompt_over_the_argv_limit_reaches_the_child_whole() {
        let (command, path) = stub("wc -c");
        let prompt = "x".repeat(OVER_ARG_MAX_STRLEN);

        let child = spawn_with_prompt(command, prompt).expect("spawning");
        let output = child.wait_with_output().await.expect("waiting");
        let _ = std::fs::remove_file(path);

        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            OVER_ARG_MAX_STRLEN.to_string()
        );
    }

    #[tokio::test]
    async fn a_child_that_never_reads_its_prompt_does_not_hang_the_runner() {
        let (command, path) = stub("echo done");
        let prompt = "x".repeat(OVER_ARG_MAX_STRLEN);

        let child = spawn_with_prompt(command, prompt).expect("spawning");
        let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
            .await
            .expect("the child is not stuck")
            .expect("waiting");
        let _ = std::fs::remove_file(path);

        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "done");
    }

    #[test]
    fn a_failed_spawn_names_the_cause_in_the_reported_error() {
        let err = spawn_with_prompt(
            Command::new("/nonexistent/ferrfleet-claude"),
            "prompt".to_owned(),
        )
        .expect_err("there is no such binary");

        let message = format!("claude execution failed: {err:#}");
        assert!(message.contains("spawning claude CLI"), "{message}");
        assert!(message.contains("No such file or directory"), "{message}");
    }
}

#[cfg(test)]
mod stop_tests {
    use super::*;
    use crate::fake_api;

    #[tokio::test]
    async fn a_lost_lease_kills_claude_instead_of_waiting_for_it() {
        let child = Command::new("sleep")
            .arg("30")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawning a stand-in for claude");
        let pid = child.id().expect("a running child has a pid");
        let queue = EventQueue::start(fake_api::sender(&fake_api::dead_api_url()));
        let mut totals = TokenTotals::default();
        let mut session_id = None;

        let started = std::time::Instant::now();
        let streamed = stream_child(
            child,
            &queue,
            &mut totals,
            &mut session_id,
            Duration::from_secs(60),
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Lost::RunOver
            },
        )
        .await
        .expect("a stop is not an error");

        assert!(matches!(streamed, Streamed::Stopped(Lost::RunOver)));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "claude kept running after FerrFleet took the run back"
        );
    }
}
