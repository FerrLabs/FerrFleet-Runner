use anyhow::{Context, Result};
use ferrfleet_shared::{ExecutorEvent, RunConfig};
use serde::Deserialize;
use std::time::Duration;
use tracing::warn;

use crate::config::Env;

/// Reponse de `GET /runs/{id}/github-token`.
#[derive(Debug, Deserialize)]
struct GithubTokenResponse {
    token: String,
}

#[derive(Clone)]
pub struct EventSender {
    env: Env,
    client: reqwest::Client,
}

impl EventSender {
    pub fn new(env: Env) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("building reqwest client");
        Self { env, client }
    }

    pub async fn fetch_config(&self) -> Result<RunConfig> {
        let url = format!("{}/runs/{}/config", self.env.api_url, self.env.run_id);
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.env.run_token)
            .send()
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
            .client
            .get(&url)
            .bearer_auth(&self.env.run_token)
            .send()
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
        self.client
            .post(&endpoint)
            .bearer_auth(&self.env.run_token)
            .json(&serde_json::json!({ "url": url }))
            .send()
            .await
            .with_context(|| format!("POST {endpoint}"))?
            .error_for_status()
            .with_context(|| format!("non-2xx from {endpoint}"))?;
        Ok(())
    }

    pub async fn send(&self, event: ExecutorEvent) {
        let url = format!("{}/runs/{}/events", self.env.api_url, self.env.run_id);
        let result = self
            .client
            .post(&url)
            .bearer_auth(&self.env.run_token)
            .json(&event)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status);
        if let Err(err) = result {
            warn!(?err, "failed to push event to api (continuing)");
        }
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
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.env.run_token)
            .json(&serde_json::json!({ "runner": claimant() }))
            .send()
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
fn claimant() -> String {
    let repo = std::env::var("GITHUB_REPOSITORY").ok();
    let run = std::env::var("GITHUB_RUN_ID").ok();
    match (repo, run) {
        (Some(repo), Some(run)) => format!("github-actions:{repo}#{run}"),
        (Some(repo), None) => format!("github-actions:{repo}"),
        _ => std::env::var("HOSTNAME").unwrap_or_else(|_| "unidentified".to_owned()),
    }
}
