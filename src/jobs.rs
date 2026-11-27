//! `shutup-and-run jobs`: the last jobs of this machine, from the history
//! file. What ran, when, for how long and how it ended. Never the env, the
//! command or anything else a job was given. `jobs logs` prints what a job
//! printed, from its log file (`src/joblogs.rs`).

use crate::cli::{emit, Out, EXIT_FAILED, EXIT_USAGE};
use crate::store::{JobRecord, Store};
use crate::{REASON_INTERRUPTED, REASON_LAUNCHER, REASON_START_FAILED, REASON_TIMEOUT};
use serde_json::{json, Value};
use std::path::Path;
use std::process::ExitCode;

pub const DEFAULT_LIMIT: usize = 20;
pub const MAX_LIMIT: usize = 200;

pub fn usage() -> &'static str {
    "\
shutup-and-run jobs [--limit N] [--json]
    the last N jobs of this machine, newest first (default 20, at most 200):
    id, image, start, end, duration, exit code, status and, when it did not
    end well, why. Status is running, ok, failed, timeout, killed or
    interrupted (the PC, WSL or Docker stopped under the job). A job that
    Docker confirmed was killed for memory says so (oom_killed). `log` is
    where its output is kept, and the command that prints it. The history
    holds at most the last 500 finished jobs, and never a job's env,
    command or payload.

shutup-and-run jobs --summary [--limit N] [--json]
    what the last N finished jobs used (default 20): the highest and the
    average memory peak, and how many processors they kept busy. With 5
    jobs measured or more, and room on this machine, it names the
    max_job_memory (twice the highest peak) and max_slots that would fit
    more jobs at once. It changes nothing.

shutup-and-run jobs logs ID|--last [--tail N] [--json]
    what a job printed (stdout and stderr): its last 256 KiB, kept for 7
    days in the logs folder next to the history. ID may be the start of an
    id. --last is the most recent job that ended. A job still running
    prints what it printed so far, read from Docker now (live: true in
    --json). --tail N prints only the last N lines. Exit 1 with
    JOB_NOT_FOUND, JOB_ID_AMBIGUOUS or JOB_LOG_MISSING."
}

/// `running`, `ok`, `failed`, `timeout`, `killed` or `interrupted`.
pub fn status_of(job: &JobRecord) -> &'static str {
    match (job.ended_at, job.reason.as_deref(), job.exit_code) {
        (None, _, _) => "running",
        (_, Some(REASON_TIMEOUT), _) => "timeout",
        (_, Some(REASON_LAUNCHER), _) => "killed",
        (_, Some(REASON_START_FAILED), _) => "failed",
        (_, Some(REASON_INTERRUPTED), _) => "interrupted",
        // SIGKILL from outside: the kernel out of memory, or `docker kill`.
        (_, _, Some(137)) => "killed",
        (_, _, Some(0)) => "ok",
        _ => "failed",
    }
}

/// Why a job did not end well, in words this program wrote. Null when it
/// ended well or still runs.
pub fn failure_of(job: &JobRecord) -> Option<String> {
    match status_of(job) {
        "timeout" => Some("ran past its time limit and was stopped".into()),
        "interrupted" => Some(
            "the PC, WSL or Docker stopped while it ran, or the worker was not running when it ended"
                .into(),
        ),
        "killed" if job.reason.as_deref() == Some(REASON_LAUNCHER) => {
            Some("stopped by the worker".into())
        }
        _ if job.oom_killed => Some(
            "ran out of memory: it reached its memory limit and the kernel killed it (confirmed by Docker)"
                .into(),
        ),
        "killed" => Some(
            "killed from outside (exit 137), and not for memory as far as Docker recorded: stopped by hand, or by the system"
                .into(),
        ),
        "failed" if job.reason.as_deref() == Some(REASON_START_FAILED) => {
            // The launcher's own fixed sentence, not container output.
            Some(format!(
                "did not start: {}",
                job.logs
                    .as_deref()
                    .filter(|m| !m.is_empty())
                    .unwrap_or("unknown")
            ))
        }
        "failed" => Some(match job.exit_code {
            Some(code) => format!("the container exited with code {code}"),
            None => "the container ended with no exit code".into(),
        }),
        _ => None,
    }
}

fn utc(unix: i64) -> String {
    crate::status::unix_to_iso(unix)
}

pub fn job_json(job: &JobRecord) -> Value {
    json!({
        "id": job.id,
        "image": job.image,
        "started_at": utc(job.started_at),
        "ended_at": job.ended_at.map(utc),
        "duration_secs": job.duration_secs(),
        "exit_code": job.exit_code,
        "status": status_of(job),
        "failure": failure_of(job),
        "oom_killed": job.oom_killed,
        "cpu_ms": job.cpu_ms,
        "memory_peak_bytes": job.memory_peak,
        "network_bytes": job.network_bytes,
        "disk_bytes": job.disk_bytes,
        "pull": pulling(job).map(|(done, total)| json!({"done_bytes": done, "total_bytes": total})),
    })
}

