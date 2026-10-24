use anyhow::{Context, Result};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub struct McpConfigFile {
    path: PathBuf,
}

impl McpConfigFile {
    pub fn write(config: &serde_json::Value) -> Result<Self> {
        let body = serde_json::to_string(config).context("serializing mcp config")?;
        let path = std::env::temp_dir().join(format!("ferrfleet-mcp-{}.json", Uuid::new_v4()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .context("creating mcp config file")?;
        let guard = Self { path };
        file.write_all(body.as_bytes())
            .context("writing mcp config file")?;
        Ok(guard)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for McpConfigFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::McpConfigFile;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_file_is_private_holds_the_config_and_is_removed_on_drop() {
        let config = serde_json::json!({"mcpServers": {"gh": {"headers": {"Authorization": "Bearer s3cret"}}}});
        let file = McpConfigFile::write(&config).expect("writing");
        let path = file.path().to_path_buf();

        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        assert_eq!(stored, config);

        drop(file);
        assert!(!path.exists());
    }

    #[test]
    fn two_files_never_share_a_path() {
        let config = serde_json::json!({});
        let first = McpConfigFile::write(&config).expect("first");
        let second = McpConfigFile::write(&config).expect("second");
        assert_ne!(first.path(), second.path());
    }
}
