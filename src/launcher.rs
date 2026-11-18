use crate::allowlist::{image_allowed, validate_image_name};
use crate::callback::validate_callback_url;
use crate::clock::{Clock, SystemClock};
use crate::config::Settings;
use crate::runtime::{Inspected, Runtime, RuntimeError, StartSpec};
use crate::store::{JobRecord, NewJob, Store, StoreError};
use crate::{
    REASON_EXITED, REASON_INTERRUPTED, REASON_LAUNCHER, REASON_START_FAILED, REASON_TIMEOUT,
};
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
    /// A run of a managed container definition, started by the worker
    /// itself. Checked like a push: the allowlist always applies.
    Managed,
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
    /// `min_idle_secs` is set and the person at the PC is using it.
    #[error("{0}")]
    UserActive(UserActive),
    /// Outside `lend_hours`, a PC on a low battery, or a person presenting.
    /// It clears by itself; nothing was stored.
    #[error("{0}")]
    NotLending(crate::lending::Hold),
    /// A critical condition of the computer. No consent lifts it; it clears
    /// by itself. Nothing was stored.
    #[error("blocked to protect this computer: {}", codes_of(.0))]
    SafetyBlocked(Vec<crate::safety::Finding>),
    /// A risk for the owner of the computer that the owner has not agreed
    /// to. Only a person at the machine can; nothing was stored.
    #[error("the owner of this computer has to authorize: {}", codes_of(.0))]
    AuthorizationRequired(Vec<crate::safety::Finding>),
    /// The machine runs managed containers and takes no job from outside.
    #[error("managed mode: this machine runs its own managed containers and takes no job")]
    ManagedMode,
    /// A managed container was asked to start while the machine takes jobs.
    #[error("managed mode is off")]
    JobsMode,
    /// This managed container already has a run that has not ended.
    #[error("already running")]
    AlreadyRunning,
    #[error("{0}")]
    Unavailable(String),
}

fn codes_of(findings: &[crate::safety::Finding]) -> String {
    findings
        .iter()
        .map(|f| f.code.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// How often a running job's counters are read (CPU throttling, CPU time,
/// memory): once per job every this long, one small Docker call, kept in
/// memory until the job ends.
pub const THROTTLE_SAMPLE_SECS: u64 = 30;

/// A running job's readings so far: the last throttling counters, the CPU
/// time at the last reading, and the most memory any reading saw.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sampled {
    pub throttling: Option<crate::runtime::Throttling>,
    pub cpu_ns: Option<u64>,
    pub memory_peak: Option<u64>,
}

impl Sampled {
    fn add(&mut self, read: crate::runtime::Stats) {
        self.throttling = read.throttling.or(self.throttling);
        self.cpu_ns = self.cpu_ns.max(read.cpu_ns);
        self.memory_peak = self.memory_peak.max(read.memory_bytes);
    }
}

/// Why a job waits under `min_idle_secs`. It can start later, unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserActive {
    pub idle_secs: u64,
    pub needed_secs: u64,
}

impl std::fmt::Display for UserActive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the PC is in use: idle for {}s, {}s needed",
            self.idle_secs, self.needed_secs
        )
    }
}

/// A callback not answered is tried again once a minute for this long after
/// its job ended, then given up as `failed`.
pub const CALLBACK_RETRY_SECS: i64 = 3600;
/// The first tries are still running this soon after a job ended.
const CALLBACK_SETTLE_SECS: i64 = 30;

