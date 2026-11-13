//! What the machine has left, read when a job asks to start. No daemon and no
//! history: two small reads per admission.
//!
//! On Windows these numbers are the WSL2 VM's, not the PC's. The VM can have
//! memory free while Windows itself is short, and the other way round. The
//! check keeps a job from starting into a full VM; `.wslconfig` is what keeps
//! the VM small on the PC.

use std::path::Path;

pub const GIB: u64 = 1024 * 1024 * 1024;

/// The shared-PC defaults. Memory figures shrink on a small machine, see
/// `shared_pc_memory`.
pub const SHARED_PC_CPUS: f64 = 1.0;
pub const SHARED_PC_MEMORY: u64 = 2 * GIB;
pub const SHARED_PC_MIN_DISK: u64 = 5 * GIB;

/// Per-job memory, and the free memory a new job needs, on a shared PC:
/// 2 GiB, or half of what the machine (the WSL2 VM) has when that is less.
/// A machine that reports nothing gets the 2 GiB.
pub fn shared_pc_memory(total: Option<u64>) -> u64 {
    match total {
        Some(total) => SHARED_PC_MEMORY.min(total / 2),
        None => SHARED_PC_MEMORY,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Free {
    pub memory: Option<u64>,
    pub disk: Option<u64>,
}

/// Why a job cannot start now. It can later, with nothing changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortage {
    pub resource: &'static str,
    pub free_bytes: u64,
    pub needed_bytes: u64,
}

impl std::fmt::Display for Shortage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "low on {}: {} free, {} needed",
            self.resource,
            human(self.free_bytes),
            human(self.needed_bytes)
        )
    }
}

/// A minimum of zero is off. A reading that is missing does not block:
/// the check protects a machine it can measure and does not stop one it cannot.
pub fn shortage(free: Free, min_memory: u64, min_disk: u64) -> Option<Shortage> {
    let short = |resource, free: Option<u64>, needed: u64| {
        free.filter(|free| needed > 0 && *free < needed)
            .map(|free| Shortage {
                resource,
                free_bytes: free,
                needed_bytes: needed,
            })
    };
    short("memory", free.memory, min_memory).or_else(|| short("disk", free.disk, min_disk))
}

pub fn read(disk_path: &Path) -> Free {
    Free {
        memory: memory_available(),
        disk: disk_free(disk_path),
    }
}

/// `MemTotal` of `/proc/meminfo`. None where that file does not exist.
pub fn memory_total() -> Option<u64> {
    meminfo("MemTotal:")
}

/// `MemAvailable`: what a new program could take without swapping.
pub fn memory_available() -> Option<u64> {
    meminfo("MemAvailable:")
}

fn meminfo(field: &str) -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    meminfo_field(&text, field)
}

pub(crate) fn meminfo_field(text: &str, field: &str) -> Option<u64> {
    let line = text.lines().find(|line| line.starts_with(field))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    kib.checked_mul(1024)
}

/// Where Docker keeps images, when it is in the usual place; otherwise the
/// folder given (the job history's).
/// shortcut: a Docker `data-root` somewhere else is not looked up. Upgrade to
/// `docker info` if a machine moves it.
pub fn disk_path(fallback: &Path) -> std::path::PathBuf {
    let docker = Path::new("/var/lib/docker");
    if docker.exists() {
        return docker.to_path_buf();
    }
    fallback
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf()
}

/// Bytes an unprivileged process could still write under `path`.
#[cfg(unix)]
pub fn disk_free(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is a NUL-terminated string and `stat` is room for one
    // statvfs, which is read only after the call says it filled it.
    let stat = unsafe {
        if libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) != 0 {
            return None;
        }
        stat.assume_init()
    };
    (stat.f_bavail as u64).checked_mul(stat.f_frsize as u64)
}

#[cfg(not(unix))]
pub fn disk_free(_path: &Path) -> Option<u64> {
    None
}

/// `2.0 GiB`, `512 MiB`.
pub fn human(bytes: u64) -> String {
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else {
        format!("{} MiB", bytes / (1024 * 1024))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_machine_gets_a_smaller_job() {
        assert_eq!(shared_pc_memory(None), 2 * GIB);
        assert_eq!(shared_pc_memory(Some(16 * GIB)), 2 * GIB);
        assert_eq!(shared_pc_memory(Some(4 * GIB)), 2 * GIB);
        // The VM of an 8 GiB PC at 40%.
        assert_eq!(shared_pc_memory(Some(3 * GIB)), 3 * GIB / 2);
    }

    #[test]
    fn a_shortage_names_the_resource_and_zero_or_unknown_never_blocks() {
        let free = Free {
            memory: Some(GIB),
            disk: Some(10 * GIB),
        };
        assert_eq!(shortage(free, GIB, 5 * GIB), None);
        let low = shortage(free, 2 * GIB, 5 * GIB).unwrap();
        assert_eq!(low.resource, "memory");
        assert_eq!(
            low.to_string(),
            "low on memory: 1.0 GiB free, 2.0 GiB needed"
        );
        let low = shortage(free, GIB, 20 * GIB).unwrap();
        assert_eq!((low.resource, low.free_bytes), ("disk", 10 * GIB));
        assert_eq!(shortage(free, 0, 0), None);
        let unknown = Free {
            memory: None,
            disk: None,
        };
        assert_eq!(shortage(unknown, 2 * GIB, 5 * GIB), None);
    }

    #[test]
    fn meminfo_is_read_in_bytes() {
        let text = "MemTotal:        3145728 kB\nMemFree:  1 kB\nMemAvailable:    1048576 kB\n";
        assert_eq!(meminfo_field(text, "MemTotal:"), Some(3 * GIB));
        assert_eq!(meminfo_field(text, "MemAvailable:"), Some(GIB));
        assert_eq!(meminfo_field(text, "Nope:"), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_disk_of_a_real_folder_is_measured() {
        assert!(disk_free(Path::new("/")).is_some());
        assert!(disk_free(Path::new("/no/such/folder")).is_none());
    }
}
