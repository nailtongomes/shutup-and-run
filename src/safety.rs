//! Is it safe, for the person who owns this computer, to start one more job?
//!
//! Asked when a job wants to start and once a second for the status, from
//! readings that are already cheap: `/proc`, one `statvfs`, and on Windows the
//! line the supervisor writes about the PC itself (`src/host.rs`). Nothing is
//! sampled in between and no running job is touched: the only thing this
//! decides is whether a new one may start.
//!
//! Three answers. `healthy`: start. `warning`: a material risk for the owner,
//! so a job starts only when the owner said yes to that risk (`Consent`).
//! `blocked`: no job starts, whatever anyone agreed to.
//!
//! It applies under `resource_profile = shared-pc`, the default on Windows.
//! A machine with profile `none` is a server its owner dedicated to jobs, and
//! is not assessed.
//!
//! The thresholds are constants, not config: a limit the operator could turn
//! off from far away would not protect the person at the PC. They are
//! conservative starting points, not measurements of any one machine.

use crate::host::Feed;
use crate::resources::{human, GIB};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const MIB: u64 = 1024 * 1024;

/// Bumped when the notice the owner agrees to changes in substance. A consent
/// given to another version no longer counts.
pub const NOTICE_VERSION: u32 = 1;

/// A PC with less memory than this, or this many processors or fewer, is
/// small: a job is a visible share of it.
pub const SMALL_MEMORY: u64 = 8 * GIB;
pub const SMALL_CPUS: u64 = 2;

/// Available memory, as a share of the total: below the first a warning,
/// below the second (or below the floor in bytes) blocked. Blocked clears
/// only above the recovery figures, so it does not flap around the line.
pub const MEMORY_WARN_PERCENT: u64 = 20;
pub const MEMORY_BLOCK_PERCENT: u64 = 10;
pub const MEMORY_RECOVER_PERCENT: u64 = 15;
/// The floor of the owner's machine (the Windows PC, or a Linux machine).
pub const OWNER_MEMORY_FLOOR: u64 = GIB;
/// The floor inside the WSL2 VM, which is small on purpose.
pub const VM_MEMORY_FLOOR: u64 = 256 * MIB;

pub const DISK_BLOCK: u64 = 2 * GIB;
pub const DISK_RECOVER: u64 = 3 * GIB;

/// Load average of the last minute, per processor, where the containers run.
pub const LOAD_WARN: f64 = 1.0;
pub const LOAD_BLOCK: f64 = 2.0;
pub const LOAD_RECOVER: f64 = 1.5;

/// How busy the Windows PC's processors were over the last feed, in percent.
pub const HOST_CPU_WARN: u64 = 75;
pub const HOST_CPU_BLOCK: u64 = 95;
pub const HOST_CPU_RECOVER: u64 = 85;

/// This many jobs killed for memory inside the window stop new jobs until
/// the pause has passed since the last of them. Then jobs start again: the
/// breaker is a pause, not a lock.
pub const OOM_BREAKER_COUNT: usize = 3;
pub const OOM_BREAKER_WINDOW_SECS: i64 = 1800;
pub const OOM_BREAKER_PAUSE_SECS: i64 = 900;

pub const TELEMETRY_MISSING: &str = "SAFETY_TELEMETRY_MISSING";
pub const MEMORY_CRITICAL: &str = "SAFETY_MEMORY_CRITICAL";
pub const DISK_CRITICAL: &str = "SAFETY_DISK_CRITICAL";
pub const CPU_CRITICAL: &str = "SAFETY_CPU_CRITICAL";
pub const OOM_BREAKER: &str = "SAFETY_OOM_BREAKER";
pub const SMALL_MACHINE: &str = "SAFETY_SMALL_MACHINE";
pub const MEMORY_LOW: &str = "SAFETY_MEMORY_LOW";
pub const CPU_BUSY: &str = "SAFETY_CPU_BUSY";
pub const HOST_UNKNOWN: &str = "SAFETY_HOST_UNKNOWN";
/// Not a finding: what the worker answers when a warning has no consent.
pub const AUTHORIZATION_REQUIRED: &str = "AUTHORIZATION_REQUIRED";

