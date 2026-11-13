//! The commands an agent runs on the machine: `config`, `status`, `doctor`,
//! `pause` and `resume`. None asks a question. Each takes `--json`, and the
//! exit codes are the same everywhere:
//!
//! 0 done, 1 it did not work or the doctor found a problem, 2 the command
//! line is wrong, 3 the config or the value is invalid (nothing was written).
//!
//! On Windows the `.exe` forwards `config`, `pause`, `resume` and `bans` to
//! this same code inside the distribution, so there is one implementation.

use crate::config::{self, ConfigError, JsonMap, Settings};
use serde_json::{json, Value};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

pub const EXIT_FAILED: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_INVALID: u8 = 3;

const ISSUES_URL: &str = "https://github.com/nailtongomes/shutup-and-run/issues/new";
/// A browser and GitHub both take a URL of about 8 KiB. The report is cut to fit.
const ISSUE_BODY_MAX: usize = 5000;
const APPLY_WAIT: Duration = Duration::from_secs(60);

pub fn usage() -> &'static str {
    "\
Every command below is non-interactive and takes --json (one JSON object on
stdout, with \"ok\": true or false and \"error\" when false).
Exit codes: 0 done; 1 failed, or the doctor found a problem; 2 wrong command
line; 3 invalid config or value, and nothing was written.

shutup-and-run config show [--reveal]
    the whole config file. Secrets print as *** unless --reveal.
shutup-and-run config get KEY [--reveal]
    one value. Prints nothing when the key is not set.
shutup-and-run config set KEY VALUE [--apply]
    validate the whole config with the new value, then write it. An invalid
    value is refused with the reason and the file is left as it was.
    A list is comma separated: config set allowlist alpine,ghcr.io/owner/img
shutup-and-run config unset KEY [--apply]
    remove a key, so its default applies.
shutup-and-run config validate
    check the file as the service would. Exit 3 with the reason when invalid.
shutup-and-run config apply
    restart the service on the file and wait until it is back. Running
    containers are left alone. A restart also resumes a paused worker.
shutup-and-run config reset
    factory reset of the config: one slot, nothing to poll, admin/admin. The
    tokens, the worker name and the listen address are kept when readable.
    Then applies it.
shutup-and-run config keys
    every key with its type and where it can be edited.

shutup-and-run status
    what the worker is doing now: state (idle, active, draining, failed,
    stopped), slots, running job ids, the last error, the last poll error.
    Exit 1 when it is not running.
shutup-and-run doctor [--report]
    check the config, Docker, the service, a forgotten pause, bans and the
    poll URL. Every failed line has a `fix:` with the command to run.
    --report also prints a diagnostic report with secrets masked and a URL
    that opens a new GitHub issue with it filled in. Exit 1 when unhealthy.
shutup-and-run pause | resume
    stop or start taking new work. Running containers are left alone.

The config is one JSON file: /etc/shutup-and-run/config.json (on Windows,
inside the shutup-and-run WSL distribution). Schema:
docs/schemas/worker-config.schema.json. Guide: docs/FOR-AGENTS.md."
}

// ---------------------------------------------------------------------------
// Output

pub struct Out {
    pub code: u8,
    pub json: Value,
    pub text: String,
}

impl Out {
    fn ok(mut json: Value, text: impl Into<String>) -> Self {
        json["ok"] = Value::Bool(true);
        Out {
            code: 0,
            json,
            text: text.into(),
        }
    }

    fn fail(code: u8, error: impl Into<String>) -> Self {
        let error = error.into();
        Out {
            code,
            json: json!({"ok": false, "error": error}),
            text: error,
        }
    }
}

fn emit(out: Out, as_json: bool) -> ExitCode {
    if as_json {
        println!("{}", out.json);
    } else if out.code == 0 {
        if !out.text.is_empty() {
            println!("{}", out.text);
        }
    } else {
        eprintln!("shutup-and-run: {}", out.text);
    }
    ExitCode::from(out.code)
}

/// Splits `--json`, `--reveal`, `--apply`, `--report` and `--help` from the rest.
struct Args {
    words: Vec<String>,
    json: bool,
    reveal: bool,
    apply: bool,
    report: bool,
    help: bool,
}

fn split_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args {
        words: Vec::new(),
        json: false,
        reveal: false,
        apply: false,
        report: false,
        help: false,
    };
    let mut literal = false;
    for arg in args {
        match arg.as_str() {
            _ if literal => out.words.push(arg.clone()),
            "--" => literal = true,
            "--json" => out.json = true,
            "--reveal" => out.reveal = true,
            "--apply" => out.apply = true,
            "--report" => out.report = true,
            "-h" | "--help" | "help" => out.help = true,
            // A value may start with a dash only after `--`.
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ => out.words.push(arg.clone()),
        }
    }
    Ok(out)
}

