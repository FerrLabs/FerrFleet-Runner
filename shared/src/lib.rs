pub mod runner_mode;
pub use runner_mode::RunnerMode;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunConfig {
    pub run_id: Uuid,
    pub agent_id: String,
    pub prompt: String,
    pub working_dir: String,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Tools removed from the session entirely.
    ///
    /// The one that actually constrains. `allowed_tools` only pre-approves —
    /// verified against the CLI, a tool absent from it still runs — whereas a
    /// disallowed tool is not visible to the model at all, and stays refused
    /// even under `bypassPermissions`.
    #[serde(default)]
    pub disallowed_tools: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub mcp_config: Option<serde_json::Value>,
    /// How the CLI decides whether a tool call may proceed.
    ///
    /// A run is unattended by construction: no human is there to answer a
    /// prompt, so any tool outside `allowed_tools` blocks until the run times
    /// out. The symptom is a run that reports success having done nothing —
    /// observed on the pr-agent, which spent its whole budget being told
    /// "This command requires approval".
    ///
    /// `bypassPermissions` is the honest default for that context. The
    /// isolation lives in the pod, not in the prompt: agent Jobs run with
    /// `allowPrivilegeEscalation: false`, every capability dropped and a
    /// read-only root filesystem. Narrow a given agent by giving it an
    /// `allowed_tools` list, not by making it ask a question nobody hears.
    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,
    /// Depot a preparer avant de lancer le CLI. `None` quand le run n'a pas
    /// de depot associe (agents hors ticket).
    #[serde(default)]
    pub checkout: Option<Checkout>,
    /// Who is responsible for having started this runner.
    ///
    /// The runner reads it to decide whether to claim the run before doing any
    /// work. A managed run needs no claim: its Kubernetes Job is already the
    /// guarantee that it executes once. An external run has no Job, so the
    /// claim is where that guarantee comes from, and a runner that skipped it
    /// could be the second one on the same run.
    ///
    /// Defaulted so a runner built against a newer API keeps working against
    /// an older one that does not send the field: absent means managed, which
    /// is what every run was before external runners existed.
    #[serde(default)]
    pub runner_mode: RunnerMode,
}

/// Parametres de preparation du depot de travail pour un run lie a un
/// ticket : quoi cloner, sur quelle branche, et si cette branche existe deja.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkout {
    /// "owner/name"
    pub repo: String,
    /// `None` = branche par defaut du depot.
    #[serde(default)]
    pub base_branch: Option<String>,
    /// Branche a creer ou a reprendre.
    pub branch: String,
    /// `true` = reprendre la branche d'une PR existante plutot que d'en
    /// creer une nouvelle.
    pub existing: bool,
}

/// Un nom de branche que git accepte, derive de la reference du ticket.
///
/// `FerrLabs/kit#88` contient un `#` et un `/` : le premier casse les
/// refspecs, le second creerait une hierarchie fantaisiste sous le prefixe.
#[must_use]
pub fn branch_name(prefix: &str, ticket_ref: &str) -> String {
    let slug: String = ticket_ref
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let mut out = String::from(prefix);
    let mut last_dash = out.ends_with('-');
    for c in slug.chars() {
        if c == '-' {
            if last_dash {
                continue;
            }
            last_dash = true;
        } else {
            last_dash = false;
        }
        out.push(c);
    }
    out.trim_end_matches('-').to_string()
}

fn default_timeout() -> u64 {
    1_800
}

/// Public so the API can reuse it when building a `RunConfig` by hand, rather
/// than repeating the literal and letting the two drift apart.
#[must_use]
pub fn default_permission_mode() -> String {
    "bypassPermissions".to_owned()
}

