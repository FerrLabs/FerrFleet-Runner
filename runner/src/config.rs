use anyhow::{Context, Result, bail};
use std::env;

#[derive(Debug, Clone)]
pub struct Env {
    pub api_url: String,
    pub run_id: String,
    pub run_token: String,
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
        })
    }
}