fn parse_or_exit(args: &[String]) -> Result<Args, ExitCode> {
    match split_args(args) {
        Ok(parsed) if parsed.help => {
            println!("{}", usage());
            Err(ExitCode::SUCCESS)
        }
        Ok(parsed) => Ok(parsed),
        Err(msg) => {
            let as_json = args.iter().any(|a| a == "--json");
            Err(emit(Out::fail(EXIT_USAGE, msg), as_json))
        }
    }
}

// ---------------------------------------------------------------------------
// config

pub fn config_cli(args: &[String]) -> ExitCode {
    let parsed = match parse_or_exit(args) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    let path = match config::json_path_for_cli() {
        Ok(path) => path,
        Err(err) => return emit(Out::fail(EXIT_FAILED, err.0), parsed.json),
    };
    let words: Vec<&str> = parsed.words.iter().map(String::as_str).collect();
    let mut out = config_command(&path, &words, parsed.reveal);
    let wants_apply = matches!(words.first(), Some(&"apply") | Some(&"reset"))
        || (parsed.apply && matches!(words.first(), Some(&"set") | Some(&"unset")));
    if out.code == 0 && wants_apply {
        match apply(&path) {
            Ok(applied) => {
                out.text = format!("{}\n{}", out.text, applied.text).trim().to_string();
                out.json["restarted"] = applied.json["restarted"].clone();
                out.json["applied"] = Value::Bool(true);
            }
            Err(msg) => out = Out::fail(EXIT_FAILED, msg),
        }
    }
    emit(out, parsed.json)
}

/// Everything `config` does to the file. Applying is separate, so this part
/// runs in a test with no service.
pub fn config_command(path: &Path, words: &[&str], reveal: bool) -> Out {
    let shown = |map: &JsonMap| {
        if reveal {
            map.clone()
        } else {
            config::masked(map)
        }
    };
    // A machine with no file yet starts from the defaults.
    let load = || -> Result<JsonMap, ConfigError> {
        if path.exists() {
            config::read_json(path)
        } else {
            Ok(config::default_map())
        }
    };
    let broken = |err: ConfigError| {
        Out::fail(
            EXIT_INVALID,
            format!(
                "{}. `shutup-and-run config reset` writes the factory defaults.",
                err.0
            ),
        )
    };
    match words {
        ["show"] => match load() {
            Ok(map) => {
                let problem = Settings::from_json_map(&map).err().map(|e| e.0);
                let map = shown(&map);
                Out::ok(
                    json!({"path": path, "config": map, "valid": problem.is_none(), "problem": problem}),
                    config::to_text(&map).trim_end(),
                )
            }
            Err(err) => broken(err),
        },
        ["keys"] => {
            let keys: Vec<Value> = config::KEYS
                .iter()
                .map(|k| {
                    json!({
                        "key": k.name,
                        "type": format!("{:?}", k.kind).to_lowercase(),
                        "secret": k.secret,
                        "page": format!("{:?}", k.page).to_lowercase(),
                    })
                })
                .collect();
            let text: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{:<24} {:<5} page: {}",
                        k["key"].as_str().unwrap_or(""),
                        k["type"].as_str().unwrap_or(""),
                        k["page"].as_str().unwrap_or("")
                    )
                })
                .collect();
            Out::ok(json!({"keys": keys}), text.join("\n"))
        }
        ["get", name] => {
            let key = match config::find_key(name) {
                Ok(key) => key,
                Err(err) => return Out::fail(EXIT_USAGE, err.0),
            };
            match load() {
                Ok(map) => {
                    let value = shown(&map).get(key.name).cloned();
                    let text = match &value {
                        Some(Value::String(s)) => s.clone(),
                        Some(other) => other.to_string(),
                        None => String::new(),
                    };
                    Out::ok(
                        json!({"key": key.name, "set": value.is_some(), "value": value}),
                        text,
                    )
                }
                Err(err) => broken(err),
            }
        }
        ["set", name, raw] => {
            let key = match config::find_key(name) {
                Ok(key) => key,
                Err(err) => return Out::fail(EXIT_USAGE, err.0),
            };
            let value = match config::parse_value(key, raw) {
                Ok(value) => value,
                Err(err) => return Out::fail(EXIT_INVALID, err.0),
            };
            let mut map = match load() {
                Ok(map) => map,
                Err(err) => return broken(err),
            };
            map.insert(key.name.to_string(), value);
            written(path, &map, key.name)
        }
        ["unset", name] => {
            let key = match config::find_key(name) {
                Ok(key) => key,
                Err(err) => return Out::fail(EXIT_USAGE, err.0),
            };
            let mut map = match load() {
                Ok(map) => map,
                Err(err) => return broken(err),
            };
            map.remove(key.name);
            written(path, &map, key.name)
        }
        ["validate"] => match load().and_then(|map| Settings::from_json_map(&map).map(|_| ())) {
            Ok(()) => Out::ok(json!({"path": path}), format!("{} is valid", path.display())),
            Err(err) => Out::fail(EXIT_INVALID, err.0),
        },
        ["apply"] => match load().and_then(|map| Settings::from_json_map(&map).map(|_| ())) {
            Ok(()) => Out::ok(json!({"path": path}), ""),
            Err(err) => Out::fail(EXIT_INVALID, err.0),
        },
        ["reset"] => {
            let old = config::read_json(path)
                .or_else(|_| config::read_json(&config::backup_path(path)))
                .ok();
            let map = config::reset_map(old.as_ref());
            match config::save_json(path, &map) {
                Ok(()) => Out::ok(
                    json!({"path": path, "reset": true}),
                    format!("{} is back to the factory defaults", path.display()),
                ),
                Err(err) => Out::fail(EXIT_FAILED, err.0),
            }
        }
        _ => Out::fail(
            EXIT_USAGE,
            "usage: shutup-and-run config show|get KEY|set KEY VALUE|unset KEY|validate|apply|reset|keys [--json]. See --help.",
        ),
    }
}

