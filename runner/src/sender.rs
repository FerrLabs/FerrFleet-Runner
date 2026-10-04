use anyhow::{Context, Result};
use ferrfleet_shared::{ExecutorEvent, RunConfig};
use reqwest::{RequestBuilder, Response};
use serde::Deserialize;
use std::time::Duration;

use crate::config::Env;
use crate::lease::{LeaseState, Lost, Stopped};
use crate::retry::{self, Backoff};

#[cfg(test)]
mod tests;

/// Reponse de `GET /runs/{id}/github-token`.
#[derive(Debug, Deserialize)]
struct GithubTokenResponse {
    token: String,
}

#[derive(Clone)]
pub struct EventSender {
    env: Env,
    client: reqwest::Client,
    backoff: Backoff,
    lease: Option<LeaseState>,
}

impl EventSender {
    pub const TIMEOUT: Duration = Duration::from_secs(10);

    pub fn new(env: Env) -> Self {
        Self::configured(env, Backoff::API_REDEPLOY, Self::TIMEOUT)
    }

    pub fn configured(env: Env, backoff: Backoff, timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("building reqwest client");
        Self {
            env,
            client,
            backoff,
            lease: None,
        }
    }

    #[must_use]
    pub fn with_lease(self, lease: LeaseState) -> Self {
        Self {
            lease: Some(lease),
            ..self
        }
    }

    pub fn lost(&self) -> Option<Lost> {
        self.lease.as_ref().and_then(LeaseState::lost)
    }

    pub async fn wait_lost(&self) -> Lost {
        match &self.lease {
            Some(lease) => lease.wait().await,
            None => std::future::pending().await,
        }
    }

    #[cfg(test)]
    pub fn for_tests(api_url: &str, backoff: Backoff, timeout: Duration) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Self::configured(
            Env {
                api_url: api_url.to_owned(),
                run_id: crate::fake_api::RUN_ID.to_owned(),
                run_token: "run-token".to_owned(),
                working_dir: None,
                runner_name: None,
            },
            backoff,
            timeout,
        )
    }

    pub fn retry_budget(&self) -> Duration {
        self.backoff.budget
    }

    async fn execute(&self, request: impl Fn() -> RequestBuilder) -> Result<Response> {
        if let Some(lost) = self.lost() {
            return Err(Stopped(lost).into());
        }
        let resp = retry::while_unprocessed(self.backoff, || request().send()).await?;
        if let Some(lease) = &self.lease {
            lease.observe(resp.status());
        }
        Ok(resp)
    }

    pub async fn heartbeat(&self) -> Result<()> {
        let url = format!("{}/runs/{}/heartbeat", self.env.api_url, self.env.run_id);
        self.execute(|| self.client.post(&url).bearer_auth(&self.env.run_token))
            .await
            .with_context(|| format!("POST {url}"))?
            .error_for_status()
            .with_context(|| format!("non-2xx from {url}"))?;
        Ok(())
    }

    pub async fn fetch_config(&self) -> Result<RunConfig> {
        let url = format!("{}/runs/{}/config", self.env.api_url, self.env.run_id);
        let resp = self
            .execute(|| self.client.get(&url).bearer_auth(&self.env.run_token))
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("non-2xx from {url}"))?;
        resp.json::<RunConfig>().await.context("decoding RunConfig")
    }

    /// Recupere un token d'installation GitHub de courte duree pour ce run,
    /// authentifie par le JWT de run (`FERRFLEET_RUN_TOKEN`) : le runner ne
    /// lit jamais de token GitHub depuis Vault, seule l'API a la clé privee
    /// de la GitHub App nécessaire pour le minter.
    pub async fn fetch_github_token(&self) -> Result<String> {
        let url = format!("{}/runs/{}/github-token", self.env.api_url, self.env.run_id);
        let resp = self
            .execute(|| self.client.get(&url).bearer_auth(&self.env.run_token))
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("non-2xx from {url}"))?;
        let body: GithubTokenResponse = resp
            .json()
            .await
            .context("decoding la reponse github-token")?;
        Ok(body.token)
    }

    /// Annonce a l'API la PR que l'agent vient d'ouvrir, ce qui renseigne
    /// `ticket_runs.pr_url` et arme la boucle de review. Sans cet appel, une
    /// review humaine sur cette PR n'est jamais reconnue comme portant sur une
    /// PR de l'agent.
    pub async fn report_pull_request(&self, url: &str) -> Result<()> {
        let endpoint = format!("{}/runs/{}/pull-request", self.env.api_url, self.env.run_id);
        let body = serde_json::json!({ "url": url });
        self.execute(|| {
            self.client
                .post(&endpoint)
                .bearer_auth(&self.env.run_token)
                .json(&body)
        })
        .await
        .with_context(|| format!("POST {endpoint}"))?
        .error_for_status()
        .with_context(|| format!("non-2xx from {endpoint}"))?;
        Ok(())
    }

    pub async fn report_result(&self, result: &serde_json::Value) -> Result<()> {
        let endpoint = format!("{}/runs/{}/result", self.env.api_url, self.env.run_id);
        let body = serde_json::json!({ "result": result });
        self.execute(|| {
            self.client
                .post(&endpoint)
                .bearer_auth(&self.env.run_token)
                .json(&body)
        })
        .await
        .with_context(|| format!("POST {endpoint}"))?
        .error_for_status()
        .with_context(|| format!("non-2xx from {endpoint}"))?;
        Ok(())
    }

    pub async fn send(&self, event: &ExecutorEvent) -> Result<()> {
        let url = format!("{}/runs/{}/events", self.env.api_url, self.env.run_id);
        self.execute(|| {
            self.client
                .post(&url)
                .bearer_auth(&self.env.run_token)
                .json(event)
        })
        .await
        .and_then(|resp| resp.error_for_status().map_err(anyhow::Error::from))
        .with_context(|| format!("POST {url}"))?;
        Ok(())
    }
}

