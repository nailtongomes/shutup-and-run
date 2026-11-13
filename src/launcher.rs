use crate::allowlist::{image_allowed, validate_image_name};
use crate::callback::{spawn_callback, validate_callback_url};
use crate::clock::{Clock, SystemClock};
use crate::config::Settings;
use crate::runtime::{Inspected, Runtime, RuntimeError, StartSpec};
use crate::store::{JobRecord, NewJob, Store, StoreError};
use crate::{REASON_EXITED, REASON_START_FAILED, REASON_TIMEOUT};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct LaunchRequest {
    pub image: String,
    pub env: HashMap<String, String>,
    pub timeout_secs: Option<u64>,
    pub cpu: Option<f64>,
    pub memory_bytes: Option<i64>,
    pub shm_bytes: Option<i64>,
    pub command: Option<Vec<String>>,
    pub job_key: Option<String>,
    pub callback_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slots {
    pub total: usize,
    pub used: usize,
    pub free: usize,
    pub draining: bool,
}

/// Where a task came from. Only a `Poll` task may skip the allowlist, and
/// only when `poll_any_image` is set. A push never does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobSource {
    Push,
    Poll,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchOutcome {
    Created(JobRecord),
    Existing(JobRecord),
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("{0}")]
    BadRequest(String),
    #[error("image is not allowlisted")]
    Forbidden,
    #[error("no free slots")]
    NoSlots { total: usize, used: usize },
    #[error("draining")]
    Draining,
    /// The machine is short of memory or disk right now. Nothing was stored.
    #[error("{0}")]
    LowResources(crate::resources::Shortage),
    #[error("{0}")]
    Unavailable(String),
}

pub struct Launcher {
    settings: Settings,
    runtime: Arc<dyn Runtime>,
    clock: Arc<dyn Clock>,
    store: Arc<Store>,
    /// shortcut: one process mutex around list+create. The count itself is the
    /// Docker list, so a restart does not forget running jobs. Two shutup-and-run
    /// processes on the same daemon can still exceed max_slots. Upgrade to a
    /// shared lock if you ever run more than one process.
    slots: Mutex<()>,
    /// Ids the reaper has decided to SIGKILL. The die event and the reaper
    /// share this so the stored reason is timeout_kill rather than exited.
    timeouts: Mutex<HashSet<String>>,
    /// Jobs answered with 202 whose image is still being pulled. They hold a
    /// slot. This set is memory only: after a restart such a row has no
    /// container and reconcile closes it.
    starting: Mutex<HashSet<String>>,
    /// Why the last poll failed, for status.json and the doctor. None after
    /// a poll that was answered.
    poll_error: std::sync::Mutex<Option<String>>,
}

impl Launcher {
    pub fn new(settings: Settings, runtime: Arc<dyn Runtime>) -> Result<Arc<Self>, StoreError> {
        Self::with_clock(settings, runtime, Arc::new(SystemClock))
    }

