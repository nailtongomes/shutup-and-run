//! Managed containers: definitions kept on this machine that the worker
//! starts by itself, with no poll URL and no push. `once` runs when a person
//! asks, `interval` runs again every so often and never twice at a time, and
//! `always` is started again whenever it ends, with a wait that grows.
//!
//! A run is an ordinary job. It goes through `Launcher::launch_managed`, so
//! the allowlist, the slots, a pause, the machine's limits, the safety of the
//! computer's owner and the certificate passthrough apply to it exactly as
//! to a pushed job, and it has a row in the history, a time limit and the
//! reaper. This module adds only what to start and when.
//!
//! The machine works one way at a time: in managed mode it takes no job from
//! outside, and outside it no managed container starts. The mode changes only
//! while nothing runs.
//!
//! What a person wants (enabled, start, stop, the mode) is written to the
//! history file by the page or the CLI. The pass below reads it and acts, so
//! the CLI needs no connection to the service, and a restart loses nothing.

use crate::cli::{emit, Out, EXIT_FAILED, EXIT_INVALID, EXIT_USAGE};
use crate::config::Settings;
use crate::launcher::{check_request, JobSource, LaunchError, LaunchRequest, Launcher};
use crate::store::{JobRecord, ManagedRow, Store};
use crate::{REASON_LAUNCHER, REASON_START_FAILED};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Read as _;
use std::process::ExitCode;
use std::sync::Arc;

pub const MAX_DEFINITIONS: usize = 32;
/// The shortest interval of an `interval` container.
pub const MIN_INTERVAL_SECS: u64 = 60;
pub const MAX_INTERVAL_SECS: u64 = 30 * 24 * 3600;
/// How often the definitions are looked at when nothing woke the pass.
pub const PASS_SECS: u64 = 10;
/// A start the machine refused for now (no slot, a pause, low memory, the
/// owner's safety, Docker down) is tried again this much later.
pub const DEFER_SECS: i64 = 30;
/// The least wait before an `always` container is started again, and the
/// first step of its backoff. It doubles with every run that did not last.
pub const BACKOFF_BASE_SECS: i64 = 30;
pub const BACKOFF_CAP_SECS: i64 = 900;
/// A run of an `always` container that lasted this long was not a crash
/// at start: the count of failures in a row goes back to zero.
pub const STABLE_SECS: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    Once,
    Interval,
    Always,
}

impl Policy {
    pub fn name(self) -> &'static str {
        match self {
            Policy::Once => "once",
            Policy::Interval => "interval",
            Policy::Always => "always",
        }
    }
}

/// A definition as a person writes it: the page's form, or a JSON file for
/// `managed apply`. A secret is `"NAME": "value"`, or `"NAME": null` for
/// "the value this machine already has", so a file can be kept anywhere.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub name: String,
    pub image: String,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub secret_env: BTreeMap<String, Option<String>>,
    #[serde(default)]
    pub cpu: Option<f64>,
    #[serde(default, deserialize_with = "size")]
    pub memory: Option<String>,
    #[serde(default, deserialize_with = "size")]
    pub shm: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    pub policy: Policy,
    #[serde(default)]
    pub interval_secs: Option<u64>,
    #[serde(default)]
    pub enabled: bool,
}

/// `"2g"` or a number of bytes.
fn size<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    match Option::<Value>::deserialize(deserializer)? {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(Value::Number(n)) => Ok(Some(n.to_string())),
        Some(_) => Err(serde::de::Error::custom(
            "a size is a string like \"2g\" or a number of bytes",
        )),
    }
}

/// A definition as it is stored: checked, and with the names of its secrets
/// but none of their values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Definition {
    pub name: String,
    pub image: String,
    pub command: Option<Vec<String>>,
    pub env: BTreeMap<String, String>,
    pub secret_env: Vec<String>,
    pub cpu: Option<f64>,
    pub memory: Option<String>,
    pub shm: Option<String>,
    pub timeout_secs: Option<u64>,
    pub policy: Policy,
    pub interval_secs: Option<u64>,
}

type Secrets = BTreeMap<String, String>;

/// A refusal with a stable code (`src/hints.rs`) and what was wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub code: &'static str,
    pub message: String,
}

impl Failure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Failure {
            code,
            message: message.into(),
        }
    }
}

impl From<crate::store::StoreError> for Failure {
    fn from(err: crate::store::StoreError) -> Self {
        Failure::new("MANAGED_FAILED", format!("job history: {err}"))
    }
}

fn invalid(message: impl Into<String>) -> Failure {
    Failure::new("MANAGED_INVALID", message)
}