/// Whether this runner may proceed with the run, for a run nothing else
/// guarantees is executed once.
pub enum Claim {
    /// Ours. Nobody else is running it.
    Granted,
    /// Someone else got there first. Not an error: the work is happening,
    /// this process simply has nothing to do.
    AlreadyTaken,
}

impl EventSender {
    /// Take ownership of an external run before doing any work.
    ///
    /// A managed run needs no claim: its Kubernetes Job is the guarantee that
    /// it executes once. An external run is started by a pipeline, where a
    /// re-run, a retried job or two workflows watching the same event all
    /// produce a second runner for the same run id. Runners cannot see each
    /// other, so the API decides, in one conditional UPDATE.
    ///
    /// The claimant string is a label for whoever reads the run afterwards.
    /// GitHub Actions fills the variables it is built from; anywhere else it
    /// degrades to the hostname, which is still better than nothing.
    pub async fn claim_run(&self) -> Result<Claim> {
        let url = format!("{}/runs/{}/claim", self.env.api_url, self.env.run_id);
        let runner = self.env.runner_name.clone().unwrap_or_else(claimant);
        let body = serde_json::json!({ "runner": runner });
        let resp = self
            .execute(|| {
                self.client
                    .post(&url)
                    .bearer_auth(&self.env.run_token)
                    .json(&body)
            })
            .await
            .with_context(|| format!("POST {url}"))?;

        if resp.status() == reqwest::StatusCode::CONFLICT {
            return Ok(Claim::AlreadyTaken);
        }
        resp.error_for_status()
            .with_context(|| format!("non-2xx from {url}"))?;
        Ok(Claim::Granted)
    }
}

/// A label for the machine running this, best-effort.
///
/// Read from the environment the pipeline already sets rather than from
/// anything we ask the operator to configure: a claim that needs setup is a
/// claim someone eventually skips.
pub(crate) fn claimant() -> String {
    let repo = std::env::var("GITHUB_REPOSITORY").ok();
    let run = std::env::var("GITHUB_RUN_ID").ok();
    match (repo, run) {
        (Some(repo), Some(run)) => format!("github-actions:{repo}#{run}"),
        (Some(repo), None) => format!("github-actions:{repo}"),
        _ => std::env::var("HOSTNAME").unwrap_or_else(|_| "unidentified".to_owned()),
    }
}