    pub fn with_clock(
        settings: Settings,
        runtime: Arc<dyn Runtime>,
        clock: Arc<dyn Clock>,
    ) -> Result<Arc<Self>, StoreError> {
        let store = Arc::new(Store::open(&settings.history_path)?);
        Ok(Arc::new(Self {
            settings,
            runtime,
            clock,
            store,
            slots: Mutex::new(()),
            timeouts: Mutex::new(HashSet::new()),
            starting: Mutex::new(HashSet::new()),
            poll_error: std::sync::Mutex::new(None),
        }))
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn token_matches(&self, provided: &str) -> bool {
        self.settings.token.matches(provided)
    }

    pub fn ban_until(&self, ip: &str) -> Result<Option<i64>, LaunchError> {
        self.store
            .active_ban(ip, self.clock.now_unix() as i64)
            .map_err(store_err)
    }

    pub fn note_auth_failure(&self, ip: &str) -> Result<crate::store::Strike, LaunchError> {
        self.store
            .record_failure(
                ip,
                self.clock.now_unix() as i64,
                self.settings.auth_failures,
                self.settings.ban_secs as i64,
            )
            .map_err(store_err)
    }

    pub fn clear_auth_failures(&self, ip: &str) -> Result<(), LaunchError> {
        self.store.clear_failures(ip).map_err(store_err)
    }

    pub fn list_bans(&self) -> Result<Vec<crate::store::Ban>, LaunchError> {
        self.store
            .list_bans(self.clock.now_unix() as i64)
            .map_err(store_err)
    }

    pub fn lift_ban(&self, ip: &str) -> Result<bool, LaunchError> {
        self.store.lift_ban(ip).map_err(store_err)
    }

    pub fn draining(&self) -> Result<bool, LaunchError> {
        self.store.draining().map_err(store_err)
    }

    pub fn set_draining(&self, draining: bool) -> Result<bool, LaunchError> {
        self.store.set_draining(draining).map_err(store_err)?;
        tracing::info!(draining, "drain mode");
        Ok(draining)
    }

    pub async fn slots(&self) -> Result<Slots, LaunchError> {
        let used = self.used_slots().await?;
        let total = self.settings.max_slots;
        let draining = self.draining()?;
        Ok(Slots {
            total,
            used,
            free: total.saturating_sub(used),
            draining,
        })
    }

    /// Set when the machine has too little memory or disk for one more job.
    /// Read fresh every time: it is two small reads, and it clears by itself.
    pub fn shortage(&self) -> Option<crate::resources::Shortage> {
        let (memory, disk) = (self.settings.min_free_memory, self.settings.min_free_disk);
        if memory == 0 && disk == 0 {
            return None;
        }
        let path = crate::resources::disk_path(&self.settings.history_path);
        crate::resources::shortage(crate::resources::read(&path), memory, disk)
    }

    pub fn set_poll_error(&self, error: Option<String>) {
        *self.poll_error.lock().expect("poll error") = error;
    }

    pub fn last_job(&self) -> Result<Option<JobRecord>, LaunchError> {
        self.store.last_job().map_err(store_err)
    }

    /// One read for the status page and for `status.json`. Docker down is
    /// `failed`; the page and the file cannot disagree about that.
    pub async fn live_status(&self) -> crate::status::Live {
        let (slots, docker_error) = match self.slots().await {
            Ok(slots) => (Some(slots), None),
            Err(err) => (None, Some(err.to_string())),
        };
        let mut job_ids = self.store.running_ids().unwrap_or_default();
        job_ids.sort();
        let last = self.last_job().ok().flatten();
        let last_job_error = match (&docker_error, &last) {
            (None, Some(job)) => crate::status::job_error(job),
            _ => None,
        };
        crate::status::Live {
            slots_used: slots.map(|s| s.used as u64),
            slots_total: slots.map(|s| s.total as u64),
            draining: slots.is_some_and(|s| s.draining),
            job_ids,
            last_job_at: last
                .as_ref()
                .map(|job| job.ended_at.unwrap_or(job.started_at)),
            last_job_error,
            docker_error,
            last,
            poll_error: self.poll_error.lock().expect("poll error").clone(),
            waiting_for: self.shortage().map(|s| s.to_string()),
            limits: crate::status::Limits::of(&self.settings),
            config_warning: self.settings.config_warning.clone(),
        }
    }

    pub fn counts(&self) -> Result<crate::store::JobCounts, LaunchError> {
        self.store.counts().map_err(store_err)
    }

    /// Answers as soon as the slot is taken and the row is written. The pull
    /// and the start happen afterwards, so a caller is not kept waiting on a
    /// registry and a full machine says so at once. A pull or start that
    /// fails ends the job with reason `start_failed`.
    pub async fn launch(
        self: &Arc<Self>,
        req: LaunchRequest,
    ) -> Result<LaunchOutcome, LaunchError> {
        self.launch_from(req, JobSource::Push).await
    }

    pub async fn launch_from(
        self: &Arc<Self>,
        req: LaunchRequest,
        source: JobSource,
    ) -> Result<LaunchOutcome, LaunchError> {
        validate_image_name(&req.image).map_err(LaunchError::BadRequest)?;
        let skip_allowlist = source == JobSource::Poll && self.settings.poll_any_image;
        if !skip_allowlist && !image_allowed(&req.image, &self.settings.allowlist) {
            return Err(LaunchError::Forbidden);
        }
        let timeout = resolve_timeout(req.timeout_secs, &self.settings)?;
        let env = validate_env(req.env)?;
        let command = validate_command(req.command)?;
        let job_key = validate_job_key(req.job_key)?;
        let callback_url = validate_callback(req.callback_url)?;
        let nano_cpus = limit_cpu(req.cpu, self.settings.max_job_cpus)?;
        let memory_bytes = limit_memory(req.memory_bytes, self.settings.max_job_memory)?;

        let spec = StartSpec {
            image: req.image.clone(),
            env,
            command,
            // Set when the container starts. Pull time does not count.
            deadline_unix: 0,
            nano_cpus,
            memory_bytes,
            shm_bytes: req.shm_bytes,
        };

        let guard = self.slots.lock().await;
        if self.draining()? {
            return Err(LaunchError::Draining);
        }
        if let Some(key) = &job_key {
            if let Some(existing) = self.store.get_by_key(key).map_err(store_err)? {
                return Ok(LaunchOutcome::Existing(existing));
            }
        }
        let used = self.used_slots().await?;
        if used >= self.settings.max_slots {
            return Err(LaunchError::NoSlots {
                total: self.settings.max_slots,
                used,
            });
        }
        // Inside the slot lock, so two requests cannot both pass on the
        // memory that only one of them will get.
        if let Some(shortage) = self.shortage() {
            return Err(LaunchError::LowResources(shortage));
        }
        let id = random_id();
        let record = NewJob {
            id: id.clone(),
            image: spec.image.clone(),
            job_key: job_key.clone(),
            started_at: self.clock.now_unix() as i64,
            callback_url,
        };
        match self.store.insert_running(&record) {
            Ok(()) => {}
            Err(StoreError::Conflict) => {
                // Another process won the job_key.
                if let Some(key) = &job_key {
                    if let Some(existing) = self.store.get_by_key(key).map_err(store_err)? {
                        return Ok(LaunchOutcome::Existing(existing));
                    }
                }
                return Err(LaunchError::Unavailable("job_key conflict".into()));
            }
            Err(err) => return Err(store_err(err)),
        }
        let saved = self
            .store
            .get(&id)
            .map_err(store_err)?
            .ok_or_else(|| LaunchError::Unavailable("job disappeared after insert".into()))?;
        self.starting.lock().await.insert(id.clone());
        drop(guard);

        tracing::info!(%id, image = %spec.image, timeout_secs = timeout, "job accepted");
        let launcher = self.clone();
        tokio::spawn(async move { launcher.pull_and_start(id, spec, timeout).await });
        Ok(LaunchOutcome::Created(saved))
    }

    async fn pull_and_start(&self, id: String, mut spec: StartSpec, timeout: u64) {
        // A pull that hangs would hold the slot forever, so it gets the job's own time limit.
        let limit = std::time::Duration::from_secs(timeout);
        let failure = match tokio::time::timeout(limit, self.runtime.pull(&spec.image)).await {
            Err(_) => Some("image pull timed out"),
            Ok(Err(err)) => {
                tracing::warn!(%id, image = %spec.image, error = %err, "pull failed");
                Some("failed to pull image")
            }
            Ok(Ok(())) => {
                spec.deadline_unix = self.clock.now_unix().saturating_add(timeout);
                match self.runtime.start(&id, &spec).await {
                    Ok(()) => None,
                    Err(err) => {
                        tracing::warn!(%id, image = %spec.image, error = %err, "start failed");
                        Some("failed to start container")
                    }
                }
            }
        };
        if let Some(message) = failure {
            // Close the row before the slot is released, so a status request
            // cannot find the id neither starting nor recorded as failed.
            let ended = self.clock.now_unix() as i64;
            match self
                .store
                .finish(&id, ended, None, REASON_START_FAILED, message)
            {
                Ok(Some(job)) => self.fire_callback(&job),
                Ok(None) => {}
                Err(err) => tracing::warn!(%id, error = %err, "could not record start failure"),
            }
        }
        self.starting.lock().await.remove(&id);
    }

    /// Running containers plus jobs accepted and not started yet.
    async fn used_slots(&self) -> Result<usize, LaunchError> {
        let running = self.runtime.running_count().await.map_err(unavailable)?;
        Ok(running + self.starting.lock().await.len())
    }

    /// Wait until no job is between accepted and started. Shutdown calls this
    /// so a restart does not drop a job that was already answered with 202.
    pub async fn settled(&self) {
        while !self.starting.lock().await.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    pub async fn job(&self, id: &str, with_logs: bool) -> Result<Option<JobRecord>, LaunchError> {
        if !valid_container_id(id) {
            return Ok(None);
        }
        let Some(mut record) = self.store.get(id).map_err(store_err)? else {
            return Ok(None);
        };
        if !record.is_finished() && !self.starting.lock().await.contains(id) {
            match self.runtime.inspect(id).await.map_err(unavailable)? {
                Inspected::Running => {}
                Inspected::Exited { exit_code } => {
                    self.complete(id, Some(exit_code)).await;
                    if let Some(updated) = self.store.get(id).map_err(store_err)? {
                        record = updated;
                    }
                }
                Inspected::NotFound => {
                    self.complete(id, None).await;
                    if let Some(updated) = self.store.get(id).map_err(store_err)? {
                        record = updated;
                    }
                }
            }
        }
        if !with_logs {
            record.logs = None;
        } else if self.starting.lock().await.contains(id) {
            record.logs = Some(String::new());
        } else if !record.is_finished() {
            record.logs = self
                .runtime
                .tail_logs(id, self.settings.log_lines)
                .await
                .ok();
        } else if record.logs.is_none() {
            record.logs = Some(String::new());
        }
        Ok(Some(record))
    }

    /// Called from the Docker die event and from the reaper.
    pub async fn on_exit(&self, id: &str, exit_code: i64) {
        self.complete(id, Some(exit_code)).await;
    }

    async fn complete(&self, id: &str, exit_code: Option<i64>) {
        let Some(current) = self.store.get(id).ok().flatten() else {
            return;
        };
        if current.is_finished() {
            let _ = self.runtime.remove(id).await;
            return;
        }
        let timed_out = self.timeouts.lock().await.contains(id);
        let reason = if timed_out {
            REASON_TIMEOUT
        } else {
            REASON_EXITED
        };
        let logs = self
            .runtime
            .tail_logs(id, self.settings.log_lines)
            .await
            .unwrap_or_default();
        let logs = clip_logs(&logs, self.settings.log_lines);
        let code = exit_code.or(if timed_out { Some(137) } else { None });
        let ended = self.clock.now_unix() as i64;
        match self.store.finish(id, ended, code, reason, &logs) {
            Ok(Some(job)) => {
                self.timeouts.lock().await.remove(id);
                let _ = self.runtime.remove(id).await;
                self.fire_callback(&job);
            }
            Ok(None) => {
                let _ = self.runtime.remove(id).await;
            }
            Err(err) => tracing::warn!(%id, error = %err, "could not finish job"),
        }
    }

    pub async fn reap_once(&self) -> Result<Vec<String>, LaunchError> {
        let now = self.clock.now_unix();
        let ids = self.runtime.list_expired(now).await.map_err(unavailable)?;
        let mut killed = Vec::new();
        for id in ids {
            self.timeouts.lock().await.insert(id.clone());
            if let Err(err) = self.runtime.kill(&id).await {
                tracing::warn!(%id, error = %err, "failed to kill expired container");
                continue;
            }
            self.complete(&id, Some(137)).await;
            tracing::info!(%id, "timeout: container killed");
            killed.push(id);
        }
        Ok(killed)
    }

    pub fn purge_once(&self) -> Result<usize, LaunchError> {
        let days = self.settings.history_days;
        let now = self.clock.now_unix() as i64;
        // history_days = 0 purges every finished row on the next pass.
        let cutoff = now.saturating_sub((days as i64).saturating_mul(86_400));
        let removed = if days == 0 {
            self.store
                .purge_finished_before(now.saturating_add(1))
                .map_err(store_err)?
        } else {
            self.store
                .purge_finished_before(cutoff)
                .map_err(store_err)?
        };
        if removed > 0 {
            tracing::info!(removed, "purged old jobs");
        }
        Ok(removed)
    }

    pub async fn prune_once(&self) -> Result<(usize, usize), LaunchError> {
        let stopped = self.runtime.list_stopped().await.map_err(unavailable)?;
        let mut containers = 0usize;
        for id in stopped {
            if self
                .store
                .get(&id)
                .map_err(store_err)?
                .is_some_and(|job| !job.is_finished())
            {
                self.complete(&id, None).await;
            }
            match self.runtime.remove(&id).await {
                Ok(()) => containers += 1,
                Err(err) => tracing::warn!(%id, error = %err, "prune could not remove container"),
            }
        }
        let mut images = 0usize;
        for image in self.store.images().map_err(store_err)? {
            match self.runtime.image_in_use(&image).await {
                Ok(true) => {}
                Ok(false) => match self.runtime.remove_image(&image).await {
                    Ok(()) => images += 1,
                    Err(err) => {
                        tracing::warn!(%image, error = %err, "prune could not remove image")
                    }
                },
                Err(err) => tracing::warn!(%image, error = %err, "prune image check failed"),
            }
        }
        if containers + images > 0 {
            tracing::info!(containers, images, "pruned");
        }
        Ok((containers, images))
    }

    /// After a restart, close rows whose containers are already gone and send
    /// any callback that was missed while the process was down.
    pub async fn reconcile(&self) {
        let ids = match self.store.running_ids() {
            Ok(ids) => ids,
            Err(err) => {
                tracing::warn!(error = %err, "reconcile");
                return;
            }
        };
        for id in ids {
            match self.runtime.inspect(&id).await {
                Ok(Inspected::Running) => {}
                Ok(Inspected::Exited { exit_code }) => self.complete(&id, Some(exit_code)).await,
                Ok(Inspected::NotFound) => self.complete(&id, None).await,
                Err(err) => tracing::warn!(%id, error = %err, "reconcile inspect failed"),
            }
        }
    }

    fn fire_callback(&self, job: &JobRecord) {
        let Some(url) = job.callback_url.clone() else {
            return;
        };
        let body = callback_body(job);
        spawn_callback(url, body);
    }
}

pub fn spawn_maintenance(launcher: Arc<Launcher>) {
    let reaper = launcher.settings.reaper_interval;
    let purge = launcher.settings.purge_interval;
    let prune = launcher.settings.prune_interval;
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(1));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut since_reap = std::time::Duration::ZERO;
        let mut since_purge = std::time::Duration::ZERO;
        let mut since_prune = std::time::Duration::ZERO;
        loop {
            ticks.tick().await;
            let step = std::time::Duration::from_secs(1);
            since_reap += step;
            since_purge += step;
            since_prune += step;
            if since_reap >= reaper {
                since_reap = std::time::Duration::ZERO;
                if let Err(err) = launcher.reap_once().await {
                    tracing::warn!(error = %err, "reaper");
                }
            }
            if since_purge >= purge {
                since_purge = std::time::Duration::ZERO;
                if let Err(err) = launcher.purge_once() {
                    tracing::warn!(error = %err, "purge");
                }
            }
            if !prune.is_zero() && since_prune >= prune {
                since_prune = std::time::Duration::ZERO;
                if let Err(err) = launcher.prune_once().await {
                    tracing::warn!(error = %err, "prune");
                }
            }
        }
    });
}

