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
    pub fn exclusive(root: &Path) -> io::Result<Option<Self>> {
        match Self::acquire(root, FileExt::try_lock) {
            Ok(lock) => Ok(Some(lock)),
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(err) => Err(err),
        }
    }

    pub fn shared(root: &Path) -> io::Result<Self> {
        Self::acquire(root, FileExt::try_lock_shared)
    }

    pub fn shared_waiting(root: &Path) -> io::Result<Self> {
        match Self::shared(root) {
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                info!(path = %root.display(), "waiting for a sweep of the work root to finish");
                let file = open_lock_file(root)?;
                FileExt::lock_shared(&file)?;
                Ok(Self { _file: file })
            }
            other => other,
        }
    }

    fn acquire(
        root: &Path,
        lock: impl FnOnce(&File) -> Result<(), TryLockError>,
    ) -> io::Result<Self> {
        let file = open_lock_file(root)?;
        lock(&file)?;
        Ok(Self { _file: file })
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
        if remove_workdir(&path) {
            removed += 1;
        }
    }
    if removed > 0 {
        info!(removed, path = %root.display(), "removed stale run directories from the work root");
    }
}

fn is_run_directory(entry: &fs::DirEntry) -> bool {
    entry.file_type().is_ok_and(|kind| kind.is_dir())
        && entry.file_name().to_str().is_some_and(is_run_id)
}

fn is_run_id(name: &str) -> bool {
    Uuid::parse_str(name).is_ok_and(|id| id.hyphenated().to_string() == name)
}

pub fn remove_workdir(workdir: &Path) -> bool {
    match fs::remove_dir_all(workdir) {
        Ok(()) => true,
        Err(err) if err.kind() == io::ErrorKind::NotFound => true,
        Err(err) => {
            warn!(?err, path = %workdir.display(), "could not remove a run directory");
            false
        }
    }
}
