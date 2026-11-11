use crate::runtime::{Inspected, Runtime, RuntimeError, StartSpec};
use async_trait::async_trait;
use std::collections::HashMap;
use tokio::sync::Mutex;

struct Inner {
    running: HashMap<String, u64>,
    killed: Vec<String>,
    pulls: Vec<String>,
    starts: u64,
    next: u64,
    fail_list: bool,
}

pub struct MockRuntime {
    inner: Mutex<Inner>,
}

impl Default for MockRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl MockRuntime {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                running: HashMap::new(),
                killed: Vec::new(),
                pulls: Vec::new(),
                starts: 0,
                next: 1,
                fail_list: false,
            }),
        }
    }

    pub async fn seed_running(&self, id: &str, deadline: u64) {
        self.inner
            .lock()
            .await
            .running
            .insert(id.to_string(), deadline);
    }

    pub async fn start_count(&self) -> u64 {
        self.inner.lock().await.starts
    }

    pub async fn pulls(&self) -> Vec<String> {
        self.inner.lock().await.pulls.clone()
    }

    pub async fn killed(&self) -> Vec<String> {
        self.inner.lock().await.killed.clone()
    }

    pub async fn running_ids(&self) -> Vec<String> {
        self.inner.lock().await.running.keys().cloned().collect()
    }

    pub async fn fail_lists(&self, yes: bool) {
        self.inner.lock().await.fail_list = yes;
    }
}

#[async_trait]
impl Runtime for MockRuntime {
    async fn pull(&self, image: &str) -> Result<(), RuntimeError> {
        self.inner.lock().await.pulls.push(image.to_string());
        Ok(())
    }

    async fn running_count(&self) -> Result<usize, RuntimeError> {
        let inner = self.inner.lock().await;
        if inner.fail_list {
            return Err(RuntimeError::new("list failed"));
        }
        Ok(inner.running.len())
    }

    async fn start(&self, spec: &StartSpec) -> Result<String, RuntimeError> {
        // Yield so a launcher that drops the slot lock between count and start
        // can be caught by the concurrent test.
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let mut inner = self.inner.lock().await;
        inner.starts += 1;
        inner.next += 1;
        let id = format!("{:064x}", inner.next);
        inner.running.insert(id.clone(), spec.deadline_unix);
        Ok(id)
    }

    async fn inspect(&self, id: &str) -> Result<Inspected, RuntimeError> {
        let inner = self.inner.lock().await;
        if inner.running.contains_key(id) {
            Ok(Inspected::Running)
        } else {
            Ok(Inspected::NotFound)
        }
    }

    async fn list_expired(&self, now_unix: u64) -> Result<Vec<String>, RuntimeError> {
        let inner = self.inner.lock().await;
        if inner.fail_list {
            return Err(RuntimeError::new("list failed"));
        }
        let mut ids: Vec<String> = inner
            .running
            .iter()
            .filter(|(_, deadline)| **deadline <= now_unix)
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        Ok(ids)
    }

    async fn kill(&self, id: &str) -> Result<(), RuntimeError> {
        let mut inner = self.inner.lock().await;
        inner.running.remove(id);
        inner.killed.push(id.to_string());
        Ok(())
    }

    async fn cleanup_created(&self) -> Result<usize, RuntimeError> {
        Ok(0)
    }
}
