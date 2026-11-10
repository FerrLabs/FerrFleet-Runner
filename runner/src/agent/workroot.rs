use std::io;
use std::path::Path;

use tracing::{info, warn};

pub fn clear_stale(root: &Path) {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return,
        Err(err) => {
            warn!(?err, path = %root.display(), "could not list the work root");
            return;
        }
    };
    let mut removed = 0usize;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        match remove_entry(&path) {
            Ok(()) => removed += 1,
            Err(err) => warn!(?err, path = %path.display(), "could not remove a stale entry"),
        }
    }
    if removed > 0 {
        info!(removed, path = %root.display(), "removed stale entries from the work root");
    }
}

fn remove_entry(path: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(path)?.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}
