use crate::RunConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptParts<'a> {
    pub stable: Option<&'a str>,
    pub per_run: &'a str,
}

impl RunConfig {
    #[must_use]
    pub fn prompt_parts(&self) -> PromptParts<'_> {
        let whole = PromptParts {
            stable: None,
            per_run: &self.prompt,
        };
        let Some(stable) = self
            .stable_prefix
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        else {
            return whole;
        };
        match self.prompt.strip_prefix(stable).map(str::trim_start) {
            Some(per_run) if !per_run.is_empty() => PromptParts {
                stable: Some(stable),
                per_run,
            },
            _ => whole,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(prompt: &str, stable_prefix: Option<&str>) -> RunConfig {
        let mut cfg: RunConfig = serde_json::from_value(serde_json::json!({
            "run_id": "01999999-9999-7999-9999-999999999999",
            "agent_id": "pr-agent",
            "prompt": prompt,
            "working_dir": "/workdir",
        }))
        .expect("a minimal config");
        cfg.stable_prefix = stable_prefix.map(str::to_owned);
        cfg
    }

    #[test]
    fn a_config_from_an_api_without_the_field_keeps_the_whole_prompt_per_run() {
        let cfg = config("## How to write\n\nreview it\n\n## Event context\n{}", None);

        assert_eq!(
            cfg.prompt_parts(),
            PromptParts {
                stable: None,
                per_run: &cfg.prompt,
            }
        );
    }

    #[test]
    fn the_stable_prefix_is_split_off_and_the_rest_is_per_run() {
        let cfg = config(
            "rules\n\nreview it\n\n## Event context\n{\"head_sha\": \"abc\"}",
            Some("rules\n\nreview it"),
        );

        let parts = cfg.prompt_parts();

        assert_eq!(parts.stable, Some("rules\n\nreview it"));
        assert_eq!(parts.per_run, "## Event context\n{\"head_sha\": \"abc\"}");
    }

    #[test]
    fn a_prefix_the_prompt_does_not_start_with_is_ignored_rather_than_losing_text() {
        let cfg = config("other rules\n\n## Event context\n{}", Some("rules"));

        assert_eq!(cfg.prompt_parts().stable, None);
        assert_eq!(cfg.prompt_parts().per_run, cfg.prompt);
    }

    #[test]
    fn a_prompt_that_is_only_the_prefix_stays_whole_so_the_user_turn_is_never_empty() {
        let cfg = config("rules\n\nreview it\n\n", Some("rules\n\nreview it"));

        assert_eq!(cfg.prompt_parts().stable, None);
        assert_eq!(cfg.prompt_parts().per_run, cfg.prompt);
    }

    #[test]
    fn a_blank_prefix_is_no_prefix() {
        let cfg = config("review it", Some("  \n"));

        assert_eq!(cfg.prompt_parts().stable, None);
    }
}
