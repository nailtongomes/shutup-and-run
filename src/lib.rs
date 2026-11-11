//! n3-launcher starts allowlisted containers on a single machine, up to a slot limit
//! read from Docker itself.

pub mod allowlist;
pub mod api;
pub mod auth;
pub mod bollard_rt;
pub mod bytesize;
pub mod clock;
pub mod config;
pub mod launcher;
pub mod registry;
pub mod runtime;

#[cfg(test)]
pub mod mock;

pub const LABEL_KEY: &str = "n3.launcher";
pub const LABEL_VALUE: &str = "1";
pub const DEADLINE_KEY: &str = "n3.deadline";
pub const NAME_PREFIX: &str = "n3-";
