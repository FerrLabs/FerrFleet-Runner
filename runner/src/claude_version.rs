use anyhow::{Context, Result, bail};
use tokio::process::Command;
use tracing::warn;

pub const MINIMUM: Version = Version(2, 1, 98);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

impl Version {
    pub fn parse(output: &str) -> Option<Self> {
        let mut parts = output.split_whitespace().next()?.split('.');
        let mut next = || parts.next()?.parse().ok();
        let version = Self(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }
}

pub async fn ensure_supported() -> Result<()> {
    let output = Command::new("claude")
        .arg("--version")
        .output()
        .await
        .context("running `claude --version`")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    match Version::parse(&stdout) {
        Some(found) if found < MINIMUM => bail!(
            "claude CLI {found} is too old, the runner needs {MINIMUM} or newer \
             (it passes --exclude-dynamic-system-prompt-sections)"
        ),
        Some(_) => Ok(()),
        None => {
            warn!(output = %stdout.trim(), "could not read the claude CLI version; continuing");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MINIMUM, Version};

    #[test]
    fn the_version_is_read_from_the_cli_banner() {
        assert_eq!(
            Version::parse("2.1.98 (Claude Code)\n"),
            Some(Version(2, 1, 98))
        );
    }

    #[test]
    fn output_that_is_not_a_version_reads_as_none() {
        assert_eq!(Version::parse(""), None);
        assert_eq!(Version::parse("claude: not found"), None);
        assert_eq!(Version::parse("2.1 (Claude Code)"), None);
        assert_eq!(Version::parse("2.1.98.4"), None);
    }

    #[test]
    fn versions_compare_numerically_not_textually() {
        assert!(Version(2, 1, 9) < MINIMUM);
        assert!(Version(2, 0, 200) < MINIMUM);
        assert!(Version(2, 1, 98) >= MINIMUM);
        assert!(Version(2, 10, 0) > MINIMUM);
        assert!(Version(3, 0, 0) > MINIMUM);
    }
}
