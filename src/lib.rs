//! shutup-and-run starts allowlisted containers on a single machine, up to a slot
//! limit read from Docker itself.

pub mod allowlist;
pub mod api;
pub mod auth;
pub mod bollard_rt;
pub mod bytesize;
pub mod callback;
pub mod cert;
pub mod certificate;
pub mod cli;
pub mod clock;
pub mod config;
pub mod devx;
pub mod hints;
pub mod host;
pub mod joblogs;
pub mod jobs;
pub mod launcher;
pub mod lending;
pub mod lock;
pub mod managed;
pub mod memory;
pub mod poll;
pub mod registry;
pub mod resources;
pub mod runtime;
pub mod safety;
pub mod status;
pub mod store;
pub mod wsl;

#[cfg(test)]
mod contract;
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
/// The container ended and the worker cannot say how: it is gone, or it
/// stopped with a non-zero code while the worker was not running.
pub const REASON_INTERRUPTED: &str = "interrupted";
