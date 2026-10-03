use anyhow::{Context, Result, bail};
use std::env;
use std::fmt;
use std::path::PathBuf;
use tokio::process::Command;

pub const POOL_TOKEN_VAR: &str = "FERRFLEET_POOL_TOKEN";
const POOL_TOKEN_PREFIX: &str = "ffrp_";

#[derive(Debug, Clone)]
pub struct Env {
    pub api_url: String,
    pub run_id: String,
    pub run_token: String,
    pub working_dir: Option<String>,
    pub runner_name: Option<String>,
}

impl Env {
    pub fn from_env() -> Result<Self> {
        let api_url = env::var("FERRFLEET_API_URL").context("FERRFLEET_API_URL is required")?;
        let run_id = env::var("FERRFLEET_RUN_ID").context("FERRFLEET_RUN_ID is required")?;
        let run_token =
            env::var("FERRFLEET_RUN_TOKEN").context("FERRFLEET_RUN_TOKEN is required")?;

        if api_url.is_empty() || run_id.is_empty() || run_token.is_empty() {
            bail!("FERRFLEET_API_URL / FERRFLEET_RUN_ID / FERRFLEET_RUN_TOKEN must not be empty");
        }
        Ok(Self {
            api_url,
            run_id,
            run_token,
            working_dir: optional("FERRFLEET_WORKING_DIR"),
            runner_name: None,
        })
    }

    pub fn apply_to(&self, cmd: &mut Command) {
        cmd.env("FERRFLEET_API_URL", &self.api_url)
            .env("FERRFLEET_RUN_ID", &self.run_id)
            .env("FERRFLEET_RUN_TOKEN", &self.run_token)
            .env_remove(POOL_TOKEN_VAR);
    }
}

#[derive(Clone)]
pub struct PoolToken(String);

impl PoolToken {
    pub fn parse(raw: String) -> Result<Self> {
        if !raw.starts_with(POOL_TOKEN_PREFIX) {
            bail!(
                "{POOL_TOKEN_VAR} does not look like a pool token: those start with {POOL_TOKEN_PREFIX}"
            );
        }
        Ok(Self(raw))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PoolToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PoolToken(..)")
    }
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub api_url: String,
    pub pool_token: PoolToken,
    pub runner_name: String,
    pub work_root: PathBuf,
}

impl AgentConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            api_url: required("FERRFLEET_API_URL")?,
            pool_token: PoolToken::parse(required(POOL_TOKEN_VAR)?)?,
            runner_name: optional("FERRFLEET_RUNNER_NAME").unwrap_or_else(crate::sender::claimant),
            work_root: optional("FERRFLEET_WORKING_DIR")
                .map_or_else(|| env::temp_dir().join("ferrfleet-runs"), PathBuf::from),
        })
    }
}

fn required(key: &str) -> Result<String> {
    optional(key).with_context(|| format!("{key} is required"))
}

fn optional(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn a_run_command_carries_the_run_identity_and_never_the_pool_token() {
        let env = Env {
            api_url: "https://api.example".to_owned(),
            run_id: "run-1".to_owned(),
            run_token: "run-token".to_owned(),
            working_dir: None,
            runner_name: None,
        };
        let mut cmd = Command::new("true");
        cmd.env(POOL_TOKEN_VAR, "ffrp_leaked");

        env.apply_to(&mut cmd);

        let vars: HashMap<String, Option<String>> = cmd
            .as_std()
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_str()?.to_owned(),
                    v.and_then(|v| v.to_str()).map(str::to_owned),
                ))
            })
            .collect();
        assert_eq!(vars["FERRFLEET_RUN_ID"].as_deref(), Some("run-1"));
        assert_eq!(vars["FERRFLEET_RUN_TOKEN"].as_deref(), Some("run-token"));
        assert_eq!(
            vars[POOL_TOKEN_VAR], None,
            "the agent could lease the pool's other runs with it"
        );
    }

    #[test]
    fn something_that_is_not_a_pool_token_is_refused_by_name() {
        let err = PoolToken::parse("eyJhbGciOi.run.token".to_owned()).unwrap_err();
        assert!(err.to_string().contains(POOL_TOKEN_VAR));
    }

    #[test]
    fn a_pool_token_never_shows_in_debug_output() {
        let token = PoolToken::parse("ffrp_secret".to_owned()).unwrap();
        assert!(!format!("{token:?}").contains("secret"));
    }
}