/// While a job's image downloads: the bytes so far and the size known so far.
pub fn pulling(job: &JobRecord) -> Option<(u64, u64)> {
    job.pull.filter(|_| !job.is_finished())
}

/// `downloading the image: 120 MiB of 450 MiB`, while it does.
pub fn pull_text(job: &JobRecord, portuguese: bool) -> Option<String> {
    let (done, total) = pulling(job)?;
    let (what, of) = if portuguese {
        ("baixando a imagem", "de")
    } else {
        ("downloading the image", "of")
    };
    Some(format!(
        "{what}: {} {of} {}",
        memory_text(done),
        memory_text(total)
    ))
}

/// `cpu 5.5 s, memory peak 300 MiB, network 12 MiB, disk 700 MiB`, from
/// what the readings saw (the disk is read at the end); `so far`
/// while the job runs. None when the job was never read (it ran under 30
/// seconds, or it has run less than that).
pub fn usage_text(job: &JobRecord) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(ms) = job.cpu_ms {
        parts.push(format!("cpu {:.1} s", ms as f64 / 1000.0));
    }
    if let Some(bytes) = job.memory_peak {
        parts.push(format!("memory peak {}", memory_text(bytes)));
    }
    if let Some(bytes) = job.network_bytes {
        parts.push(format!("network {}", memory_text(bytes)));
    }
    if let Some(bytes) = job.disk_bytes {
        parts.push(format!("disk {}", memory_text(bytes)));
    }
    let so_far = if job.ended_at.is_none() {
        " so far"
    } else {
        ""
    };
    (!parts.is_empty()).then(|| format!("{}{so_far}", parts.join(", ")))
}

/// What the last finished jobs used, from the readings kept in their rows.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Summary {
    /// Finished jobs looked at.
    pub jobs: usize,
    /// Of those, the ones with a memory reading: a job shorter than the 30
    /// seconds between readings may have none.
    pub measured: usize,
    /// Processors in use on average over a job's run, as `cpu_ms` over its
    /// duration: the mean of the jobs, and the most one used.
    pub cpus_mean: Option<f64>,
    pub cpus_max: Option<f64>,
    pub memory_peak_mean: Option<u64>,
    pub memory_peak_max: Option<u64>,
}

/// Fewer measured jobs than this make no suggestion.
pub const MIN_MEASURED: usize = 5;

pub fn summary(jobs: &[JobRecord]) -> Summary {
    let finished: Vec<&JobRecord> = jobs.iter().filter(|j| j.is_finished()).collect();
    let peaks: Vec<u64> = finished.iter().filter_map(|j| j.memory_peak).collect();
    let cpus: Vec<f64> = finished
        .iter()
        .filter_map(|j| {
            let secs = j.duration_secs().filter(|s| *s > 0)?;
            Some(j.cpu_ms? as f64 / (secs as f64 * 1000.0))
        })
        .collect();
    Summary {
        jobs: finished.len(),
        measured: peaks.len(),
        cpus_mean: (!cpus.is_empty()).then(|| cpus.iter().sum::<f64>() / cpus.len() as f64),
        cpus_max: cpus.iter().copied().reduce(f64::max),
        memory_peak_mean: (!peaks.is_empty())
            .then(|| peaks.iter().sum::<u64>() / peaks.len() as u64),
        memory_peak_max: peaks.iter().copied().max(),
    }
}

/// What the jobs of one image used, from the same readings.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ImageSummary {
    pub image: String,
    pub jobs: usize,
    pub measured: usize,
    pub memory_peak_max: Option<u64>,
    pub memory_peak_mean: Option<u64>,
    /// The `memory` to ask in a request for this image: twice its highest
    /// peak, rounded up to 256 MiB. Only from `MIN_MEASURED_PER_IMAGE`.
    pub memory_to_ask: Option<u64>,
    /// What one job of it received and sent on the network, on average:
    /// what it costs a PC on a metered plan. Its pull is not in it.
    pub network_bytes_mean: Option<u64>,
    /// What one job of it wrote inside its container, on average.
    pub disk_bytes_mean: Option<u64>,
}

/// Fewer measured jobs of one image than this make no suggestion for it.
pub const MIN_MEASURED_PER_IMAGE: usize = 3;