pub fn callback_body(job: &JobRecord) -> Value {
    json!({
        "id": job.id,
        "job_key": job.job_key,
        "image": job.image,
        "status": job.status(),
        "exit_code": job.exit_code,
        "reason": job.reason,
        "started_at": job.started_at,
        "ended_at": job.ended_at,
        "duration_secs": job.duration_secs(),
    })
}

pub fn metrics_text(slots: Slots, counts: crate::store::JobCounts) -> String {
    format!(
        "\
# HELP shutup_and_run_slots_total Configured slot capacity.
# TYPE shutup_and_run_slots_total gauge
shutup_and_run_slots_total {total}
# HELP shutup_and_run_slots_used Running containers labeled shutup-and-run=1.
# TYPE shutup_and_run_slots_used gauge
shutup_and_run_slots_used {used}
# HELP shutup_and_run_slots_free Slots not currently taken. Not offered while draining.
# TYPE shutup_and_run_slots_free gauge
shutup_and_run_slots_free {free}
# HELP shutup_and_run_draining 1 while new jobs are refused.
# TYPE shutup_and_run_draining gauge
shutup_and_run_draining {draining}
# HELP shutup_and_run_jobs_started_total Jobs recorded in SQLite, including those still running.
# TYPE shutup_and_run_jobs_started_total counter
shutup_and_run_jobs_started_total {started}
# HELP shutup_and_run_jobs_finished_total Jobs that have ended.
# TYPE shutup_and_run_jobs_finished_total counter
shutup_and_run_jobs_finished_total {finished}
# HELP shutup_and_run_jobs_timeout_total Jobs ended with reason timeout_kill.
# TYPE shutup_and_run_jobs_timeout_total counter
shutup_and_run_jobs_timeout_total {timeouts}
",
        total = slots.total,
        used = slots.used,
        free = slots.free,
        draining = if slots.draining { 1 } else { 0 },
        started = counts.started,
        finished = counts.finished,
        timeouts = counts.timeouts,
    )
}

