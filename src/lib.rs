//! shutup-and-run starts allowlisted containers on a single machine, up to a slot
//! limit read from Docker itself.

pub mod allowlist;
pub mod api;
pub mod auth;
pub mod bollard_rt;
pub mod bytesize;
pub mod callback;
pub mod cert;
pub mod clock;
pub mod config;
pub mod launcher;
pub mod lock;
pub mod poll;
pub mod registry;
pub mod runtime;
pub mod status;
pub mod store;
pub mod wsl;

#[cfg(test)]
pub mod mock;

pub const LABEL_KEY: &str = "shutup-and-run";
pub const LABEL_VALUE: &str = "1";
pub const DEADLINE_KEY: &str = "shutup-and-run.deadline";
pub const NAME_PREFIX: &str = "shutup-and-run-";

pub const REASON_EXITED: &str = "exited";
pub const REASON_TIMEOUT: &str = "timeout_kill";
pub const REASON_LAUNCHER: &str = "launcher_kill";
pub const REASON_START_FAILED: &str = "start_failed";
