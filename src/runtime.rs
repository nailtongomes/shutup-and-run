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
    async fn start(&self, spec: &StartSpec) -> Result<String, RuntimeError>;
    async fn inspect(&self, id: &str) -> Result<Inspected, RuntimeError>;
    async fn list_expired(&self, now_unix: u64) -> Result<Vec<String>, RuntimeError>;
    async fn kill(&self, id: &str) -> Result<(), RuntimeError>;
    /// Remove our containers left in `created` after a crash between create and start.
    async fn cleanup_created(&self) -> Result<usize, RuntimeError>;
}