fn unavailable(err: RuntimeError) -> LaunchError {
    tracing::warn!(error = %err, "docker");
    LaunchError::Unavailable("docker is unavailable".into())
}

fn store_err(err: StoreError) -> LaunchError {
    tracing::warn!(error = %err, "sqlite");
    LaunchError::Unavailable("job history is unavailable".into())
}

fn resolve_timeout(requested: Option<u64>, settings: &Settings) -> Result<u64, LaunchError> {
    let timeout = requested.unwrap_or(settings.default_timeout_secs);
    if timeout == 0 {
        return Err(LaunchError::BadRequest(
            "timeout_secs must be greater than zero".into(),
        ));
    }
    if timeout > settings.max_timeout_secs {
        return Err(LaunchError::BadRequest(format!(
            "timeout_secs {timeout} is above the maximum {}",
            settings.max_timeout_secs
        )));
    }
    Ok(timeout)
}

fn validate_env(env: HashMap<String, String>) -> Result<Vec<String>, LaunchError> {
    if env.len() > 128 {
        return Err(LaunchError::BadRequest(
            "env accepts at most 128 variables".into(),
        ));
    }
    let mut out = Vec::with_capacity(env.len());
    for (key, value) in env {
        let ok = !key.is_empty()
            && key.len() <= 256
            && key
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !ok {
            return Err(LaunchError::BadRequest(format!(
                "invalid environment variable: {key}"
            )));
        }
        if value.len() > 16 * 1024 || value.contains('\0') {
            return Err(LaunchError::BadRequest(format!("invalid value for {key}")));
        }
        out.push(format!("{key}={value}"));
    }
    Ok(out)
}

