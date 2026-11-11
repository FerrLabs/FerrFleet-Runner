use anyhow::{Result, bail};
use ferrfleet_shared::RunnerMode;
use serde_json::Value;
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const MIN_IMAGE_WITH_MCP_GRAFANA: &str = "1.9.0";

pub fn check(config: Option<&Value>, mode: RunnerMode, path: Option<&OsStr>) -> Result<()> {
    for (name, command) in stdio_servers(config) {
        if !mode.is_external() {
            bail!(
                "MCP server \"{name}\" runs \"{command}\" as a local process, which managed runs refuse: it only runs on a runner pool"
            );
        }
        if !resolves(command, path) {
            bail!(
                "MCP server \"{name}\" runs \"{command}\", which is not on PATH in this runner; use a runner image {MIN_IMAGE_WITH_MCP_GRAFANA} or later (the action's binary mode does not ship it)"
            );
        }
    }
    Ok(())
}

fn stdio_servers(config: Option<&Value>) -> impl Iterator<Item = (&str, &str)> {
    config
        .and_then(|config| config.get("mcpServers"))
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(name, server)| {
            let command = server.get("command")?.as_str()?;
            Some((name.as_str(), command))
        })
}

fn resolves(command: &str, path: Option<&OsStr>) -> bool {
    if command.contains('/') {
        return is_executable(Path::new(command));
    }
    path.is_some_and(|path| {
        std::env::split_paths(path).any(|dir| is_executable(&dir.join(command)))
    })
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use uuid::Uuid;

    struct Bin {
        dir: PathBuf,
    }

    impl Bin {
        fn with(name: &str, mode: u32) -> Self {
            let dir = std::env::temp_dir().join(format!("ferrfleet-preflight-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join(name);
            std::fs::write(&file, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
            Self { dir }
        }

        fn path(&self) -> std::ffi::OsString {
            self.dir.clone().into_os_string()
        }
    }

    impl Drop for Bin {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn grafana() -> Value {
        json!({"mcpServers": {"grafana": {
            "command": "mcp-grafana",
            "args": ["--disable-write"],
            "env": {"GRAFANA_URL": "http://grafana:3000"}
        }}})
    }

    fn http_only() -> Value {
        json!({"mcpServers": {"github": {"type": "http", "url": "https://mcp.example.com"}}})
    }

    #[test]
    fn a_managed_run_with_a_stdio_server_is_refused_even_when_the_command_exists() {
        let bin = Bin::with("mcp-grafana", 0o755);
        let err = check(Some(&grafana()), RunnerMode::Managed, Some(&bin.path())).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("grafana"), "{message}");
        assert!(message.contains("mcp-grafana"), "{message}");
        assert!(message.contains("managed"), "{message}");
    }

    #[test]
    fn a_managed_run_with_http_servers_only_is_unchanged() {
        check(Some(&http_only()), RunnerMode::Managed, None).unwrap();
        check(Some(&json!({"mcpServers": {}})), RunnerMode::Managed, None).unwrap();
        check(None, RunnerMode::Managed, None).unwrap();
    }

    #[test]
    fn an_external_run_passes_when_the_command_is_on_path() {
        let bin = Bin::with("mcp-grafana", 0o755);
        check(Some(&grafana()), RunnerMode::External, Some(&bin.path())).unwrap();
    }

    #[test]
    fn an_external_run_with_a_missing_command_fails_naming_it_and_the_image() {
        let bin = Bin::with("something-else", 0o755);
        let err = check(Some(&grafana()), RunnerMode::External, Some(&bin.path())).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("mcp-grafana"), "{message}");
        assert!(message.contains(MIN_IMAGE_WITH_MCP_GRAFANA), "{message}");
    }

    #[test]
    fn a_file_on_path_that_is_not_executable_does_not_count() {
        let bin = Bin::with("mcp-grafana", 0o644);
        assert!(check(Some(&grafana()), RunnerMode::External, Some(&bin.path())).is_err());
    }

    #[test]
    fn an_external_run_without_any_path_fails_instead_of_passing() {
        assert!(check(Some(&grafana()), RunnerMode::External, None).is_err());
    }

    #[test]
    fn a_command_given_as_a_path_is_checked_where_it_points() {
        let bin = Bin::with("server", 0o755);
        let present = json!({"mcpServers": {"s": {"command": bin.dir.join("server")}}});
        let absent = json!({"mcpServers": {"s": {"command": bin.dir.join("nope")}}});
        check(Some(&present), RunnerMode::External, None).unwrap();
        assert!(check(Some(&absent), RunnerMode::External, None).is_err());
    }

    #[test]
    fn an_external_run_with_http_servers_only_needs_nothing_on_path() {
        check(Some(&http_only()), RunnerMode::External, None).unwrap();
    }
}