fn valid_name(name: &str) -> bool {
    (1..=40).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Checks a definition against this machine and splits it into what is
/// stored in the open and the secret values. `stored` is what the machine
/// already holds for this name. The image, env, command, timeout, cpu and
/// memory go through `check_request`, the same code that admits a job.
pub fn check(
    input: Input,
    settings: &Settings,
    stored: &Secrets,
) -> Result<(Definition, Secrets), Failure> {
    if !valid_name(&input.name) {
        return Err(invalid(
            "name must be 1-40 characters of lowercase letters, digits, '-' or '_', and start with a letter or digit",
        ));
    }
    match (input.policy, input.interval_secs) {
        (Policy::Interval, None) => {
            return Err(invalid("policy interval needs interval_secs"));
        }
        (Policy::Interval, Some(secs))
            if !(MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS).contains(&secs) =>
        {
            return Err(invalid(format!(
                "interval_secs must be between {MIN_INTERVAL_SECS} and {MAX_INTERVAL_SECS}"
            )));
        }
        (Policy::Once | Policy::Always, Some(_)) => {
            return Err(invalid("interval_secs is only for policy interval"));
        }
        _ => {}
    }
    for (label, value) in [("memory", &input.memory), ("shm", &input.shm)] {
        if let Some(text) = value {
            crate::bytesize::parse_qty(text).map_err(|err| invalid(format!("{label}: {err}")))?;
        }
    }
    let mut secrets = Secrets::new();
    for (key, value) in &input.secret_env {
        if input.env.contains_key(key) {
            return Err(invalid(format!("{key} is in both env and secret_env")));
        }
        match value.as_ref().or_else(|| stored.get(key)) {
            Some(value) if !value.is_empty() => {
                secrets.insert(key.clone(), value.clone());
            }
            Some(_) => return Err(invalid(format!("the secret {key} is empty"))),
            // Named and not given yet: saved, and it does not start until
            // the value is here.
            None => {}
        }
    }
    let definition = Definition {
        name: input.name,
        image: input.image.trim().to_string(),
        command: input.command.filter(|c| !c.is_empty()),
        env: input.env,
        secret_env: input.secret_env.into_keys().collect(),
        cpu: input.cpu,
        memory: input.memory,
        shm: input.shm,
        timeout_secs: input.timeout_secs,
        policy: input.policy,
        interval_secs: input.interval_secs,
    };
    // A value still to come is checked as a name only.
    let mut all = secrets.clone();
    for key in &definition.secret_env {
        all.entry(key.clone()).or_insert_with(|| "x".to_string());
    }
    match check_request(
        request_with(&definition, all, settings),
        settings,
        JobSource::Managed,
    ) {
        Ok(_) => Ok((definition, secrets)),
        Err(LaunchError::Forbidden) => Err(invalid(format!(
            "image {} is not in this machine's allowlist [{}]",
            definition.image,
            settings.allowlist.join(", ")
        ))),
        Err(err) => Err(invalid(err.to_string())),
    }
}

fn request_with(definition: &Definition, secrets: Secrets, settings: &Settings) -> LaunchRequest {
    let bytes = |text: &Option<String>| {
        text.as_deref()
            .and_then(|t| crate::bytesize::parse_qty(t).ok())
    };
    LaunchRequest {
        image: definition.image.clone(),
        env: definition.env.clone().into_iter().chain(secrets).collect(),
        // An `always` container is meant to stay: with no time limit named
        // it gets the longest this machine allows, and is started again
        // when that ends.
        timeout_secs: definition.timeout_secs.or(match definition.policy {
            Policy::Always => Some(settings.max_timeout_secs),
            _ => None,
        }),
        cpu: definition.cpu,
        memory_bytes: bytes(&definition.memory),
        shm_bytes: bytes(&definition.shm),
        command: definition.command.clone(),
        job_key: None,
        callback_url: None,
    }
}

fn parse_row(row: &ManagedRow) -> Option<(Definition, Secrets)> {
    Some((
        serde_json::from_str(&row.spec).ok()?,
        serde_json::from_str(&row.secrets).unwrap_or_default(),
    ))
}

fn missing_secrets(definition: &Definition, secrets: &Secrets) -> Vec<String> {
    definition
        .secret_env
        .iter()
        .filter(|key| !secrets.contains_key(*key))
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// What a person asks for. Each is one write to the history file; the pass
// acts on it. The page and the CLI both call these.

fn row_of(store: &Store, name: &str) -> Result<ManagedRow, Failure> {
    store.managed_row(name)?.ok_or_else(|| {
        Failure::new(
            "MANAGED_NOT_FOUND",
            format!("there is no managed container named {name}"),
        )
    })
}

/// Creates or replaces a definition. An edit applies to the next run; a
/// run in progress keeps what it was started with.
pub fn save(store: &Store, settings: &Settings, input: Input) -> Result<Definition, Failure> {
    let existing = store.managed_row(&input.name)?;
    if existing.is_none() && store.managed_rows()?.len() >= MAX_DEFINITIONS {
        return Err(Failure::new(
            "MANAGED_INVALID",
            format!("this machine keeps at most {MAX_DEFINITIONS} managed containers"),
        ));
    }
    let stored: Secrets = existing
        .as_ref()
        .and_then(|row| serde_json::from_str(&row.secrets).ok())
        .unwrap_or_default();
    let enabled = input.enabled;
    let (definition, secrets) = check(input, settings, &stored)?;
    store.save_managed(
        &definition.name,
        &serde_json::to_string(&definition).unwrap_or_default(),
        &serde_json::to_string(&secrets).unwrap_or_default(),
        enabled,
    )?;
    Ok(definition)
}

pub fn save_json(store: &Store, settings: &Settings, bytes: &[u8]) -> Result<Definition, Failure> {
    let input: Input = serde_json::from_slice(bytes).map_err(|err| invalid(err.to_string()))?;
    save(store, settings, input)
}

pub fn set_enabled(store: &Store, name: &str, enabled: bool) -> Result<(), Failure> {
    row_of(store, name)?;
    store.set_managed_enabled(name, enabled)?;
    Ok(())
}

/// Asks for a start now. `Ok(false)`: it is already running.
pub fn start(store: &Store, name: &str) -> Result<bool, Failure> {
    let row = row_of(store, name)?;
    if !store.managed_mode()? {
        return Err(Failure::new(
            "MANAGED_MODE_OFF",
            "managed mode is off: this machine takes jobs from outside and starts no managed container",
        ));
    }
    if !row.enabled {
        return Err(Failure::new(
            "MANAGED_DISABLED",
            format!("{name} is disabled, and a disabled container does not start"),
        ));
    }
    if store.managed_running(name)?.is_some() {
        return Ok(false);
    }
    store.request_managed_start(name)?;
    Ok(true)
}

/// Asks for a stop. The container is killed by the service's next pass, and
/// it stays stopped until a person starts it. `Ok(false)`: it was not
/// running, and is now held all the same.
pub fn stop(store: &Store, name: &str) -> Result<bool, Failure> {
    row_of(store, name)?;
    let running = store.managed_running(name)?.is_some();
    store.request_managed_stop(name)?;
    Ok(running)
}

pub fn remove(store: &Store, name: &str) -> Result<(), Failure> {
    row_of(store, name)?;
    if !store.delete_managed(name)? {
        return Err(Failure::new(
            "MANAGED_RUNNING",
            format!("{name} is running: stop it before removing it"),
        ));
    }
    Ok(())
}

/// Sets, or with None removes, the value of one secret.
pub fn set_secret(
    store: &Store,
    name: &str,
    key: &str,
    value: Option<&str>,
) -> Result<(), Failure> {
    let row = row_of(store, name)?;
    let (definition, mut secrets) = parse_row(&row).ok_or_else(|| {
        invalid(format!(
            "the stored definition of {name} is damaged: save it again"
        ))
    })?;
    if !definition.secret_env.iter().any(|k| k == key) {
        return Err(invalid(format!(
            "{name} has no secret named {key}: its secret_env is [{}]",
            definition.secret_env.join(", ")
        )));
    }
    match value {
        Some("") => return Err(invalid(format!("the secret {key} is empty"))),
        Some(value) if value.len() > 16 * 1024 || value.contains('\0') => {
            return Err(invalid(format!("invalid value for {key}")))
        }
        Some(value) => secrets.insert(key.to_string(), value.to_string()),
        None => secrets.remove(key),
    };
    store.set_managed_secrets(name, &serde_json::to_string(&secrets).unwrap_or_default())?;
    Ok(())
}

/// Changes the mode, only while the worker is idle. From the CLI; the page
/// goes through `Launcher::set_managed_mode`, which ends in the same
/// statement.
pub fn set_mode(store: &Store, managed: bool) -> Result<(), Failure> {
    if store.set_managed_mode(managed)? {
        return Ok(());
    }
    Err(busy(store.running_ids().map(|ids| ids.len()).unwrap_or(0)))
}

pub fn busy(running: usize) -> Failure {
    Failure::new(
        "MANAGED_BUSY",
        format!(
            "the worker is busy: {running} container(s) running or starting. The mode changes only while nothing runs, and nothing was changed"
        ),
    )
}

// ---------------------------------------------------------------------------
// What the page and the CLI show. Never an env value, a secret or a log.

#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub definition: Definition,
    pub enabled: bool,
    /// `running`, `starting`, `scheduled`, `backoff`, `waiting`,
    /// `needs_secret`, `stopped`, `idle`, `disabled`, or `off` while the
    /// machine is not in managed mode.
    pub state: &'static str,
    /// Why it waits, in this program's words.
    pub note: Option<String>,
    pub running_job: Option<String>,
    pub last: Option<JobRecord>,
    pub failures: i64,
    /// When the next automatic start is due, when one is.
    pub next_start_at: Option<i64>,
    pub secrets_missing: Vec<String>,
}

pub fn views(store: &Store, now: i64) -> Result<(bool, Vec<View>), Failure> {
    let mode = store.managed_mode()?;
    let mut out = Vec::new();
    for row in store.managed_rows()? {
        let Some((definition, secrets)) = parse_row(&row) else {
            continue;
        };
        let running_job = store.managed_running(&row.name)?;
        let last = store.managed_last(&row.name)?;
        let secrets_missing = missing_secrets(&definition, &secrets);
        let due = next_due(&row, &definition, last.as_ref());
        let waiting = row.note.is_some() && now < row.next_attempt_at;
        let (state, next_start_at) = if running_job.is_some() {
            ("running", None)
        } else if !row.enabled {
            ("disabled", None)
        } else if !mode {
            ("off", None)
        } else if !secrets_missing.is_empty() {
            ("needs_secret", None)
        } else if waiting {
            ("waiting", Some(row.next_attempt_at))
        } else if row.start_requested {
            ("starting", None)
        } else if row.held {
            ("stopped", None)
        } else {
            match (definition.policy, due) {
                (Policy::Once, _) => ("idle", None),
                (Policy::Always, Some(at)) if row.failures > 0 && now < at => ("backoff", Some(at)),
                (Policy::Interval, Some(at)) if now < at => ("scheduled", Some(at)),
                _ => ("starting", None),
            }
        };
        out.push(View {
            definition,
            enabled: row.enabled,
            state,
            note: row.note.clone().filter(|_| waiting),
            running_job,
            last,
            failures: row.failures,
            next_start_at,
            secrets_missing,
        });
    }
    Ok((mode, out))
}

pub fn view_json(view: &View) -> Value {
    let d = &view.definition;
    json!({
        "name": d.name,
        "image": d.image,
        "policy": d.policy.name(),
        "interval_secs": d.interval_secs,
        "enabled": view.enabled,
        "state": view.state,
        "note": view.note,
        "running_job": view.running_job,
        "failures": view.failures,
        "next_start_at": view.next_start_at.map(crate::status::unix_to_iso),
        "secrets_missing": view.secrets_missing,
        "last": view.last.as_ref().map(crate::jobs::job_json),
    })
}

/// A definition as `managed show` prints it, which `managed apply` takes
/// back unchanged: every secret is `null`, "the value this machine has".
pub fn definition_json(definition: &Definition, enabled: bool) -> Value {
    let secrets: BTreeMap<&str, Value> = definition
        .secret_env
        .iter()
        .map(|key| (key.as_str(), Value::Null))
        .collect();
    json!({
        "name": definition.name,
        "image": definition.image,
        "command": definition.command,
        "env": definition.env,
        "secret_env": secrets,
        "cpu": definition.cpu,
        "memory": definition.memory,
        "shm": definition.shm,
        "timeout_secs": definition.timeout_secs,
        "policy": definition.policy.name(),
        "interval_secs": definition.interval_secs,
        "enabled": enabled,
    })
}

// ---------------------------------------------------------------------------
// The pass: what is wanted against what is there.

pub fn backoff_secs(failures: i64) -> i64 {
    let steps = failures.clamp(1, 16) - 1;
    (BACKOFF_BASE_SECS << steps).min(BACKOFF_CAP_SECS)
}

/// What the end of a run means for the next start: the failures in a row
/// and the earliest automatic start. A stop a person asked for is not a
/// failure and changes neither.
pub fn account(policy: Policy, failures: i64, job: &JobRecord, now: i64) -> (i64, i64) {
    if job.reason.as_deref() == Some(REASON_LAUNCHER) {
        return (failures, 0);
    }
    let ended = job.ended_at.unwrap_or(now);
    let never_started = job.reason.as_deref() == Some(REASON_START_FAILED);
    match policy {
        Policy::Always => {
            // What counts is whether it stayed up, not its exit code: a
            // container that exits 0 at once would loop just the same.
            if !never_started && job.duration_secs().unwrap_or(0) >= STABLE_SECS {
                (0, ended + BACKOFF_BASE_SECS)
            } else {
                let failures = failures + 1;
                (failures, ended + backoff_secs(failures))
            }
        }
        // The interval is the wait. The count is for the person reading.
        Policy::Interval | Policy::Once => {
            if crate::jobs::status_of(job) == "ok" {
                (0, 0)
            } else {
                (failures + 1, 0)
            }
        }
    }
}

/// When the next automatic start is due. None: only a person starts it.
fn next_due(row: &ManagedRow, definition: &Definition, last: Option<&JobRecord>) -> Option<i64> {
    let started = row
        .last_started_at
        .into_iter()
        .chain(last.map(|job| job.started_at))
        .max();
    match definition.policy {
        Policy::Once => None,
        // Start to start. A run that took longer than the interval is
        // followed by the next at once, never joined by it; intervals
        // missed while the service was down are one run, not several.
        Policy::Interval => Some(
            started
                .map(|at| at + definition.interval_secs.unwrap_or(MIN_INTERVAL_SECS) as i64)
                .unwrap_or(0)
                .max(row.next_attempt_at),
        ),
        Policy::Always => Some(
            started
                .map(|at| at + BACKOFF_BASE_SECS)
                .unwrap_or(0)
                .max(row.next_attempt_at),
        ),
    }
}

fn deferral(err: &LaunchError) -> String {
    match err {
        LaunchError::Draining => "the worker is paused".to_string(),
        LaunchError::NoSlots { total, used } => format!("no free slot ({used} of {total} in use)"),
        LaunchError::Forbidden => "image is not allowlisted".to_string(),
        other => other.to_string(),
    }
}

/// One look at every definition. It ends the runs a person asked to stop,
/// counts the runs that ended, and starts what is due and may start. It
/// never starts a second run of a definition, and never anything outside
/// managed mode.
pub async fn pass(launcher: &Arc<Launcher>) {
    let _one = launcher.managed_pass.lock().await;
    let store = launcher.store().clone();
    let rows = match store.managed_rows() {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(error = %err, "managed containers");
            return;
        }
    };
    let mode = store.managed_mode().unwrap_or(false);
    for row in rows {
        if let Err(err) = one(launcher, &store, row, mode).await {
            tracing::warn!(error = %err, "managed containers");
        }
    }
}