/// Where the containers run: the machine itself on Linux, the WSL2 VM on
/// Windows. A reading that failed is None, and None is never "fine".
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Inner {
    pub cpus: Option<u64>,
    pub memory_total: Option<u64>,
    pub memory_available: Option<u64>,
    pub disk_free: Option<u64>,
    pub load1: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Readings {
    /// The worker is the WSL2 side of a Windows PC, so `host` is expected.
    pub windows: bool,
    pub inner: Inner,
    /// The Windows PC, from the supervisor. None: no fresh line from it.
    pub host: Option<Feed>,
}

impl Readings {
    pub fn read(windows: bool, disk_path: &std::path::Path) -> Self {
        Self {
            windows,
            inner: Inner {
                cpus: std::thread::available_parallelism()
                    .ok()
                    .map(|n| n.get() as u64),
                memory_total: crate::resources::memory_total(),
                memory_available: crate::resources::memory_available(),
                disk_free: crate::resources::disk_free(disk_path),
                load1: load1(),
            },
            host: if windows {
                crate::host::current()
            } else {
                None
            },
        }
    }

    /// A machine nothing is wrong with. Tests start from it and break one thing.
    pub fn healthy() -> Self {
        Self {
            windows: false,
            inner: Inner {
                cpus: Some(8),
                memory_total: Some(16 * GIB),
                memory_available: Some(12 * GIB),
                disk_free: Some(100 * GIB),
                load1: Some(0.2),
            },
            host: None,
        }
    }
}

/// The first field of `/proc/loadavg`.
fn load1() -> Option<f64> {
    load1_of(&std::fs::read_to_string("/proc/loadavg").ok()?)
}

pub(crate) fn load1_of(text: &str) -> Option<f64> {
    text.split_whitespace()
        .next()?
        .parse()
        .ok()
        .filter(|v: &f64| v.is_finite() && *v >= 0.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Healthy,
    Warning,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub code: String,
    pub level: Level,
    /// The numbers behind it, for the status and the log.
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub findings: Vec<Finding>,
}

impl Assessment {
    pub fn level(&self) -> Level {
        if self.findings.iter().any(|f| f.level == Level::Blocked) {
            Level::Blocked
        } else if self.findings.is_empty() {
            Level::Healthy
        } else {
            Level::Warning
        }
    }

    pub fn of(&self, level: Level) -> Vec<&Finding> {
        self.findings.iter().filter(|f| f.level == level).collect()
    }

    /// The codes of a level, for a signature or a record.
    pub fn codes(&self, level: Level) -> Vec<String> {
        self.of(level).iter().map(|f| f.code.clone()).collect()
    }
}

/// The owner's yes: to which notice, to which risks, when, and through which
/// of the two places a person can give it (`page` or `terminal`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Consent {
    pub notice_version: u32,
    pub codes: Vec<String>,
    pub at: i64,
    pub channel: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Admission {
    Open,
    AuthorizationRequired,
    Blocked,
}

/// The warnings the consent does not cover. A consent to another notice
/// covers none.
pub fn uncovered<'a>(assessment: &'a Assessment, consent: Option<&Consent>) -> Vec<&'a Finding> {
    let covered = |code: &str| {
        consent.is_some_and(|c| {
            c.notice_version == NOTICE_VERSION && c.codes.iter().any(|have| have == code)
        })
    };
    assessment
        .of(Level::Warning)
        .into_iter()
        .filter(|f| !covered(&f.code))
        .collect()
}

/// Blocked wins over everything: no consent is read before it.
pub fn admission(assessment: &Assessment, consent: Option<&Consent>) -> Admission {
    if assessment.level() == Level::Blocked {
        Admission::Blocked
    } else if uncovered(assessment, consent).is_empty() {
        Admission::Open
    } else {
        Admission::AuthorizationRequired
    }
}

/// Which critical conditions are being held until they recover. Lives in the
/// service; a restart forgets it and starts from the plain thresholds.
pub type Latch = HashSet<&'static str>;

/// True while `bad`, and after that until `recovered`.
fn held(latch: &mut Latch, key: &'static str, bad: bool, recovered: bool) -> bool {
    if bad {
        latch.insert(key);
    } else if recovered {
        latch.remove(key);
    }
    latch.contains(key)
}

fn percent_of(total: u64, percent: u64) -> u64 {
    total / 100 * percent
}

