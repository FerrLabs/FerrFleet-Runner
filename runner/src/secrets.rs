use serde_json::Value;

use crate::claude::{claude_env_path, env_file_entries};
use crate::config::Env;
use crate::redactor::Secret;

const CREDENTIAL_WORDS: [&str; 11] = [
    "TOKEN",
    "KEY",
    "APIKEY",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "CREDENTIALS",
    "AUTH",
    "AUTHORIZATION",
    "COOKIE",
];

pub fn names_a_credential(name: &str) -> bool {
    name.split(['_', '-']).any(|word| {
        CREDENTIAL_WORDS
            .iter()
            .any(|c| word.eq_ignore_ascii_case(c))
    })
}

pub fn of_process() -> Vec<Secret> {
    let inherited = std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)));
    let mut secrets = named(inherited);
    if let Ok(contents) = std::fs::read_to_string(claude_env_path()) {
        secrets.extend(named(
            env_file_entries(&contents).map(|(name, value)| (name.to_owned(), value.to_owned())),
        ));
    }
    secrets
}

pub fn of_run(env: &Env) -> Vec<Secret> {
    let mut secrets = of_process();
    secrets.extend(Secret::new("FERRFLEET_RUN_TOKEN", &env.run_token));
    secrets
}

pub fn of_mcp(config: Option<&Value>) -> Vec<Secret> {
    let servers = config
        .and_then(|config| config.get("mcpServers"))
        .and_then(Value::as_object);
    let mut secrets = Vec::new();
    for server in servers.into_iter().flat_map(serde_json::Map::values) {
        for (name, value) in credential_strings(server.get("env")) {
            secrets.extend(Secret::new(name, value));
        }
        for (name, value) in credential_strings(server.get("headers")) {
            secrets.extend(Secret::new(name, value));
            if let Some((_, credential)) = value.trim().split_once(char::is_whitespace) {
                secrets.extend(Secret::new(name, credential.trim()));
            }
        }
    }
    secrets
}

fn named(vars: impl Iterator<Item = (String, String)>) -> Vec<Secret> {
    vars.filter(|(name, _)| names_a_credential(name))
        .filter_map(|(name, value)| Secret::new(&name, &value))
        .collect()
}

fn credential_strings(map: Option<&Value>) -> impl Iterator<Item = (&str, &str)> {
    map.and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(name, _)| names_a_credential(name))
        .filter_map(|(name, value)| Some((name.as_str(), value.as_str()?)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{named, names_a_credential, of_mcp};
    use crate::redactor::Redactor;
    use crate::test_support::fake_value;

    #[test]
    fn credential_names_are_recognised_by_whole_word() {
        for name in [
            "FERRFLEET_RUN_TOKEN",
            "FERRFLEET_POOL_TOKEN",
            "FERRFLEET_ORG_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "Authorization",
            "X-Api-Key",
            "db_password",
        ] {
            assert!(names_a_credential(name), "{name}");
        }
        for name in [
            "TOKENIZERS_PARALLELISM",
            "KEYBOARD",
            "FERRFLEET_RUN_ID",
            "FERRFLEET_API_URL",
            "PATH",
            "X-MCP-Toolsets",
            "SONARQUBE_READ_ONLY",
        ] {
            assert!(!names_a_credential(name), "{name}");
        }
    }

    #[test]
    fn only_credential_variables_long_enough_are_registered() {
        let token = fake_value(114);
        let vars = [
            ("CLAUDE_CODE_OAUTH_TOKEN", token.as_str()),
            ("GH_TOKEN", "short"),
            ("HOME", "/home/runner/work"),
            ("FERRFLEET_API_URL", "https://api.example.com"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned()));

        let redactor = Redactor::new(named(vars.into_iter()));

        assert_eq!(
            redactor.output(&format!(
                "{token} short /home/runner/work https://api.example.com"
            )),
            "<redacted> short /home/runner/work https://api.example.com"
        );
    }

    #[test]
    fn mcp_header_values_and_credential_env_values_are_registered() {
        let bearer = fake_value(126);
        let api_key = fake_value(127);
        let pat = fake_value(128);
        let config = json!({
            "mcpServers": {
                "grafana": {
                    "type": "http",
                    "url": "https://mcp.example.com/grafana",
                    "headers": {
                        "Authorization": format!("Bearer {bearer}"),
                        "X-MCP-Toolsets": "default,actions"
                    }
                },
                "sonar": {"type": "http", "url": "https://s.example", "headers": {"X-Api-Key": api_key}},
                "gh": {
                    "command": "github-mcp",
                    "env": {"GITHUB_PERSONAL_ACCESS_TOKEN": pat, "GITHUB_HOST": "https://github.example.com"}
                }
            }
        });

        let redactor = Redactor::new(of_mcp(Some(&config)));
        let out = redactor.output(&format!(
            "{bearer} {api_key} {pat} default,actions https://github.example.com"
        ));

        assert_eq!(
            out,
            "<redacted> <redacted> <redacted> default,actions https://github.example.com"
        );
    }

    #[test]
    fn a_config_without_servers_registers_nothing() {
        assert_eq!(of_mcp(None).len(), 0);
        assert_eq!(of_mcp(Some(&json!({"other": 1}))).len(), 0);
        assert_eq!(
            of_mcp(Some(&json!({"mcpServers": {"x": {"env": "nope"}}}))).len(),
            0
        );
    }
}