/// The model an agent runs on when it declares none.
///
/// Previously nothing was passed and the CLI decided — a default nobody could
/// see, name, or reason about. The agent form said as much, in as many words:
/// "Default (whatever the CLI picks)". That is an odd thing to offer as the
/// first option of a list, and it made cost and latency unattributable: two
/// agents left on the default could silently run on different models.
///
/// `sonnet` rather than a pinned version: the alias follows the newest release,
/// so agents stay current with nothing to refresh. An agent that needs
/// reproducibility pins a concrete id instead.
#[must_use]
pub fn default_model() -> &'static str {
    "sonnet"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecutorEvent {
    SessionStarted {
        session_id: String,
        /// The model the CLI resolved for this session.
        ///
        /// Carried here because an alias like `sonnet` is what we store and
        /// send, so this is the only place the concrete version is ever
        /// visible. Without it nobody can say afterwards which model produced
        /// a run — neither for cost attribution nor for comparing two models.
        #[serde(default)]
        model: Option<String>,
        timestamp: DateTime<Utc>,
    },
    AssistantMessage {
        content: String,
        timestamp: DateTime<Utc>,
    },
    ToolUse {
        tool_name: String,
        tool_use_id: String,
        input: serde_json::Value,
        timestamp: DateTime<Utc>,
    },
    ToolResult {
        tool_use_id: String,
        output: serde_json::Value,
        is_error: bool,
        timestamp: DateTime<Utc>,
    },
    Usage {
        input_tokens: u32,
        output_tokens: u32,
        cache_creation_input_tokens: u32,
        cache_read_input_tokens: u32,
        timestamp: DateTime<Utc>,
    },
    Error {
        message: String,
        /// Set only when the CLI's own `result` line carried its `is_error`
        /// flag — an API-level failure (quota exhausted, rate limited,
        /// overloaded), as opposed to an application-level one (a refused
        /// prompt, a tool failure, our own process-spawn error).
        ///
        /// The API's provider-outage handling keys off this flag rather than
        /// matching `message` text: a free-form message built by chaining
        /// errors from arbitrary layers is not a safe thing to pattern-match
        /// on (an ordinary GitHub API "rate limit" would otherwise trip it).
        #[serde(default)]
        provider_signal: bool,
        timestamp: DateTime<Utc>,
    },
    Completed {
        exit_code: i32,
        session_id: Option<String>,
        timestamp: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    pub exit_code: i32,
    pub session_id: Option<String>,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub cache_creation_input_tokens: u32,
    pub cache_read_input_tokens: u32,
    pub cost_usd: f64,
    pub duration_ms: u64,
}

pub mod pricing {
    /// Fixed Sonnet-only rate table, hard-coded before per-org credits existed.
    ///
    /// Still used by `api/runner/src/main.rs`, which is out of scope here.
    /// The API itself now prices from `ferrfleet_api::credits::pricing`, which
    /// reads per-model rates from `model_pricing` keyed on the run's
    /// `planned_model` rather than assuming every run is Sonnet.
    #[deprecated(note = "use ferrfleet_api::credits::pricing instead")]
    #[must_use]
    pub fn cost_usd_for_sonnet(
        input_tokens: u32,
        output_tokens: u32,
        cache_creation: u32,
        cache_read: u32,
    ) -> f64 {
        let input = f64::from(input_tokens) * 3.0 / 1_000_000.0;
        let output = f64::from(output_tokens) * 15.0 / 1_000_000.0;
        let cache_write = f64::from(cache_creation) * 3.75 / 1_000_000.0;
        let cache_hit = f64::from(cache_read) * 0.30 / 1_000_000.0;
        input + output + cache_write + cache_hit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_config_deserializes_with_defaults() {
        let json = r#"{
            "run_id": "01999999-9999-7999-9999-999999999999",
            "agent_id": "hello-world",
            "prompt": "say hi",
            "working_dir": "/workdir"
        }"#;
        let cfg: RunConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.timeout_seconds, 1800);
        assert!(cfg.allowed_tools.is_empty());
        assert!(cfg.disallowed_tools.is_empty());
        assert!(cfg.session_id.is_none());
        // A config written before this field existed must not deserialize into
        // an interactive run — that is the state that hangs unattended.
        assert_eq!(cfg.permission_mode, "bypassPermissions");
    }

    /// An agent that sets the mode explicitly keeps it: the default exists to
    /// stop runs hanging, not to override a deliberate choice.
    #[test]
    fn an_explicit_permission_mode_is_preserved() {
        let json = r#"{
            "run_id": "01999999-9999-7999-9999-999999999999",
            "agent_id": "hello-world",
            "prompt": "say hi",
            "working_dir": "/workdir",
            "permission_mode": "acceptEdits"
        }"#;
        let cfg: RunConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.permission_mode, "acceptEdits");
    }

    #[test]
    fn executor_event_round_trip() {
        let evt = ExecutorEvent::Usage {
            input_tokens: 100,
            output_tokens: 200,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 50,
            timestamp: Utc::now(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(json.contains("\"type\":\"usage\""));
        let parsed: ExecutorEvent = serde_json::from_str(&json).unwrap();
        match parsed {
            ExecutorEvent::Usage {
                input_tokens,
                output_tokens,
                ..
            } => {
                assert_eq!(input_tokens, 100);
                assert_eq!(output_tokens, 200);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    #[allow(deprecated, reason = "still exercising the deprecated fn itself")]
    fn pricing_known_values() {
        let cost = pricing::cost_usd_for_sonnet(1_000_000, 1_000_000, 0, 0);
        assert!((cost - 18.0).abs() < 0.001);
    }

    #[test]
    fn branch_names_are_git_safe() {
        assert_eq!(
            branch_name("ferrfleet/ticket-", "FT-142"),
            "ferrfleet/ticket-ft-142"
        );
        // Une reference GitHub porte des caracteres que git refuse.
        assert_eq!(
            branch_name("ferrfleet/ticket-", "FerrLabs/kit#88"),
            "ferrfleet/ticket-ferrlabs-kit-88"
        );
        // Pas de tiret final, pas de doublons de separateurs.
        assert_eq!(
            branch_name("ferrfleet/ticket-", "FT -- 9 "),
            "ferrfleet/ticket-ft-9"
        );
        // Le reste des caracteres que git refuse dans une ref
        // (~ ^ : ? * [ \) sont aussi normalises un par un.
        assert_eq!(
            branch_name("ferrfleet/ticket-", "FT~1^2:3?4*5[6\\7"),
            "ferrfleet/ticket-ft-1-2-3-4-5-6-7"
        );
        // Un point de tete ou une sequence ".." ne produisent pas de tiret
        // en trop : meme logique de dedoublonnage que pour les espaces.
        assert_eq!(
            branch_name("ferrfleet/ticket-", ".FT-9"),
            "ferrfleet/ticket-ft-9"
        );
        assert_eq!(
            branch_name("ferrfleet/ticket-", "FT..9"),
            "ferrfleet/ticket-ft-9"
        );
        // Une reference entierement non alphanumerique s'effondre sur le
        // prefixe nu : deux tickets distincts dont la reference ne contient
        // aucun caractere alphanumerique collisionneraient sur le meme nom
        // de branche. Documente ici plutot que corrige : le contrat de
        // `branch_name` ne garantit pas l'unicite, seulement la validite
        // syntaxique pour git ; l'appelant (tache 9, cote API) doit fournir
        // une reference de ticket qui contient au moins un caractere
        // alphanumerique.
        assert_eq!(branch_name("ferrfleet/ticket-", "###"), "ferrfleet/ticket");
    }
}