/// `oom_times` are the Unix times of the jobs Docker confirmed were killed
/// for memory, newest last or in any order.
pub fn assess(readings: &Readings, oom_times: &[i64], now: i64, latch: &mut Latch) -> Assessment {
    let mut findings = Vec::new();
    let mut add = |code: &str, level: Level, detail: String| {
        findings.push(Finding {
            code: code.to_string(),
            level,
            detail,
        })
    };
    let inner = &readings.inner;
    let place = if readings.windows {
        "WSL2"
    } else {
        "this machine"
    };

    // What cannot be measured is not assumed to be fine.
    let missing: Vec<&str> = [
        (
            "memory",
            inner.memory_total.and(inner.memory_available).is_none(),
        ),
        ("disk", inner.disk_free.is_none()),
        ("CPU load", inner.load1.is_none() || inner.cpus.is_none()),
    ]
    .iter()
    .filter(|(_, gone)| *gone)
    .map(|(name, _)| *name)
    .collect();
    if !missing.is_empty() {
        add(
            TELEMETRY_MISSING,
            Level::Blocked,
            format!("{place}: could not read {}", missing.join(", ")),
        );
    }

    // The owner's machine: the PC on Windows, the machine itself elsewhere.
    let (owner_cpus, owner_total, owner_available, owner_name) = if readings.windows {
        let host = readings.host.unwrap_or_default();
        (
            host.cpus,
            host.memory_total,
            host.memory_available,
            "this PC",
        )
    } else {
        (
            inner.cpus,
            inner.memory_total,
            inner.memory_available,
            "this machine",
        )
    };

    if readings.windows {
        let host = readings.host;
        let gone: Vec<&str> = match host {
            None => vec!["anything"],
            Some(host) => [
                (
                    "memory",
                    host.memory_total.and(host.memory_available).is_none(),
                ),
                ("processors", host.cpus.is_none()),
                ("CPU use", host.cpu_busy_percent.is_none()),
                ("disk", host.disk_available.is_none()),
            ]
            .iter()
            .filter(|(_, gone)| *gone)
            .map(|(name, _)| *name)
            .collect(),
        };
        if !gone.is_empty() {
            add(
                HOST_UNKNOWN,
                Level::Warning,
                format!("Windows did not report {} of this PC", gone.join(", ")),
            );
        }
        // The VM's own floor. Its warning band is the per-job free memory
        // the admission already asks for.
        if let (Some(total), Some(available)) = (inner.memory_total, inner.memory_available) {
            let bad = available < percent_of(total, MEMORY_BLOCK_PERCENT).max(VM_MEMORY_FLOOR);
            let recovered =
                available >= percent_of(total, MEMORY_RECOVER_PERCENT).max(VM_MEMORY_FLOOR / 2 * 3);
            if held(latch, "vm-memory", bad, recovered) {
                add(
                    MEMORY_CRITICAL,
                    Level::Blocked,
                    format!("WSL2: {} of {} available", human(available), human(total)),
                );
            }
        }
        if let Some(busy) = host.and_then(|h| h.cpu_busy_percent) {
            if held(
                latch,
                "host-cpu",
                busy >= HOST_CPU_BLOCK,
                busy < HOST_CPU_RECOVER,
            ) {
                add(
                    CPU_CRITICAL,
                    Level::Blocked,
                    format!("this PC: processors {busy}% busy"),
                );
            } else if busy >= HOST_CPU_WARN {
                add(
                    CPU_BUSY,
                    Level::Warning,
                    format!("this PC: processors {busy}% busy"),
                );
            }
        }
        if let Some(free) = host.and_then(|h| h.disk_available) {
            if held(latch, "host-disk", free < DISK_BLOCK, free >= DISK_RECOVER) {
                add(
                    DISK_CRITICAL,
                    Level::Blocked,
                    format!("this PC: {} of disk free", human(free)),
                );
            }
        }
    }

    if let (Some(total), Some(available)) = (owner_total, owner_available) {
        let bad = available < percent_of(total, MEMORY_BLOCK_PERCENT).max(OWNER_MEMORY_FLOOR);
        let recovered =
            available >= percent_of(total, MEMORY_RECOVER_PERCENT).max(OWNER_MEMORY_FLOOR / 2 * 3);
        let detail = format!(
            "{owner_name}: {} of {} available",
            human(available),
            human(total)
        );
        if held(latch, "owner-memory", bad, recovered) {
            add(MEMORY_CRITICAL, Level::Blocked, detail);
        } else if available < percent_of(total, MEMORY_WARN_PERCENT) {
            add(MEMORY_LOW, Level::Warning, detail);
        }
    }
    let small_memory = owner_total.filter(|total| *total < SMALL_MEMORY);
    let small_cpus = owner_cpus.filter(|cpus| *cpus <= SMALL_CPUS);
    if small_memory.is_some() || small_cpus.is_some() {
        let mut parts = Vec::new();
        if let Some(cpus) = owner_cpus {
            parts.push(format!("{cpus} processors"));
        }
        if let Some(total) = owner_total {
            parts.push(format!("{} of memory", human(total)));
        }
        add(
            SMALL_MACHINE,
            Level::Warning,
            format!("{owner_name}: {}", parts.join(", ")),
        );
    }

    if let Some(free) = inner.disk_free {
        if held(latch, "inner-disk", free < DISK_BLOCK, free >= DISK_RECOVER) {
            add(
                DISK_CRITICAL,
                Level::Blocked,
                format!("{place}: {} of disk free", human(free)),
            );
        }
    }
    if let (Some(load), Some(cpus)) = (inner.load1, inner.cpus) {
        let per_cpu = load / cpus.max(1) as f64;
        let detail = format!("{place}: load {load:.2} on {cpus} processors");
        if held(
            latch,
            "inner-cpu",
            per_cpu >= LOAD_BLOCK,
            per_cpu < LOAD_RECOVER,
        ) {
            add(CPU_CRITICAL, Level::Blocked, detail);
        } else if per_cpu >= LOAD_WARN {
            add(CPU_BUSY, Level::Warning, detail);
        }
    }

    if let Some(until) = breaker_until(oom_times, now) {
        add(
            OOM_BREAKER,
            Level::Blocked,
            format!(
                "{OOM_BREAKER_COUNT} or more jobs ran out of memory in {} minutes; new jobs start again in {} s",
                OOM_BREAKER_WINDOW_SECS / 60,
                until - now
            ),
        );
    }
    Assessment { findings }
}

