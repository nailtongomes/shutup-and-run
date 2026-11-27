use crate::runtime::{Inspected, Progress, Runtime, RuntimeError, StartSpec};
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
    /// What a pull reports before it waits for `release_pull`.
    held_pull: Option<(u64, u64)>,
    starts: u64,
    specs: Vec<StartSpec>,
    fail_list: bool,
    fail_start: bool,
    exit_on_start: Option<i64>,
    oom_on_start: bool,
    oom: std::collections::HashSet<String>,
    stats: Option<crate::runtime::Stats>,
    disk: Option<u64>,
    screens: HashMap<String, std::net::SocketAddr>,
}

pub struct MockRuntime {
    inner: Mutex<Inner>,
    pull_gate: tokio::sync::Notify,
}

impl Default for MockRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl MockRuntime {
    pub fn new() -> Self {
        Self {
            pull_gate: tokio::sync::Notify::new(),
            inner: Mutex::new(Inner {
                running: HashMap::new(),
                stopped: HashMap::new(),
                logs: HashMap::new(),
                image_of: HashMap::new(),
                killed: Vec::new(),
                removed: Vec::new(),
                removed_images: Vec::new(),
                pulls: Vec::new(),
                held_pull: None,
                starts: 0,
                specs: Vec::new(),
                fail_list: false,
                fail_start: false,
                exit_on_start: None,
                oom_on_start: false,
                oom: std::collections::HashSet::new(),
                stats: None,
                disk: None,
                screens: HashMap::new(),
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

    /// Where the container of `id` serves its screen while it runs.
    pub async fn set_screen(&self, id: &str, address: std::net::SocketAddr) {
        self.inner
            .lock()
            .await
            .screens
            .insert(id.to_string(), address);
    }

    /// The container is gone and nobody was told, as after a power cut.
    pub async fn vanish(&self, id: &str) {
        self.inner.lock().await.running.remove(id);
    }

    /// The container stopped with `code` and no die event was delivered.
    pub async fn stop_unseen(&self, id: &str, code: i64) {
        let mut inner = self.inner.lock().await;
        inner.running.remove(id);
        inner.stopped.insert(id.to_string(), code);
    }

    pub async fn start_count(&self) -> u64 {
        self.inner.lock().await.starts
    }

    /// What each started container was given, in start order.
    pub async fn specs(&self) -> Vec<StartSpec> {
        self.inner.lock().await.specs.clone()
    }

    /// The next pulls report `done` of `total` and wait for `release_pull`.
    pub async fn hold_pull(&self, done: u64, total: u64) {
        self.inner.lock().await.held_pull = Some((done, total));
    }

    pub async fn release_pull(&self) {
        self.inner.lock().await.held_pull = None;
        self.pull_gate.notify_one();
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

    /// Every container started from now on is killed for memory at once:
    /// exit 137, and Docker says OOMKilled.
    pub async fn oom_on_start(&self, yes: bool) {
        let mut inner = self.inner.lock().await;
        inner.oom_on_start = yes;
        inner.exit_on_start = yes.then_some(137);
    }

    /// What every running container reports about CPU throttling.
    pub async fn throttle(&self, throttling: Option<crate::runtime::Throttling>) {
        self.inner.lock().await.stats = throttling.map(|t| crate::runtime::Stats {
            throttling: Some(t),
            ..Default::default()
        });
    }

    /// What every running container reports, all of it.
    pub async fn set_stats(&self, stats: Option<crate::runtime::Stats>) {
        self.inner.lock().await.stats = stats;
    }

    /// What every stopped container says it wrote to its disk.
    pub async fn set_disk(&self, bytes: Option<u64>) {
        self.inner.lock().await.disk = bytes;
    }

    /// What the container printed, in place of the default line.
    pub async fn set_output(&self, id: &str, text: &str) {
        self.inner
            .lock()
            .await
            .logs
            .insert(id.to_string(), text.to_string());
    }

    /// The container stopped with `code`, as a die event would say.
    pub async fn stop(&self, id: &str, code: i64) {
        self.stop_unseen(id, code).await
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
    async fn pull(&self, image: &str, progress: &Progress<'_>) -> Result<(), RuntimeError> {
        let held = {
            let mut inner = self.inner.lock().await;
            inner.pulls.push(image.to_string());
            inner.held_pull
        };
        if let Some((done, total)) = held {
            progress(done, total);
            self.pull_gate.notified().await;
        }
        Ok(())
    }

    /// An image a pull brought here before.
    async fn has_image(&self, image: &str) -> bool {
        self.inner.lock().await.pulls.iter().any(|i| i == image)
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
                if inner.oom_on_start {
                    inner.oom.insert(id.clone());
                }
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

    async fn screen_address(&self, id: &str) -> Result<Option<std::net::SocketAddr>, RuntimeError> {
        let inner = self.inner.lock().await;
        Ok(inner
            .running
            .contains_key(id)
            .then(|| inner.screens.get(id).copied())
            .flatten())
    }

    async fn oom_killed(&self, id: &str) -> Result<bool, RuntimeError> {
        Ok(self.inner.lock().await.oom.contains(id))
    }

    async fn stats(&self, id: &str) -> Result<Option<crate::runtime::Stats>, RuntimeError> {
        let inner = self.inner.lock().await;
        Ok(inner.stats.filter(|_| inner.running.contains_key(id)))
    }

    async fn disk_written(&self, id: &str) -> Result<Option<u64>, RuntimeError> {
        let inner = self.inner.lock().await;
        Ok(inner.disk.filter(|_| inner.stopped.contains_key(id)))
    }
}
