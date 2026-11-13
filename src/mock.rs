use crate::runtime::{Inspected, Runtime, RuntimeError, StartSpec};
use async_trait::async_trait;
use std::collections::HashMap;
use tokio::sync::Mutex;

struct Inner {
    running: HashMap<String, u64>,
    stopped: HashMap<String, i64>,
    logs: HashMap<String, String>,
    image_of: HashMap<String, String>,
    killed: Vec<String>,
    removed: Vec<String>,
    removed_images: Vec<String>,
    pulls: Vec<String>,
    starts: u64,
    specs: Vec<StartSpec>,
    fail_list: bool,
    fail_start: bool,
    exit_on_start: Option<i64>,
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
                stopped: HashMap::new(),
                logs: HashMap::new(),
                image_of: HashMap::new(),
                killed: Vec::new(),
                removed: Vec::new(),
                removed_images: Vec::new(),
                pulls: Vec::new(),
                starts: 0,
                specs: Vec::new(),
                fail_list: false,
                fail_start: false,
                exit_on_start: None,
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

    /// What each started container was given, in start order.
    pub async fn specs(&self) -> Vec<StartSpec> {
        self.inner.lock().await.specs.clone()
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

    /// Every container started from now on has already exited with `code`,
    /// like `hello-world`, instead of running until it is killed.
    pub async fn exit_on_start(&self, code: Option<i64>) {
        self.inner.lock().await.exit_on_start = code;
    }

    pub async fn fail_start(&self, yes: bool) {
        self.inner.lock().await.fail_start = yes;
    }

    pub async fn removed_images(&self) -> Vec<String> {
        self.inner.lock().await.removed_images.clone()
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

    async fn start(&self, id: &str, spec: &StartSpec) -> Result<(), RuntimeError> {
        // Yield so a launcher that drops the slot lock between count and start
        // can be caught by the concurrent test.
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let mut inner = self.inner.lock().await;
        if inner.fail_start {
            return Err(RuntimeError::new("start failed"));
        }
        inner.starts += 1;
        inner.specs.push(spec.clone());
        let id = id.to_string();
        match inner.exit_on_start {
            Some(code) => {
                inner.stopped.insert(id.clone(), code);
            }
            None => {
                inner.running.insert(id.clone(), spec.deadline_unix);
            }
        }
        inner.logs.insert(id.clone(), format!("log {id}\n"));
        inner.image_of.insert(id, spec.image.clone());
        Ok(())
    }

    async fn applied(&self, id: &str) -> Result<Option<crate::runtime::Applied>, RuntimeError> {
        let inner = self.inner.lock().await;
        if !inner.running.contains_key(id) && !inner.stopped.contains_key(id) {
            return Ok(None);
        }
        Ok(inner
            .specs
            .iter()
            .rev()
            .find(|spec| inner.image_of.get(id) == Some(&spec.image))
            .map(|spec| crate::runtime::Applied {
                nano_cpus: spec.nano_cpus.unwrap_or(0),
                memory_bytes: spec.memory_bytes.unwrap_or(0),
                memory_swap: spec.memory_bytes.unwrap_or(0),
            }))
    }

    async fn inspect(&self, id: &str) -> Result<Inspected, RuntimeError> {
        let inner = self.inner.lock().await;
        if inner.running.contains_key(id) {
            Ok(Inspected::Running)
        } else if let Some(code) = inner.stopped.get(id) {
            Ok(Inspected::Exited { exit_code: *code })
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
        inner.stopped.insert(id.to_string(), 137);
        inner.killed.push(id.to_string());
        Ok(())
    }

    async fn tail_logs(&self, id: &str, _lines: usize) -> Result<String, RuntimeError> {
        let inner = self.inner.lock().await;
        inner
            .logs
            .get(id)
            .cloned()
            .ok_or_else(|| RuntimeError::new("no logs"))
    }

    async fn remove(&self, id: &str) -> Result<(), RuntimeError> {
        let mut inner = self.inner.lock().await;
        inner.running.remove(id);
        inner.stopped.remove(id);
        inner.removed.push(id.to_string());
        Ok(())
    }

    async fn list_stopped(&self) -> Result<Vec<String>, RuntimeError> {
        let inner = self.inner.lock().await;
        let mut ids: Vec<String> = inner.stopped.keys().cloned().collect();
        ids.sort();
        Ok(ids)
    }

    async fn image_in_use(&self, image: &str) -> Result<bool, RuntimeError> {
        let inner = self.inner.lock().await;
        let live = inner
            .running
            .keys()
            .chain(inner.stopped.keys())
            .any(|id| inner.image_of.get(id).map(String::as_str) == Some(image));
        Ok(live)
    }

    async fn remove_image(&self, image: &str) -> Result<(), RuntimeError> {
        self.inner
            .lock()
            .await
            .removed_images
            .push(image.to_string());
        Ok(())
    }

    async fn cleanup_created(&self) -> Result<usize, RuntimeError> {
        Ok(0)
    }
}