/// When the pause after repeated OOM kills ends, while it is on.
pub fn breaker_until(oom_times: &[i64], now: i64) -> Option<i64> {
    let recent: Vec<i64> = oom_times
        .iter()
        .copied()
        .filter(|at| *at <= now && now - *at < OOM_BREAKER_WINDOW_SECS)
        .collect();
    let last = recent.iter().copied().max()?;
    let until = last + OOM_BREAKER_PAUSE_SECS;
    (recent.len() >= OOM_BREAKER_COUNT && now < until).then_some(until)
}

/// What the status, the page and the doctor show. `applies` is false on a
/// machine that is not assessed (profile `none`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub applies: bool,
    pub level: Level,
    pub admission: Admission,
    pub notice_version: u32,
    pub reasons: Vec<Finding>,
    /// The warnings among `reasons` the owner has not agreed to.
    pub needs_authorization: Vec<String>,
    pub authorization: Option<Consent>,
    /// The newest few, newest first.
    #[serde(default)]
    pub incidents: Vec<crate::store::Incident>,
}

impl Default for Report {
    fn default() -> Self {
        Self {
            applies: false,
            level: Level::Healthy,
            admission: Admission::Open,
            notice_version: NOTICE_VERSION,
            reasons: Vec::new(),
            needs_authorization: Vec::new(),
            authorization: None,
            incidents: Vec::new(),
        }
    }
}

impl Report {
    pub fn new(assessment: &Assessment, consent: Option<Consent>) -> Self {
        Self {
            applies: true,
            level: assessment.level(),
            admission: admission(assessment, consent.as_ref()),
            notice_version: NOTICE_VERSION,
            reasons: assessment.findings.clone(),
            needs_authorization: uncovered(assessment, consent.as_ref())
                .iter()
                .map(|f| f.code.clone())
                .collect(),
            authorization: consent,
            incidents: Vec::new(),
        }
    }

    pub fn blocked(&self) -> Vec<&Finding> {
        self.reasons
            .iter()
            .filter(|f| f.level == Level::Blocked)
            .collect()
    }

    pub fn unauthorized(&self) -> Vec<&Finding> {
        self.reasons
            .iter()
            .filter(|f| self.needs_authorization.contains(&f.code))
            .collect()
    }

    /// Why no job starts, in one line. None when one may.
    pub fn waiting_for(&self) -> Option<String> {
        let list = |found: Vec<&Finding>| {
            found
                .iter()
                .map(|f| format!("{} ({})", f.code, f.detail))
                .collect::<Vec<_>>()
                .join("; ")
        };
        match self.admission {
            Admission::Open => None,
            Admission::Blocked => Some(format!(
                "blocked to protect this computer: {}",
                list(self.blocked())
            )),
            Admission::AuthorizationRequired => Some(format!(
                "the owner of this computer has to authorize: {}",
                list(self.unauthorized())
            )),
        }
    }