fn validate_command(command: Option<Vec<String>>) -> Result<Option<Vec<String>>, LaunchError> {
    let Some(command) = command else {
        return Ok(None);
    };
    if command.is_empty() {
        return Ok(None);
    }
    if command.len() > 32 {
        return Err(LaunchError::BadRequest(
            "command accepts at most 32 arguments".into(),
        ));
    }
    for arg in &command {
        if arg.is_empty() || arg.len() > 4096 || arg.contains('\0') {
            return Err(LaunchError::BadRequest("invalid command argument".into()));
        }
    }
    Ok(Some(command))
}

fn validate_job_key(job_key: Option<String>) -> Result<Option<String>, LaunchError> {
    let Some(key) = job_key else {
        return Ok(None);
    };
    if key.is_empty()
        || key.len() > 200
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
    {
        return Err(LaunchError::BadRequest(
            "job_key must be 1-200 characters of letters, digits, '.', '_', ':' or '-'".into(),
        ));
    }
    Ok(Some(key))
}

fn validate_callback(callback_url: Option<String>) -> Result<Option<String>, LaunchError> {
    let Some(url) = callback_url else {
        return Ok(None);
    };
    validate_callback_url(&url).map_err(LaunchError::BadRequest)?;
    Ok(Some(url))
}

/// A job that names no limit gets the machine's; one that names more than the
/// machine's is refused, not quietly cut, so its sender learns the limit.
fn limit_cpu(requested: Option<f64>, max: Option<f64>) -> Result<Option<i64>, LaunchError> {
    if let (Some(cpu), Some(max)) = (requested, max) {
        if cpu > max {
            return Err(LaunchError::BadRequest(format!(
                "cpu {cpu} is above this machine's limit of {max} per job"
            )));
        }
    }
    requested.or(max).map(nano_cpus).transpose()
}

fn limit_memory(requested: Option<i64>, max: Option<u64>) -> Result<Option<i64>, LaunchError> {
    if let (Some(bytes), Some(max)) = (requested, max) {
        if bytes as u64 > max {
            return Err(LaunchError::BadRequest(format!(
                "memory {bytes} is above this machine's limit of {max} bytes per job"
            )));
        }
    }
    Ok(requested.or(max.map(|max| max.min(i64::MAX as u64) as i64)))
}

fn nano_cpus(cpu: f64) -> Result<i64, LaunchError> {
    if !cpu.is_finite() || cpu <= 0.0 || cpu > 128.0 {
        return Err(LaunchError::BadRequest(
            "cpu must be a number between 0 and 128".into(),
        ));
    }
    let nano = (cpu * 1_000_000_000.0).round();
    if nano < 1.0 || nano > i64::MAX as f64 {
        return Err(LaunchError::BadRequest("cpu is invalid".into()));
    }
    Ok(nano as i64)
}

pub fn valid_container_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_hexdigit())
}