fn written(path: &Path, map: &JsonMap, name: &str) -> Out {
    match config::save_json(path, map) {
        Ok(()) => Out::ok(
            json!({"path": path, "key": name, "applied": false}),
            format!("{name} saved. `shutup-and-run config apply` restarts the service on it."),
        ),
        Err(err) => Out::fail(EXIT_INVALID, format!("not saved: {}", err.0)),
    }
}

/// Ask the running service to restart on the file, and wait for the new one.
/// The service re-executes itself on SIGHUP, so this needs no service manager.
pub fn apply(path: &Path) -> Result<Out, String> {
    let map = config::read_json(path).map_err(|e| e.0)?;
    let settings = Settings::from_json_map(&map).map_err(|e| e.0)?;
    let status_path = crate::status::status_path(&settings.history_path);
    let before = read_status(&status_path);
    let running = before
        .as_ref()
        .filter(|s| !is_stopped(s))
        .and_then(|s| s["pid"].as_u64())
        .filter(|pid| is_our_process(*pid));
    let Some(pid) = running else {
        return Ok(Out::ok(
            json!({"restarted": false}),
            "the service is not running; it reads the config when it starts",
        ));
    };
    let started_before = before
        .as_ref()
        .map(|s| s["started_at"].clone())
        .unwrap_or(Value::Null);
    hang_up(pid)?;
    let deadline = std::time::Instant::now() + APPLY_WAIT;
    while std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        let Some(now) = read_status(&status_path) else {
            continue;
        };
        if now["started_at"] == started_before || now["state"] == "stopped" {
            continue;
        }
        if now["state"] == "failed" && now["slots_total"].is_null() {
            return Err(format!(
                "the service restarted and failed: {}. Run `shutup-and-run doctor`.",
                now["last_error"].as_str().unwrap_or("no reason recorded")
            ));
        }
        return Ok(Out::ok(
            json!({"restarted": true}),
            "applied: the service restarted on the new config",
        ));
    }
    Err(format!(
        "the service did not come back within {} s. Run `shutup-and-run doctor`.",
        APPLY_WAIT.as_secs()
    ))
}

fn read_status(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn is_stopped(status: &Value) -> bool {
    status["state"] == "stopped"
        || crate::status::is_stale(
            status["updated_at"].as_str().unwrap_or(""),
            crate::status::now_unix(),
        )
}

fn is_our_process(pid: u64) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|name| name.trim() == "shutup-and-run")
        .unwrap_or(false)
}