    /// What changes when the state changes, and not when a number moves.
    pub fn signature(&self) -> String {
        let codes: Vec<&str> = self.reasons.iter().map(|f| f.code.as_str()).collect();
        format!(
            "{:?}|{}|{}|{:?}|{}",
            self.admission,
            codes.join(","),
            self.needs_authorization.join(","),
            self.authorization.as_ref().map(|c| c.at),
            self.incidents.first().map(|i| i.at).unwrap_or(0)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantError {
    /// No warning is waiting for a yes.
    NothingToAuthorize,
    Store(String),
}

impl std::fmt::Display for GrantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NothingToAuthorize => write!(f, "nothing is waiting for an authorization"),
            Self::Store(msg) => write!(f, "{msg}"),
        }
    }
}

/// Records the owner's yes to the warnings in `codes`, on top of what the
/// same notice was already agreed to. `channel` is `page` or `terminal`: the
/// two places where a person is asked. Nothing else calls this.
pub fn grant(
    store: &crate::store::Store,
    codes: &[String],
    channel: &str,
    now: i64,
) -> Result<Consent, GrantError> {
    if codes.is_empty() {
        return Err(GrantError::NothingToAuthorize);
    }
    let store_err = |err: crate::store::StoreError| GrantError::Store(err.to_string());
    let mut all: Vec<String> = store
        .safety_consent()
        .map_err(store_err)?
        .filter(|old| old.notice_version == NOTICE_VERSION)
        .map(|old| old.codes)
        .unwrap_or_default();
    for code in codes {
        if !all.contains(code) {
            all.push(code.clone());
        }
    }
    all.sort();
    let consent = Consent {
        notice_version: NOTICE_VERSION,
        codes: all,
        at: now,
        channel: channel.to_string(),
    };
    store
        .set_safety_consent(Some(&consent))
        .map_err(store_err)?;
    let detail = format!(
        "notice v{NOTICE_VERSION}, on the {channel}: {}",
        consent.codes.join(", ")
    );
    store
        .add_incident(now, "authorization_granted", &detail)
        .map_err(store_err)?;
    tracing::info!(%detail, "the owner authorized jobs under a warning");
    Ok(consent)
}

/// Withdraws the authorization. True when there was one.
pub fn revoke(store: &crate::store::Store, channel: &str, now: i64) -> Result<bool, GrantError> {
    let store_err = |err: crate::store::StoreError| GrantError::Store(err.to_string());
    let had = store.safety_consent().map_err(store_err)?.is_some();
    store.set_safety_consent(None).map_err(store_err)?;
    if had {
        store
            .add_incident(now, "authorization_revoked", &format!("on the {channel}"))
            .map_err(store_err)?;
        tracing::info!(channel, "the owner withdrew the authorization");
    }
    Ok(had)
}

/// The notice the owner reads before saying yes. English and Portuguese.
/// A change of substance here is a new `NOTICE_VERSION`.
pub fn notice(portuguese: bool) -> &'static str {
    if portuguese {
        "Rodar tarefas usa o processador, a memória e o disco deste computador. Nas condições \
         listadas acima isso traz um risco real para quem usa o computador: programas podem ficar \
         lentos ou parar de responder, a ventoinha e o consumo de bateria podem aumentar e, no pior \
         caso, o sistema pode fechar programas ou precisar ser reiniciado, com perda de trabalho não \
         salvo.\n\n\
         O que o worker faz para limitar o risco: cada tarefa tem limite de processador e de \
         memória; nenhuma tarefa nova começa quando memória, disco ou processador estão em nível \
         crítico, e isso não pode ser liberado por ninguém; uma tarefa que estoura a memória é \
         encerrada pelo próprio limite e, se isso se repetir, as tarefas novas fazem uma pausa; \
         tarefas em execução não são interrompidas por causa disso. Você pode pausar o worker ou \
         retirar esta autorização a qualquer momento nesta página.\n\n\
         Essas proteções reduzem o risco, mas não o eliminam. A autorização vale para as condições \
         listadas; se aparecer uma condição nova, o worker pergunta de novo. Autorizar não muda as \
         responsabilidades que o fornecedor deste programa ou do serviço tem com você por lei ou \
         por contrato."
    } else {
        "Running jobs uses this computer's processor, memory and disk. Under the conditions \
         listed above that is a real risk for whoever uses the computer: programs may get slow or \
         stop responding, the fan and the battery use may go up and, in the worst case, the system \
         may close programs or need a restart, and unsaved work may be lost.\n\n\
         What the worker does to limit the risk: every job has a processor and a memory limit; no \
         new job starts while memory, disk or processor are at a critical level, and nobody can \
         override that; a job that runs out of memory is stopped by its own limit and, if that \
         repeats, new jobs pause; running jobs are not stopped because of it. You can pause the \
         worker or withdraw this authorization at any time on this page.\n\n\
         These safeguards reduce the risk. They do not remove it. The authorization covers the \
         conditions listed; if a new one appears, the worker asks again. Authorizing does not \
         change the responsibilities that the provider of this program or of the service has \
         toward you by law or by contract."
    }
}