async fn one(
    launcher: &Arc<Launcher>,
    store: &Store,
    row: ManagedRow,
    mode: bool,
) -> Result<(), crate::store::StoreError> {
    let name = row.name.as_str();
    let Some((definition, secrets)) = parse_row(&row) else {
        return Ok(());
    };
    let mut running = store.managed_running(name)?;
    // Ask Docker about the run: a container that ended with nobody told
    // (Docker restarted, the event was missed) is closed here.
    if let Some(id) = &running {
        match launcher.job(id, false).await {
            Ok(Some(job)) if job.is_finished() => running = None,
            Ok(_) => {}
            // Docker did not answer. Nothing is known, so nothing is done.
            Err(_) => return Ok(()),
        }
    }
    if row.stop_requested {
        if let Some(id) = &running {
            match launcher.stop(id).await {
                Ok(_) => tracing::info!(name, %id, "managed container stopped on request"),
                // Docker did not answer: the request stays for the next pass.
                Err(_) => return Ok(()),
            }
        }
        store.clear_managed_stop(name)?;
        // Counted on the next pass, which the end of the job wakes.
        return Ok(());
    }

    // Count the last run once it has ended.
    let last = store.managed_last(name)?;
    let now = launcher.now();
    let mut next_attempt_at = row.next_attempt_at;
    let mut failures = row.failures;
    match &last {
        Some(job) if job.is_finished() && row.accounted_job.as_deref() != Some(&job.id) => {
            (failures, next_attempt_at) = account(definition.policy, row.failures, job, now);
            store.note_managed_ended(name, &job.id, failures, next_attempt_at)?;
            if failures > 0 && definition.policy == Policy::Always {
                tracing::warn!(
                    name,
                    failures,
                    wait_secs = (next_attempt_at - now).max(0),
                    "managed container ended early; waiting before the next start"
                );
            }
        }
        // The history no longer has the run that was started (it was purged
        // before it was counted). Nothing says it went well.
        None if row.last_job.is_some() && row.accounted_job != row.last_job => {
            failures += 1;
            next_attempt_at = now + backoff_secs(failures);
            store.note_managed_ended(
                name,
                row.last_job.as_deref().unwrap_or_default(),
                failures,
                next_attempt_at,
            )?;
        }
        _ => {}
    }
    if running.is_some() {
        return Ok(());
    }
    if !mode || !row.enabled {
        if row.start_requested {
            store.clear_managed_start(name)?;
        }
        return Ok(());
    }

    let counted = ManagedRow {
        next_attempt_at,
        failures,
        ..row.clone()
    };
    let due = if row.start_requested {
        // Asked by a person: now, unless the machine has just refused it.
        now >= next_attempt_at || row.note.is_none()
    } else {
        !row.held && next_due(&counted, &definition, last.as_ref()).is_some_and(|at| now >= at)
    };
    if !due {
        return Ok(());
    }
    let missing = missing_secrets(&definition, &secrets);
    let refused = if missing.is_empty() {
        let request = request_with(&definition, secrets, launcher.settings());
        match launcher.launch_managed(name, request).await {
            Ok(job) => {
                store.note_managed_started(name, &job.id, job.started_at)?;
                tracing::info!(name, id = %job.id, policy = definition.policy.name(), "managed container started");
                return Ok(());
            }
            Err(LaunchError::AlreadyRunning | LaunchError::JobsMode) => return Ok(()),
            Err(err) => deferral(&err),
        }
    } else {
        format!(
            "the secret {} has no value on this machine",
            missing.join(", ")
        )
    };
    // Not a failure of the container: it did not run. Try again later, and
    // say why once.
    if row.note.as_deref() != Some(refused.as_str()) {
        tracing::warn!(name, why = %refused, "managed container not started; trying again later");
    }
    store.note_managed_deferred(name, &refused, now + DEFER_SECS)?;
    Ok(())
}

