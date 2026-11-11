use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;

use fs4::{FileExt, TryLockError};
use tracing::{info, warn};
use uuid::Uuid;

const LOCK_FILE: &str = ".ferrfleet.lock";

#[derive(Debug)]
pub struct RootLock {
    _file: File,
}

impl RootLock {
    pub fn exclusive(root: &Path) -> Option<Self> {
        Self::acquire(root, FileExt::try_lock)
    }

    pub fn shared(root: &Path) -> Option<Self> {
        Self::acquire(root, FileExt::try_lock_shared)
    }

    fn acquire(root: &Path, lock: impl FnOnce(&File) -> Result<(), TryLockError>) -> Option<Self> {
        let file = match open_lock_file(root) {
            Ok(file) => file,
            Err(err) => {
                warn!(?err, path = %root.display(), "could not open the work root lock");
                return None;
            }
        };
        match lock(&file) {
            Ok(()) => Some(Self { _file: file }),
            Err(TryLockError::WouldBlock) => None,
            Err(TryLockError::Error(err)) => {
                warn!(?err, path = %root.display(), "could not lock the work root");
                None
            }
        }
    }
}

fn open_lock_file(root: &Path) -> io::Result<File> {
    fs::create_dir_all(root)?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(LOCK_FILE))
}

pub fn clear_stale(root: &Path) {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return,
        Err(err) => {
            warn!(?err, path = %root.display(), "could not list the work root");
            return;
        }
    };
    let mut removed = 0usize;
    for entry in entries.filter_map(Result::ok).filter(is_run_directory) {
        let path = entry.path();
        match fs::remove_dir_all(&path) {
            Ok(()) => removed += 1,
            Err(err) => {
                warn!(?err, path = %path.display(), "could not remove a stale run directory");
            }
        }
    }
    if removed > 0 {
        info!(removed, path = %root.display(), "removed stale run directories from the work root");
    }
}

fn is_run_directory(entry: &fs::DirEntry) -> bool {
    entry.file_type().is_ok_and(|kind| kind.is_dir())
        && entry
            .file_name()
            .to_str()
            .is_some_and(|name| Uuid::parse_str(name).is_ok())
}