/// One line a person understands, per code. The numbers are in the detail.
pub fn label(code: &str, portuguese: bool) -> &'static str {
    let (en, pt) = match code {
        TELEMETRY_MISSING => (
            "The worker cannot measure this machine, so it starts nothing",
            "O worker não consegue medir esta máquina, então não inicia nada",
        ),
        MEMORY_CRITICAL => (
            "Memory is critically low",
            "A memória está em nível crítico",
        ),
        DISK_CRITICAL => (
            "Disk space is critically low",
            "O espaço em disco está em nível crítico",
        ),
        CPU_CRITICAL => ("The processor is saturated", "O processador está saturado"),
        OOM_BREAKER => (
            "Jobs ran out of memory repeatedly: new jobs are paused for a while",
            "Tarefas estouraram a memória repetidamente: tarefas novas em pausa por um tempo",
        ),
        SMALL_MACHINE => (
            "This is a small computer: a job takes a visible share of it",
            "Este é um computador pequeno: uma tarefa ocupa uma parte visível dele",
        ),
        MEMORY_LOW => ("Memory is low", "A memória está baixa"),
        CPU_BUSY => ("The processor is busy", "O processador está ocupado"),
        HOST_UNKNOWN => (
            "Windows is not reporting this PC's memory, processor or disk",
            "O Windows não está informando memória, processador ou disco deste PC",
        ),
        _ => ("Unknown condition", "Condição desconhecida"),
    };
    if portuguese {
        pt
    } else {
        en
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(readings: &Readings) -> Assessment {
        assess(readings, &[], 1_000_000, &mut Latch::new())
    }

    fn codes(assessment: &Assessment) -> Vec<(&str, Level)> {
        assessment
            .findings
            .iter()
            .map(|f| (f.code.as_str(), f.level))
            .collect()
    }

    fn windows_pc(cpus: u64, total: u64, available: u64) -> Readings {
        Readings {
            windows: true,
            inner: Inner {
                cpus: Some(cpus),
                memory_total: Some(2 * GIB),
                memory_available: Some(GIB + GIB / 2),
                disk_free: Some(100 * GIB),
                load1: Some(0.1),
            },
            host: Some(Feed {
                idle_secs: Some(600),
                memory_total: Some(total),
                memory_available: Some(available),
                cpus: Some(cpus),
                cpu_busy_percent: Some(10),
                disk_available: Some(80 * GIB),
            }),
        }
    }

    #[test]
    fn a_roomy_machine_is_healthy_and_needs_no_consent() {
        let found = run(&Readings::healthy());
        assert_eq!(found.level(), Level::Healthy);
        assert_eq!(admission(&found, None), Admission::Open);
        let found = run(&windows_pc(16, 16 * GIB, 9 * GIB));
        assert_eq!(codes(&found), vec![]);
    }

    #[test]
    fn a_reading_that_is_missing_blocks_and_is_never_healthy() {
        for broken in [
            Inner {
                memory_available: None,
                ..Readings::healthy().inner
            },
            Inner {
                disk_free: None,
                ..Readings::healthy().inner
            },
            Inner {
                load1: None,
                ..Readings::healthy().inner
            },
        ] {
            let found = run(&Readings {
                inner: broken,
                ..Readings::healthy()
            });
            assert_eq!(
                codes(&found),
                vec![(TELEMETRY_MISSING, Level::Blocked)],
                "{broken:?}"
            );
        }
        let all = run(&Readings::default());
        assert!(all.findings[0].detail.contains("memory, disk, CPU load"));
    }

    #[test]
    fn a_windows_pc_that_says_nothing_about_itself_needs_the_owner() {
        let mut silent = windows_pc(16, 16 * GIB, 9 * GIB);
        silent.host = None;
        let found = run(&silent);
        assert_eq!(codes(&found), vec![(HOST_UNKNOWN, Level::Warning)]);
        assert_eq!(admission(&found, None), Admission::AuthorizationRequired);

        // An older supervisor: memory, and no CPU use or disk.
        let mut partial = windows_pc(16, 16 * GIB, 9 * GIB);
        partial.host = Some(Feed {
            cpu_busy_percent: None,
            disk_available: None,
            ..partial.host.unwrap()
        });
        let found = run(&partial);
        assert_eq!(codes(&found), vec![(HOST_UNKNOWN, Level::Warning)]);
        assert!(found.findings[0].detail.contains("CPU use, disk"));
    }

    #[test]
    fn the_windows_pc_and_the_vm_are_judged_apart() {
        // The VM has room; the PC itself does not.
        let found = run(&windows_pc(8, 16 * GIB, GIB));
        assert_eq!(codes(&found), vec![(MEMORY_CRITICAL, Level::Blocked)]);
        assert!(found.findings[0].detail.starts_with("this PC:"));
        // The PC has room; the VM is full.
        let mut vm_full = windows_pc(8, 16 * GIB, 9 * GIB);
        vm_full.inner.memory_available = Some(100 * MIB);
        let found = run(&vm_full);
        assert_eq!(codes(&found), vec![(MEMORY_CRITICAL, Level::Blocked)]);
        assert!(found.findings[0].detail.starts_with("WSL2:"));
        // Between 10% and 20% of the PC: a warning.
        let found = run(&windows_pc(8, 16 * GIB, 3 * GIB));
        assert_eq!(codes(&found), vec![(MEMORY_LOW, Level::Warning)]);
    }

    #[test]
    fn two_processors_and_five_gigabytes_is_a_small_machine_until_measured() {
        let found = run(&windows_pc(2, 5 * GIB, 2 * GIB));
        assert_eq!(codes(&found), vec![(SMALL_MACHINE, Level::Warning)]);
        assert_eq!(admission(&found, None), Admission::AuthorizationRequired);
        // The same PC with under a gigabyte left is blocked outright.
        let found = run(&windows_pc(2, 5 * GIB, 900 * MIB));
        assert_eq!(found.level(), Level::Blocked);
    }

    #[test]
    fn cpu_and_disk_have_a_warning_and_a_critical_line() {
        let with = |load: f64, disk: u64| {
            run(&Readings {
                inner: Inner {
                    load1: Some(load),
                    disk_free: Some(disk),
                    ..Readings::healthy().inner
                },
                ..Readings::healthy()
            })
        };
        assert_eq!(codes(&with(7.9, 100 * GIB)), vec![]);
        assert_eq!(
            codes(&with(8.0, 100 * GIB)),
            vec![(CPU_BUSY, Level::Warning)]
        );
        assert_eq!(
            codes(&with(16.0, 100 * GIB)),
            vec![(CPU_CRITICAL, Level::Blocked)]
        );
        assert_eq!(
            codes(&with(0.1, GIB)),
            vec![(DISK_CRITICAL, Level::Blocked)]
        );

        let mut busy = windows_pc(8, 16 * GIB, 9 * GIB);
        busy.host.as_mut().unwrap().cpu_busy_percent = Some(80);
        assert_eq!(codes(&run(&busy)), vec![(CPU_BUSY, Level::Warning)]);
        busy.host.as_mut().unwrap().cpu_busy_percent = Some(97);
        assert_eq!(codes(&run(&busy)), vec![(CPU_CRITICAL, Level::Blocked)]);
        busy.host.as_mut().unwrap().cpu_busy_percent = Some(10);
        busy.host.as_mut().unwrap().disk_available = Some(GIB);
        assert_eq!(codes(&run(&busy)), vec![(DISK_CRITICAL, Level::Blocked)]);
    }

    #[test]
    fn a_critical_condition_clears_only_past_its_recovery_line() {
        let mut latch = Latch::new();
        let mut at = |available: u64, load: f64| {
            let readings = Readings {
                inner: Inner {
                    memory_available: Some(available),
                    load1: Some(load),
                    ..Readings::healthy().inner
                },
                ..Readings::healthy()
            };
            assess(&readings, &[], 0, &mut latch).level()
        };
        // 16 GiB: blocked under 1.6 GiB, recovers at 2.4 GiB.
        assert_eq!(at(4 * GIB, 0.1), Level::Healthy);
        assert_eq!(at(GIB, 0.1), Level::Blocked);
        assert_eq!(at(2 * GIB, 0.1), Level::Blocked, "still under recovery");
        assert_eq!(at(4 * GIB, 0.1), Level::Healthy);
        // 8 processors: blocked from load 16, recovers under 12.
        assert_eq!(at(4 * GIB, 16.0), Level::Blocked);
        assert_eq!(at(4 * GIB, 13.0), Level::Blocked);
        assert_eq!(at(4 * GIB, 11.0), Level::Warning);
        assert_eq!(at(4 * GIB, 1.0), Level::Healthy);
    }

    #[test]
    fn consent_covers_the_risks_it_named_and_never_a_block() {
        let small = run(&windows_pc(2, 5 * GIB, 2 * GIB));
        let yes = |codes: &[&str], version: u32| Consent {
            notice_version: version,
            codes: codes.iter().map(|c| c.to_string()).collect(),
            at: 1,
            channel: "page".into(),
        };
        assert_eq!(
            admission(&small, Some(&yes(&[SMALL_MACHINE], NOTICE_VERSION))),
            Admission::Open
        );
        // Another notice, or a risk the owner was not shown: asked again.
        assert_eq!(
            admission(&small, Some(&yes(&[SMALL_MACHINE], NOTICE_VERSION + 1))),
            Admission::AuthorizationRequired
        );
        let mut busier = windows_pc(2, 5 * GIB, 2 * GIB);
        busier.host.as_mut().unwrap().cpu_busy_percent = Some(80);
        let busier = run(&busier);
        let given = yes(&[SMALL_MACHINE], NOTICE_VERSION);
        assert_eq!(
            admission(&busier, Some(&given)),
            Admission::AuthorizationRequired
        );
        assert_eq!(
            uncovered(&busier, Some(&given))
                .iter()
                .map(|f| f.code.as_str())
                .collect::<Vec<_>>(),
            vec![CPU_BUSY]
        );
        // Every code there is, agreed to: a block is still a block.
        let everything = yes(
            &[
                SMALL_MACHINE,
                MEMORY_LOW,
                CPU_BUSY,
                HOST_UNKNOWN,
                MEMORY_CRITICAL,
                TELEMETRY_MISSING,
                OOM_BREAKER,
            ],
            NOTICE_VERSION,
        );
        let blocked = run(&windows_pc(2, 5 * GIB, 500 * MIB));
        assert_eq!(admission(&blocked, Some(&everything)), Admission::Blocked);
        assert_eq!(
            admission(&run(&Readings::default()), Some(&everything)),
            Admission::Blocked
        );
    }

    #[test]
    fn repeated_oom_kills_pause_new_jobs_and_the_pause_ends() {
        let now = 100_000;
        let healthy = Readings::healthy();
        let at = |ooms: &[i64], now: i64| assess(&healthy, ooms, now, &mut Latch::new());
        assert_eq!(at(&[now - 10, now - 20], now).level(), Level::Healthy);
        let open = at(&[now - 10, now - 20, now - 30], now);
        assert_eq!(codes(&open), vec![(OOM_BREAKER, Level::Blocked)]);
        assert_eq!(
            breaker_until(&[now - 10, now - 20, now - 30], now),
            Some(now + 890)
        );
        // Fifteen minutes after the last one, jobs start again.
        assert_eq!(
            at(&[now - 10, now - 20, now - 30], now + 900).level(),
            Level::Healthy
        );
        // Three spread over more than the window never trip it.
        assert_eq!(
            at(&[now - 10, now - 1000, now - 2000], now).level(),
            Level::Healthy
        );
    }

    #[test]
    fn the_load_average_is_read_and_the_notice_does_not_promise_too_much() {
        assert_eq!(load1_of("0.52 0.40 0.30 1/200 999\n"), Some(0.52));
        assert_eq!(load1_of(""), None);
        assert_eq!(load1_of("nan 0 0"), None);
        #[cfg(target_os = "linux")]
        {
            let real = Readings::read(false, std::path::Path::new("/"));
            assert!(real.inner.load1.is_some() && real.inner.memory_available.is_some());
        }
        for portuguese in [false, true] {
            let text = notice(portuguese);
            assert!(text.contains(if portuguese {
                "não o eliminam"
            } else {
                "do not remove it"
            }));
            assert!(text.contains(if portuguese { "por lei" } else { "by law" }));
        }
        for code in [
            TELEMETRY_MISSING,
            MEMORY_CRITICAL,
            DISK_CRITICAL,
            CPU_CRITICAL,
            OOM_BREAKER,
            SMALL_MACHINE,
            MEMORY_LOW,
            CPU_BUSY,
            HOST_UNKNOWN,
        ] {
            assert_ne!(label(code, false), "Unknown condition", "{code}");
            assert_ne!(label(code, true), "Condição desconhecida", "{code}");
        }
    }
}