/// 32 hex characters. Unique on this machine: the time, a counter, the process
/// id, and the standard library's per-process random hash seed, hashed together.
pub(crate) fn random_id() -> String {
    use sha2::{Digest, Sha256};
    use std::hash::{BuildHasher, Hasher};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seed = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    let digest = Sha256::digest(format!("{nanos}-{n}-{}-{seed}", std::process::id()));
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

fn clip_logs(text: &str, lines: usize) -> String {
    let mut kept: Vec<&str> = text.lines().collect();
    if kept.len() > lines {
        kept = kept.split_off(kept.len() - lines);
    }
    let mut joined = kept.join("\n");
    if text.ends_with('\n') && !joined.is_empty() {
        joined.push('\n');
    }
    if joined.len() > 256 * 1024 {
        let mut start = joined.len() - 256 * 1024;
        while start < joined.len() && !joined.is_char_boundary(start) {
            start += 1;
        }
        joined = joined[start..].to_string();
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::config::Settings;
    use crate::mock::MockRuntime;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn settings(slots: usize) -> Settings {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("shutup-and-run-launch-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Settings::from_toml_and_env(
            &format!(
                r#"
                token = "test-token-value-ok"
                max_slots = {slots}
                allowlist = ["alpine", "ghcr.io/owner/selenium-worker"]
                default_timeout_secs = 30
                max_timeout_secs = 120
                history_path = '{}'
                history_days = 30
                "#,
                dir.join("jobs.sqlite").display()
            ),
            &HashMap::new(),
        )
        .unwrap()
    }

    fn harness(slots: usize) -> (Arc<Launcher>, Arc<MockRuntime>, Arc<FakeClock>) {
        let mock = Arc::new(MockRuntime::new());
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let runtime: Arc<dyn Runtime> = mock.clone();
        let launcher = Launcher::with_clock(settings(slots), runtime, clock.clone()).unwrap();
        (launcher, mock, clock)
    }

    fn job(image: &str, timeout: Option<u64>) -> LaunchRequest {
        LaunchRequest {
            image: image.into(),
            env: HashMap::new(),
            timeout_secs: timeout,
            cpu: None,
            memory_bytes: None,
            shm_bytes: None,
            command: Some(vec!["sleep".into(), "30".into()]),
            job_key: None,
            callback_url: None,
        }
    }

    fn harness_with(
        slots: usize,
        change: impl FnOnce(&mut Settings),
    ) -> (Arc<Launcher>, Arc<MockRuntime>) {
        let mock = Arc::new(MockRuntime::new());
        let runtime: Arc<dyn Runtime> = mock.clone();
        let mut settings = settings(slots);
        change(&mut settings);
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        (
            Launcher::with_clock(settings, runtime, clock).unwrap(),
            mock,
        )
    }

    const MIB: u64 = 1024 * 1024;

    #[tokio::test]
    async fn every_job_gets_the_machines_limits_and_none_gets_more() {
        let limited = |s: &mut Settings| {
            s.max_job_cpus = Some(1.0);
            s.max_job_memory = Some(1024 * MIB);
            s.poll_url = Some("http://127.0.0.1:9/poll".into());
            s.poll_any_image = true;
        };
        let (launcher, mock) = harness_with(4, limited);
        // No limits asked: the machine's. A push and a polled task alike.
        launcher.launch(job("alpine", None)).await.unwrap();
        launcher
            .launch_from(job("some/other-image", None), JobSource::Poll)
            .await
            .unwrap();
        // Less than the machine's: what was asked.
        let mut small = job("alpine", None);
        small.cpu = Some(0.5);
        small.memory_bytes = Some((256 * MIB) as i64);
        launcher.launch(small).await.unwrap();
        // Exactly the machine's is allowed.
        let mut exact = job("alpine", None);
        exact.cpu = Some(1.0);
        exact.memory_bytes = Some((1024 * MIB) as i64);
        launcher.launch(exact).await.unwrap();
        launcher.settled().await;
        let mut given: Vec<(Option<i64>, Option<i64>)> = mock
            .specs()
            .await
            .iter()
            .map(|spec| (spec.nano_cpus, spec.memory_bytes))
            .collect();
        given.sort();
        let full = (Some(1_000_000_000), Some((1024 * MIB) as i64));
        assert_eq!(
            given,
            vec![
                (Some(500_000_000), Some((256 * MIB) as i64)),
                full,
                full,
                full
            ]
        );

        // More than the machine's is refused, with the limit, from either source.
        let (launcher, mock) = harness_with(4, limited);
        for source in [JobSource::Push, JobSource::Poll] {
            let mut greedy = job("alpine", None);
            greedy.cpu = Some(2.0);
            match launcher.launch_from(greedy, source).await {
                Err(LaunchError::BadRequest(msg)) => {
                    assert_eq!(msg, "cpu 2 is above this machine's limit of 1 per job")
                }
                other => panic!("{other:?}"),
            }
            let mut greedy = job("alpine", None);
            greedy.memory_bytes = Some((2048 * MIB) as i64);
            match launcher.launch_from(greedy, source).await {
                Err(LaunchError::BadRequest(msg)) => assert!(
                    msg.starts_with("memory 2147483648 is above this machine's limit of"),
                    "{msg}"
                ),
                other => panic!("{other:?}"),
            }
            let mut invalid = job("alpine", None);
            invalid.cpu = Some(0.0);
            assert!(matches!(
                launcher.launch_from(invalid, source).await,
                Err(LaunchError::BadRequest(_))
            ));
        }
        launcher.settled().await;
        assert_eq!(mock.start_count().await, 0);
        assert_eq!(launcher.counts().unwrap().started, 0);

        // Without the profile nothing is added and nothing is refused.
        let (launcher, mock) = harness_with(1, |_| {});
        let mut big = job("alpine", None);
        big.cpu = Some(8.0);
        launcher.launch(big).await.unwrap();
        launcher.settled().await;
        let spec = &mock.specs().await[0];
        assert_eq!(
            (spec.nano_cpus, spec.memory_bytes),
            (Some(8_000_000_000), None)
        );
    }

    #[tokio::test]
    async fn a_machine_short_of_memory_or_disk_takes_no_job() {
        // More than any machine has free, so the real reading is always short.
        const NEVER: u64 = u64::MAX / 2;
        for (resource, change) in [
            (
                "memory",
                (|s| s.min_free_memory = NEVER) as fn(&mut Settings),
            ),
            ("disk", |s| s.min_free_disk = NEVER),
        ] {
            let (launcher, mock) = harness_with(2, change);
            let mut keyed = job("alpine", None);
            keyed.job_key = Some("k1".into());
            match launcher.launch(keyed).await {
                Err(LaunchError::LowResources(short)) => {
                    assert_eq!(short.resource, resource);
                    assert_eq!(short.needed_bytes, NEVER);
                }
                other => panic!("{resource}: {other:?}"),
            }
            launcher.settled().await;
            // Nothing was stored, the job_key included, and no slot is held.
            assert_eq!(mock.start_count().await, 0);
            assert_eq!(launcher.counts().unwrap().started, 0);
            assert_eq!(launcher.slots().await.unwrap().free, 2);
            let live = launcher.live_status().await;
            assert!(live
                .waiting_for
                .unwrap()
                .starts_with(&format!("low on {resource}")));
        }
        // A bar the machine clears lets the job in.
        let (launcher, mock) = harness_with(1, |s| {
            s.min_free_memory = 1;
            s.min_free_disk = 1;
        });
        launcher.launch(job("alpine", None)).await.unwrap();
        launcher.settled().await;
        assert_eq!(mock.start_count().await, 1);
        assert!(launcher.live_status().await.waiting_for.is_none());
    }

    #[tokio::test]
    async fn only_a_poll_task_skips_the_allowlist_and_only_when_configured() {
        let mock = Arc::new(MockRuntime::new());
        let runtime: Arc<dyn Runtime> = mock.clone();
        let mut settings = settings(2);
        settings.poll_url = Some("http://127.0.0.1:9/poll".into());

        // Default: the allowlist applies to both sources.
        let launcher = Launcher::new(settings.clone(), runtime.clone()).unwrap();
        for source in [JobSource::Push, JobSource::Poll] {
            let err = launcher
                .launch_from(job("redis:7", None), source)
                .await
                .unwrap_err();
            assert!(matches!(err, LaunchError::Forbidden), "{source:?}: {err}");
        }

        // poll_any_image: the poll task runs, the pushed job is still refused,
        // and the image name is still checked.
        settings.poll_any_image = true;
        let launcher = Launcher::new(settings, runtime).unwrap();
        let err = launcher
            .launch_from(job("redis:7", None), JobSource::Push)
            .await
            .unwrap_err();
        assert!(matches!(err, LaunchError::Forbidden));
        let err = launcher
            .launch_from(job("Bad Image", None), JobSource::Poll)
            .await
            .unwrap_err();
        assert!(matches!(err, LaunchError::BadRequest(_)));
        assert!(matches!(
            launcher
                .launch_from(job("redis:7", None), JobSource::Poll)
                .await
                .unwrap(),
            LaunchOutcome::Created(_)
        ));
        launcher.settled().await;
        assert_eq!(mock.start_count().await, 1);
    }

    #[tokio::test]
    async fn preseeded_containers_count_as_slots() {
        let (launcher, mock, _) = harness(2);
        mock.seed_running("aa11", 1_700_000_100).await;
        mock.seed_running("bb22", 1_700_000_100).await;
        let err = launcher.launch(job("alpine", None)).await.unwrap_err();
        assert!(matches!(err, LaunchError::NoSlots { used: 2, total: 2 }));
        assert_eq!(mock.start_count().await, 0);
    }

    #[tokio::test]
    async fn timeout_kills_and_history_survives_a_new_process() {
        let (launcher, mock, clock) = harness(1);
        let LaunchOutcome::Created(created) =
            launcher.launch(job("alpine", Some(10))).await.unwrap()
        else {
            panic!("expected a new job");
        };
        launcher.settled().await;
        let id = created.id;
        clock.set(1_700_000_009);
        assert!(launcher.reap_once().await.unwrap().is_empty());
        let running = launcher.job(&id, false).await.unwrap().unwrap();
        assert_eq!(running.status(), "running");

        clock.set(1_700_000_010);
        assert_eq!(launcher.reap_once().await.unwrap(), vec![id.clone()]);
        assert_eq!(mock.killed().await, vec![id.clone()]);
        let finished = launcher.job(&id, true).await.unwrap().unwrap();
        assert_eq!(finished.reason.as_deref(), Some(REASON_TIMEOUT));
        assert_eq!(finished.exit_code, Some(137));
        assert!(finished.logs.unwrap().contains("log "));

        let path = launcher.settings().history_path.clone();
        let clock2 = Arc::new(FakeClock::new(1_700_000_011));
        let empty = Arc::new(MockRuntime::new());
        let runtime: Arc<dyn Runtime> = empty.clone();
        let mut settings = settings(1);
        settings.history_path = path;
        settings.token = launcher.settings().token.clone();
        let restarted = Launcher::with_clock(settings, runtime, clock2).unwrap();
        let again = restarted.job(&id, true).await.unwrap().unwrap();
        assert_eq!(again.reason.as_deref(), Some(REASON_TIMEOUT));
        assert_eq!(again.exit_code, Some(137));
    }

    #[tokio::test]
    async fn a_running_job_is_visible_from_a_new_process() {
        let (launcher, mock, clock) = harness(1);
        let LaunchOutcome::Created(created) = launcher.launch(job("alpine", None)).await.unwrap()
        else {
            panic!("created");
        };
        launcher.settled().await;
        let path = launcher.settings().history_path.clone();
        let runtime: Arc<dyn Runtime> = mock.clone();
        let mut restarted_settings = settings(1);
        restarted_settings.history_path = path;
        let restarted = Launcher::with_clock(restarted_settings, runtime, clock).unwrap();
        let view = restarted.job(&created.id, false).await.unwrap().unwrap();
        assert_eq!(view.status(), "running");
        assert_eq!(view.id, created.id);
    }

    #[tokio::test]
    async fn start_failure_is_recorded_and_the_key_is_not_retried() {
        let (launcher, mock, _) = harness(1);
        mock.fail_start(true).await;
        let mut req = job("alpine", None);
        req.job_key = Some("once".into());
        // The answer is still 202. The failure shows on the job afterwards.
        let LaunchOutcome::Created(created) = launcher.launch(req.clone()).await.unwrap() else {
            panic!("expected a new job");
        };
        let id = created.id;
        launcher.settled().await;
        assert_eq!(launcher.slots().await.unwrap().used, 0);
        let LaunchOutcome::Existing(existing) = launcher.launch(req).await.unwrap() else {
            panic!("retry should see the failed row");
        };
        assert_eq!(existing.logs.as_deref(), Some("failed to start container"));
        assert_eq!(existing.id, id);
        assert_eq!(existing.reason.as_deref(), Some(REASON_START_FAILED));
        assert_eq!(mock.start_count().await, 0);
    }

    #[tokio::test]
    async fn a_job_holds_its_slot_while_the_image_is_pulled() {
        let (launcher, mock, _) = harness(1);
        let LaunchOutcome::Created(created) = launcher.launch(job("alpine", None)).await.unwrap()
        else {
            panic!("created");
        };
        // Nothing has started yet, and the slot is already taken.
        assert_eq!(mock.start_count().await, 0);
        assert_eq!(launcher.slots().await.unwrap().used, 1);
        let err = launcher.launch(job("alpine", None)).await.unwrap_err();
        assert!(matches!(err, LaunchError::NoSlots { used: 1, total: 1 }));
        let view = launcher.job(&created.id, true).await.unwrap().unwrap();
        assert_eq!(view.status(), "running");
        launcher.settled().await;
        assert_eq!(mock.start_count().await, 1);
        assert_eq!(launcher.slots().await.unwrap().used, 1);
        assert_eq!(mock.running_ids().await, vec![created.id]);
    }

    #[tokio::test]
    async fn same_job_key_does_not_start_twice() {
        let (launcher, mock, _) = harness(2);
        let mut first = job("alpine", None);
        first.job_key = Some("run-1".into());
        let second = first.clone();
        let LaunchOutcome::Created(created) = launcher.launch(first).await.unwrap() else {
            panic!("created");
        };
        launcher.settled().await;
        let LaunchOutcome::Existing(existing) = launcher.launch(second).await.unwrap() else {
            panic!("existing");
        };
        assert_eq!(created.id, existing.id);
        assert_eq!(mock.start_count().await, 1);
    }

    #[tokio::test]
    async fn drain_rejects_before_a_container_starts() {
        let (launcher, mock, _) = harness(1);
        launcher.set_draining(true).unwrap();
        let err = launcher.launch(job("alpine", None)).await.unwrap_err();
        assert!(matches!(err, LaunchError::Draining));
        assert_eq!(mock.start_count().await, 0);
        assert!(launcher.slots().await.unwrap().draining);
        launcher.set_draining(false).unwrap();
        assert!(launcher.launch(job("alpine", None)).await.is_ok());
        launcher.settled().await;
    }

    #[tokio::test]
    async fn purge_uses_the_configured_window() {
        let (launcher, _, clock) = harness(1);
        let LaunchOutcome::Created(created) =
            launcher.launch(job("alpine", Some(10))).await.unwrap()
        else {
            panic!("created");
        };
        launcher.settled().await;
        clock.set(1_700_000_000 + 10);
        launcher.reap_once().await.unwrap();
        clock.set(1_700_000_000 + 10 + 31 * 86_400);
        assert_eq!(launcher.purge_once().unwrap(), 1);
        assert!(launcher.job(&created.id, false).await.unwrap().is_none());
    }
}