#[cfg(unix)]
fn hang_up(pid: u64) -> Result<(), String> {
    // SAFETY: kill takes two integers and touches no memory of ours.
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGHUP) };
    if rc == 0 {
        Ok(())
    } else {
        Err(format!(
            "could not signal the service (pid {pid}): {}. Run this as root.",
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(not(unix))]
fn hang_up(_pid: u64) -> Result<(), String> {
    Err("apply runs inside the distribution".into())
}

// ---------------------------------------------------------------------------
// pause, resume

/// The drain flag, written where the running service reads it on every
/// request. No HTTP, so it works with any listen address and with TLS.
pub fn pause_cli(pause: bool, args: &[String]) -> ExitCode {
    let parsed = match parse_or_exit(args) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    let done = Settings::history_path()
        .map_err(|e| e.0)
        .and_then(|path| {
            if path.exists() {
                crate::store::Store::open(&path).map_err(|e| e.to_string())
            } else {
                Err(format!(
                    "{} does not exist: the service has not run yet",
                    path.display()
                ))
            }
        })
        .and_then(|store| store.set_draining(pause).map_err(|e| e.to_string()));
    let out = match done {
        Ok(()) => Out::ok(
            json!({"paused": pause}),
            if pause {
                "paused: no new work is taken"
            } else {
                "resumed: the worker takes work again"
            },
        ),
        Err(msg) => Out::fail(EXIT_FAILED, msg),
    };
    emit(out, parsed.json)
}

// ---------------------------------------------------------------------------
// status

pub fn status_cli(args: &[String]) -> ExitCode {
    let parsed = match parse_or_exit(args) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    let path = if cfg!(windows) {
        Ok(crate::wsl::data_dir().join("status.json"))
    } else {
        Settings::history_path()
            .map(|history| crate::status::status_path(&history))
            .map_err(|e| e.0)
    };
    let out = match path {
        Ok(path) => status_out(&path, crate::status::now_unix()),
        Err(msg) => Out::fail(EXIT_FAILED, msg),
    };
    emit(out, parsed.json)
}

pub fn status_out(path: &Path, now: i64) -> Out {
    let Some(mut status) = read_status(path).filter(Value::is_object) else {
        return Out::fail(
            EXIT_FAILED,
            format!(
                "{} is missing or unreadable: the service has not run. Run `shutup-and-run doctor`.",
                path.display()
            ),
        );
    };
    let stale = crate::status::is_stale(status["updated_at"].as_str().unwrap_or(""), now);
    status["stale"] = Value::Bool(stale);
    let state = status["state"].as_str().unwrap_or("unknown").to_string();
    let mut text = format!(
        "state: {}\nslots: {} of {} in use\n",
        if stale {
            "dead (status.json is stale)"
        } else {
            &state
        },
        status["slots_used"],
        status["slots_total"]
    );
    for field in ["last_error", "poll_error", "config_warning"] {
        if let Some(value) = status[field].as_str() {
            text.push_str(&format!("{field}: {value}\n"));
        }
    }
    let running = !stale && matches!(state.as_str(), "idle" | "active" | "draining");
    let mut out = Out::ok(status, text.trim_end());
    if !running {
        out.code = EXIT_FAILED;
        out.json["ok"] = Value::Bool(false);
        out.json["error"] = Value::String(format!(
            "the worker is not running ({})",
            if stale { "stale" } else { &state }
        ));
        out.text = format!("{}\nRun `shutup-and-run doctor`.", out.text);
    }
    out
}

// ---------------------------------------------------------------------------
// doctor

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    /// A failed check that does not make the doctor exit 1: the worker runs,
    /// and this is something to know about.
    pub soft: bool,
    /// What was found: the cause, when it failed.
    pub detail: String,
    /// The command or the step that fixes it. Empty when it is fine.
    pub fix: String,
}

pub fn check(name: &str, ok: bool, detail: impl Into<String>) -> Check {
    Check {
        name: name.to_string(),
        ok,
        soft: false,
        detail: detail.into(),
        fix: String::new(),
    }
}

pub fn soft(name: &str, ok: bool, detail: impl Into<String>) -> Check {
    Check {
        soft: true,
        ..check(name, ok, detail)
    }
}

impl Check {
    pub fn fix(mut self, fix: impl Into<String>) -> Self {
        if !self.ok {
            self.fix = fix.into();
        }
        self
    }

    fn mark(&self) -> &'static str {
        if self.ok {
            "ok"
        } else if self.soft {
            "warn"
        } else {
            "fail"
        }
    }
}

pub fn report(checks: &[Check]) -> (String, bool) {
    let mut text = String::new();
    let mut healthy = true;
    for c in checks {
        let mark = match c.mark() {
            "ok" => "ok  ",
            "warn" => "--  ",
            _ => {
                healthy = false;
                "FAIL"
            }
        };
        text.push_str(&format!("{mark} {:<22} {}\n", c.name, c.detail));
        if !c.fix.is_empty() {
            text.push_str(&format!("     {:<22} fix: {}\n", "", c.fix));
        }
    }
    (text, healthy)
}

pub fn checks_json(checks: &[Check]) -> Value {
    checks
        .iter()
        .map(|c| {
            json!({
                "name": c.name,
                "status": c.mark(),
                "detail": c.detail,
                "fix": if c.fix.is_empty() { Value::Null } else { Value::String(c.fix.clone()) },
            })
        })
        .collect()
}

