use anyhow::{Context, Result};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub struct PrivateFile {
    path: PathBuf,
}

impl PrivateFile {
    pub fn write(name: &str, body: &[u8]) -> Result<Self> {
        let path = std::env::temp_dir().join(format!("ferrfleet-{name}-{}", Uuid::new_v4()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("creating {name} file"))?;
        let guard = Self { path };
        file.write_all(body)
            .with_context(|| format!("writing {name} file"))?;
        Ok(guard)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PrivateFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::PrivateFile;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_file_is_private_holds_the_body_and_is_removed_on_drop() {
        let file = PrivateFile::write("test", b"s3cret").expect("writing");
        let path = file.path().to_path_buf();

        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(std::fs::read(&path).expect("read"), b"s3cret");

        drop(file);
        assert!(!path.exists());
    }

    #[test]
    fn two_files_never_share_a_path() {
        let first = PrivateFile::write("test", b"").expect("first");
        let second = PrivateFile::write("test", b"").expect("second");
        assert_ne!(first.path(), second.path());
    }
}
