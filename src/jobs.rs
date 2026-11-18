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

shutup-and-run jobs logs ID|--last [--tail N] [--json]
    what a job printed (stdout and stderr): its last 256 KiB, kept for 7
    days in the logs folder next to the history. ID may be the start of an
    id. --last is the most recent job that ended. --tail N prints only the
    last N lines. Exit 1 with JOB_NOT_FOUND, JOB_ID_AMBIGUOUS or
    JOB_LOG_MISSING."
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
    })
}

/// `cpu 5.5 s, memory peak 300 MiB`, from what the readings saw. None when
/// the job was never read (it ran under 30 seconds, or it is still running).
pub fn usage_text(job: &JobRecord) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(ms) = job.cpu_ms {
        parts.push(format!("cpu {:.1} s", ms as f64 / 1000.0));
    }
    if let Some(bytes) = job.memory_peak {
        parts.push(format!("memory peak {}", memory_text(bytes)));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
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
            usage_text(job)
                .map(|used| format!("\n    used: {used}"))
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

/// `jobs logs ID|--last [--tail N]`.
pub fn logs_out(history: &Path, wanted: Option<&str>, tail: Option<usize>) -> Out {
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
    let Some(info) = crate::joblogs::info(history, &id) else {
        let why = match &job {
            Some(job) if !job.is_finished() => {
                "it is still running; its log is written when it ends"
            }
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
        "log": shown,
    });
    if let Some(job) = &job {
        value["image"] = json!(job.image);
        value["status"] = json!(status_of(job));
        value["exit_code"] = json!(job.exit_code);
    }
    Out::ok(value, shown.strip_suffix('\n').unwrap_or(shown))
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
        Ok(path) => logs_out(&path, wanted, tail),
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
    let mut words = args.iter().filter(|a| *a != "--json");
    while let Some(word) = words.next() {
        match word.as_str() {
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
        Ok(path) => jobs_out(&path, limit),
        Err(err) => Out::fail(EXIT_FAILED, err.0),
    };
    emit(out, as_json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewJob;

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
                })
                .unwrap();
        };
        let noisy = "PASSWORD=container-output-secret";
        start("a-ok", 1_000);
        store
            .finish("a-ok", 1_030, Some(0), crate::REASON_EXITED, noisy)
            .unwrap();
        store
            .set_usage("a-ok", Some(12_345), Some(300 << 20))
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
        assert!(out.text.contains("used: cpu 12.3 s, memory peak 300 MiB"));
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
                "duration_secs",
                "ended_at",
                "exit_code",
                "failure",
                "id",
                "image",
                "log",
                "memory_peak_bytes",
                "oom_killed",
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