/// One summary per image, most jobs first: a `max_job_memory` fits the
/// heaviest, and each request can ask for what its own image needs.
pub fn by_image(jobs: &[JobRecord]) -> Vec<ImageSummary> {
    let mut images: Vec<ImageSummary> = Vec::new();
    for job in jobs.iter().filter(|j| j.is_finished()) {
        let at = match images.iter().position(|i| i.image == job.image) {
            Some(at) => at,
            None => {
                images.push(ImageSummary {
                    image: job.image.clone(),
                    jobs: 0,
                    measured: 0,
                    memory_peak_max: None,
                    memory_peak_mean: None,
                    memory_to_ask: None,
                    network_bytes_mean: None,
                    disk_bytes_mean: None,
                });
                images.len() - 1
            }
        };
        images[at].jobs += 1;
    }
    for image in &mut images {
        let peaks: Vec<u64> = jobs
            .iter()
            .filter(|j| j.is_finished() && j.image == image.image)
            .filter_map(|j| j.memory_peak)
            .collect();
        image.measured = peaks.len();
        image.memory_peak_max = peaks.iter().copied().max();
        image.memory_peak_mean =
            (!peaks.is_empty()).then(|| peaks.iter().sum::<u64>() / peaks.len() as u64);
        image.memory_to_ask = image
            .memory_peak_max
            .filter(|_| image.measured >= MIN_MEASURED_PER_IMAGE)
            .map(twice_rounded);
        let mean = |of: fn(&JobRecord) -> Option<u64>| {
            let values: Vec<u64> = jobs
                .iter()
                .filter(|j| j.is_finished() && j.image == image.image)
                .filter_map(of)
                .collect();
            (!values.is_empty()).then(|| values.iter().sum::<u64>() / values.len() as u64)
        };
        image.network_bytes_mean = mean(|j| j.network_bytes);
        image.disk_bytes_mean = mean(|j| j.disk_bytes);
    }
    images.sort_by(|a, b| b.jobs.cmp(&a.jobs).then_with(|| a.image.cmp(&b.image)));
    images
}

/// `  alpine: 6 jobs, 5 measured, memory peak 380 MiB at most (210 MiB on
/// average), network 40 MiB a job, disk 1.2 GiB a job; ask for memory 768m`.
pub fn image_text(image: &ImageSummary) -> String {
    let mut text = format!(
        "  {}: {} jobs, {} measured",
        image.image, image.jobs, image.measured
    );
    if let (Some(max), Some(mean)) = (image.memory_peak_max, image.memory_peak_mean) {
        text.push_str(&format!(
            ", memory peak {} at most ({} on average)",
            memory_text(max),
            memory_text(mean)
        ));
    }
    if let Some(bytes) = image.network_bytes_mean {
        text.push_str(&format!(", network {} a job", memory_text(bytes)));
    }
    if let Some(bytes) = image.disk_bytes_mean {
        text.push_str(&format!(", disk {} a job", memory_text(bytes)));
    }
    if let Some(ask) = image.memory_to_ask {
        text.push_str(&format!("; ask for memory {}", size_value(ask)));
    }
    text
}

/// Twice a peak, rounded up to 256 MiB: room for a peak the readings
/// every 30 seconds missed.
fn twice_rounded(peak: u64) -> u64 {
    let step = 256 * MIB;
    (peak * 2).div_ceil(step).max(1) * step
}

/// A `max_job_memory` that fits what the jobs used, and how many slots it
/// and `max_job_cpus` leave room for on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Capacity {
    pub max_job_memory: u64,
    pub max_slots: u64,
}

const MIB: u64 = 1024 * 1024;

/// Twice the highest peak, rounded up to 256 MiB, and never above the limit
/// in force; the slots are what fits in the machine's memory and
/// processors at that size. None without enough readings, without a
/// memory limit, or when that is no more slots than there are now.
pub fn capacity(
    summary: &Summary,
    max_job_memory: Option<u64>,
    max_job_cpus: Option<f64>,
    max_slots: usize,
    memory_total: Option<u64>,
    cpus: usize,
) -> Option<Capacity> {
    if summary.measured < MIN_MEASURED {
        return None;
    }
    let limit = max_job_memory?;
    let memory = twice_rounded(summary.memory_peak_max?).min(limit);
    let mut slots = memory_total? / memory;
    if let Some(per_job) = max_job_cpus.filter(|c| *c > 0.0) {
        slots = slots.min((cpus as f64 / per_job).floor() as u64);
    }
    let slots = slots.min(256);
    (slots > max_slots as u64).then_some(Capacity {
        max_job_memory: memory,
        max_slots: slots,
    })
}

/// A size as `config set` takes it: `768m`, `2g`.
pub fn size_value(bytes: u64) -> String {
    let mib = bytes / MIB;
    if mib.is_multiple_of(1024) {
        format!("{}g", mib / 1024)
    } else {
        format!("{mib}m")
    }
}

/// One line on what the jobs used: `12 jobs, 10 measured: memory peak
/// 380 MiB at most (210 MiB on average); 0.90 of a processor at most (0.40
/// on average)`.
pub fn summary_text(summary: &Summary) -> String {
    let mut text = format!("{} jobs, {} measured", summary.jobs, summary.measured);
    if let (Some(max), Some(mean)) = (summary.memory_peak_max, summary.memory_peak_mean) {
        text.push_str(&format!(
            ": memory peak {} at most ({} on average)",
            memory_text(max),
            memory_text(mean)
        ));
    }
    if let (Some(max), Some(mean)) = (summary.cpus_max, summary.cpus_mean) {
        text.push_str(&format!(
            "; {max:.2} of a processor at most ({mean:.2} on average)"
        ));
    }
    text
}

/// A job's memory peak: `300 MiB`, `1.5 GiB`, and `< 1 MiB` for a job that
/// held almost nothing, which `0 MiB` would misstate.
pub fn memory_text(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        "< 1 MiB".to_string()
    } else {
        crate::resources::human(bytes)
    }
}

