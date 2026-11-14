//! `shutup-and-run jobs`: the last jobs of this machine, from the history
//! file. What ran, when, for how long and how it ended. Never the env, the
//! command, the logs or anything else a job was given.

use crate::cli::{emit, Out, EXIT_FAILED, EXIT_USAGE};
use crate::store::{JobRecord, Store};
use crate::{REASON_LAUNCHER, REASON_START_FAILED, REASON_TIMEOUT};
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
    end well, why. Status is running, ok, failed, timeout or killed. The
    history holds at most the last 500 finished jobs, and never a job's env,
    command, payload or logs."
}

/// `running`, `ok`, `failed`, `timeout` or `killed`.
pub fn status_of(job: &JobRecord) -> &'static str {
    match (job.ended_at, job.reason.as_deref(), job.exit_code) {
        (None, _, _) => "running",
        (_, Some(REASON_TIMEOUT), _) => "timeout",
        (_, Some(REASON_LAUNCHER), _) => "killed",
        (_, Some(REASON_START_FAILED), _) => "failed",
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
        "killed" if job.reason.as_deref() == Some(REASON_LAUNCHER) => {
            Some("stopped by the worker".into())
        }
        "killed" => {
            Some("killed from outside (exit 137): out of memory, or stopped by hand".into())
        }
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
    })
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
        text.push_str(&format!(
            "{}  {:<8} {:>6}  exit {:<4} {}  {}{}\n",
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
            failure_of(job)
                .map(|why| format!("\n    why: {why}"))
                .unwrap_or_default(),
        ));
    }
    if jobs.is_empty() {
        text.push_str("no jobs yet");
    }
    let list: Vec<Value> = jobs.iter().map(job_json).collect();
    Out::ok(json!({"jobs": list, "limit": limit}), text.trim_end())
}

pub fn jobs_cli(args: &[String]) -> ExitCode {
    let as_json = args.iter().any(|a| a == "--json");
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
                })
                .unwrap();
        };
        let noisy = "PASSWORD=container-output-secret";
        start("a-ok", 1_000);
        store
            .finish("a-ok", 1_030, Some(0), crate::REASON_EXITED, noisy)
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
        assert_eq!(jobs[4]["failure"], "the container exited with code 3");
        assert_eq!(jobs[1]["failure"], "did not start: failed to pull image");
        assert!(jobs[1]["exit_code"].is_null());
        assert!(jobs[0]["ended_at"].is_null() && jobs[0]["duration_secs"].is_null());
        assert!(jobs[3]["failure"].as_str().unwrap().contains("time limit"));

        // Exactly these fields: no env, command, key, callback or logs.
        let mut fields: Vec<&str> = ok.as_object().unwrap().keys().map(String::as_str).collect();
        fields.sort_unstable();
        assert_eq!(
            fields,
            [
                "duration_secs",
                "ended_at",
                "exit_code",
                "failure",
                "id",
                "image",
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
