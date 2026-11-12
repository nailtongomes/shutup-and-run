//! One instance per machine. The process holds an exclusive lock on a file
//! for as long as it lives; the OS drops it when the process ends, however
//! it ends, so there is no stale lock to clean up.

use std::fs::{File, TryLockError};
use std::path::Path;

/// Keep the returned file alive. Dropping it releases the lock.
pub fn acquire(path: &Path) -> Result<File, String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
    }
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(format!(
            "another shutup-and-run is already running ({} is locked)",
            path.display()
        )),
        Err(TryLockError::Error(e)) => Err(format!("{}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_acquire_is_refused_until_the_first_is_dropped() {
        let path = std::env::temp_dir().join(format!("sar-lock-{}.lock", std::process::id()));
        let first = acquire(&path).unwrap();
        let refused = acquire(&path).unwrap_err();
        assert!(refused.contains("already running"), "{refused}");
        drop(first);
        acquire(&path).unwrap();
        let _ = std::fs::remove_file(&path);
    }
}
