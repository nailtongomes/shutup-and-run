use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct StartSpec {
    pub image: String,
    pub env: Vec<String>,
    pub command: Option<Vec<String>>,
    pub deadline_unix: u64,
    pub nano_cpus: Option<i64>,
    pub memory_bytes: Option<i64>,
    pub shm_bytes: Option<i64>,
    /// Read-only file mounts, `host:container:ro`. Only the machine's own
    /// certificate passthrough fills this. A request has no field for it.
    pub binds: Vec<String>,
}

/// What Docker says it applied to a container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    pub nano_cpus: i64,
    pub memory_bytes: i64,
    pub memory_swap: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inspected {
    Running,
    Exited { exit_code: i64 },
    NotFound,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct RuntimeError(pub String);

impl RuntimeError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

#[async_trait]
pub trait Runtime: Send + Sync {
    async fn pull(&self, image: &str) -> Result<(), RuntimeError>;
    async fn running_count(&self) -> Result<usize, RuntimeError>;
    /// `id` is the job id. The container is named after it, and every other
    /// call takes that same id.
    async fn start(&self, id: &str, spec: &StartSpec) -> Result<(), RuntimeError>;
    async fn inspect(&self, id: &str) -> Result<Inspected, RuntimeError>;
    /// The limits on a container that still exists. None when it is gone.
    async fn applied(&self, id: &str) -> Result<Option<Applied>, RuntimeError>;
    async fn list_expired(&self, now_unix: u64) -> Result<Vec<String>, RuntimeError>;
    async fn kill(&self, id: &str) -> Result<(), RuntimeError>;
    async fn tail_logs(&self, id: &str, lines: usize) -> Result<String, RuntimeError>;
    async fn remove(&self, id: &str) -> Result<(), RuntimeError>;
    /// Stopped containers that still carry our label.
    async fn list_stopped(&self) -> Result<Vec<String>, RuntimeError>;
    async fn image_in_use(&self, image: &str) -> Result<bool, RuntimeError>;
    async fn remove_image(&self, image: &str) -> Result<(), RuntimeError>;
    /// Remove our containers left in `created` after a crash between create and start.
    async fn cleanup_created(&self) -> Result<usize, RuntimeError>;
    /// Did the kernel kill a process of this stopped container for memory?
    /// What Docker recorded (`State.OOMKilled`), not a guess from exit 137.
    async fn oom_killed(&self, _id: &str) -> Result<bool, RuntimeError> {
        Ok(false)
    }
    /// The CPU periods a running container has had, and how many of them it
    /// spent held back at its CPU limit. None where the daemon does not say.
    async fn cpu_throttling(&self, _id: &str) -> Result<Option<Throttling>, RuntimeError> {
        Ok(None)
    }
    /// The end of a container's stdout and stderr, at most `max_bytes`, for
    /// the job's log file. A runtime that streams overrides this.
    async fn output(&self, id: &str, max_bytes: usize) -> Result<Vec<u8>, RuntimeError> {
        let text = self.tail_logs(id, 100_000).await?;
        Ok(crate::joblogs::clip(text.as_bytes(), max_bytes).to_vec())
    }
}

/// Counted by the kernel since the container started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Throttling {
    pub periods: u64,
    pub throttled_periods: u64,
}

impl Throttling {
    /// Held back in at least half of its periods, over at least twenty
    /// seconds of them (a period is 100 ms): the job wanted clearly more CPU
    /// than its limit, for long enough not to be a start-up spike.
    pub fn significant(&self) -> bool {
        self.periods >= 200 && self.throttled_periods * 2 >= self.periods
    }
}