pub fn spawn(launcher: Arc<Launcher>) {
    tokio::spawn(async move {
        loop {
            pass(&launcher).await;
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(PASS_SECS)) => {}
                _ = launcher.managed_changed.notified() => {
                    // Never a spin, whatever keeps waking it.
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// shutup-and-run managed

pub fn usage() -> &'static str {
    "\
shutup-and-run managed [list] [--json]
    the managed containers of this machine: policy, state, the last run.
shutup-and-run managed mode [on|off]
    managed mode. On: the worker starts its own managed containers and takes
    no job from the poll URL or from POST /v1/jobs. Off (the default): the
    opposite. It changes only while no container runs or is starting.
shutup-and-run managed apply FILE|-
    create or replace one definition from a JSON file (examples/managed/).
    Checked like a job before anything is written.
shutup-and-run managed show NAME
    the definition, as `apply` takes it. Secret values are never printed.
shutup-and-run managed secret NAME KEY [--unset]
    set the value of a secret, read from stdin; it goes nowhere else.
shutup-and-run managed enable|disable NAME
    a disabled container is never started. One that is running when it is
    disabled finishes its run; `stop` ends it now.
shutup-and-run managed start|stop NAME
    start now, or kill the running container. A stopped container stays
    stopped until `start`. The running service acts within 10 seconds.
shutup-and-run managed remove NAME

Policies: once (runs only on `start`), interval (again every interval_secs,
never two at a time), always (started again whenever it ends, after a wait
that doubles from 30 s to 15 min while its runs last under a minute).
No questions, --json, exit codes 0 ok, 1 failed, 2 usage, 3 invalid."
}

fn failed(failure: Failure) -> Out {
    let code = match failure.code {
        "MANAGED_INVALID" => EXIT_INVALID,
        _ => EXIT_FAILED,
    };
    let mut out = Out::fail(code, failure.message);
    out.json["code"] = json!(failure.code);
    out
}

fn list_text(mode: bool, views: &[View], now: i64) -> String {
    let mut text = format!(
        "managed mode: {}\n",
        if mode {
            "on (no job is taken from outside)"
        } else {
            "off (jobs are taken from outside; no managed container starts)"
        }
    );
    for view in views {
        let d = &view.definition;
        let policy = match d.interval_secs {
            Some(secs) => format!("interval {secs}s"),
            None => d.policy.name().to_string(),
        };
        let last = match &view.last {
            Some(job) => format!(
                "last {} {}",
                crate::jobs::status_of(job),
                crate::status::unix_to_iso(job.ended_at.unwrap_or(job.started_at))
            ),
            None => "never ran".to_string(),
        };
        let next = match view.next_start_at {
            Some(at) => format!(", next in {}s", (at - now).max(0)),
            None => String::new(),
        };
        text.push_str(&format!(
            "{:<24} {:<14} {:<12} {last}{next}  {}\n",
            d.name, policy, view.state, d.image
        ));
        if let Some(note) = &view.note {
            text.push_str(&format!("    waiting: {note}\n"));
        }
        if view.failures > 0 {
            text.push_str(&format!("    failures in a row: {}\n", view.failures));
        }
        if !view.secrets_missing.is_empty() {
            text.push_str(&format!(
                "    secrets without a value: {}\n",
                view.secrets_missing.join(", ")
            ));
        }
    }
    if views.is_empty() {
        text.push_str("no managed containers");
    }
    text.trim_end().to_string()
}

/// Every subcommand against a history file. `stdin` is what `apply -` and
/// `secret` read.
pub fn run(
    store: &Store,
    settings: &Settings,
    words: &[&str],
    stdin: &mut dyn std::io::Read,
    now: i64,
) -> Out {
    let read = |file: &str, stdin: &mut dyn std::io::Read| -> Result<Vec<u8>, Out> {
        let mut bytes = Vec::new();
        if file == "-" {
            std::io::Read::take(stdin, 1024 * 1024)
                .read_to_end(&mut bytes)
                .map_err(|err| Out::fail(EXIT_FAILED, format!("stdin: {err}")))?;
            Ok(bytes)
        } else {
            std::fs::read(file).map_err(|err| Out::fail(EXIT_FAILED, format!("{file}: {err}")))
        }
    };
    let done = |result: Result<(), Failure>, json: Value, text: String| match result {
        Ok(()) => Out::ok(json, text),
        Err(failure) => failed(failure),
    };
    match *words {
        [] | ["list"] => match views(store, now) {
            Ok((mode, views)) => Out::ok(
                json!({
                    "mode": if mode { "managed" } else { "jobs" },
                    "containers": views.iter().map(view_json).collect::<Vec<_>>(),
                }),
                list_text(mode, &views, now),
            ),
            Err(failure) => failed(failure),
        },
        ["mode"] => match store.managed_mode() {
            Ok(mode) => Out::ok(
                json!({"mode": if mode { "managed" } else { "jobs" }}),
                format!("managed mode: {}", if mode { "on" } else { "off" }),
            ),
            Err(err) => failed(err.into()),
        },
        ["mode", to @ ("on" | "off")] => done(
            set_mode(store, to == "on"),
            json!({"mode": if to == "on" { "managed" } else { "jobs" }}),
            if to == "on" {
                "managed mode is on: enabled managed containers start, and no job is taken from outside".to_string()
            } else {
                "managed mode is off: the worker takes jobs from outside again".to_string()
            },
        ),
        ["apply", file] => {
            let bytes = match read(file, stdin) {
                Ok(bytes) => bytes,
                Err(out) => return out,
            };
            match save_json(store, settings, &bytes) {
                Ok(definition) => {
                    let missing = store
                        .managed_row(&definition.name)
                        .ok()
                        .flatten()
                        .and_then(|row| parse_row(&row))
                        .map(|(d, s)| missing_secrets(&d, &s))
                        .unwrap_or_default();
                    let mut text = format!("saved {}", definition.name);
                    if !missing.is_empty() {
                        text.push_str(&format!(
                            "\nit will not start until these secrets have a value: {}\n\
                             set each with: shutup-and-run managed secret {} KEY   (value on stdin)",
                            missing.join(", "),
                            definition.name
                        ));
                    }
                    Out::ok(
                        json!({"name": definition.name, "secrets_missing": missing}),
                        text,
                    )
                }
                Err(failure) => failed(failure),
            }
        }
        ["show", name] => match row_of(store, name) {
            Ok(row) => match parse_row(&row) {
                Some((definition, _)) => {
                    let json = definition_json(&definition, row.enabled);
                    let text = serde_json::to_string_pretty(&json).unwrap_or_default();
                    Out::ok(json!({"definition": json}), text)
                }
                None => failed(invalid(format!(
                    "the stored definition of {name} is damaged: save it again"
                ))),
            },
            Err(failure) => failed(failure),
        },
        ["secret", name, key, "--unset"] => done(
            set_secret(store, name, key, None),
            json!({"name": name, "secret": key, "set": false}),
            format!("{key} of {name} has no value now"),
        ),
        ["secret", name, key] => {
            let mut value = String::new();
            if let Err(err) = std::io::Read::take(&mut *stdin, 64 * 1024).read_to_string(&mut value)
            {
                return Out::fail(EXIT_FAILED, format!("stdin: {err}"));
            }
            done(
                set_secret(store, name, key, Some(value.trim_end_matches(['\n', '\r']))),
                json!({"name": name, "secret": key, "set": true}),
                format!("{key} of {name} is set"),
            )
        }
        [verb @ ("enable" | "disable"), name] => done(
            set_enabled(store, name, verb == "enable"),
            json!({"name": name, "enabled": verb == "enable"}),
            if verb == "enable" {
                format!("{name} is enabled")
            } else {
                format!("{name} is disabled: nothing starts it. A run in progress finishes; `shutup-and-run managed stop {name}` ends it now")
            },
        ),
        ["start", name] => match start(store, name) {
            Ok(asked) => Out::ok(
                json!({"name": name, "requested": asked, "already_running": !asked}),
                if asked {
                    format!("start of {name} requested: the running service acts within {PASS_SECS} seconds")
                } else {
                    format!("{name} is already running")
                },
            ),
            Err(failure) => failed(failure),
        },
        ["stop", name] => match stop(store, name) {
            Ok(running) => Out::ok(
                json!({"name": name, "requested": running, "held": true}),
                if running {
                    format!("stop of {name} requested: the running service acts within {PASS_SECS} seconds. It stays stopped until `start`")
                } else {
                    format!("{name} was not running. It stays stopped until `start`")
                },
            ),
            Err(failure) => failed(failure),
        },
        ["remove", name] => done(
            remove(store, name),
            json!({"name": name, "removed": true}),
            format!("removed {name}"),
        ),
        _ => Out::fail(EXIT_USAGE, format!("unknown command. {}", usage())),
    }
}

pub fn managed_cli(args: &[String]) -> ExitCode {
    let as_json = args.iter().any(|a| a == "--json");
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "-h" | "--help" | "help"))
    {
        println!("{}", usage());
        return ExitCode::SUCCESS;
    }
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| *a != "--json")
        .collect();
    let settings = match Settings::load() {
        Ok(settings) => settings,
        Err(err) => return emit(Out::fail(EXIT_INVALID, err.0), as_json),
    };
    let store = match Store::open(&settings.history_path) {
        Ok(store) => store,
        Err(err) => {
            return emit(
                Out::fail(
                    EXIT_FAILED,
                    format!("{}: {err}", settings.history_path.display()),
                ),
                as_json,
            )
        }
    };
    let out = run(
        &store,
        &settings,
        &words,
        &mut std::io::stdin(),
        crate::status::now_unix(),
    );
    emit(out, as_json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::launcher::LaunchOutcome;
    use crate::mock::MockRuntime;
    use crate::runtime::Runtime;
    use crate::safety::Readings;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    const T0: u64 = 1_700_000_000;
    const IMAGE: &str = "ghcr.io/nailtongomes/rpa-worker-selenium:latest";
    const SECRET: &str = "tok-5f1c-never-shown";
    const GIB: u64 = 1024 * 1024 * 1024;

    struct Machine {
        launcher: Arc<Launcher>,
        mock: Arc<MockRuntime>,
        clock: Arc<FakeClock>,
        settings: Settings,
    }

    impl Machine {
        fn store(&self) -> &Store {
            self.launcher.store()
        }

        /// One pass, and every start it made has become a container.
        async fn pass(&self) {
            pass(&self.launcher).await;
            self.launcher.settled().await;
        }

        fn advance(&self, secs: u64) {
            self.clock
                .set(crate::clock::Clock::now_unix(&*self.clock) + secs);
        }

        async fn running(&self, name: &str) -> Option<String> {
            self.store().managed_running(name).unwrap()
        }

        /// The container ends and Docker says so.
        async fn end(&self, name: &str, code: i64) {
            let id = self.running(name).await.expect("a run to end");
            self.mock.stop(&id, code).await;
            self.launcher.on_exit(&id, code).await;
        }

        fn view(&self, name: &str) -> View {
            let (_, all) = views(self.store(), self.launcher.now()).unwrap();
            all.into_iter()
                .find(|v| v.definition.name == name)
                .expect("the definition")
        }

        /// The same history and the same Docker, a new process.
        async fn restarted(&self) -> Machine {
            let runtime: Arc<dyn Runtime> = self.mock.clone();
            let launcher =
                Launcher::with_clock(self.settings.clone(), runtime, self.clock.clone()).unwrap();
            launcher.reconcile().await;
            Machine {
                launcher,
                mock: self.mock.clone(),
                clock: self.clock.clone(),
                settings: self.settings.clone(),
            }
        }
    }

    fn settings(slots: usize, extra: &str) -> Settings {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("shutup-and-run-managed-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Settings::from_toml_and_env(
            &format!(
                r#"
                token = "test-token-value-ok"
                max_slots = {slots}
                allowlist = ["alpine", "ghcr.io/nailtongomes/rpa-worker-selenium"]
                default_timeout_secs = 600
                max_timeout_secs = 86400
                history_path = '{}'
                history_days = 30
                resource_profile = "none"
                {extra}
                "#,
                dir.join("jobs.sqlite").display()
            ),
            &HashMap::new(),
        )
        .unwrap()
    }

    fn machine_with(slots: usize, extra: &str, change: impl FnOnce(&mut Settings)) -> Machine {
        let mut settings = settings(slots, extra);
        change(&mut settings);
        let mock = Arc::new(MockRuntime::new());
        let clock = Arc::new(FakeClock::new(T0));
        let runtime: Arc<dyn Runtime> = mock.clone();
        let launcher = Launcher::with_clock(settings.clone(), runtime, clock.clone()).unwrap();
        Machine {
            launcher,
            mock,
            clock,
            settings,
        }
    }

    /// A machine already in managed mode.
    fn machine(slots: usize) -> Machine {
        let m = machine_with(slots, "", |_| {});
        assert!(m.store().set_managed_mode(true).unwrap());
        m
    }

    fn input(value: Value) -> Input {
        serde_json::from_value(value).unwrap()
    }

    fn selenium(name: &str, policy: &str) -> Value {
        let mut value = json!({
            "name": name,
            "image": IMAGE,
            "env": {"SCRIPT_URL": "https://www.example.com/files/my_script.py", "USE_XVFB": "1", "DISPLAY": ":99"},
            "secret_env": {"API_TOKEN": SECRET},
            "cpu": 1,
            "memory": "2g",
            "shm": "2g",
            "timeout_secs": 3600,
            "policy": policy,
            "enabled": true,
        });
        if policy == "interval" {
            value["interval_secs"] = json!(300);
        }
        value
    }

    fn add(m: &Machine, value: Value) {
        save(m.store(), &m.settings, input(value)).unwrap();
    }

    fn push() -> LaunchRequest {
        LaunchRequest {
            image: "alpine".into(),
            env: HashMap::new(),
            timeout_secs: None,
            cpu: None,
            memory_bytes: None,
            shm_bytes: None,
            command: None,
            job_key: None,
            callback_url: None,
        }
    }

    #[test]
    fn a_definition_is_checked_like_a_job_before_it_is_saved() {
        let m = machine_with(1, "", |s| s.max_job_memory = Some(4 * GIB));
        let refused = |change: &dyn Fn(&mut Value)| {
            let mut value = selenium("rpa", "interval");
            change(&mut value);
            let parsed: Result<Input, _> = serde_json::from_value(value);
            match parsed {
                Err(err) => err.to_string(),
                Ok(input) => {
                    let failure = save(m.store(), &m.settings, input).unwrap_err();
                    assert_eq!(failure.code, "MANAGED_INVALID");
                    failure.message
                }
            }
        };
        assert!(refused(&|v| v["name"] = json!("Has Spaces")).contains("name must be"));
        assert!(refused(&|v| v["image"] = json!("docker.io/someone/else")).contains("allowlist"));
        assert!(refused(&|v| v["interval_secs"] = json!(5)).contains("between 60"));
        assert!(refused(&|v| {
            v.as_object_mut().unwrap().remove("interval_secs");
        })
        .contains("needs interval_secs"));
        assert!(refused(&|v| v["policy"] = json!("always")).contains("only for policy interval"));
        assert!(refused(&|v| v["policy"] = json!("hourly")).contains("unknown variant"));
        assert!(refused(&|v| v["memory"] = json!("lots")).contains("memory"));
        // Above the machine's limit per job: refused, never cut down.
        assert!(refused(&|v| v["memory"] = json!("8g")).contains("above this machine's limit"));
        assert!(refused(&|v| v["timeout_secs"] = json!(999_999_999)).contains("above the maximum"));
        assert!(refused(&|v| v["env"]["BAD NAME"] = json!("1")).contains("invalid environment"));
        assert!(
            refused(&|v| v["env"]["API_TOKEN"] = json!("x")).contains("both env and secret_env")
        );
        // What a job may not set, a managed container may not either.
        for field in ["mounts", "ports", "privileged", "network", "volumes"] {
            assert!(
                refused(&|v| v[field] = json!(true)).contains("unknown field"),
                "{field}"
            );
        }
        // Nothing of all that reached the history.
        assert!(m.store().managed_rows().unwrap().is_empty());
        add(&m, selenium("rpa", "interval"));
        assert_eq!(m.store().managed_rows().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn definitions_and_secrets_survive_a_restart_and_a_secret_is_shown_nowhere() {
        let m = machine(2);
        add(&m, selenium("rpa", "always"));
        let again = m.restarted().await;
        let row = again.store().managed_row("rpa").unwrap().unwrap();
        assert!(row.enabled);
        assert!(again.store().managed_mode().unwrap(), "the mode is kept");
        again.pass().await;

        // The container got the secret, with the rest of the request.
        let spec = &again.mock.specs().await[0];
        assert!(spec.env.contains(&format!("API_TOKEN={SECRET}")));
        assert!(spec.env.contains(&"USE_XVFB=1".to_string()));
        assert_eq!(spec.image, IMAGE);
        assert_eq!(spec.nano_cpus, Some(1_000_000_000));
        assert_eq!(spec.memory_bytes, Some(2 * GIB as i64));
        assert_eq!(spec.shm_bytes, Some(2 * GIB as i64));
        assert_eq!(spec.deadline_unix, T0 + 3600);
        assert!(spec.binds.is_empty());

        // Nothing a person or a file can read has it.
        let (mode, all) = views(again.store(), again.launcher.now()).unwrap();
        let live = again.launcher.live_status().await;
        let fields = crate::status::managed_editor_for_test(&all[0].definition);
        let mut shown = vec![
            row.spec.clone(),
            all.iter().map(|v| view_json(v).to_string()).collect(),
            definition_json(&all[0].definition, true).to_string(),
            list_text(mode, &all, T0 as i64),
            String::from_utf8(crate::status::document(&live, T0 as i64, false, None)).unwrap(),
            crate::status::managed_view(
                mode,
                &all,
                &Default::default(),
                T0 as i64,
                crate::status::Lang::Pt,
                None,
            ),
            fields,
        ];
        for words in [vec!["list"], vec!["show", "rpa"]] {
            let out = run(
                again.store(),
                &again.settings,
                &words,
                &mut std::io::empty(),
                T0 as i64,
            );
            shown.push(out.text);
            shown.push(out.json.to_string());
        }
        for text in &shown {
            assert!(!text.contains(SECRET), "the secret is in: {text}");
        }
        // The name of the secret is there, and the plain env is for the admin.
        assert!(shown[2].contains("\"API_TOKEN\":null"));

        // An edit that names the secret without a value keeps the stored one.
        let mut edit = selenium("rpa", "always");
        edit["secret_env"] = json!({"API_TOKEN": null});
        edit["env"]["USE_VNC"] = json!("0");
        add(&again, edit);
        let kept: Secrets =
            serde_json::from_str(&again.store().managed_row("rpa").unwrap().unwrap().secrets)
                .unwrap();
        assert_eq!(kept["API_TOKEN"], SECRET);
    }

    #[tokio::test]
    async fn a_secret_with_no_value_holds_the_start_until_it_is_set() {
        let m = machine(1);
        let mut value = selenium("rpa", "always");
        value["secret_env"] = json!({"API_TOKEN": null, "CERT_ID": "abc"});
        add(&m, value);
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 0);
        let view = m.view("rpa");
        assert_eq!(view.secrets_missing, ["API_TOKEN"]);
        assert_eq!(view.state, "needs_secret");

        let out = run(
            m.store(),
            &m.settings,
            &["secret", "rpa", "API_TOKEN"],
            &mut format!("{SECRET}\n").as_bytes(),
            T0 as i64,
        );
        assert_eq!(out.code, 0, "{}", out.text);
        assert!(!out.text.contains(SECRET) && !out.json.to_string().contains(SECRET));
        m.pass().await;
        let env = &m.mock.specs().await[0].env;
        assert!(
            env.contains(&format!("API_TOKEN={SECRET}"))
                && env.contains(&"CERT_ID=abc".to_string())
        );
        // A secret the definition does not name is refused.
        let out = run(
            m.store(),
            &m.settings,
            &["secret", "rpa", "OTHER"],
            &mut "v".as_bytes(),
            0,
        );
        assert_eq!(
            (out.code, out.json["code"].as_str()),
            (EXIT_INVALID, Some("MANAGED_INVALID"))
        );
    }

    #[tokio::test]
    async fn the_two_modes_never_mix_and_change_only_while_the_worker_is_idle() {
        let m = machine_with(2, "", |s| {
            s.poll_url = Some("http://127.0.0.1:9/poll".into());
        });
        add(&m, selenium("rpa", "always"));

        // Jobs mode, the default: a push runs, and no managed container does.
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 0);
        assert_eq!(m.view("rpa").state, "off");
        let Ok(LaunchOutcome::Created(job)) = m.launcher.launch(push()).await else {
            panic!("a push in jobs mode");
        };
        m.launcher.settled().await;
        assert_eq!(
            start(m.store(), "rpa").unwrap_err().code,
            "MANAGED_MODE_OFF"
        );

        // Busy: the mode does not change, from the service or from the CLI.
        assert!(!m.launcher.set_managed_mode(true).await.unwrap());
        let out = run(
            m.store(),
            &m.settings,
            &["mode", "on"],
            &mut std::io::empty(),
            0,
        );
        assert_eq!(
            (out.code, out.json["code"].as_str()),
            (EXIT_FAILED, Some("MANAGED_BUSY"))
        );
        assert!(out.text.contains("1 container(s)"), "{}", out.text);
        assert!(!m.store().managed_mode().unwrap());

        // Idle: it does.
        m.mock.stop(&job.id, 0).await;
        m.launcher.on_exit(&job.id, 0).await;
        assert!(m.launcher.set_managed_mode(true).await.unwrap());

        // Managed mode: nothing from outside is admitted, by any path.
        assert!(matches!(
            m.launcher.launch(push()).await,
            Err(LaunchError::ManagedMode)
        ));
        assert!(matches!(
            m.launcher.launch_from(push(), JobSource::Poll).await,
            Err(LaunchError::ManagedMode)
        ));
        // The poller does not even ask: this URL would be an error.
        let client = crate::poll::client().unwrap();
        assert_eq!(
            crate::poll::poll_once(&m.launcher, &client).await.unwrap(),
            crate::poll::PollOutcome::Idle
        );
        // The store refuses the row itself, whoever writes it.
        let outside = crate::store::NewJob {
            id: "ab12".into(),
            image: "alpine".into(),
            job_key: None,
            started_at: 0,
            callback_url: None,
            managed: None,
        };
        assert!(matches!(
            m.store().insert_running(&outside),
            Err(crate::store::StoreError::Mode)
        ));
        assert_eq!(m.store().running_ids().unwrap().len(), 0);

        // And the managed container runs.
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 2);
        assert_eq!(m.view("rpa").state, "running");

        // Back to jobs only when it has stopped. A restart keeps the mode.
        assert!(!m.launcher.set_managed_mode(false).await.unwrap());
        let again = m.restarted().await;
        assert!(again.launcher.managed_mode().unwrap());
        stop(again.store(), "rpa").unwrap();
        again.pass().await;
        assert!(again.launcher.set_managed_mode(false).await.unwrap());
        again.pass().await;
        assert!(again.launcher.launch(push()).await.is_ok());
        again.launcher.settled().await;
        // A managed run cannot be written outside managed mode either.
        assert!(matches!(
            again.launcher.launch_managed("rpa", push()).await,
            Err(LaunchError::JobsMode)
        ));
    }

    #[tokio::test]
    async fn an_interval_container_runs_start_to_start_and_never_twice_at_once() {
        let m = machine(4);
        add(&m, selenium("rpa", "interval"));
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 1, "the first run is at once");

        // Still running when the next is due, and long after: no second one.
        for _ in 0..5 {
            m.advance(200);
            m.pass().await;
        }
        assert_eq!(m.mock.start_count().await, 1);
        assert_eq!(m.mock.running_ids().await.len(), 1);

        // It ends late: the next follows at once, and only one.
        m.end("rpa", 0).await;
        m.pass().await;
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 2);

        // It ends early: the next waits for the interval, counted from the start.
        m.advance(10);
        m.end("rpa", 0).await;
        m.pass().await;
        assert_eq!(m.view("rpa").state, "scheduled");
        m.advance(289);
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 2);
        m.advance(1);
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 3);

        // A run that fails is not tried again early, and is counted.
        m.advance(5);
        m.end("rpa", 2).await;
        m.pass().await;
        m.advance(200);
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 3);
        assert_eq!(m.view("rpa").failures, 1);
        m.advance(100);
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 4);
        m.end("rpa", 0).await;
        m.pass().await;
        assert_eq!(m.view("rpa").failures, 0);

        // The service was down for ten intervals: one run, not ten.
        m.advance(3000);
        let again = m.restarted().await;
        for _ in 0..3 {
            again.pass().await;
        }
        assert_eq!(again.mock.start_count().await, 5);
    }

    #[tokio::test]
    async fn a_restart_finds_the_running_container_and_starts_no_second_one() {
        let m = machine(4);
        add(&m, selenium("keep", "always"));
        add(&m, selenium("often", "interval"));
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 2);
        m.advance(4000);
        let again = m.restarted().await;
        again.pass().await;
        again.pass().await;
        assert_eq!(again.mock.start_count().await, 2);
        assert_eq!(again.mock.running_ids().await.len(), 2);

        // The worker died between writing the job and noting it on the
        // definition: the job row alone is enough to know it runs.
        let m = machine(4);
        add(&m, selenium("rpa", "always"));
        m.store()
            .insert_running(&crate::store::NewJob {
                id: "feed01".into(),
                image: IMAGE.into(),
                job_key: None,
                started_at: T0 as i64,
                callback_url: None,
                managed: Some("rpa".into()),
            })
            .unwrap();
        m.mock.seed_running("feed01", T0 + 3600).await;
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 0);
        assert_eq!(m.view("rpa").state, "running");
    }

    #[tokio::test]
    async fn an_always_container_that_keeps_crashing_is_restarted_ever_more_slowly() {
        let m = machine(2);
        add(&m, selenium("rpa", "always"));
        m.mock.exit_on_start(Some(1)).await;
        // One simulated hour, a pass every second.
        let mut starts_at = Vec::new();
        for second in 0..3600u64 {
            let before = m.mock.start_count().await;
            m.pass().await;
            if m.mock.start_count().await > before {
                starts_at.push(second);
            }
            m.advance(1);
        }
        // 30, 60, 120, 240, 480, then 900 at most: nine starts, not 3600.
        assert!((6..=9).contains(&starts_at.len()), "{starts_at:?}");
        let gaps: Vec<u64> = starts_at.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.windows(2).all(|w| w[1] >= w[0]), "{gaps:?}");
        assert!(gaps[0] >= 30 && *gaps.last().unwrap() <= 902, "{gaps:?}");
        assert_eq!(m.mock.running_ids().await.len(), 0);
        let view = m.view("rpa");
        assert_eq!(view.failures as usize, starts_at.len());
        assert_eq!(view.state, "backoff");
        assert_eq!(backoff_secs(1), 30);
        assert_eq!(backoff_secs(5), 480);
        assert_eq!(backoff_secs(6), 900);
        assert_eq!(backoff_secs(i64::MAX), 900);

        // It is fixed: the next try stays up, and a run that lasted clears the count.
        m.mock.exit_on_start(None).await;
        m.advance(900);
        m.pass().await;
        assert_eq!(m.view("rpa").state, "running");
        m.advance(600);
        m.end("rpa", 1).await;
        m.pass().await;
        assert_eq!(m.view("rpa").failures, 0);
        let count = m.mock.start_count().await;
        m.advance(29);
        m.pass().await;
        assert_eq!(m.mock.start_count().await, count, "never sooner than 30 s");
        m.advance(1);
        m.pass().await;
        assert_eq!(m.mock.start_count().await, count + 1);

        // An image that exits 0 at once is a loop all the same.
        m.end("rpa", 0).await;
        m.pass().await;
        assert_eq!(m.view("rpa").failures, 1);
    }

    #[tokio::test]
    async fn a_start_that_fails_is_counted_and_a_machine_that_refuses_is_only_waited_for() {
        // The container cannot be created: each try is a failure, with backoff.
        let m = machine(2);
        add(&m, selenium("rpa", "always"));
        m.mock.fail_start(true).await;
        for _ in 0..200 {
            m.pass().await;
            m.advance(1);
        }
        assert_eq!(m.mock.start_count().await, 0);
        let tries = m.store().counts().unwrap().started;
        assert!((3..=4).contains(&tries), "{tries} tries in 200 s");
        assert_eq!(m.view("rpa").failures as u64, tries);
        assert_eq!(
            m.view("rpa").last.unwrap().reason.as_deref(),
            Some(REASON_START_FAILED)
        );

        // Docker does not answer: nothing is written, nothing is counted.
        let m = machine(2);
        add(&m, selenium("rpa", "always"));
        m.mock.fail_lists(true).await;
        for _ in 0..100 {
            m.pass().await;
            m.advance(1);
        }
        assert_eq!(m.store().counts().unwrap().started, 0);
        let view = m.view("rpa");
        assert_eq!((view.failures, view.state), (0, "waiting"));
        assert_eq!(view.note.as_deref(), Some("docker is unavailable"));
        m.mock.fail_lists(false).await;
        m.advance(DEFER_SECS as u64);
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 1);
        assert_eq!(m.view("rpa").state, "running");

        // Docker goes away under a running container: the pass leaves it be.
        m.mock.fail_lists(true).await;
        m.advance(100);
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 1);
    }

    #[tokio::test]
    async fn pressure_a_pause_and_full_slots_defer_a_start_and_never_force_it() {
        let m = machine_with(1, "", |s| s.min_free_memory = 2 * GIB);
        assert!(m.store().set_managed_mode(true).unwrap());
        add(&m, selenium("first", "interval"));
        add(&m, selenium("second", "always"));
        let mut low = Readings::healthy();
        low.inner.memory_available = Some(GIB / 2);
        m.launcher.set_readings(low);
        m.pass().await;
        assert_eq!(m.store().counts().unwrap().started, 0);
        let view = m.view("first");
        assert_eq!(view.state, "waiting");
        assert!(
            view.note.as_deref().unwrap().starts_with("low on memory"),
            "{:?}",
            view.note
        );

        // Memory is back, but the worker is paused.
        m.launcher.set_readings(Readings::healthy());
        m.launcher.set_draining(true).unwrap();
        m.advance(DEFER_SECS as u64);
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 0);
        assert_eq!(
            m.view("second").note.as_deref(),
            Some("the worker is paused")
        );

        // Resumed: one slot, so one of the two runs and the other waits for it.
        m.launcher.set_draining(false).unwrap();
        m.advance(DEFER_SECS as u64);
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 1);
        assert_eq!(m.view("first").state, "running");
        assert!(m.view("second").note.unwrap().starts_with("no free slot"));
        m.advance(DEFER_SECS as u64);
        m.end("first", 0).await;
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 2);
        assert_eq!(m.view("second").state, "running");
        // None of the waiting was a failure of either container.
        assert_eq!(
            (m.view("first").failures, m.view("second").failures),
            (0, 0)
        );
    }

    #[tokio::test]
    async fn the_safety_of_the_computers_owner_stands_before_a_managed_start() {
        let m = machine_with(2, "", |s| s.shared_pc = true);
        assert!(m.store().set_managed_mode(true).unwrap());
        add(&m, selenium("rpa", "always"));
        let mut critical = Readings::healthy();
        critical.inner.memory_available = Some(GIB / 8);
        m.launcher.set_readings(critical);
        m.pass().await;
        assert_eq!(m.store().counts().unwrap().started, 0);
        let note = m.view("rpa").note.unwrap();
        assert!(note.contains("SAFETY_MEMORY_CRITICAL"), "{note}");
        m.launcher.set_readings(Readings::healthy());
        m.advance(DEFER_SECS as u64);
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 1);
    }

    #[tokio::test]
    async fn an_interrupted_run_is_seen_and_followed_by_one_new_run() {
        let m = machine(2);
        add(&m, selenium("rpa", "always"));
        m.pass().await;
        let id = m.running("rpa").await.unwrap();
        // The container is gone and no event said so (Docker restarted).
        m.advance(500);
        m.mock.vanish(&id).await;
        m.pass().await;
        let last = m.view("rpa").last.unwrap();
        assert_eq!(last.reason.as_deref(), Some(crate::REASON_INTERRUPTED));
        assert_eq!(m.mock.start_count().await, 1);
        m.advance(BACKOFF_BASE_SECS as u64);
        m.pass().await;
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 2);
        assert_eq!(m.mock.running_ids().await.len(), 1);
    }

    #[tokio::test]
    async fn once_runs_only_when_asked_and_stop_holds_until_a_person_starts_again() {
        let m = machine(3);
        add(&m, selenium("report", "once"));
        add(&m, selenium("keep", "always"));
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 1, "only the always one");
        assert_eq!(m.view("report").state, "idle");

        assert!(start(m.store(), "report").unwrap());
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 2);
        assert!(!start(m.store(), "report").unwrap(), "already running");
        m.end("report", 0).await;
        m.advance(5000);
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 2, "once is once");

        // Stop: killed now, recorded as stopped by the worker, not a failure,
        // and not started again however long it stays that way.
        let id = m.running("keep").await.unwrap();
        assert!(stop(m.store(), "keep").unwrap());
        m.pass().await;
        assert_eq!(m.mock.killed().await, vec![id.clone()]);
        let job = m.store().get(&id).unwrap().unwrap();
        assert_eq!(job.reason.as_deref(), Some(REASON_LAUNCHER));
        for _ in 0..5 {
            m.advance(1000);
            m.pass().await;
        }
        let view = m.view("keep");
        assert_eq!((view.state, view.failures), ("stopped", 0));
        assert_eq!(m.mock.start_count().await, 2);
        // Still held after a restart.
        let again = m.restarted().await;
        again.pass().await;
        assert_eq!(again.mock.start_count().await, 2);
        // Start lifts it.
        assert!(start(again.store(), "keep").unwrap());
        again.pass().await;
        assert_eq!(again.mock.start_count().await, 3);

        assert_eq!(
            start(again.store(), "nobody").unwrap_err().code,
            "MANAGED_NOT_FOUND"
        );
        assert_eq!(
            remove(again.store(), "keep").unwrap_err().code,
            "MANAGED_RUNNING"
        );
        remove(again.store(), "report").unwrap();
        assert!(again.store().managed_row("report").unwrap().is_none());
    }

    #[tokio::test]
    async fn disabling_starts_nothing_new_and_lets_the_run_in_progress_finish() {
        let m = machine(2);
        add(&m, selenium("rpa", "interval"));
        m.pass().await;
        set_enabled(m.store(), "rpa", false).unwrap();
        m.pass().await;
        assert!(m.mock.killed().await.is_empty(), "disable does not kill");
        assert_eq!(m.view("rpa").state, "running");
        m.end("rpa", 0).await;
        for _ in 0..5 {
            m.advance(1000);
            m.pass().await;
        }
        assert_eq!(m.mock.start_count().await, 1);
        assert_eq!(m.view("rpa").state, "disabled");
        assert_eq!(
            start(m.store(), "rpa").unwrap_err().code,
            "MANAGED_DISABLED"
        );
        set_enabled(m.store(), "rpa", true).unwrap();
        m.pass().await;
        assert_eq!(m.mock.start_count().await, 2);

        // The allowlist is edited under a saved definition: it stops starting.
        m.end("rpa", 0).await;
        let mut strict = m.settings.clone();
        strict.allowlist = vec!["alpine".into()];
        let runtime: Arc<dyn Runtime> = m.mock.clone();
        let strict = Machine {
            launcher: Launcher::with_clock(strict.clone(), runtime, m.clock.clone()).unwrap(),
            mock: m.mock.clone(),
            clock: m.clock.clone(),
            settings: strict,
        };
        strict.advance(1000);
        strict.pass().await;
        assert_eq!(strict.mock.start_count().await, 2);
        assert_eq!(
            strict.view("rpa").note.as_deref(),
            Some("image is not allowlisted")
        );
    }

    #[test]
    fn the_cli_applies_a_file_and_keeps_its_exit_codes() {
        let m = machine(1);
        let go = |words: &[&str], stdin: &str| {
            run(
                m.store(),
                &m.settings,
                words,
                &mut stdin.as_bytes(),
                T0 as i64,
            )
        };
        let file = include_str!("../examples/managed/rpa-selenium.json");
        let out = go(&["apply", "-"], file);
        assert_eq!(out.code, 0, "{}", out.text);
        assert_eq!(out.json["name"], "rpa-selenium");
        // The example names its secrets and holds no value for them.
        assert_eq!(
            out.json["secrets_missing"],
            json!(["API_TOKEN", "CERTIFICATE_ID"])
        );
        assert!(out.text.contains("managed secret rpa-selenium"));

        // `show` is what `apply` takes.
        let shown = go(&["show", "rpa-selenium"], "");
        assert_eq!(
            go(&["apply", "-"], &shown.json["definition"].to_string()).code,
            0
        );
        let list = go(&["list"], "");
        assert_eq!(list.json["mode"], "managed");
        assert_eq!(list.json["containers"][0]["state"], "needs_secret");
        assert_eq!(list.json["containers"][0]["policy"], "interval");

        assert_eq!(go(&["apply", "-"], "{not json").code, EXIT_INVALID);
        assert_eq!(go(&["frobnicate"], "").code, EXIT_USAGE);
        let missing = go(&["enable", "nobody"], "");
        assert_eq!(
            (missing.code, missing.json["code"].as_str()),
            (EXIT_FAILED, Some("MANAGED_NOT_FOUND"))
        );
        assert_eq!(go(&["disable", "rpa-selenium"], "").json["enabled"], false);
        assert_eq!(go(&["mode", "off"], "").json["mode"], "jobs");
        assert_eq!(go(&["mode"], "").json["mode"], "jobs");
        assert_eq!(go(&["remove", "rpa-selenium"], "").code, 0);
    }
}
