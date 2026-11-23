//! One instance per machine. The process holds an exclusive lock on a file
//! for as long as it lives; the OS drops it when the process ends, however
//! it ends, so there is no stale lock to clean up.

use std::fs::File;
#[cfg(not(target_os = "android"))]
use std::fs::TryLockError;
use std::path::Path;

/// How the error of a lock held by another process begins.
pub const HELD: &str = "another shutup-and-run is already running";

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
    match try_lock(&file) {
        Ok(true) => Ok(file),
        Ok(false) => Err(format!("{HELD} ({} is locked)", path.display())),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Locked now (true), or held by another process (false).
#[cfg(not(target_os = "android"))]
fn try_lock(file: &File) -> std::io::Result<bool> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

/// The standard library has no `try_lock` on Android: the same `flock`.
#[cfg(target_os = "android")]
fn try_lock(file: &File) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd;
    // SAFETY: flock on a descriptor this File owns and keeps open; the lock
    // goes with the descriptor, as std's does.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(false)
    } else {
        Err(error)
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