/// The most finished jobs the history keeps, whatever `history_days` is.
pub const MAX_HISTORY_ROWS: usize = 500;

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
    /// Ids a person asked to stop, so the stored reason is launcher_kill.
    stops: Mutex<HashSet<String>>,
    /// One pass over the managed containers at a time.
    pub(crate) managed_pass: Mutex<()>,
    /// Told when a person changed something about a managed container.
    pub(crate) managed_changed: tokio::sync::Notify,
    /// Jobs answered with 202 whose image is still being pulled. They hold a
    /// slot. This set is memory only: after a restart such a row has no
    /// container and reconcile closes it.
    starting: Mutex<HashSet<String>>,
    /// Why the last poll failed, for status.json and the doctor. None after
    /// a poll that was answered.
    poll_error: std::sync::Mutex<Option<String>>,
    /// When the last request to the poll URL was sent and how it ended:
    /// `task`, `nothing`, `task_rejected`, `not_approved`, `error`.
    last_poll: std::sync::Mutex<Option<(i64, &'static str)>>,
    /// Told when a job that ran has ended, so the poller can ask for the
    /// next one without waiting for its interval.
    job_ended: tokio::sync::Notify,
    /// Where the owner's authorization and the incidents are kept: this
    /// machine's history. A demo runs on a throwaway history and still
    /// answers to the machine's own.
    safety_store: Arc<Store>,
    /// Critical conditions held until they recover (`safety::Latch`).
    latch: std::sync::Mutex<crate::safety::Latch>,
    /// The admission and the blocked codes last seen by the status loop,
    /// so a change is logged and recorded once.
    safety_seen: std::sync::Mutex<Option<(crate::safety::Admission, Vec<String>)>>,
    /// Readings given instead of the machine's own. Tests only.
    readings: std::sync::Mutex<Option<crate::safety::Readings>>,
    /// What the readings of each running job have said so far.
    samples: std::sync::Mutex<HashMap<String, Sampled>>,
    /// The hour of the week last written to `memory_hours`: slot, when, and
    /// the lowest written, so a reading only writes when it says more.
    memory_hour: std::sync::Mutex<Option<(u32, i64, u64)>>,
    /// Since when new jobs wait for memory, and up to when that wait is
    /// already counted in `memory_waits`.
    memory_wait: std::sync::Mutex<Option<(i64, i64)>>,
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
        Self::build(settings, runtime, clock, None)
    }

    /// A launcher on a history of its own (a demo) that reads the owner's
    /// authorization and writes incidents in `machine`, the real history.
    pub fn for_demo(
        settings: Settings,
        runtime: Arc<dyn Runtime>,
        machine: Option<Arc<Store>>,
    ) -> Result<Arc<Self>, StoreError> {
        Self::build(settings, runtime, Arc::new(SystemClock), machine)
    }

    fn build(
        settings: Settings,
        runtime: Arc<dyn Runtime>,
        clock: Arc<dyn Clock>,
        machine: Option<Arc<Store>>,
    ) -> Result<Arc<Self>, StoreError> {
        let store = Arc::new(Store::open(&settings.history_path)?);
        Ok(Arc::new(Self {
            safety_store: machine.unwrap_or_else(|| store.clone()),
            latch: std::sync::Mutex::new(crate::safety::Latch::new()),
            safety_seen: std::sync::Mutex::new(None),
            // A test must not depend on how busy the machine running it is.
            readings: std::sync::Mutex::new(cfg!(test).then(crate::safety::Readings::healthy)),
            samples: std::sync::Mutex::new(HashMap::new()),
            memory_hour: std::sync::Mutex::new(None),
            memory_wait: std::sync::Mutex::new(None),
            settings,
            runtime,
            clock,
            store,
            slots: Mutex::new(()),
            timeouts: Mutex::new(HashSet::new()),
            stops: Mutex::new(HashSet::new()),
            managed_pass: Mutex::new(()),
            managed_changed: tokio::sync::Notify::new(),
            starting: Mutex::new(HashSet::new()),
            poll_error: std::sync::Mutex::new(None),
            last_poll: std::sync::Mutex::new(None),
            job_ended: tokio::sync::Notify::new(),
        }))
    }

    pub fn runtime(&self) -> &Arc<dyn Runtime> {
        &self.runtime
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub(crate) fn store(&self) -> &Arc<Store> {
        &self.store
    }

    pub(crate) fn now(&self) -> i64 {
        self.clock.now_unix() as i64
    }

    /// True while the machine runs managed containers and takes no job.
    /// Read from the history every time: the CLI changes it there.
    pub fn managed_mode(&self) -> Result<bool, LaunchError> {
        self.store.managed_mode().map_err(store_err)
    }

    /// Changes the mode, only while nothing runs or is starting. Under the
    /// slot lock, so no admission is half way through. `Ok(false)`: busy.
    pub async fn set_managed_mode(&self, managed: bool) -> Result<bool, LaunchError> {
        let _guard = self.slots.lock().await;
        let changed = self.store.set_managed_mode(managed).map_err(store_err)?;
        if changed {
            tracing::info!(managed, "mode");
            self.managed_changed.notify_one();
        }
        Ok(changed)
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
        self.store
            .draining_at(self.clock.now_unix() as i64)
            .map_err(store_err)
    }

    pub fn set_draining(&self, draining: bool) -> Result<bool, LaunchError> {
        self.store.set_draining(draining).map_err(store_err)?;
        tracing::info!(draining, "drain mode");
        Ok(draining)
    }

    /// A pause that ends by itself after `secs`. Returns when.
    pub fn pause_for(&self, secs: u64) -> Result<i64, LaunchError> {
        let until = self.clock.now_unix() as i64 + secs as i64;
        self.store.pause_until(until).map_err(store_err)?;
        tracing::info!(until, "paused for a while");
        Ok(until)
    }

    /// When the pause ends by itself. None for no pause, or one until
    /// someone resumes.
    pub fn paused_until(&self) -> Option<i64> {
        self.store
            .paused_until(self.clock.now_unix() as i64)
            .ok()
            .flatten()
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

    /// What the machine says about itself now. A handful of small reads.
    fn read_machine(&self) -> crate::safety::Readings {
        if let Some(given) = *self.readings.lock().expect("readings") {
            return given;
        }
        crate::safety::Readings::read(
            self.settings.worker_os == "windows",
            &crate::resources::disk_path(&self.settings.history_path),
        )
    }

    /// Readings to use instead of the machine's own.
    #[cfg(test)]
    pub fn set_readings(&self, readings: crate::safety::Readings) {
        *self.readings.lock().expect("readings") = Some(readings);
    }

    /// Set when the machine has too little memory or disk for one more job.
    /// Read fresh every time: it is two small reads, and it clears by itself.
    pub fn shortage(&self) -> Option<crate::resources::Shortage> {
        self.shortage_for(None, &self.read_machine())
    }

    /// On a shared PC a job that asks for more memory than the usual needs
    /// that much free, not the usual: its limit is what it may come to use.
    fn shortage_for(
        &self,
        job_memory: Option<i64>,
        machine: &crate::safety::Readings,
    ) -> Option<crate::resources::Shortage> {
        let disk = self.settings.min_free_disk;
        let mut memory = self.settings.min_free_memory;
        if self.settings.shared_pc && memory > 0 {
            memory = memory.max(job_memory.unwrap_or(0).max(0) as u64);
        }
        if memory == 0 && disk == 0 {
            return None;
        }
        let free = crate::resources::Free {
            memory: machine.inner.memory_available,
            disk: machine.inner.disk_free,
        };
        crate::resources::shortage(free, memory, disk)
    }

    /// Is it safe for the owner of this computer to start one more job, and
    /// did the owner agree to the risks there are? Only under the shared-PC
    /// profile; elsewhere the answer is always "open".
    pub fn safety(&self) -> crate::safety::Report {
        self.safety_with(&self.read_machine())
    }

    fn safety_with(&self, machine: &crate::safety::Readings) -> crate::safety::Report {
        if !self.settings.shared_pc {
            return crate::safety::Report::default();
        }
        let now = self.clock.now_unix() as i64;
        // A history that cannot be read is no reason to call the machine safe:
        // without the consent a warning still asks, and a block needs neither.
        let ooms = self
            .safety_store
            .oom_times(now - crate::safety::OOM_BREAKER_WINDOW_SECS)
            .unwrap_or_default();
        let consent = self.safety_store.safety_consent().ok().flatten();
        let assessment = crate::safety::assess(
            machine,
            &ooms,
            now,
            &mut self.latch.lock().expect("safety latch"),
        );
        crate::safety::Report::new(&assessment, consent)
    }

    /// Logs and records a change of the admission, once. Called from the
    /// status loop, so a number that moves every second writes nothing.
    fn note_safety(&self, report: &crate::safety::Report) {
        use crate::safety::Admission;
        let blocked: Vec<String> = report.blocked().iter().map(|f| f.code.clone()).collect();
        let now = (report.admission, blocked);
        let mut seen = self.safety_seen.lock().expect("safety seen");
        if seen.as_ref() == Some(&now) {
            return;
        }
        let was = seen.replace(now).map(|(admission, _)| admission);
        let why = report.waiting_for().unwrap_or_default();
        let at = self.clock.now_unix() as i64;
        if was == Some(Admission::Blocked) && report.admission != Admission::Blocked {
            tracing::info!("safety: the critical condition is over");
            let _ = self.safety_store.add_incident(at, "recovered", "");
        }
        match report.admission {
            Admission::Blocked => {
                tracing::warn!(%why, "safety: new jobs are blocked");
                let _ = self.safety_store.add_incident(at, "blocked", &why);
            }
            Admission::AuthorizationRequired => {
                tracing::warn!(%why, "safety: new jobs wait for the owner's authorization");
            }
            Admission::Open => {}
        }
    }

    /// The owner said yes, at this machine, to the warnings there are now.
    /// `channel` is where: the local page is the only caller in the service.
    pub fn authorize(
        &self,
        channel: &str,
    ) -> Result<crate::safety::Consent, crate::safety::GrantError> {
        let report = self.safety();
        crate::safety::grant(
            &self.safety_store,
            &report.needs_authorization,
            channel,
            self.clock.now_unix() as i64,
        )
    }

    pub fn revoke_authorization(&self, channel: &str) -> Result<bool, crate::safety::GrantError> {
        crate::safety::revoke(&self.safety_store, channel, self.clock.now_unix() as i64)
    }

    /// One reading of each running job's counters, kept until the job ends.
    /// A job that ends before its first reading has none.
    pub async fn sample_throttling(&self) {
        let ids = self.store.running_ids().unwrap_or_default();
        for id in ids {
            if self.starting.lock().await.contains(&id) {
                continue;
            }
            if let Ok(Some(read)) = self.runtime.stats(&id).await {
                self.samples
                    .lock()
                    .expect("samples")
                    .entry(id)
                    .or_default()
                    .add(read);
            }
        }
    }

    /// Set while `min_idle_secs` is on and the Windows supervisor says the
    /// person touched the keyboard or mouse more recently than that. A
    /// reading that is missing or old does not block, like `shortage`.
    pub fn user_active(&self) -> Option<UserActive> {
        user_active(self.settings.min_idle_secs, crate::host::current())
    }

    /// Outside `lend_hours`, on a battery below the minimum, or while the
    /// person presents (`src/lending.rs`).
    pub fn lending_hold(&self) -> Option<crate::lending::Hold> {
        crate::lending::hold(
            self.settings.lend_hours.as_ref(),
            crate::host::current(),
            self.clock.now_unix() as i64,
        )
    }

    /// Why no new job starts now though a slot may be free.
    pub fn waiting_for(&self) -> Option<String> {
        let machine = self.read_machine();
        self.waiting_for_with(&self.safety_with(&machine), &machine)
    }

    fn waiting_for_with(
        &self,
        safety: &crate::safety::Report,
        machine: &crate::safety::Readings,
    ) -> Option<String> {
        safety
            .waiting_for()
            .or_else(|| self.shortage_for(None, machine).map(|s| s.to_string()))
            .or_else(|| self.user_active().map(|u| u.to_string()))
            .or_else(|| self.lending_hold().map(|h| h.to_string()))
    }

    pub fn delivery(&self) -> crate::store::Delivery {
        self.store.delivery().unwrap_or_default()
    }

    /// Resolves when a job that ran has ended since the last call.
    pub async fn job_ended(&self) {
        self.job_ended.notified().await
    }

    /// Records a request to the poll URL: now, and how it ended.
    pub fn note_poll(&self, status: &'static str) {
        *self.last_poll.lock().expect("last poll") = Some((self.clock.now_unix() as i64, status));
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
        let machine = self.read_machine();
        let mut safety = self.safety_with(&machine);
        self.note_safety(&safety);
        safety.incidents = self.safety_store.incidents(5).unwrap_or_default();
        let last_job_error = match (&docker_error, &last) {
            (None, Some(job)) => crate::status::job_error(job),
            _ => None,
        };
        let (managed_mode, managed) =
            crate::managed::views(&self.store, self.now()).unwrap_or_default();
        let memory = self.memory_view(
            &safety,
            &machine,
            slots.is_some_and(|s| s.free > 0 && !s.draining),
        );
        crate::status::Live {
            slots_used: slots.map(|s| s.used as u64),
            slots_total: slots.map(|s| s.total as u64),
            draining: slots.is_some_and(|s| s.draining),
            paused_until: self.paused_until(),
            job_ids,
            last_job_at: last
                .as_ref()
                .map(|job| job.ended_at.unwrap_or(job.started_at)),
            last_job_error,
            docker_error,
            last,
            poll_error: self.poll_error.lock().expect("poll error").clone(),
            last_poll: *self.last_poll.lock().expect("last poll"),
            waiting_for: self.waiting_for_with(&safety, &machine),
            computer: crate::status::Computer::read(self, &machine),
            safety,
            limits: crate::status::Limits::of(&self.settings),
            config_warning: self.settings.config_warning.clone(),
            managed_mode,
            managed,
            memory,
        }
    }

    /// What memory keeps from starting now, and what the history says about
    /// it. Records, as it goes, the owner's lowest memory in this hour of
    /// the week and the time new jobs waited for memory (`open`: a slot is
    /// free and the worker is not paused).
    fn memory_view(
        &self,
        safety: &crate::safety::Report,
        machine: &crate::safety::Readings,
        open: bool,
    ) -> crate::memory::View {
        use crate::memory::{self, View};
        let now = self.clock.now_unix() as i64;
        let offset = crate::lending::offset_secs(machine.host, now);
        let shortage = self.shortage_for(None, machine);
        let need = memory::need(safety, machine, shortage.as_ref());

        let owner = if machine.windows {
            machine
                .host
                .and_then(|h| h.memory_total.zip(h.memory_available))
        } else {
            machine
                .inner
                .memory_total
                .zip(machine.inner.memory_available)
        };
        if let Some((total, available)) = owner {
            let slot = memory::slot_of(now, offset);
            let mut last = self.memory_hour.lock().expect("memory hour");
            let write = match *last {
                Some((at_slot, at, lowest)) => {
                    at_slot != slot || available < lowest || now - at >= 600
                }
                None => true,
            };
            if write
                && self
                    .store
                    .note_memory_hour(slot, now, available, total)
                    .is_ok()
            {
                let lowest = match *last {
                    Some((at_slot, _, lowest)) if at_slot == slot => lowest.min(available),
                    _ => available,
                };
                *last = Some((slot, now, lowest));
            }
        }

        let day = |at: i64| (at + offset).div_euclid(86_400);
        let mut wait = self.memory_wait.lock().expect("memory wait");
        match (*wait, need.is_some() && open) {
            (None, true) => *wait = Some((now, now)),
            (Some((since, counted)), true) if now - counted >= 60 => {
                let _ = self
                    .store
                    .add_memory_wait(day(counted), (now - counted) as u64);
                *wait = Some((since, now));
            }
            (Some((_, counted)), false) => {
                if now > counted {
                    let _ = self
                        .store
                        .add_memory_wait(day(counted), (now - counted) as u64);
                }
                *wait = None;
            }
            _ => {}
        }
        let (today, week) = self.store.memory_waited(day(now)).unwrap_or_default();
        let ongoing = wait.map_or(0, |(_, counted)| (now - counted).max(0) as u64);
        let short_hours = self
            .store
            .memory_hours(now - memory::WEEK_SECS)
            .ok()
            .and_then(|hours| memory::short_hours(&hours, self.settings.lend_hours.as_ref()));
        View {
            need,
            waiting_since: wait.map(|(since, _)| since),
            waited_today_secs: today + ongoing,
            waited_week_secs: week + ongoing,
            short_hours,
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
        if source == JobSource::Managed {
            return Err(LaunchError::BadRequest(
                "a managed run names its definition".into(),
            ));
        }
        self.admit(req, source, None).await
    }

    /// One run of the managed container `name`. Every check a pushed job
    /// passes, this passes too, and it takes a slot like one.
    pub async fn launch_managed(
        self: &Arc<Self>,
        name: &str,
        req: LaunchRequest,
    ) -> Result<JobRecord, LaunchError> {
        match self.admit(req, JobSource::Managed, Some(name)).await? {
            LaunchOutcome::Created(job) | LaunchOutcome::Existing(job) => Ok(job),
        }
    }

    async fn admit(
        self: &Arc<Self>,
        req: LaunchRequest,
        source: JobSource,
        managed: Option<&str>,
    ) -> Result<LaunchOutcome, LaunchError> {
        let Checked {
            spec,
            timeout,
            job_key,
            callback_url,
        } = check_request(req, &self.settings, source)?;

        let guard = self.slots.lock().await;
        // The two ways of working never mix: in managed mode nothing from
        // outside is admitted, and outside it no managed container starts.
        match (self.managed_mode()?, managed.is_some()) {
            (true, false) => return Err(LaunchError::ManagedMode),
            (false, true) => return Err(LaunchError::JobsMode),
            _ => {}
        }
        if let Some(name) = managed {
            if self
                .store
                .managed_running(name)
                .map_err(store_err)?
                .is_some()
            {
                return Err(LaunchError::AlreadyRunning);
            }
        }
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
        // memory that only one of them will get. The computer's owner comes
        // first: a block, then the owner's say, then this job's own room.
        let machine = self.read_machine();
        let safety = self.safety_with(&machine);
        match safety.admission {
            crate::safety::Admission::Open => {}
            crate::safety::Admission::Blocked => {
                return Err(LaunchError::SafetyBlocked(
                    safety.blocked().into_iter().cloned().collect(),
                ))
            }
            crate::safety::Admission::AuthorizationRequired => {
                return Err(LaunchError::AuthorizationRequired(
                    safety.unauthorized().into_iter().cloned().collect(),
                ))
            }
        }
        if let Some(shortage) = self.shortage_for(spec.memory_bytes, &machine) {
            return Err(LaunchError::LowResources(shortage));
        }
        if let Some(active) = self.user_active() {
            return Err(LaunchError::UserActive(active));
        }
        if let Some(hold) = self.lending_hold() {
            return Err(LaunchError::NotLending(hold));
        }
        let id = random_id();
        let record = NewJob {
            id: id.clone(),
            image: spec.image.clone(),
            job_key: job_key.clone(),
            started_at: self.clock.now_unix() as i64,
            callback_url,
            managed: managed.map(str::to_string),
        };
        match self.store.insert_running(&record) {
            Ok(()) => {}
            // Another process changed the mode since the check above.
            Err(StoreError::Mode) if managed.is_some() => return Err(LaunchError::JobsMode),
            Err(StoreError::Mode) => return Err(LaunchError::ManagedMode),
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
        let pulled = tokio::time::timeout(limit, self.runtime.pull(&spec.image)).await;
        let stopped = self.stops.lock().await.remove(&id);
        let failure = match pulled {
            _ if stopped => Some("stopped before it started"),
            Err(_) => Some("image pull timed out"),
            Ok(Err(err)) => {
                tracing::warn!(%id, image = %spec.image, error = %err, "pull failed");
                Some("failed to pull image")
            }
            Ok(Ok(())) => {
                spec.deadline_unix = self.clock.now_unix().saturating_add(timeout);
                // The owner's certificate, when the machine shares one. Staged
                // per job and never logged: only the failure's kind is.
                let staged = if self.settings.certificate_enabled {
                    crate::certificate::stage(&self.settings, &id, &mut spec)
                } else {
                    Ok(())
                };
                match staged {
                    Err(err) => {
                        tracing::warn!(%id, error = %err, "certificate passthrough");
                        Some("certificate passthrough is on, but the certificate could not be given to the job")
                    }
                    Ok(()) => match self.runtime.start(&id, &spec).await {
                        Ok(()) => None,
                        Err(err) => {
                            tracing::warn!(%id, image = %spec.image, error = %err, "start failed");
                            Some("failed to start container")
                        }
                    },
                }
            }
        };
        if let Some(message) = failure {
            crate::certificate::unstage(&self.settings, &id);
            // Close the row before the slot is released, so a status request
            // cannot find the id neither starting nor recorded as failed.
            let ended = self.clock.now_unix() as i64;
            let reason = if stopped {
                REASON_LAUNCHER
            } else {
                REASON_START_FAILED
            };
            match self.store.finish(&id, ended, None, reason, message) {
                Ok(Some(job)) => self.fire_callback(&job),
                Ok(None) => {}
                Err(err) => tracing::warn!(%id, error = %err, "could not record start failure"),
            }
        }
        self.starting.lock().await.remove(&id);
        // A stop asked between the pull and the start.
        if failure.is_none() && self.stops.lock().await.contains(&id) {
            let _ = self.stop(&id).await;
        }
    }

    /// Stops a job on a person's word: SIGKILL, and the row says
    /// `launcher_kill`. A job still pulling its image ends before it starts.
    /// `Ok(false)` when there is no such unfinished job.
    pub async fn stop(&self, id: &str) -> Result<bool, LaunchError> {
        match self.store.get(id).map_err(store_err)? {
            Some(job) if !job.is_finished() => {}
            _ => return Ok(false),
        }
        self.stops.lock().await.insert(id.to_string());
        if self.starting.lock().await.contains(id) {
            return Ok(true);
        }
        let code = match self.runtime.kill(id).await {
            Ok(()) => Some(137),
            // It may have ended by itself a moment ago.
            Err(err) => match self.runtime.inspect(id).await.map_err(unavailable)? {
                Inspected::Exited { exit_code } => Some(exit_code),
                Inspected::NotFound => None,
                Inspected::Running => {
                    self.stops.lock().await.remove(id);
                    return Err(unavailable(err));
                }
            },
        };
        self.complete(id, code, true).await;
        tracing::info!(%id, "stopped on request");
        Ok(true)
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
                    self.complete(id, Some(exit_code), true).await;
                    if let Some(updated) = self.store.get(id).map_err(store_err)? {
                        record = updated;
                    }
                }
                Inspected::NotFound => {
                    self.complete(id, None, true).await;
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
        self.complete(id, Some(exit_code), true).await;
    }

    /// `witnessed` is false for a container found stopped when the worker
    /// started: it ended while nobody was watching.
    async fn complete(&self, id: &str, exit_code: Option<i64>, witnessed: bool) {
        let Some(current) = self.store.get(id).ok().flatten() else {
            return;
        };
        if current.is_finished() {
            let _ = self.runtime.remove(id).await;
            return;
        }
        let timed_out = self.timeouts.lock().await.contains(id);
        let reason = if !timed_out && self.stops.lock().await.contains(id) {
            REASON_LAUNCHER
        } else {
            end_reason(timed_out, exit_code, witnessed)
        };
        // Asked while the container still exists, and written on the row
        // before it is closed, so the callback is built with it.
        let oom = exit_code.is_some() && self.runtime.oom_killed(id).await.unwrap_or(false);
        if oom {
            let _ = self.store.mark_oom(id);
        }
        let logs = self
            .runtime
            .tail_logs(id, self.settings.log_lines)
            .await
            .unwrap_or_default();
        let logs = clip_logs(&logs, self.settings.log_lines);
        let code = exit_code.or(if timed_out { Some(137) } else { None });
        let ended = self.clock.now_unix() as i64;
        // The container has stopped, so its copies of the certificate go.
        crate::certificate::unstage(&self.settings, id);
        match self.store.finish(id, ended, code, reason, &logs) {
            Ok(Some(job)) => {
                // Only the call that closed the row: the die event and a
                // status request can both get here for the same job.
                let sampled = self.samples.lock().expect("samples").remove(id);
                if let Some(sampled) = sampled {
                    let cpu_ms = sampled.cpu_ns.map(|ns| ns / 1_000_000);
                    let _ = self.store.set_usage(id, cpu_ms, sampled.memory_peak);
                }
                self.note_resource_incidents(&job, sampled.and_then(|s| s.throttling));
                // Before the container goes, with its output.
                self.save_output(id).await;
                self.timeouts.lock().await.remove(id);
                self.stops.lock().await.remove(id);
                let _ = self.runtime.remove(id).await;
                self.fire_callback(&job);
                self.managed_changed.notify_one();
                // A job that never started does not wake the poller: a task
                // whose image cannot be pulled must not be asked for in a loop.
                self.job_ended.notify_one();
                self.give_back_memory();
            }
            Ok(None) => {
                let _ = self.runtime.remove(id).await;
            }
            Err(err) => tracing::warn!(%id, error = %err, "could not finish job"),
        }
    }

    /// On a Windows PC, once the last running job has ended, the WSL2 VM
    /// drops the cache that job filled, so `vmmemWSL` hands the memory back
    /// to Windows instead of keeping it until WSL restarts. Linux keeps its
    /// cache: that machine is the owner's, and the cache is its business.
    fn give_back_memory(&self) {
        if self.settings.worker_os != "windows" {
            return;
        }
        if !self
            .store
            .running_ids()
            .map(|ids| ids.is_empty())
            .unwrap_or(false)
        {
            return;
        }
        tokio::task::spawn_blocking(|| match crate::memory::give_back() {
            Ok(()) => tracing::info!("gave the memory of the last job back to Windows"),
            Err(err) => tracing::warn!(error = %err, "could not drop the WSL2 cache"),
        });
    }

    /// The job's log file. A failure is a warning: the job has ended either
    /// way, and the worker goes on.
    async fn save_output(&self, id: &str) {
        let output = match self.runtime.output(id, crate::joblogs::MAX_BYTES).await {
            Ok(output) => output,
            Err(err) => {
                tracing::warn!(%id, error = %err, "job log: could not read the output");
                return;
            }
        };
        if let Err(err) = crate::joblogs::save(&self.settings.history_path, id, &output) {
            tracing::warn!(%id, error = %err, "job log: could not write it");
        }
    }

    /// Recent jobs, newest first, for the local page.
    pub fn recent_jobs(&self, limit: usize) -> Result<Vec<JobRecord>, LaunchError> {
        self.store.recent(limit).map_err(store_err)
    }

    /// What Docker and the kernel said about how a job used its limits,
    /// recorded once as it ends: killed for memory, or held at its CPU limit
    /// most of the time. For the owner; the job's exit code is not changed.
    fn note_resource_incidents(
        &self,
        job: &JobRecord,
        throttled: Option<crate::runtime::Throttling>,
    ) {
        let at = self.clock.now_unix() as i64;
        if let Some(read) = throttled.filter(|read| read.significant()) {
            let detail = format!(
                "job {} ({}) was held at its CPU limit in {}% of its time",
                job.id,
                job.image,
                read.throttled_periods * 100 / read.periods.max(1)
            );
            tracing::warn!(%detail, "cpu throttling");
            let _ = self.safety_store.add_incident(at, "cpu_throttled", &detail);
        }
        if job.oom_killed {
            let detail = format!(
                "job {} ({}) reached its memory limit and the kernel killed it",
                job.id, job.image
            );
            tracing::warn!(%detail, "out of memory");
            let _ = self.safety_store.add_incident(at, "oom", &detail);
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
            self.complete(&id, Some(137), true).await;
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
        let removed = removed
            + self
                .store
                .purge_beyond(MAX_HISTORY_ROWS)
                .map_err(store_err)?;
        if removed > 0 {
            tracing::info!(removed, "purged old jobs");
        }
        let logs = crate::joblogs::purge(&self.settings.history_path, std::time::SystemTime::now());
        if logs > 0 {
            tracing::info!(logs, "deleted job logs older than 7 days");
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
                self.complete(&id, None, true).await;
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
                Ok(Inspected::Exited { exit_code }) => {
                    self.complete(&id, Some(exit_code), false).await
                }
                Ok(Inspected::NotFound) => self.complete(&id, None, false).await,
                Err(err) => tracing::warn!(%id, error = %err, "reconcile inspect failed"),
            }
        }
        // Certificate copies of jobs that ended while this process was down.
        let running: HashSet<String> = self
            .store
            .running_ids()
            .unwrap_or_default()
            .into_iter()
            .collect();
        crate::certificate::sweep(&self.settings, &running);
    }

    fn fire_callback(&self, job: &JobRecord) {
        let Some(url) = job.callback_url.clone() else {
            return;
        };
        let body = callback_body(job);
        let (store, id) = (self.store.clone(), job.id.clone());
        tokio::spawn(async move {
            if crate::callback::deliver(&url, &body, crate::callback::FIRST_TRIES).await {
                let _ = store.set_callback_state(&id, "delivered");
            }
        });
    }

    /// One more try for each callback that was not answered, and the end of
    /// the ones older than `CALLBACK_RETRY_SECS`. Returns how many were tried.
    pub async fn retry_callbacks(&self) -> Result<usize, LaunchError> {
        let now = self.clock.now_unix() as i64;
        let expired = self
            .store
            .expire_callbacks(now - CALLBACK_RETRY_SECS)
            .map_err(store_err)?;
        if expired > 0 {
            tracing::warn!(expired, "gave up on callbacks nobody answered for an hour");
        }
        let due = self
            .store
            .pending_callbacks(now - CALLBACK_RETRY_SECS, now - CALLBACK_SETTLE_SECS)
            .map_err(store_err)?;
        for job in &due {
            let Some(url) = job.callback_url.as_deref() else {
                continue;
            };
            if crate::callback::deliver(url, &callback_body(job), 1).await {
                let _ = self.store.set_callback_state(&job.id, "delivered");
                tracing::info!(id = %job.id, "callback delivered on a later try");
            }
        }
        Ok(due.len())
    }
}

pub fn spawn_maintenance(launcher: Arc<Launcher>) {
    let reaper = launcher.settings.reaper_interval;
    let purge = launcher.settings.purge_interval;
    let prune = launcher.settings.prune_interval;
    // A loop of its own: a receiver that hangs must not delay the reaper.
    let callbacks = launcher.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            if let Err(err) = callbacks.retry_callbacks().await {
                tracing::warn!(error = %err, "callbacks");
            }
        }
    });
    // Also apart: a Docker that is slow to answer about one job's CPU must
    // not hold the reaper either.
    let throttling = launcher.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(THROTTLE_SAMPLE_SECS)).await;
            throttling.sample_throttling().await;
        }
    });
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