/// The command that prints the log of job `id`.
pub fn log_command(id: &str) -> String {
    format!("shutup-and-run jobs logs {id}")
}

/// Where a job's log is, or null when it has none (it never started a
/// container, it still runs, or its log was deleted after 7 days).
pub fn log_json(history: &Path, id: &str) -> Value {
    match crate::joblogs::info(history, id) {
        Some(info) => json!({
            "path": info.path.display().to_string(),
            "bytes": info.bytes,
            "command": log_command(id),
        }),
        None => Value::Null,
    }
}

pub fn jobs_out(history: &Path, limit: usize) -> Out {
    if !history.exists() {
        return Out::ok(
            json!({"jobs": [], "limit": limit}),
            "no jobs yet: the service has not run on this machine",
        );
    }
    let jobs = match Store::open(history).and_then(|store| store.recent(limit)) {
        Ok(jobs) => jobs,
        Err(err) => return Out::fail(EXIT_FAILED, format!("{}: {err}", history.display())),
    };
    let mut text = String::new();
    for job in &jobs {
        let has_log = crate::joblogs::info(history, &job.id).is_some();
        text.push_str(&format!(
            "{}  {:<8} {:>6}  exit {:<4} {}  {}{}{}{}\n",
            utc(job.started_at),
            status_of(job),
            job.duration_secs()
                .map(|s| format!("{s}s"))
                .unwrap_or_else(|| "-".into()),
            job.exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".into()),
            job.id,
            job.image,
            pull_text(job, false)
                .or_else(|| usage_text(job).map(|used| format!("used: {used}")))
                .map(|line| format!("\n    {line}"))
                .unwrap_or_default(),
            failure_of(job)
                .map(|why| format!("\n    why: {why}"))
                .unwrap_or_default(),
            if has_log && failure_of(job).is_some() {
                format!("\n    log: {}", log_command(&job.id))
            } else {
                String::new()
            },
        ));
    }
    if jobs.is_empty() {
        text.push_str("no jobs yet");
    }
    let list: Vec<Value> = jobs
        .iter()
        .map(|job| {
            let mut value = job_json(job);
            value["log"] = log_json(history, &job.id);
            value
        })
        .collect();
    Out::ok(json!({"jobs": list, "limit": limit}), text.trim_end())
}

