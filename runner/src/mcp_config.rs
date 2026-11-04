use crate::private_file::PrivateFile;
use anyhow::{Context, Result};

pub fn write(config: &serde_json::Value) -> Result<PrivateFile> {
    let body = serde_json::to_vec(config).context("serializing mcp config")?;
    PrivateFile::write("mcp", &body)
}