/// How a job that held a container ended. A timeout the worker itself
/// enforced stays a timeout. Otherwise the job was interrupted when the
/// container is gone (no exit code), or when it stopped with a non-zero code
/// while the worker was not running: nobody saw why.
fn end_reason(timed_out: bool, exit_code: Option<i64>, witnessed: bool) -> &'static str {
    match (timed_out, exit_code) {
        (true, _) => REASON_TIMEOUT,
        (false, None) => REASON_INTERRUPTED,
        (false, Some(code)) if code != 0 && !witnessed => REASON_INTERRUPTED,
        (false, Some(_)) => REASON_EXITED,
    }
}

pub(crate) fn user_active(
    min_idle_secs: u64,
    feed: Option<crate::host::Feed>,
) -> Option<UserActive> {
    let idle_secs = feed?.idle_secs?;
    (min_idle_secs > 0 && idle_secs < min_idle_secs).then_some(UserActive {
        idle_secs,
        needed_secs: min_idle_secs,
    })
}

pub fn callback_body(job: &JobRecord) -> Value {
    let mut body = json!({
        "id": job.id,
        "job_key": job.job_key,
        "image": job.image,
        "status": job.status(),
        "exit_code": job.exit_code,
        "reason": job.reason,
        "started_at": job.started_at,
        "ended_at": job.ended_at,
        "duration_secs": job.duration_secs(),
    });
    // Present only when true, so every other body is what it always was.
    if job.oom_killed {
        body["oom_killed"] = Value::Bool(true);
    }
    body
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

/// A request that passed every check that needs no Docker and no slot.
#[derive(Debug, Clone)]
pub struct Checked {
    pub spec: StartSpec,
    pub timeout: u64,
    pub job_key: Option<String>,
    pub callback_url: Option<String>,
}

/// The static half of admission: the image name, the allowlist, the timeout,
/// env, command, job_key, callback URL and the machine's CPU and memory
/// limits. `launch_from` and `job validate` both call it, so they agree.
/// It reads nothing but the settings and changes nothing.
pub fn check_request(
    req: LaunchRequest,
    settings: &Settings,
    source: JobSource,
) -> Result<Checked, LaunchError> {
    validate_image_name(&req.image).map_err(LaunchError::BadRequest)?;
    let skip_allowlist = source == JobSource::Poll && settings.poll_any_image;
    if !skip_allowlist && !image_allowed(&req.image, &settings.allowlist) {
        return Err(LaunchError::Forbidden);
    }
    let timeout = resolve_timeout(req.timeout_secs, settings)?;
    let env = validate_env(req.env)?;
    let command = validate_command(req.command)?;
    let job_key = validate_job_key(req.job_key)?;
    let callback_url = validate_callback(req.callback_url)?;
    let nano_cpus = limit_cpu(req.cpu, settings.max_job_cpus)?;
    let memory_bytes = limit_memory(req.memory_bytes, settings.max_job_memory)?;
    Ok(Checked {
        spec: StartSpec {
            image: req.image,
            env,
            command,
            // Set when the container starts. Pull time does not count.
            deadline_unix: 0,
            nano_cpus,
            memory_bytes,
            shm_bytes: req.shm_bytes,
            binds: Vec::new(),
        },
        timeout,
        job_key,
        callback_url,
    })
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
                resource_profile = "none"
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

    /// Linux only: the readings come from /proc/meminfo and statvfs.
    #[cfg(target_os = "linux")]
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

    async fn started(launcher: &Arc<Launcher>, request: LaunchRequest) -> String {
        let id = match launcher.launch(request).await.unwrap() {
            LaunchOutcome::Created(job) | LaunchOutcome::Existing(job) => job.id,
        };
        launcher.settled().await;
        id
    }

    #[tokio::test]
    async fn every_job_that_ran_leaves_its_output_in_a_log_file_and_the_doctor_names_it() {
        let (launcher, mock, _clock) = harness(3);
        let history = launcher.settings().history_path.clone();

        let ok = started(&launcher, job("alpine", None)).await;
        mock.set_output(&ok, "hello from the job\n").await;
        mock.stop(&ok, 0).await;
        launcher.on_exit(&ok, 0).await;
        let saved = crate::joblogs::path(&history, &ok);
        assert_eq!(
            std::fs::read_to_string(&saved).unwrap(),
            "hello from the job\n"
        );
        assert!(crate::cli::last_job_check(&history).is_none());

        let bad = started(&launcher, job("alpine", None)).await;
        mock.set_output(&bad, "step 1\nTraceback: boom\n").await;
        mock.stop(&bad, 3).await;
        launcher.on_exit(&bad, 3).await;
        // By the start of its id, and --last, with --tail.
        let out = crate::jobs::logs_out(&history, Some(&bad[..8]), None);
        assert_eq!(out.code, 0, "{}", out.json);
        assert_eq!(out.json["status"], "failed");
        assert_eq!(out.json["exit_code"], 3);
        assert!(out.text.contains("Traceback: boom"));
        let last = crate::jobs::logs_out(&history, None, Some(1));
        assert_eq!(last.json["id"], bad.as_str());
        assert_eq!(last.text, "Traceback: boom");
        // `jobs --json` says where, and the doctor which command reads it.
        let listed = crate::jobs::jobs_out(&history, 5);
        assert_eq!(
            listed.json["jobs"][0]["log"]["command"],
            format!("shutup-and-run jobs logs {bad}")
        );
        assert!(listed
            .text
            .contains(&format!("log: shutup-and-run jobs logs {bad}")));
        let check = crate::cli::last_job_check(&history).unwrap();
        assert!(check.soft && !check.ok);
        assert_eq!(check.code(), "LAST_JOB_FAILED");
        assert_eq!(
            check.hint(),
            format!("Read what it printed: `shutup-and-run jobs logs {bad}`.")
        );

        // A person's stop and a timeout keep theirs too.
        let stopped = started(&launcher, job("alpine", None)).await;
        assert!(launcher.stop(&stopped).await.unwrap());
        assert!(crate::joblogs::info(&history, &stopped).is_some());

        // Not there: an unknown id, a job that never started a container.
        let unknown = crate::jobs::logs_out(&history, Some("nope"), None);
        assert_eq!(
            (unknown.code, unknown.json["code"].as_str()),
            (1, Some("JOB_NOT_FOUND"))
        );
        mock.fail_start(true).await;
        let never = started(&launcher, job("alpine", None)).await;
        mock.fail_start(false).await;
        let missing = crate::jobs::logs_out(&history, Some(&never), None);
        assert_eq!(missing.json["code"], "JOB_LOG_MISSING");
        assert!(missing.text.contains("never started"));
        assert_eq!(
            crate::cli::last_job_check(&history).unwrap().code(),
            "LAST_JOB_FAILED_NO_LOG"
        );

        // A log that cannot be written is a warning, not a failed worker.
        std::fs::remove_dir_all(crate::joblogs::dir(&history)).unwrap();
        std::fs::write(crate::joblogs::dir(&history), b"in the way").unwrap();
        let unlucky = started(&launcher, job("alpine", None)).await;
        mock.stop(&unlucky, 0).await;
        launcher.on_exit(&unlucky, 0).await;
        assert_eq!(
            ended(&launcher, &unlucky).await,
            (Some(crate::REASON_EXITED.into()), Some(0))
        );
        started(&launcher, job("alpine", None)).await;
    }

    async fn ended(launcher: &Launcher, id: &str) -> (Option<String>, Option<i64>) {
        let job = launcher.job(id, false).await.unwrap().unwrap();
        (job.reason, job.exit_code)
    }

    #[tokio::test]
    async fn a_job_that_ended_while_nobody_watched_is_interrupted() {
        let (launcher, mock, _) = harness(4);
        let mut ids = Vec::new();
        for _ in 0..4 {
            ids.push(started(&launcher, job("alpine", None)).await);
        }
        // The worker comes back after the PC was shut down under three jobs.
        mock.vanish(&ids[0]).await;
        mock.stop_unseen(&ids[1], 255).await;
        mock.stop_unseen(&ids[2], 0).await;
        launcher.reconcile().await;
        let interrupted = Some(REASON_INTERRUPTED.to_string());
        let exited = Some(REASON_EXITED.to_string());
        assert_eq!(ended(&launcher, &ids[0]).await, (interrupted.clone(), None));
        assert_eq!(ended(&launcher, &ids[1]).await, (interrupted, Some(255)));
        // It finished well while the worker was away: that is what it is.
        assert_eq!(ended(&launcher, &ids[2]).await, (exited.clone(), Some(0)));
        // The fourth still runs, and nothing was started again.
        assert_eq!(ended(&launcher, &ids[3]).await, (None, None));
        assert_eq!(mock.start_count().await, 4);
        // A non-zero exit the worker saw is a job that failed, as before.
        launcher.on_exit(&ids[3], 255).await;
        assert_eq!(ended(&launcher, &ids[3]).await, (exited, Some(255)));
        // A second pass finds nothing to end twice.
        launcher.reconcile().await;
        assert_eq!(launcher.delivery().interrupted, 2);
        assert_eq!(launcher.counts().unwrap().finished, 4);
    }

    #[tokio::test]
    async fn an_interruption_reaches_the_callback_and_a_refused_callback_is_tried_again() {
        use std::sync::Mutex as StdMutex;
        // A receiver that is down for the first three requests.
        let seen: Arc<StdMutex<Vec<Value>>> = Arc::new(StdMutex::new(Vec::new()));
        let hits = seen.clone();
        let app = axum::Router::new().route(
            "/callback",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                let hits = hits.clone();
                async move {
                    let mut hits = hits.lock().unwrap();
                    hits.push(body);
                    if hits.len() <= 3 {
                        axum::http::StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        axum::http::StatusCode::NO_CONTENT
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/callback", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let wait_for = |count: usize| {
            let seen = seen.clone();
            async move {
                for _ in 0..500 {
                    if seen.lock().unwrap().len() >= count {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                panic!("{count} callbacks never came");
            }
        };

        let (launcher, mock, clock) = harness(2);
        let mut request = job("alpine", None);
        request.job_key = Some("case-0001".into());
        request.callback_url = Some(url);
        let id = started(&launcher, request).await;
        mock.vanish(&id).await;
        clock.set(1_700_000_100);
        launcher.reconcile().await;
        wait_for(3).await;
        let body = seen.lock().unwrap()[0].clone();
        assert_eq!(body["id"], id.as_str());
        assert_eq!(body["job_key"], "case-0001");
        assert_eq!(body["status"], "exited");
        assert_eq!(body["reason"], "interrupted");
        assert!(body["exit_code"].is_null());
        // Nobody answered, and the page says so.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let waiting = launcher.delivery();
        assert_eq!(
            (
                waiting.interrupted,
                waiting.interrupted_unnotified,
                waiting.pending,
                waiting.failed
            ),
            (1, 1, 1, 0)
        );
        // Not while the first tries may still be running.
        assert_eq!(launcher.retry_callbacks().await.unwrap(), 0);
        clock.set(1_700_000_100 + 31);
        assert_eq!(launcher.retry_callbacks().await.unwrap(), 1);
        assert_eq!(seen.lock().unwrap().len(), 4);
        assert_eq!(seen.lock().unwrap()[3], body);
        let done = launcher.delivery();
        assert_eq!(
            (done.interrupted, done.interrupted_unnotified, done.pending),
            (1, 0, 0)
        );
        // Answered once, it is not sent again, and the job was not run again.
        assert_eq!(launcher.retry_callbacks().await.unwrap(), 0);
        assert_eq!(seen.lock().unwrap().len(), 4);
        assert_eq!(mock.start_count().await, 1);

        // A receiver that never answers: an hour of tries, then given up.
        let mut request = job("alpine", None);
        request.callback_url = Some("http://127.0.0.1:9/nobody".into());
        let lost = started(&launcher, request).await;
        launcher.on_exit(&lost, 0).await;
        clock.set(1_700_000_131 + CALLBACK_RETRY_SECS as u64 + 1);
        assert_eq!(launcher.retry_callbacks().await.unwrap(), 0);
        let gave_up = launcher.delivery();
        assert_eq!((gave_up.pending, gave_up.failed), (0, 1));
        // A job without a callback URL is only recorded.
        let quiet = started(&launcher, job("alpine", None)).await;
        mock.vanish(&quiet).await;
        launcher.reconcile().await;
        let last = launcher.delivery();
        assert_eq!(
            (last.interrupted, last.interrupted_unnotified, last.pending),
            (2, 0, 0)
        );
    }

    #[tokio::test]
    async fn a_pc_in_use_makes_a_new_job_wait_and_an_unknown_idle_time_does_not() {
        use crate::host::Feed;
        let idle = |secs| {
            Some(Feed {
                idle_secs: Some(secs),
                ..Feed::default()
            })
        };
        assert_eq!(user_active(0, idle(0)), None);
        assert_eq!(user_active(300, None), None);
        assert_eq!(user_active(300, Some(Feed::default())), None);
        assert_eq!(user_active(300, idle(300)), None);
        assert_eq!(
            user_active(300, idle(299)),
            Some(UserActive {
                idle_secs: 299,
                needed_secs: 300
            })
        );

        // The only test that sets the feed and a launcher that reads it.
        let (launcher, mock) = harness_with(2, |s| s.min_idle_secs = 300);
        crate::host::note(idle(1000).unwrap());
        let first = started(&launcher, job("alpine", None)).await;
        // The person comes back: the running job stays, a new one waits.
        crate::host::note(idle(2).unwrap());
        match launcher.launch(job("alpine", None)).await {
            Err(LaunchError::UserActive(active)) => assert_eq!(active.idle_secs, 2),
            other => panic!("{other:?}"),
        }
        assert_eq!(mock.start_count().await, 1);
        assert_eq!(mock.killed().await, Vec::<String>::new());
        assert_eq!(ended(&launcher, &first).await, (None, None));
        let live = launcher.live_status().await;
        assert_eq!(
            live.waiting_for.as_deref(),
            Some("the PC is in use: idle for 2s, 300s needed")
        );
        assert!(live.computer.user_active);
        crate::host::note(idle(300).unwrap());
        started(&launcher, job("alpine", None)).await;
        assert_eq!(mock.start_count().await, 2);
    }

    fn with_certificate(s: &mut Settings) {
        let base = s.history_path.parent().unwrap().to_path_buf();
        s.certificate_dir = base.join("certificate");
        s.secrets_dir = base.join("secrets");
        crate::certificate::import(&s.certificate_dir, b"not-a-real-pfx", "the-password").unwrap();
        s.certificate_enabled = true;
    }

    #[tokio::test]
    async fn every_container_gets_the_certificate_when_passthrough_is_on_and_none_when_off() {
        // Off, the default: nothing is mounted and a job's env is its own.
        let (launcher, mock) = harness_with(1, |_| {});
        assert!(!launcher.settings().certificate_enabled);
        let mut plain = job("alpine", None);
        plain
            .env
            .insert("SHUTUP_AND_RUN_CERT_PATH".into(), "/mine".into());
        launcher.launch(plain).await.unwrap();
        launcher.settled().await;
        let spec = &mock.specs().await[0];
        assert!(spec.binds.is_empty());
        assert_eq!(spec.env, vec!["SHUTUP_AND_RUN_CERT_PATH=/mine"]);

        // On: two jobs at once, each with its own read-only copies.
        let (launcher, mock) = harness_with(2, with_certificate);
        let secrets = launcher.settings().secrets_dir.clone();
        let mut sneaky = job("alpine", None);
        sneaky
            .env
            .insert("SHUTUP_AND_RUN_CERT_PATH".into(), "/etc/shadow".into());
        sneaky
            .env
            .insert("SHUTUP_AND_RUN_CERT_PASSWORD_FILE".into(), "/x".into());
        sneaky.env.insert("JOB".into(), "a".into());
        let first = match launcher.launch(sneaky).await.unwrap() {
            LaunchOutcome::Created(job) => job.id,
            other => panic!("{other:?}"),
        };
        let second = match launcher.launch(job("alpine", None)).await.unwrap() {
            LaunchOutcome::Created(job) => job.id,
            other => panic!("{other:?}"),
        };
        launcher.settled().await;
        for spec in mock.specs().await {
            assert_eq!(spec.binds.len(), 2);
            assert!(spec.binds[0].ends_with(":/run/shutup-and-run/certificate/client.pfx:ro"));
            assert!(spec.binds[1].ends_with(":/run/shutup-and-run/secrets/certificate-password:ro"));
            // The paths are the machine's, whatever the job said. The password is in no variable.
            assert!(spec.env.contains(
                &"SHUTUP_AND_RUN_CERT_PATH=/run/shutup-and-run/certificate/client.pfx".to_string()
            ));
            assert!(spec.env.contains(
                &"SHUTUP_AND_RUN_CERT_PASSWORD_FILE=/run/shutup-and-run/secrets/certificate-password"
                    .to_string()
            ));
            assert!(!spec
                .env
                .iter()
                .any(|e| e.contains("/etc/shadow") || e.ends_with("=/x")));
            assert!(!spec.env.iter().any(|e| e.contains("the-password")));
        }
        for id in [&first, &second] {
            assert_eq!(
                std::fs::read_to_string(secrets.join(id).join("certificate-password")).unwrap(),
                "the-password"
            );
        }
        // A job that ends, here by its timeout, takes its copies with it.
        // The other job's stay, and the stored job never held the password.
        launcher.on_exit(&first, 0).await;
        assert!(!secrets.join(&first).exists());
        assert!(secrets.join(&second).exists());
        let stored = format!("{:?}", launcher.job(&first, true).await.unwrap().unwrap());
        assert!(!stored.contains("the-password") && !stored.contains("certificate"));
        assert!(
            !callback_body(&launcher.job(&first, false).await.unwrap().unwrap())
                .to_string()
                .contains("password")
        );
        // After a restart, copies of jobs that are gone are swept.
        std::fs::create_dir_all(secrets.join("0123456789abcdef0123456789abcdef")).unwrap();
        launcher.reconcile().await;
        assert!(!secrets.join("0123456789abcdef0123456789abcdef").exists());
        assert!(secrets.join(&second).exists());

        // A start that fails leaves nothing behind either.
        let (launcher, mock) = harness_with(1, with_certificate);
        mock.fail_start(true).await;
        launcher.launch(job("alpine", None)).await.unwrap();
        launcher.settled().await;
        assert_eq!(
            std::fs::read_dir(&launcher.settings().secrets_dir)
                .unwrap()
                .count(),
            0
        );

        // On, but the certificate is gone: the job fails rather than run without it.
        let (launcher, mock) = harness_with(1, |s| {
            with_certificate(s);
            crate::certificate::forget(&s.certificate_dir);
        });
        launcher.launch(job("alpine", None)).await.unwrap();
        launcher.settled().await;
        assert_eq!(mock.start_count().await, 0);
        let failed = launcher.last_job().unwrap().unwrap();
        assert_eq!(failed.reason.as_deref(), Some(REASON_START_FAILED));
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
    async fn a_pause_for_a_while_ends_by_itself() {
        let (launcher, _, clock) = harness(1);
        let until = launcher.pause_for(3600).unwrap();
        assert_eq!(until, 1_700_000_000 + 3600);
        assert!(launcher.draining().unwrap());
        assert_eq!(launcher.paused_until(), Some(until));
        let err = launcher.launch(job("alpine", None)).await.unwrap_err();
        assert!(matches!(err, LaunchError::Draining));
        clock.set(1_700_000_000 + 3600);
        assert!(!launcher.draining().unwrap());
        assert_eq!(launcher.paused_until(), None);
        assert!(launcher.launch(job("alpine", None)).await.is_ok());
        launcher.settled().await;
        // A pause until someone resumes has no end.
        launcher.set_draining(true).unwrap();
        assert!(launcher.draining().unwrap());
        assert_eq!(launcher.paused_until(), None);
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

    // ----- safety of the computer's owner (shared-pc profile) -----

    use crate::safety::{Admission, Level, Readings};

    const GIB: u64 = 1024 * MIB;
    const T0: u64 = 1_700_000_000;

    /// A shared PC, with the memory and disk waits off unless a test sets them.
    fn shared(slots: usize) -> (Arc<Launcher>, Arc<MockRuntime>, Arc<FakeClock>) {
        let mock = Arc::new(MockRuntime::new());
        let runtime: Arc<dyn Runtime> = mock.clone();
        let mut settings = settings(slots);
        settings.shared_pc = true;
        let clock = Arc::new(FakeClock::new(T0));
        let launcher = Launcher::with_clock(settings, runtime, clock.clone()).unwrap();
        (launcher, mock, clock)
    }

    #[tokio::test]
    async fn memory_says_what_to_free_counts_the_wait_and_remembers_the_hour() {
        let (launcher, _, clock) = shared(2);
        launcher.set_readings(with_memory(GIB));
        let live = launcher.live_status().await;
        let need = live.memory.need.unwrap();
        assert_eq!(need.place, "machine");
        assert_eq!(need.to_free_bytes, 16 * GIB / 100 * 15 - GIB);
        assert_eq!(live.memory.waiting_since, Some(T0 as i64));
        assert_eq!(live.memory.waited_today_secs, 0);
        clock.set(T0 + 90);
        let live = launcher.live_status().await;
        assert_eq!(live.memory.waited_today_secs, 90);
        assert_eq!(live.memory.waited_week_secs, 90);
        // The hour ran short, so a lend_hours that leaves it out is offered.
        let hours = live.memory.short_hours.unwrap();
        let suggested = hours.suggested_lend_hours.unwrap();
        assert!(crate::lending::LendHours::parse(&suggested).is_ok());

        launcher.set_readings(Readings::healthy());
        clock.set(T0 + 100);
        let live = launcher.live_status().await;
        assert_eq!(live.memory.need, None);
        assert_eq!(live.memory.waiting_since, None);
        assert_eq!(live.memory.waited_today_secs, 100);
        // Counted once, however often the status is read.
        assert_eq!(launcher.live_status().await.memory.waited_today_secs, 100);

        // A paused worker is not waiting for memory.
        launcher.set_readings(with_memory(GIB));
        launcher.set_draining(true).unwrap();
        clock.set(T0 + 200);
        assert_eq!(launcher.live_status().await.memory.waiting_since, None);
    }

    fn with_memory(available: u64) -> Readings {
        let mut readings = Readings::healthy();
        readings.inner.memory_available = Some(available);
        readings
    }

    /// The 2 vCPU, 5 GB Windows PC of the manual test, with room left.
    fn small_windows_pc() -> Readings {
        Readings {
            windows: true,
            inner: crate::safety::Inner {
                cpus: Some(2),
                memory_total: Some(2 * GIB),
                memory_available: Some(GIB + GIB / 2),
                disk_free: Some(50 * GIB),
                load1: Some(0.1),
            },
            host: Some(crate::host::Feed {
                idle_secs: Some(900),
                memory_total: Some(5 * GIB),
                memory_available: Some(2 * GIB),
                cpus: Some(2),
                cpu_busy_percent: Some(8),
                disk_available: Some(40 * GIB),
                ..Default::default()
            }),
        }
    }

    async fn refused(launcher: &Arc<Launcher>, source: JobSource) -> LaunchError {
        let err = launcher
            .launch_from(job("alpine", None), source)
            .await
            .unwrap_err();
        launcher.settled().await;
        err
    }

    #[tokio::test]
    async fn a_healthy_shared_pc_starts_jobs_and_asks_nobody() {
        let (launcher, mock, _) = shared(2);
        let report = launcher.safety();
        assert!(report.applies);
        assert_eq!(
            (report.level, report.admission),
            (Level::Healthy, Admission::Open)
        );
        started(&launcher, job("alpine", None)).await;
        assert_eq!(mock.start_count().await, 1);
        let live = launcher.live_status().await;
        assert!(live.waiting_for.is_none());
        assert!(live.safety.incidents.is_empty());
    }

    #[tokio::test]
    async fn a_dedicated_server_is_not_assessed_whatever_its_readings() {
        let (launcher, mock, _) = harness(1);
        launcher.set_readings(Readings::default());
        assert!(!launcher.safety().applies);
        started(&launcher, job("alpine", None)).await;
        assert_eq!(mock.start_count().await, 1);
    }

    #[tokio::test]
    async fn critical_memory_blocks_every_path_stores_nothing_and_recovers_past_the_line() {
        let (launcher, mock, _) = shared(2);
        launcher.set_readings(with_memory(GIB));
        for source in [JobSource::Push, JobSource::Poll] {
            match refused(&launcher, source).await {
                LaunchError::SafetyBlocked(found) => {
                    assert_eq!(found[0].code, crate::safety::MEMORY_CRITICAL)
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(mock.start_count().await, 0);
        assert_eq!(launcher.counts().unwrap().started, 0);
        assert_eq!(launcher.slots().await.unwrap().free, 2);
        let live = launcher.live_status().await;
        assert!(live
            .waiting_for
            .as_deref()
            .unwrap()
            .starts_with("blocked to protect this computer: SAFETY_MEMORY_CRITICAL"));
        // Seen once by the status loop: one incident, however often it looks.
        launcher.live_status().await;
        let kinds = |live: &crate::status::Live| -> Vec<String> {
            live.safety
                .incidents
                .iter()
                .map(|i| i.kind.clone())
                .collect()
        };
        assert_eq!(kinds(&launcher.live_status().await), vec!["blocked"]);

        // Better, and not yet past the recovery line (2.4 GiB of 16): held.
        launcher.set_readings(with_memory(2 * GIB));
        assert!(matches!(
            refused(&launcher, JobSource::Push).await,
            LaunchError::SafetyBlocked(_)
        ));
        launcher.set_readings(with_memory(8 * GIB));
        started(&launcher, job("alpine", None)).await;
        assert_eq!(mock.start_count().await, 1);
        assert_eq!(
            kinds(&launcher.live_status().await),
            vec!["recovered", "blocked"]
        );
    }

    #[tokio::test]
    async fn saturated_cpu_a_full_disk_and_missing_readings_each_block() {
        type Break = fn(&mut Readings);
        let cases: Vec<(&str, Break)> = vec![
            (crate::safety::CPU_CRITICAL, |r| r.inner.load1 = Some(40.0)),
            (crate::safety::DISK_CRITICAL, |r| {
                r.inner.disk_free = Some(GIB)
            }),
            (crate::safety::TELEMETRY_MISSING, |r| {
                r.inner.memory_available = None
            }),
            (crate::safety::TELEMETRY_MISSING, |r| r.inner.load1 = None),
            (crate::safety::TELEMETRY_MISSING, |r| {
                r.inner.disk_free = None
            }),
        ];
        for (code, change) in cases {
            let (launcher, mock, _) = shared(1);
            let mut readings = Readings::healthy();
            change(&mut readings);
            launcher.set_readings(readings);
            match refused(&launcher, JobSource::Push).await {
                LaunchError::SafetyBlocked(found) => assert_eq!(found[0].code, code),
                other => panic!("{code}: {other:?}"),
            }
            assert_eq!(mock.start_count().await, 0, "{code}");
            // The same machine, fine again, takes the job.
            launcher.set_readings(Readings::healthy());
            started(&launcher, job("alpine", None)).await;
            assert_eq!(mock.start_count().await, 1, "{code}");
        }
    }

    #[tokio::test]
    async fn a_job_needs_its_own_memory_free_not_only_the_usual() {
        let (launcher, mock, _) = shared(2);
        // The usual need is 1 GiB; 3 GiB are free of 16.
        let mut readings = with_memory(3 * GIB + 300 * MIB);
        readings.inner.memory_total = Some(16 * GIB);
        launcher.set_readings(readings);
        let mut big = job("alpine", None);
        big.memory_bytes = Some((4 * GIB) as i64);
        // With the wait off nothing is compared.
        started(&launcher, big.clone()).await;
        assert_eq!(mock.start_count().await, 1);

        let mock = Arc::new(MockRuntime::new());
        let runtime: Arc<dyn Runtime> = mock.clone();
        let mut cfg = settings(2);
        cfg.shared_pc = true;
        cfg.min_free_memory = GIB;
        let launcher = Launcher::with_clock(cfg, runtime, Arc::new(FakeClock::new(T0))).unwrap();
        // 20% of 16 GiB is 3.2: above the warning line, under the job's 4 GiB.
        let mut readings = with_memory(3 * GIB + 300 * MIB);
        readings.inner.memory_total = Some(16 * GIB);
        launcher.set_readings(readings);
        match launcher.launch(big).await {
            Err(LaunchError::LowResources(short)) => {
                assert_eq!((short.resource, short.needed_bytes), ("memory", 4 * GIB))
            }
            other => panic!("{other:?}"),
        }
        // A job of the usual size still fits.
        started(&launcher, job("alpine", None)).await;
        assert_eq!(mock.start_count().await, 1);
    }

    #[tokio::test]
    async fn a_small_pc_waits_for_its_owner_and_the_yes_is_kept_until_the_risk_changes() {
        let (launcher, mock, clock) = shared(4);
        launcher.set_readings(small_windows_pc());
        // Push, poll: no path starts a job, and nothing is stored.
        for source in [JobSource::Push, JobSource::Poll] {
            match refused(&launcher, source).await {
                LaunchError::AuthorizationRequired(found) => {
                    assert_eq!(found.len(), 1);
                    assert_eq!(found[0].code, crate::safety::SMALL_MACHINE);
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(mock.start_count().await, 0);
        assert_eq!(launcher.counts().unwrap().started, 0);
        let live = launcher.live_status().await;
        assert_eq!(live.safety.admission, Admission::AuthorizationRequired);
        assert!(live
            .waiting_for
            .unwrap()
            .starts_with("the owner of this computer has to authorize: SAFETY_SMALL_MACHINE"));

        // The owner says yes on the page: recorded, and jobs start, one after
        // the other, with no further question.
        let consent = launcher.authorize("page").unwrap();
        assert_eq!(consent.codes, vec![crate::safety::SMALL_MACHINE]);
        assert_eq!((consent.channel.as_str(), consent.at), ("page", T0 as i64));
        assert_eq!(consent.notice_version, crate::safety::NOTICE_VERSION);
        started(&launcher, job("alpine", None)).await;
        started(&launcher, job("alpine", None)).await;
        assert_eq!(mock.start_count().await, 2);
        let report = launcher.live_status().await.safety;
        assert_eq!(
            (report.level, report.admission),
            (Level::Warning, Admission::Open)
        );
        assert_eq!(report.incidents[0].kind, "authorization_granted");
        assert!(report.incidents[0]
            .detail
            .contains("notice v1, on the page"));
        // A second yes with nothing new to agree to is refused as such.
        assert_eq!(
            launcher.authorize("page"),
            Err(crate::safety::GrantError::NothingToAuthorize)
        );

        // A risk the owner was not shown: asked again, for that one only.
        let mut busier = small_windows_pc();
        busier.host.as_mut().unwrap().cpu_busy_percent = Some(80);
        launcher.set_readings(busier);
        match refused(&launcher, JobSource::Push).await {
            LaunchError::AuthorizationRequired(found) => {
                assert_eq!(found.len(), 1);
                assert_eq!(found[0].code, crate::safety::CPU_BUSY)
            }
            other => panic!("{other:?}"),
        }
        clock.set(T0 + 60);
        let consent = launcher.authorize("page").unwrap();
        assert_eq!(
            consent.codes,
            vec![crate::safety::CPU_BUSY, crate::safety::SMALL_MACHINE]
        );
        started(&launcher, job("alpine", None)).await;

        // The yes never lifts a block: the same PC with 600 MiB left.
        let mut critical = small_windows_pc();
        critical.host.as_mut().unwrap().memory_available = Some(600 * MIB);
        launcher.set_readings(critical);
        assert!(matches!(
            refused(&launcher, JobSource::Push).await,
            LaunchError::SafetyBlocked(_)
        ));

        // Withdrawn: the question is back.
        launcher.set_readings(small_windows_pc());
        assert!(launcher.revoke_authorization("page").unwrap());
        assert!(matches!(
            refused(&launcher, JobSource::Poll).await,
            LaunchError::AuthorizationRequired(_)
        ));
        assert_eq!(mock.start_count().await, 3);
        assert_eq!(
            launcher.live_status().await.safety.incidents[0].kind,
            "authorization_revoked"
        );
    }

    #[tokio::test]
    async fn the_yes_survives_a_restart_and_a_stale_host_feed_asks_again() {
        let (launcher, _, _) = shared(2);
        launcher.set_readings(small_windows_pc());
        launcher.authorize("terminal").unwrap();
        let mock = Arc::new(MockRuntime::new());
        let runtime: Arc<dyn Runtime> = mock.clone();
        let restarted = Launcher::with_clock(
            launcher.settings().clone(),
            runtime,
            Arc::new(FakeClock::new(T0 + 5)),
        )
        .unwrap();
        restarted.set_readings(small_windows_pc());
        started(&restarted, job("alpine", None)).await;
        assert_eq!(mock.start_count().await, 1);

        // The supervisor went quiet: the PC is not known to be fine.
        let mut silent = small_windows_pc();
        silent.host = None;
        restarted.set_readings(silent);
        match refused(&restarted, JobSource::Push).await {
            LaunchError::AuthorizationRequired(found) => {
                assert_eq!(found[0].code, crate::safety::HOST_UNKNOWN)
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_confirmed_oom_is_told_from_a_plain_kill_and_three_pause_new_jobs() {
        let (launcher, mock, clock) = shared(4);
        // Killed from outside, exit 137, and Docker does not say OOM.
        let plain = started(&launcher, job("alpine", None)).await;
        mock.stop(&plain, 137).await;
        launcher.on_exit(&plain, 137).await;
        let killed = launcher.job(&plain, false).await.unwrap().unwrap();
        assert!(!killed.oom_killed);
        assert!(callback_body(&killed).get("oom_killed").is_none());
        assert!(launcher.live_status().await.safety.incidents.is_empty());

        mock.oom_on_start(true).await;
        for n in 0..2u64 {
            clock.set(T0 + n * 60);
            let id = started(&launcher, job("alpine", None)).await;
            let job = launcher.job(&id, false).await.unwrap().unwrap();
            assert!(job.oom_killed);
            assert_eq!(
                (job.reason.as_deref(), job.exit_code),
                (Some(REASON_EXITED), Some(137))
            );
            assert_eq!(callback_body(&job)["oom_killed"], true);
            assert!(crate::jobs::failure_of(&job)
                .unwrap()
                .contains("ran out of memory"));
        }
        // The die event and a status request racing for the same job: once.
        let raced = launcher.last_job().unwrap().unwrap().id;
        tokio::join!(launcher.on_exit(&raced, 137), launcher.on_exit(&raced, 137));
        let ooms = |live: crate::status::Live| {
            live.safety
                .incidents
                .iter()
                .filter(|i| i.kind == "oom")
                .count()
        };
        assert_eq!(ooms(launcher.live_status().await), 2);
        // Two are not a pattern yet.
        assert_eq!(launcher.safety().admission, Admission::Open);
        clock.set(T0 + 120);
        let third = started(&launcher, job("alpine", None)).await;
        launcher.job(&third, false).await.unwrap();
        let starts = mock.start_count().await;

        // Three inside half an hour: nothing new starts, on any path.
        match refused(&launcher, JobSource::Poll).await {
            LaunchError::SafetyBlocked(found) => {
                assert_eq!(found[0].code, crate::safety::OOM_BREAKER)
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(mock.start_count().await, starts);
        let live = launcher.live_status().await;
        assert_eq!(live.safety.admission, Admission::Blocked);
        assert_eq!(
            live.safety
                .incidents
                .iter()
                .filter(|i| i.kind == "oom")
                .count(),
            3
        );
        // The pause is bounded: fifteen minutes after the last one it is over.
        clock.set(T0 + 120 + 899);
        assert_eq!(launcher.safety().admission, Admission::Blocked);
        clock.set(T0 + 120 + 900);
        mock.oom_on_start(false).await;
        started(&launcher, job("alpine", None)).await;
        assert_eq!(mock.start_count().await, starts + 1);
    }

    #[tokio::test]
    async fn a_job_held_at_its_cpu_limit_most_of_the_time_is_recorded() {
        use crate::runtime::Throttling;
        let (launcher, mock, _) = shared(2);
        let busy = started(&launcher, job("alpine", None)).await;
        mock.throttle(Some(Throttling {
            periods: 600,
            throttled_periods: 540,
        }))
        .await;
        launcher.sample_throttling().await;
        mock.stop(&busy, 0).await;
        launcher.on_exit(&busy, 0).await;
        let incidents = launcher.live_status().await.safety.incidents;
        assert_eq!(incidents[0].kind, "cpu_throttled");
        assert!(incidents[0].detail.contains("90% of its time"));
        // The job itself ended well: throttling is the limit doing its work.
        assert_eq!(
            ended(&launcher, &busy).await,
            (Some(REASON_EXITED.into()), Some(0))
        );

        // A short burst, or a job never sampled, records nothing.
        let calm = started(&launcher, job("alpine", None)).await;
        mock.throttle(Some(Throttling {
            periods: 100,
            throttled_periods: 90,
        }))
        .await;
        launcher.sample_throttling().await;
        mock.stop(&calm, 0).await;
        launcher.on_exit(&calm, 0).await;
        assert_eq!(launcher.live_status().await.safety.incidents.len(), 1);
        assert!(!Throttling {
            periods: 1000,
            throttled_periods: 400
        }
        .significant());
    }

    #[tokio::test]
    async fn a_job_records_its_cpu_time_and_its_memory_peak() {
        use crate::runtime::Stats;
        let (launcher, mock) = harness_with(2, |_| {});
        let id = started(&launcher, job("alpine", None)).await;
        let read = |cpu_ns, memory| Stats {
            cpu_ns: Some(cpu_ns),
            memory_bytes: Some(memory),
            ..Stats::default()
        };
        mock.set_stats(Some(read(2_000_000_000, 300 << 20))).await;
        launcher.sample_throttling().await;
        // The peak is the most any reading saw; the CPU time, the last.
        mock.set_stats(Some(read(5_500_000_000, 120 << 20))).await;
        launcher.sample_throttling().await;
        mock.stop(&id, 0).await;
        launcher.on_exit(&id, 0).await;
        let row = launcher.store.get(&id).unwrap().unwrap();
        assert_eq!(
            (row.cpu_ms, row.memory_peak),
            (Some(5_500), Some(300 << 20))
        );
        // Never read: nothing is made up.
        mock.set_stats(None).await;
        let quick = started(&launcher, job("alpine", None)).await;
        mock.stop(&quick, 0).await;
        launcher.on_exit(&quick, 0).await;
        let row = launcher.store.get(&quick).unwrap().unwrap();
        assert_eq!((row.cpu_ms, row.memory_peak), (None, None));
    }

    #[tokio::test]
    async fn outside_lend_hours_a_job_waits_and_nothing_is_stored() {
        // The test clock's day and hour are not one minute of Monday.
        let (launcher, mock) = harness_with(2, |s| {
            s.lend_hours = Some(crate::lending::LendHours::parse("mon 00:00-00:01").unwrap())
        });
        let keyed = LaunchRequest {
            job_key: Some("k-1".into()),
            ..job("alpine", None)
        };
        match launcher.launch(keyed).await {
            Err(LaunchError::NotLending(hold)) => {
                assert_eq!(hold.code, crate::lending::OUTSIDE_LEND_HOURS);
                assert!(hold
                    .detail
                    .starts_with("outside lend_hours mon 00:00-00:01 (now "));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(mock.start_count().await, 0);
        assert!(launcher.store.get_by_key("k-1").unwrap().is_none());
        let live = launcher.live_status().await;
        assert!(live.waiting_for.unwrap().starts_with("outside lend_hours"));
        assert_eq!(
            live.computer.hold.map(|h| h.code).as_deref(),
            Some(crate::lending::OUTSIDE_LEND_HOURS)
        );
    }

    #[tokio::test]
    async fn docker_failing_under_a_shared_pc_fails_the_request_and_not_the_worker() {
        let (launcher, mock, _) = shared(1);
        mock.fail_lists(true).await;
        assert!(matches!(
            refused(&launcher, JobSource::Push).await,
            LaunchError::Unavailable(_)
        ));
        let live = launcher.live_status().await;
        assert!(live.docker_error.is_some());
        mock.fail_lists(false).await;
        started(&launcher, job("alpine", None)).await;
        assert_eq!(mock.start_count().await, 1);
    }
}