/// Which job `wanted` names: a whole id or the start of one, among the
/// jobs of the history and the logs on disk. `None` is `--last`.
fn resolve(history: &Path, store: Option<&Store>, wanted: Option<&str>) -> Result<String, Out> {
    let known: Vec<JobRecord> = store
        .and_then(|s| s.recent(crate::launcher::MAX_HISTORY_ROWS).ok())
        .unwrap_or_default();
    let Some(wanted) = wanted else {
        return known
            .iter()
            .find(|job| job.is_finished())
            .map(|job| job.id.clone())
            .ok_or_else(|| {
                coded(
                    "JOB_NOT_FOUND",
                    "no job has ended on this machine yet".into(),
                )
            });
    };
    let mut ids: Vec<String> = known.into_iter().map(|job| job.id).collect();
    ids.extend(crate::joblogs::ids(history));
    ids.sort();
    ids.dedup();
    if ids.iter().any(|id| id == wanted) {
        return Ok(wanted.to_string());
    }
    let matches: Vec<&String> = ids.iter().filter(|id| id.starts_with(wanted)).collect();
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(coded(
            "JOB_NOT_FOUND",
            format!("no job {wanted} on this machine"),
        )),
        many => Err(coded(
            "JOB_ID_AMBIGUOUS",
            format!(
                "{wanted} is the start of {} jobs: {}",
                many.len(),
                many.iter()
                    .take(5)
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

fn coded(code: &str, error: String) -> Out {
    let mut out = Out::fail(EXIT_FAILED, error);
    out.json["code"] = json!(code);
    out
}

/// What a running job printed so far, read from Docker now: its id in,
/// the end of its output out.
pub type Live<'a> = &'a dyn Fn(&str) -> Result<Vec<u8>, String>;

/// `jobs logs ID|--last [--tail N]`. A job that is still running has no
/// file yet: what it printed so far comes from `live`.
pub fn logs_out(history: &Path, wanted: Option<&str>, tail: Option<usize>, live: Live) -> Out {
    let store = if history.exists() {
        Store::open(history).ok()
    } else {
        None
    };
    let id = match resolve(history, store.as_ref(), wanted) {
        Ok(id) => id,
        Err(out) => return out,
    };
    let job = store.as_ref().and_then(|s| s.get(&id).ok().flatten());
    let info = crate::joblogs::info(history, &id);
    if let (None, Some(running)) = (&info, job.as_ref().filter(|j| !j.is_finished())) {
        return live_out(running, tail, live);
    }
    let Some(info) = info else {
        let why = match &job {
            Some(job) if job.reason.as_deref() == Some(REASON_START_FAILED) => {
                "it never started a container, so it printed nothing"
            }
            _ => "it was deleted after 7 days, or the job printed nothing the worker could read",
        };
        return coded("JOB_LOG_MISSING", format!("job {id} has no log: {why}"));
    };
    let bytes = match std::fs::read(&info.path) {
        Ok(bytes) => bytes,
        Err(err) => return Out::fail(EXIT_FAILED, format!("{}: {err}", info.path.display())),
    };
    let text = String::from_utf8_lossy(&bytes);
    let shown = match tail {
        Some(n) => crate::joblogs::tail_lines(&text, n),
        None => &text,
    };
    let mut value = json!({
        "id": id,
        "path": info.path.display().to_string(),
        "bytes": info.bytes,
        "written_at": utc(info.written_at),
        "live": false,
        "log": shown,
    });
    if let Some(job) = &job {
        value["image"] = json!(job.image);
        value["status"] = json!(status_of(job));
        value["exit_code"] = json!(job.exit_code);
    }
    Out::ok(value, shown.strip_suffix('\n').unwrap_or(shown))
}

/// A running job's output so far, from Docker; its log file is written
/// when it ends.
fn live_out(job: &JobRecord, tail: Option<usize>, live: Live) -> Out {
    let bytes = match live(&job.id) {
        Ok(bytes) => bytes,
        Err(err) => {
            return coded(
                "JOB_LOG_MISSING",
                format!(
                    "job {} is running and Docker did not give what it printed so far: {err}",
                    job.id
                ),
            )
        }
    };
    let text = String::from_utf8_lossy(&bytes);
    let shown = match tail {
        Some(n) => crate::joblogs::tail_lines(&text, n),
        None => &text,
    };
    let value = json!({
        "id": job.id,
        "path": null,
        "bytes": bytes.len(),
        "written_at": null,
        "live": true,
        "log": shown,
        "image": job.image,
        "status": status_of(job),
        "exit_code": null,
    });
    Out::ok(value, shown.strip_suffix('\n').unwrap_or(shown))
}

/// The end of a running container's output, asked of Docker on a thread of
/// its own: the CLI may already be inside a runtime.
fn docker_output(id: &str) -> Result<Vec<u8>, String> {
    use crate::runtime::Runtime;
    let id = id.to_string();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| err.to_string())?;
        runtime.block_on(async {
            let docker = crate::bollard_rt::connect_docker()?;
            let docker = crate::bollard_rt::BollardRuntime::new(docker, None);
            tokio::time::timeout(
                std::time::Duration::from_secs(20),
                docker.output(&id, crate::joblogs::MAX_BYTES),
            )
            .await
            .map_err(|_| "no answer in 20 s".to_string())?
            .map_err(|err| err.to_string())
        })
    })
    .join()
    .unwrap_or_else(|_| Err("the thread that asked Docker failed".into()))
}

fn logs_cli(args: &[String], as_json: bool) -> ExitCode {
    let mut wanted: Option<&str> = None;
    let mut last = false;
    let mut tail = None;
    let mut words = args.iter().filter(|a| *a != "--json");
    while let Some(word) = words.next() {
        match word.as_str() {
            "--last" => last = true,
            "--tail" => match words.next().and_then(|n| n.parse::<usize>().ok()) {
                Some(n) if n >= 1 => tail = Some(n),
                _ => {
                    return emit(
                        Out::fail(EXIT_USAGE, "--tail takes a number of lines, 1 or more"),
                        as_json,
                    )
                }
            },
            "-h" | "--help" => {
                println!("{}", usage());
                return ExitCode::SUCCESS;
            }
            other if !other.starts_with('-') && wanted.is_none() => wanted = Some(other),
            other => {
                return emit(
                    Out::fail(EXIT_USAGE, format!("unknown option {other}. {}", usage())),
                    as_json,
                )
            }
        }
    }
    if last == wanted.is_some() {
        return emit(
            Out::fail(
                EXIT_USAGE,
                "name one job: `jobs logs ID` or `jobs logs --last`",
            ),
            as_json,
        );
    }
    let out = match crate::config::Settings::history_path() {
        Ok(path) => logs_out(&path, wanted, tail, &docker_output),
        Err(err) => Out::fail(EXIT_FAILED, err.0),
    };
    emit(out, as_json)
}

pub fn jobs_cli(args: &[String]) -> ExitCode {
    let as_json = args.iter().any(|a| a == "--json");
    if let Some(at) = args.iter().position(|a| a != "--json") {
        if args[at] == "logs" {
            let mut rest = args.to_vec();
            rest.remove(at);
            return logs_cli(&rest, as_json);
        }
    }
    let mut limit = DEFAULT_LIMIT;
    let mut summarize = false;
    let mut words = args.iter().filter(|a| *a != "--json");
    while let Some(word) = words.next() {
        match word.as_str() {
            "--summary" => summarize = true,
            "-h" | "--help" | "help" => {
                println!("{}", usage());
                return ExitCode::SUCCESS;
            }
            "--limit" => match words.next().and_then(|n| n.parse::<usize>().ok()) {
                Some(n) if (1..=MAX_LIMIT).contains(&n) => limit = n,
                _ => {
                    return emit(
                        Out::fail(
                            EXIT_USAGE,
                            format!("--limit takes a number from 1 to {MAX_LIMIT}"),
                        ),
                        as_json,
                    )
                }
            },
            other => {
                return emit(
                    Out::fail(EXIT_USAGE, format!("unknown option {other}. {}", usage())),
                    as_json,
                )
            }
        }
    }
    let out = match crate::config::Settings::history_path() {
        Ok(path) if summarize => {
            let settings = crate::cli::settings_for_cli();
            summary_out(&path, limit, settings.as_ref(), Machine::here())
        }
        Ok(path) => jobs_out(&path, limit),
        Err(err) => Out::fail(EXIT_FAILED, err.0),
    };
    emit(out, as_json)
}

/// What a suggestion is measured against: the memory and processors this
/// worker sees (inside WSL2, its share of the PC).
#[derive(Debug, Clone, Copy)]
pub struct Machine {
    pub memory_total: Option<u64>,
    pub cpus: usize,
}

impl Machine {
    pub fn here() -> Self {
        Self {
            memory_total: crate::resources::memory_total(),
            cpus: std::thread::available_parallelism().map_or(1, |n| n.get()),
        }
    }
}

/// The capacity the last `limit` jobs leave room for, with the settings in
/// force. None when the settings could not be read.
pub fn capacity_of(
    summary: &Summary,
    settings: &crate::config::Settings,
    machine: Machine,
) -> Option<Capacity> {
    // On a shared PC its jobs may commit its memory less what it keeps
    // for its owner (`Launcher::committed_for`): more slots would wait.
    let memory = match settings.shared_pc {
        true => machine
            .memory_total
            .map(|total| total.saturating_sub(settings.min_free_memory)),
        false => machine.memory_total,
    };
    capacity(
        summary,
        settings.max_job_memory,
        settings.max_job_cpus,
        settings.max_slots,
        memory,
        machine.cpus,
    )
}

/// `jobs --summary`: what the last jobs used and, when there is room, the
/// limits that would fit more of them. It changes nothing.
pub fn summary_out(
    history: &Path,
    limit: usize,
    settings: Option<&crate::config::Settings>,
    machine: Machine,
) -> Out {
    let jobs = if history.exists() {
        match Store::open(history).and_then(|store| store.recent(limit)) {
            Ok(jobs) => jobs,
            Err(err) => return Out::fail(EXIT_FAILED, format!("{}: {err}", history.display())),
        }
    } else {
        Vec::new()
    };
    let summary = summary(&jobs);
    let images = by_image(&jobs);
    let mut text = summary_text(&summary);
    for image in &images {
        text.push('\n');
        text.push_str(&image_text(image));
    }
    let mut value =
        json!({"summary": summary, "images": images, "limit": limit, "suggestion": null});
    if let Some(settings) = settings {
        let limits = json!({
            "max_job_memory_bytes": settings.max_job_memory,
            "max_job_cpus": settings.max_job_cpus,
            "max_slots": settings.max_slots,
        });
        text.push_str(&format!(
            "\nlimits: {} memory, {} CPU per job, {} slot(s)",
            settings
                .max_job_memory
                .map_or("no".into(), crate::resources::human),
            settings.max_job_cpus.map_or("no".into(), |c| c.to_string()),
            settings.max_slots
        ));
        value["limits"] = limits;
        match capacity_of(&summary, settings, machine) {
            Some(fit) => {
                text.push_str(&format!(
                    "\nroom for more: max_job_memory {} would fit {} slots (now {})",
                    size_value(fit.max_job_memory),
                    fit.max_slots,
                    settings.max_slots
                ));
                value["suggestion"] = json!({
                    "max_job_memory": size_value(fit.max_job_memory),
                    "max_slots": fit.max_slots,
                });
            }
            None if summary.measured < MIN_MEASURED => text.push_str(&format!(
                "\nno suggestion: fewer than {MIN_MEASURED} jobs with a memory reading"
            )),
            None => text.push_str("\nno room for more slots at these limits"),
        }
    }
    Out::ok(value, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewJob;

    #[test]
    fn each_image_gets_its_own_peak_and_the_memory_to_ask_for() {
        let mib = 1024 * 1024;
        let job = |n: i64, image: &str, peak: Option<u64>| JobRecord {
            id: format!("job-{n}"),
            image: image.into(),
            job_key: None,
            started_at: n * 1000,
            ended_at: Some(n * 1000 + 60),
            exit_code: Some(0),
            reason: None,
            callback_url: None,
            logs: None,
            oom_killed: false,
            cpu_ms: None,
            memory_peak: peak.map(|p| p * mib),
            network_bytes: peak.map(|p| p * mib / 10),
            disk_bytes: peak.map(|p| p * mib * 2),
            pull: None,
        };
        let jobs = vec![
            job(1, "rpa", Some(900)),
            job(2, "rpa", Some(1100)),
            job(3, "rpa", Some(1000)),
            job(4, "report", Some(200)),
            job(5, "report", None),
            JobRecord {
                ended_at: None,
                ..job(6, "report", Some(5000))
            },
        ];
        let images = by_image(&jobs);
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].image, "rpa");
        assert_eq!((images[0].jobs, images[0].measured), (3, 3));
        assert_eq!(images[0].memory_peak_max, Some(1100 * mib));
        // Twice 1100 MiB is 2200, rounded up to 2304.
        assert_eq!(images[0].memory_to_ask, Some(2304 * mib));
        // Two finished jobs, one measured: numbers, no suggestion; the
        // running one is not counted.
        assert_eq!((images[1].jobs, images[1].measured), (2, 1));
        assert_eq!(images[1].memory_to_ask, None);
        assert_eq!(
            image_text(&images[1]),
            "  report: 2 jobs, 1 measured, memory peak 200 MiB at most (200 MiB on average), network 20 MiB a job, disk 400 MiB a job"
        );
    }

    #[test]
    fn the_summary_suggests_a_memory_limit_and_slots_only_from_enough_readings() {
        let dir = std::env::temp_dir().join(format!(
            "shutup-and-run-jobs-summary-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("jobs.sqlite");
        let store = Store::open(&path).unwrap();
        let mib = 1024 * 1024;
        let add = |n: i64, peak: u64| {
            let id = format!("job-{n}");
            store
                .insert_running(&NewJob {
                    id: id.clone(),
                    image: "registry.example/protocol:1".into(),
                    job_key: None,
                    started_at: n * 1000,
                    callback_url: None,
                    managed: None,
                    memory_limit: None,
                    concurrency_key: None,
                })
                .unwrap();
            store
                .finish(&id, n * 1000 + 100, Some(0), crate::REASON_EXITED, "")
                .unwrap();
            // 40 s of processor in 100 s: 0.4 of one.
            store
                .set_usage(&id, Some(40_000), Some(peak), None)
                .unwrap();
        };
        for n in 1..=4 {
            add(n, (100 + 70 * n as u64) * mib);
        }
        let mut settings =
            crate::config::Settings::from_json_map(&crate::config::default_map()).unwrap();
        settings.max_job_memory = Some(2048 * mib);
        settings.max_job_cpus = Some(1.0);
        settings.max_slots = 1;
        let machine = Machine {
            memory_total: Some(6 * 1024 * mib),
            cpus: 4,
        };
        let out = summary_out(&path, 20, Some(&settings), machine);
        assert_eq!(
            out.json["suggestion"],
            Value::Null,
            "4 readings are too few"
        );
        assert!(out.text.contains("fewer than 5 jobs"), "{}", out.text);

        add(5, 380 * mib);
        let out = summary_out(&path, 20, Some(&settings), machine);
        assert_eq!(out.json["summary"]["measured"], 5);
        assert_eq!(out.json["summary"]["memory_peak_max"], 380 * mib);
        assert_eq!(out.json["summary"]["cpus_max"], 0.4);
        // Twice 380 MiB is 760, rounded up to 768; 6 GiB holds 8 of those,
        // and 4 processors 4 jobs of 1.
        assert_eq!(
            out.json["suggestion"],
            json!({"max_job_memory": "768m", "max_slots": 4})
        );
        assert!(
            out.text
                .contains("max_job_memory 768m would fit 4 slots (now 1)"),
            "{}",
            out.text
        );
        let check = crate::cli::capacity_check(&settings, &path, machine).unwrap();
        assert_eq!(check.code(), "ROOM_FOR_MORE_SLOTS");
        assert!(check
            .detail
            .contains("at max_job_memory 768m, 4 slots fit (now 1)"));
        // On a shared PC that keeps 4 GiB for its owner, its jobs may
        // commit 2 GiB together: 2 of 768m, not 4.
        let mut shared = settings.clone();
        shared.shared_pc = true;
        shared.min_free_memory = 4 * 1024 * mib;
        let check = crate::cli::capacity_check(&shared, &path, machine).unwrap();
        assert!(check
            .detail
            .contains("at max_job_memory 768m, 2 slots fit (now 1)"));

        // Already as many slots as fit: nothing to say.
        settings.max_slots = 4;
        assert!(crate::cli::capacity_check(&settings, &path, machine).is_none());
        // Never a limit above the one in force.
        let summary = summary(&store.recent(20).unwrap());
        let tight = capacity(&summary, Some(512 * mib), None, 1, Some(2048 * mib), 4);
        assert_eq!(
            tight,
            Some(Capacity {
                max_job_memory: 512 * mib,
                max_slots: 4
            })
        );
        assert_eq!(capacity(&summary, None, None, 1, Some(2048 * mib), 4), None);
        assert_eq!(size_value(2048 * mib), "2g");
        // One line per image: here one image, five measured.
        assert_eq!(
            out.json["images"][0]["image"],
            "registry.example/protocol:1"
        );
        assert_eq!(out.json["images"][0]["memory_to_ask"], 768 * mib);
        assert!(out.text.contains(
            "\n  registry.example/protocol:1: 5 jobs, 5 measured, memory peak 380 MiB at most (296 MiB on average); ask for memory 768m"
        ), "{}", out.text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn jobs_lists_the_last_ones_with_how_each_ended_and_nothing_a_job_was_given() {
        let dir = std::env::temp_dir().join(format!("shutup-and-run-jobs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("jobs.sqlite");
        assert_eq!(jobs_out(&path, 20).json["jobs"], json!([]));

        let store = Store::open(&path).unwrap();
        let start = |id: &str, at: i64| {
            store
                .insert_running(&NewJob {
                    id: id.into(),
                    image: "registry.example/protocol:1".into(),
                    job_key: Some(format!("key-{id}")),
                    started_at: at,
                    callback_url: Some("https://api.example/result?secret=callback-secret".into()),
                    managed: None,
                    memory_limit: None,
                    concurrency_key: None,
                })
                .unwrap();
        };
        let noisy = "PASSWORD=container-output-secret";
        start("a-ok", 1_000);
        store
            .finish("a-ok", 1_030, Some(0), crate::REASON_EXITED, noisy)
            .unwrap();
        store
            .set_usage("a-ok", Some(12_345), Some(300 << 20), Some(12 << 20))
            .unwrap();
        start("b-failed", 1_100);
        store
            .finish("b-failed", 1_105, Some(3), crate::REASON_EXITED, noisy)
            .unwrap();
        start("c-timeout", 1_200);
        store
            .finish("c-timeout", 1_500, Some(137), REASON_TIMEOUT, noisy)
            .unwrap();
        start("d-killed", 1_600);
        store
            .finish("d-killed", 1_601, Some(137), REASON_LAUNCHER, noisy)
            .unwrap();
        start("e-start", 1_700);
        store
            .finish(
                "e-start",
                1_702,
                None,
                REASON_START_FAILED,
                "failed to pull image",
            )
            .unwrap();
        start("f-running", 1_800);

        let out = jobs_out(&path, 20);
        assert_eq!(out.code, 0);
        let jobs = out.json["jobs"].as_array().unwrap();
        let seen: Vec<(&str, &str)> = jobs
            .iter()
            .map(|j| (j["id"].as_str().unwrap(), j["status"].as_str().unwrap()))
            .collect();
        assert_eq!(
            seen,
            [
                ("f-running", "running"),
                ("e-start", "failed"),
                ("d-killed", "killed"),
                ("c-timeout", "timeout"),
                ("b-failed", "failed"),
                ("a-ok", "ok"),
            ]
        );
        let ok = &jobs[5];
        assert_eq!(ok["image"], "registry.example/protocol:1");
        assert_eq!(ok["started_at"], "1970-01-01T00:16:40Z");
        assert_eq!(ok["ended_at"], "1970-01-01T00:17:10Z");
        assert_eq!(ok["duration_secs"], 30);
        assert_eq!(ok["exit_code"], 0);
        assert!(ok["failure"].is_null());
        assert_eq!(
            (ok["cpu_ms"].clone(), ok["memory_peak_bytes"].clone()),
            (json!(12_345), json!(300 << 20))
        );
        assert!(jobs[4]["cpu_ms"].is_null() && jobs[4]["memory_peak_bytes"].is_null());
        assert_eq!(ok["network_bytes"], json!(12 << 20));
        assert!(out
            .text
            .contains("used: cpu 12.3 s, memory peak 300 MiB, network 12 MiB"));
        assert_eq!(jobs[4]["failure"], "the container exited with code 3");
        assert_eq!(jobs[1]["failure"], "did not start: failed to pull image");
        assert!(jobs[1]["exit_code"].is_null());
        assert!(jobs[0]["ended_at"].is_null() && jobs[0]["duration_secs"].is_null());
        assert!(jobs[3]["failure"].as_str().unwrap().contains("time limit"));

        // Exactly these fields: no env, command, key, callback, and no
        // output (only where its log is, null here: nothing was saved).
        let mut fields: Vec<&str> = ok.as_object().unwrap().keys().map(String::as_str).collect();
        fields.sort_unstable();
        assert_eq!(
            fields,
            [
                "cpu_ms",
                "disk_bytes",
                "duration_secs",
                "ended_at",
                "exit_code",
                "failure",
                "id",
                "image",
                "log",
                "memory_peak_bytes",
                "network_bytes",
                "oom_killed",
                "pull",
                "started_at",
                "status"
            ]
        );
        for output in [out.json.to_string(), out.text.clone()] {
            for secret in ["container-output-secret", "callback-secret", "key-a-ok"] {
                assert!(!output.contains(secret), "{secret} in {output}");
            }
        }
        assert!(out.text.contains("why: the container exited with code 3"));

        // --limit, newest first.
        let two = jobs_out(&path, 2);
        assert_eq!(two.json["jobs"].as_array().unwrap().len(), 2);
        assert_eq!(two.json["jobs"][0]["id"], "f-running");

        // The history has a ceiling: the oldest finished rows go, a running
        // one stays whatever its age.
        assert_eq!(store.purge_beyond(2).unwrap(), 3);
        let left: Vec<String> = store
            .recent(50)
            .unwrap()
            .into_iter()
            .map(|j| j.id)
            .collect();
        assert_eq!(left, ["f-running", "e-start", "d-killed"]);
    }
}