pub fn checks_from_json(value: &Value) -> Vec<Check> {
    value
        .as_array()
        .map(|list| {
            list.iter()
                .map(|c| Check {
                    name: c["name"].as_str().unwrap_or("?").to_string(),
                    ok: c["status"] == "ok",
                    soft: c["status"] == "warn",
                    detail: c["detail"].as_str().unwrap_or("").to_string(),
                    fix: c["fix"].as_str().unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Replace every secret with `***`. The last step before a report leaves
/// the machine, whatever text ended up in it.
pub fn scrub(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for secret in secrets {
        out = out.replace(secret.as_str(), config::MASK);
    }
    out
}

/// The diagnostic report: version, checks, the masked config and status.json.
pub fn diagnostic_report(
    platform: &str,
    checks: &[Check],
    config: Option<&Value>,
    status: Option<&Value>,
    secrets: &[String],
) -> String {
    let (lines, healthy) = report(checks);
    let mut text = format!(
        "shutup-and-run {} on {platform}\nresult: {}\n\nchecks:\n{lines}",
        env!("CARGO_PKG_VERSION"),
        if healthy { "healthy" } else { "unhealthy" }
    );
    if let Some(config) = config {
        text.push_str(&format!("\nconfig (secrets masked):\n{config:#}\n"));
    }
    if let Some(status) = status {
        text.push_str(&format!("\nstatus.json:\n{status:#}\n"));
    }
    scrub(&text, secrets)
}

/// A link that opens a new issue with the report in it. Nobody is logged in
/// here: the person who opens the link is the one who submits it.
pub fn issue_url(report: &str, checks: &[Check]) -> String {
    let failing: Vec<&str> = checks
        .iter()
        .filter(|c| !c.ok && !c.soft)
        .map(|c| c.name.as_str())
        .collect();
    let title = if failing.is_empty() {
        "worker report".to_string()
    } else {
        format!("doctor: {} failed", failing.join(", "))
    };
    let mut body: String = report.chars().take(ISSUE_BODY_MAX).collect();
    if body.len() < report.len() {
        body.push_str("\n[cut to fit the URL]");
    }
    let body = format!("```\n{body}\n```\n");
    format!(
        "{ISSUES_URL}?template=worker-problem.md&title={}&body={}",
        percent(&title),
        percent(&body)
    )
}

fn percent(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// What `doctor` prints, from the checks and what the report needs.
pub fn doctor_out(
    platform: &str,
    checks: &[Check],
    config: Option<&Value>,
    status: Option<&Value>,
    secrets: &[String],
    with_report: bool,
) -> Out {
    // Whatever a check quoted, no secret leaves in any of the outputs.
    let checks: Vec<Check> = checks
        .iter()
        .map(|c| Check {
            detail: scrub(&c.detail, secrets),
            fix: scrub(&c.fix, secrets),
            ..c.clone()
        })
        .collect();
    let checks = checks.as_slice();
    let (lines, healthy) = report(checks);
    let full = diagnostic_report(platform, checks, config, status, secrets);
    let url = issue_url(&full, checks);
    let mut text = lines;
    text.push_str(if healthy {
        "the worker is ready"
    } else {
        "something is wrong: run the `fix:` under each FAIL, then the doctor again.\n\
         Still failing? `shutup-and-run doctor --report` prints a report and an issue link."
    });
    if with_report {
        text = format!(
            "{text}\n\n---- report (no secrets) ----\n{full}\n---- open an issue with it ----\n{url}"
        );
    }
    Out {
        code: if healthy { 0 } else { EXIT_FAILED },
        json: json!({
            "ok": healthy,
            "checks": checks_json(checks),
            "config": config,
            "status": status,
            "report": full,
            "issue_url": url,
        }),
        text,
    }
}

pub async fn doctor_cli(args: &[String]) -> ExitCode {
    let parsed = match parse_or_exit(args) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    let found = examine().await;
    let platform = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
    let out = doctor_out(
        &platform,
        &found.checks,
        found.config.as_ref(),
        found.status.as_ref(),
        &found.secrets,
        parsed.report,
    );
    // The doctor's text goes to stdout even when unhealthy: it is the answer.
    if parsed.json {
        println!("{}", out.json);
    } else {
        println!("{}", out.text);
    }
    ExitCode::from(out.code)
}

pub struct Found {
    pub checks: Vec<Check>,
    pub config: Option<Value>,
    pub status: Option<Value>,
    pub secrets: Vec<String>,
}

/// The checks that run where the service runs: on Linux, or inside the
/// distribution on Windows.
async fn examine() -> Found {
    let mut checks = Vec::new();
    let mut config_shown = None;
    let mut secrets = Vec::new();
    let json_path = config::json_path_for_cli().ok();

    let settings = match &json_path {
        Some(path) => {
            let read = if path.exists() {
                config::read_json(path)
            } else {
                Err(ConfigError(format!("{} does not exist", path.display())))
            };
            if let Ok(map) = &read {
                secrets = config::secret_values(map);
                config_shown = Some(Value::Object(config::masked(map)));
            }
            match read.and_then(|map| Settings::from_json_map(&map)) {
                Ok(_) => checks.push(check("config", true, path.display().to_string())),
                Err(err) => checks.push(check("config", false, err.0).fix(
                    "`shutup-and-run config validate` names the bad value; set it again with \
                     `shutup-and-run config set KEY VALUE`, or `shutup-and-run config reset` \
                     for the factory defaults",
                )),
            }
            Some(Settings::load_json(path))
        }
        None => match Settings::load() {
            Ok(settings) => {
                checks.push(check("config", true, "config.toml and environment"));
                Some(settings)
            }
            Err(err) => {
                checks.push(check("config", false, err.0).fix(
                    "fix /etc/shutup-and-run/config.toml or the environment, and run this as \
                     root with the service's environment",
                ));
                None
            }
        },
    };
    let on_windows = settings.as_ref().is_some_and(|s| s.worker_os == "windows");
    let start = if on_windows {
        "`shutup-and-run wsl start`"
    } else {
        "`systemctl restart shutup-and-run`"
    };

    let docker = match crate::bollard_rt::connect_docker() {
        Ok(docker) => tokio::time::timeout(Duration::from_secs(10), docker.ping())
            .await
            .map_err(|_| "no answer in 10 s".to_string())
            .and_then(|r| r.map(|_| ()).map_err(|e| e.to_string())),
        Err(err) => Err(err),
    };
    checks.push(match docker {
        Ok(()) => check("docker", true, "answers"),
        Err(err) => check("docker", false, err).fix(if on_windows {
            "`shutup-and-run wsl stop`, then `shutup-and-run wsl start`. Still failing: \
             `shutup-and-run wsl reset` rebuilds the distribution"
        } else {
            "`systemctl start docker`, and run this as root"
        }),
    });

    let Some(settings) = settings else {
        return Found {
            checks,
            config: config_shown,
            status: None,
            secrets,
        };
    };
    let status = read_status(&crate::status::status_path(&settings.history_path));
    let now = crate::status::now_unix();
    let fresh = status
        .as_ref()
        .filter(|s| !crate::status::is_stale(s["updated_at"].as_str().unwrap_or(""), now));
    let last_error = fresh
        .and_then(|s| s["last_error"].as_str())
        .unwrap_or("")
        .to_string();
    checks.push(match fresh.map(|s| s["state"].as_str().unwrap_or("")) {
        Some(state @ ("idle" | "active" | "draining")) => check(
            "service",
            true,
            format!("running ({state}), listening on {}", settings.listen),
        ),
        Some("failed") if last_error.contains("listen") || last_error.contains("in use") => {
            check("service", false, format!("failed: {last_error}")).fix(format!(
            "the port is taken. Pick another: `shutup-and-run config set listen 127.0.0.1:8788`, \
                 then {start}"
        ))
        }
        Some("failed") => check("service", false, format!("failed: {last_error}")).fix(format!(
            "{start}. If Docker is the cause, fix the docker line first"
        )),
        _ => check("service", false, "not running").fix(start),
    });
    if let Some(warning) = &settings.config_warning {
        checks.push(
            soft("config-in-use", false, warning.clone())
                .fix("`shutup-and-run config reset`, then set the poll URL again"),
        );
    }

    // The flag itself, not the status file, which is up to a second behind.
    let paused = settings.history_path.exists()
        && crate::store::Store::open(&settings.history_path)
            .and_then(|store| store.draining())
            .unwrap_or(false);
    if paused {
        checks.push(
            soft("paused", false, "the worker is paused and takes no work")
                .fix("`shutup-and-run resume`"),
        );
    }

    if settings.history_path.exists() {
        if let Ok(bans) =
            crate::store::Store::open(&settings.history_path).and_then(|store| store.list_bans(now))
        {
            let ips: Vec<String> = bans.iter().map(|b| b.ip.clone()).collect();
            if let Some(local) = ips.iter().find(|ip| ip.starts_with("127.") || *ip == "::1") {
                checks.push(
                    check(
                        "bans",
                        false,
                        format!(
                            "this machine's own address {local} is banned after wrong tokens (401)"
                        ),
                    )
                    .fix(format!("`shutup-and-run bans lift {local}`")),
                );
            } else if !ips.is_empty() {
                checks.push(
                    soft("bans", false, format!("banned: {}", ips.join(", ")))
                        .fix("`shutup-and-run bans lift IP` if one of them is yours"),
                );
            }
        }
    }

    match &settings.poll_url {
        None => checks.push(
            soft("poll", false, "no poll URL: the worker is idle")
                .fix("`shutup-and-run config set poll_url URL --apply`"),
        ),
        Some(url) => {
            let reach = reach(url).await;
            let poll_error = fresh.and_then(|s| s["poll_error"].as_str());
            checks.push(match (reach, poll_error) {
                (Err(err), _) => check("poll", false, format!("cannot reach {url}: {err}")).fix(
                    "check the URL (`shutup-and-run config get poll_url`), the internet and DNS. \
                     On Windows, when the URL is on this PC or this LAN, the Windows firewall \
                     blocks WSL on a Public network: see docs/FOR-AGENTS.md",
                ),
                (Ok(()), Some(err)) if err.contains("not approved") => soft(
                    "poll",
                    false,
                    format!("{url} answers, and has not approved this machine ({err})"),
                )
                .fix(format!(
                    "approve worker {} at the poll URL; nothing to do on this machine",
                    settings.worker_name
                )),
                (Ok(()), Some(err)) => check("poll", false, format!("{url}: {err}")).fix(
                    "check the URL and its ?user= filter with `shutup-and-run config get poll_url`",
                ),
                (Ok(()), None) => check("poll", true, format!("{url} is reachable")),
            });
        }
    }

    Found {
        checks,
        config: config_shown,
        status,
        secrets,
    }
}

/// Can a TCP connection be opened to the poll URL's host? Nothing is sent,
/// so the poll URL sees no request and enrolls nobody.
async fn reach(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
    let host = parsed.host_str().ok_or("no host in the URL")?.to_string();
    let port = parsed.port_or_known_default().unwrap_or(443);
    match tokio::time::timeout(
        Duration::from_secs(8),
        tokio::net::TcpStream::connect((host.as_str(), port)),
    )
    .await
    {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) => Err(format!("{host}:{port}: {err}")),
        Err(_) => Err(format!("{host}:{port}: no answer in 8 s")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("shutup-and-run-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.json")
    }

    #[test]
    fn config_set_get_and_show_work_on_the_one_file() {
        let path = temp("set-get");
        let out = config_command(
            &path,
            &["set", "poll_url", "https://jobs.example/poll?user=ana"],
            false,
        );
        assert_eq!(out.code, 0, "{}", out.text);
        assert_eq!(out.json["ok"], true);
        assert!(path.exists() && config::backup_path(&path).exists());

        let got = config_command(&path, &["get", "poll_url"], false);
        assert_eq!(got.text, "https://jobs.example/poll?user=ana");
        assert_eq!(got.json["value"], "https://jobs.example/poll?user=ana");

        assert_eq!(
            config_command(&path, &["set", "max_slots", "3"], false).code,
            0
        );
        assert_eq!(
            config_command(&path, &["get", "max_slots"], false).json["value"],
            3
        );
        let list = config_command(&path, &["set", "allowlist", "alpine, ghcr.io/o/img"], false);
        assert_eq!(list.code, 0);
        let on_disk = config::read_json(&path).unwrap();
        assert_eq!(on_disk["allowlist"], json!(["alpine", "ghcr.io/o/img"]));

        // The token is masked unless asked for.
        let token = on_disk["token"].as_str().unwrap().to_string();
        let shown = config_command(&path, &["show"], false);
        assert_eq!(shown.json["config"]["token"], "***");
        assert!(!shown.text.contains(&token));
        assert_eq!(shown.json["valid"], true);
        let revealed = config_command(&path, &["get", "token"], true);
        assert_eq!(revealed.text, token);

        let unset = config_command(&path, &["unset", "poll_url"], false);
        assert_eq!(unset.code, 0);
        let gone = config_command(&path, &["get", "poll_url"], false);
        assert_eq!(gone.json["set"], false);
        assert_eq!(gone.text, "");
        assert_eq!(config_command(&path, &["validate"], false).code, 0);
    }

    #[test]
    fn an_invalid_value_is_refused_and_the_file_is_untouched() {
        let path = temp("invalid");
        assert_eq!(
            config_command(&path, &["set", "max_slots", "2"], false).code,
            0
        );
        let before = std::fs::read(&path).unwrap();

        for (key, value) in [
            ("max_slots", "0"),
            ("max_slots", "many"),
            ("poll_url", "ftp://nope"),
            ("poll_interval_secs", "0"),
            ("listen", "not-an-address"),
            ("token", "short"),
            ("allowlist", "bad image*"),
            ("admin_password", "a:b"),
        ] {
            let out = config_command(&path, &["set", key, value], false);
            assert_eq!(out.code, EXIT_INVALID, "{key}={value}");
            assert_eq!(out.json["ok"], false);
            assert!(!out.json["error"].as_str().unwrap().is_empty());
            assert_eq!(std::fs::read(&path).unwrap(), before, "{key}={value}");
        }
        // Removing a required key is refused too.
        assert_eq!(
            config_command(&path, &["unset", "token"], false).code,
            EXIT_INVALID
        );
        assert_eq!(
            config_command(&path, &["set", "nope", "1"], false).code,
            EXIT_USAGE
        );
        assert_eq!(
            config_command(&path, &["frobnicate"], false).code,
            EXIT_USAGE
        );
    }

    #[test]
    fn a_broken_file_validates_as_invalid_and_reset_repairs_it() {
        let path = temp("broken");
        assert_eq!(
            config_command(&path, &["set", "worker_name", "DESK-07"], false).code,
            0
        );
        let token = config::read_json(&path).unwrap()["token"].clone();
        std::fs::write(&path, b"{ this is not json").unwrap();
        assert_eq!(
            config_command(&path, &["validate"], false).code,
            EXIT_INVALID
        );
        assert_eq!(
            config_command(&path, &["set", "max_slots", "2"], false).code,
            EXIT_INVALID
        );

        // The service still starts: on the copy from the last save.
        let settings = Settings::load_json(&path);
        assert_eq!(settings.worker_name, "DESK-07");
        assert!(settings.config_warning.unwrap().contains("last save"));

        assert_eq!(config_command(&path, &["reset"], false).code, 0);
        let map = config::read_json(&path).unwrap();
        assert_eq!(map["token"], token, "reset keeps the machine's token");
        assert_eq!(map["worker_name"], "DESK-07");
        assert_eq!(map["max_slots"], 1);
        assert!(map.get("poll_url").is_none());
        assert!(Settings::load_json(&path).config_warning.is_none());

        // With no copy either, the built-in defaults run.
        std::fs::write(&path, b"[]").unwrap();
        std::fs::write(config::backup_path(&path), b"").unwrap();
        let settings = Settings::load_json(&path);
        assert_eq!(settings.max_slots, 1);
        assert!(settings.poll_url.is_none());
        assert!(settings
            .config_warning
            .unwrap()
            .contains("built-in defaults"));
    }

    #[test]
    fn the_report_and_the_issue_link_carry_no_secret() {
        let path = temp("report");
        for (key, value) in [
            ("token", "api-token-0123456789abcdef"),
            ("poll_token", "poll-token-0123456789abcdef"),
            ("admin_password", "hunter2-secret"),
            ("poll_url", "https://jobs.example/poll?user=ana"),
        ] {
            assert_eq!(
                config_command(&path, &["set", key, value], false).code,
                0,
                "{key}"
            );
        }
        let map = config::read_json(&path).unwrap();
        let secrets = config::secret_values(&map);
        let masked = Value::Object(config::masked(&map));
        // A check that quotes a secret by accident is scrubbed as well.
        let checks = vec![
            check("docker", true, "answers"),
            check("poll", false, "poll-token-0123456789abcdef was refused").fix("approve it"),
        ];
        let out = doctor_out("linux x86_64", &checks, Some(&masked), None, &secrets, true);
        assert_eq!(out.code, EXIT_FAILED);
        let everything = format!("{}\n{}", out.text, out.json);
        for secret in [
            "api-token-0123456789abcdef",
            "poll-token-0123456789abcdef",
            "hunter2-secret",
        ] {
            assert!(!everything.contains(secret), "{secret} leaked");
            assert!(
                !everything.contains(&percent(secret)),
                "{secret} leaked in the URL"
            );
        }
        // The poll URL and its filter are not a secret.
        assert!(out.json["report"].as_str().unwrap().contains("?user=ana"));
        let url = out.json["issue_url"].as_str().unwrap();
        assert!(url.starts_with(
            "https://github.com/nailtongomes/shutup-and-run/issues/new?template=worker-problem.md&title=doctor%3A%20poll%20failed&body="
        ));
        assert!(url.len() < 8000 * 3);
        assert_eq!(out.json["checks"][1]["status"], "fail");
        assert_eq!(out.json["checks"][1]["fix"], "approve it");
        assert!(out.text.contains("fix: approve it"));
        assert_eq!(checks_from_json(&out.json["checks"])[1].fix, "approve it");
    }

    #[test]
    fn status_reads_the_file_and_exits_1_when_the_worker_is_not_running() {
        let path = temp("status").with_file_name("status.json");
        assert_eq!(status_out(&path, 0).code, EXIT_FAILED);
        std::fs::write(
            &path,
            br#"{"state":"draining","slots_total":2,"slots_used":0,"updated_at":"2023-11-14T22:13:20Z","poll_error":"poll returned 403"}"#,
        )
        .unwrap();
        let fresh = status_out(&path, 1_700_000_010);
        assert_eq!(fresh.code, 0);
        assert_eq!(fresh.json["state"], "draining");
        assert_eq!(fresh.json["stale"], false);
        assert!(fresh.text.contains("poll_error: poll returned 403"));
        let stale = status_out(&path, 1_700_000_000 + 600);
        assert_eq!(stale.code, EXIT_FAILED);
        assert_eq!(stale.json["ok"], false);
        assert_eq!(stale.json["stale"], true);
    }

    #[test]
    fn flags_are_split_from_words() {
        let args: Vec<String> = ["set", "--json", "worker_name", "--", "--odd"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let parsed = split_args(&args).unwrap();
        assert!(parsed.json && !parsed.apply);
        assert_eq!(parsed.words, ["set", "worker_name", "--odd"]);
        assert!(split_args(&["--nope".to_string()]).is_err());
        assert!(usage().contains("config set KEY VALUE"));
        assert!(usage().contains("Exit codes"));
    }
}
