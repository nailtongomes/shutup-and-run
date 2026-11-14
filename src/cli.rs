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
    the whole config file. Secrets, and the values of the poll URL's query,
    print as *** unless --reveal.
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
    poll URL. Every failed line has a `hint:` with the command to run.
    --report also prints a diagnostic report with secrets masked and a URL
    that opens a new GitHub issue with it filled in. Exit 1 when unhealthy.
shutup-and-run pause | resume
    stop or start taking new work. Running containers are left alone.
shutup-and-run jobs [--limit N]
    the last N jobs, newest first (default 20): id, image, start, end,
    duration, exit code, status (running, ok, failed, timeout, killed) and
    why it failed. Never a job's env, command, payload or logs.

Every failure, and every doctor or check line that is not ok, carries a
`hint`: the next step, short, with the command when there is one. In JSON it
is the `hint` field beside `code`. Its language is the config key `language`:
    shutup-and-run config set language pt
(en is the default). docs/FOR-AGENTS.md lists every code with its hint.

The poll URL may carry a query that pairs this machine with a person
(?cpf=...&email=...). It is only a filter: the poll token is what the server
approves. Its values print as *** everywhere; `config get poll_url --reveal`
shows them.

Resource limits, for a machine someone is working on:
    shutup-and-run config set resource_profile shared-pc --apply
turns on the shared-PC profile (the default on Windows): each job gets at most
1 CPU and 2g of memory with no extra swap, and no new job starts with less
than 2g of memory or 5g of disk free. On a machine with under 4g the two
memory figures are half of what it has. `none` turns the profile off. Each
figure has its own key: max_job_cpus, max_job_memory, min_free_memory,
min_free_disk (sizes like 2g or 512m; 0 turns a min_free check off). A job
that asks for more than the machine's limit is refused with the limit in the
answer. `status --json` shows the limits in force under `limits`, and
`waiting_for` says why no job starts while memory or disk is short.

First run, with no orchestrator: `shutup-and-run demo --help` documents
version, config check, job validate, demo, demo sign and certificate.

The config is one JSON file: /etc/shutup-and-run/config.json (on Windows,
inside the shutup-and-run WSL distribution). Schema:
docs/schemas/worker-config.schema.json. Guide: docs/FOR-AGENTS.md."
}

// ---------------------------------------------------------------------------
// Output

#[derive(Debug)]
pub struct Out {
    pub code: u8,
    pub json: Value,
    pub text: String,
}

impl Out {
    pub(crate) fn ok(mut json: Value, text: impl Into<String>) -> Self {
        json["ok"] = Value::Bool(true);
        Out {
            code: 0,
            json,
            text: text.into(),
        }
    }

    pub(crate) fn fail(code: u8, error: impl Into<String>) -> Self {
        let error = error.into();
        Out {
            code,
            json: json!({"ok": false, "error": error}),
            text: error,
        }
    }
}

pub(crate) fn emit(mut out: Out, as_json: bool) -> ExitCode {
    crate::hints::attach(&mut out.json, out.code);
    if as_json {
        println!("{}", out.json);
    } else if out.code == 0 {
        if !out.text.is_empty() {
            println!("{}", out.text);
        }
    } else {
        eprintln!("shutup-and-run: {}", out.text);
        if let Some(hint) = out.json["hint"].as_str() {
            eprintln!("hint: {hint}");
        }
    }
    ExitCode::from(out.code)
}

/// The JSON of a failure that has only a message, for the places that print
/// one without an `Out`.
pub fn failure_json(code: &str, error: &str) -> Value {
    let mut value = json!({"ok": false, "code": code, "error": error});
    crate::hints::attach(&mut value, EXIT_FAILED);
    value
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
                let loaded = Settings::from_json_map(&map);
                let problem = loaded.as_ref().err().map(|e| e.0.clone());
                // What runs, defaults filled in: a key that is not in the
                // file still has a value, and this is where it is seen.
                let effective = loaded.as_ref().ok().map(effective_json);
                let in_force = loaded
                    .as_ref()
                    .ok()
                    .map(|s| {
                        format!(
                            "\nin force, defaults filled in:\n  poll_interval_secs {}\n  {}",
                            s.poll_interval.as_secs(),
                            crate::status::Limits::of(s).line()
                        )
                    })
                    .unwrap_or_default();
                let map = shown(&map);
                Out::ok(
                    json!({"path": path, "config": map, "valid": problem.is_none(), "problem": problem, "effective": effective}),
                    format!("{}{in_force}", config::to_text(&map).trim_end()),
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
        ["validate"] => match load().and_then(|map| Settings::from_json_map(&map)) {
            Ok(settings) => {
                let warning = poll_interval_note(&settings);
                Out::ok(
                    json!({"path": path, "warning": warning}),
                    match &warning {
                        Some(note) => format!("{} is valid\nwarning: {note}", path.display()),
                        None => format!("{} is valid", path.display()),
                    },
                )
            }
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

/// The values the service runs on for the keys that have a default worth
/// seeing: the resource limits and the poll interval.
pub fn effective_json(settings: &Settings) -> Value {
    let mut value = serde_json::to_value(crate::status::Limits::of(settings)).unwrap_or_default();
    value["poll_interval_secs"] = json!(settings.poll_interval.as_secs());
    value["max_slots"] = json!(settings.max_slots);
    value
}

/// Said wherever a config is looked at, when its interval was under the minimum.
pub fn poll_interval_note(settings: &Settings) -> Option<String> {
    settings.poll_interval_raised_from.map(|asked| {
        format!(
            "poll_interval_secs is {asked} in the config, under the minimum of {min}: the worker polls every {min} s",
            min = config::MIN_POLL_INTERVAL_SECS
        )
    })
}

fn poll_interval_check(settings: &Settings) -> Option<Check> {
    poll_interval_note(settings)
        .map(|note| soft("poll-interval", false, note).coded("POLL_INTERVAL_RAISED"))
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
    if let Some(limits) = serde_json::from_value::<crate::status::Limits>(status["limits"].clone())
        .ok()
        .filter(|limits| !limits.profile.is_empty())
    {
        text.push_str(&format!("limits: {}\n", limits.line()));
    }
    if let (Some(at), Some(how)) = (
        status["last_poll_at"].as_str(),
        status["last_poll_status"].as_str(),
    ) {
        text.push_str(&format!("last poll: {at} ({how})\n"));
    }
    for field in ["last_error", "poll_error", "waiting_for", "config_warning"] {
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
    /// What the hint of this check's code names: an address, an image, a
    /// number. It fills the `{}` of the catalogue's sentence.
    pub arg: String,
    /// A stable identifier of what was found, for a program: `DOCKER_UNAVAILABLE`.
    /// Empty means the default, the name and the outcome: `DOCKER_OK`.
    pub code: String,
    /// The next step, already in a language: set only on a check read back
    /// from another process. Empty means the catalogue's hint for the code.
    pub hint: String,
}

pub fn check(name: &str, ok: bool, detail: impl Into<String>) -> Check {
    Check {
        name: name.to_string(),
        ok,
        soft: false,
        detail: detail.into(),
        arg: String::new(),
        code: String::new(),
        hint: String::new(),
    }
}

pub fn soft(name: &str, ok: bool, detail: impl Into<String>) -> Check {
    Check {
        soft: true,
        ..check(name, ok, detail)
    }
}

impl Check {
    /// The detail the hint names. Kept only on a check that is not ok.
    pub fn with(mut self, arg: impl Into<String>) -> Self {
        if !self.ok {
            self.arg = arg.into();
        }
        self
    }

    /// Names what went wrong. Kept only on a check that is not ok, so one
    /// expression can build both outcomes.
    pub fn coded(mut self, code: &str) -> Self {
        if !self.ok {
            self.code = code.to_string();
        }
        self
    }

    /// The stable code: the one given, or NAME_OK, NAME_WARNING, NAME_FAILED.
    pub fn code(&self) -> String {
        if !self.code.is_empty() {
            return self.code.clone();
        }
        let name = self.name.to_ascii_uppercase().replace('-', "_");
        let outcome = match self.mark() {
            "ok" => "OK",
            "warn" => "WARNING",
            _ => "FAILED",
        };
        format!("{name}_{outcome}")
    }

    /// The next step in the configured language. Empty when the check is ok.
    pub fn hint(&self) -> String {
        if self.ok {
            String::new()
        } else if !self.hint.is_empty() {
            self.hint.clone()
        } else {
            crate::hints::hint(&self.code()).replace("{}", &self.arg)
        }
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
        if !c.ok {
            text.push_str(&format!("     {:<22} hint: {}\n", "", c.hint()));
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
                "code": c.code(),
                "status": c.mark(),
                "detail": c.detail,
                "hint": if c.ok { Value::Null } else { Value::String(c.hint()) },
                // The same sentence under its old name, for a reader of 0.2.8.
                "fix": if c.ok { Value::Null } else { Value::String(c.hint()) },
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
                    arg: String::new(),
                    code: c["code"].as_str().unwrap_or("").to_string(),
                    hint: c["hint"].as_str().unwrap_or("").to_string(),
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
    recent: &[String],
) -> String {
    let (lines, healthy) = report(checks);
    let build = crate::devx::version_json();
    let mut text = format!(
        "shutup-and-run {} on {platform}\nbuild: {} {}, runtime {}, api {}\nresult: {}\n",
        env!("CARGO_PKG_VERSION"),
        build["platform"].as_str().unwrap_or("?"),
        build["architecture"].as_str().unwrap_or("?"),
        build["runtime"].as_str().unwrap_or("?"),
        build["api_version"].as_str().unwrap_or("?"),
        if healthy { "healthy" } else { "unhealthy" }
    );
    // What a person triaging an issue looks for first, before the raw JSON.
    let failing: Vec<String> = checks.iter().filter(|c| !c.ok).map(|c| c.code()).collect();
    if !failing.is_empty() {
        text.push_str(&format!("codes: {}\n", failing.join(", ")));
    }
    if let Some(status) = status {
        if let Some(limits) =
            serde_json::from_value::<crate::status::Limits>(status["limits"].clone())
                .ok()
                .filter(|limits| !limits.profile.is_empty())
        {
            text.push_str(&format!("limits: {}\n", limits.line()));
        }
        for field in ["last_error", "poll_error", "waiting_for", "config_warning"] {
            if let Some(value) = status[field].as_str() {
                text.push_str(&format!("{field}: {value}\n"));
            }
        }
    }
    if let Some(config) = config {
        text.push_str(&format!(
            "certificate passthrough: {}\n",
            if config["certificate_enabled"] == true {
                "ON"
            } else {
                "off"
            }
        ));
    }
    text.push_str(&format!("\nchecks:\n{lines}"));
    if !recent.is_empty() {
        text.push_str("\nrecent warnings and errors:\n");
        for line in recent {
            text.push_str(&format!("{line}\n"));
        }
    }
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
    recent: &[String],
    with_report: bool,
) -> Out {
    // Whatever a check quoted, no secret leaves in any of the outputs.
    let checks: Vec<Check> = checks
        .iter()
        .map(|c| Check {
            detail: scrub(&c.detail, secrets),
            hint: scrub(&c.hint(), secrets),
            ..c.clone()
        })
        .collect();
    let checks = checks.as_slice();
    let (lines, healthy) = report(checks);
    let full = diagnostic_report(platform, checks, config, status, secrets, recent);
    let url = issue_url(&full, checks);
    let mut text = lines;
    text.push_str(if healthy {
        "the worker is ready"
    } else {
        "something is wrong: follow the `hint:` under each FAIL, then run the doctor again.\n\
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
        &[],
        parsed.report,
    );
    let mut out = out;
    crate::hints::attach(&mut out.json, out.code);
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
    let (mut found, settings) = base_checks().await;
    if let Some(settings) = settings {
        service_checks(&mut found, &settings).await;
    }
    found
}

/// The config and Docker: what every other check stands on. The settings
/// come back when there is something to run on, a fallback included.
pub async fn base_checks() -> (Found, Option<Settings>) {
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
                Err(err) => checks.push(check("config", false, err.0).coded("CONFIG_INVALID")),
            }
            Some(Settings::load_json(path))
        }
        None => match Settings::load() {
            Ok(settings) => {
                checks.push(check("config", true, "config.toml and environment"));
                Some(settings)
            }
            Err(err) => {
                checks.push(check("config", false, err.0).coded("CONFIG_INVALID"));
                None
            }
        },
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
        Err(err) => check("docker", false, err).coded("DOCKER_UNAVAILABLE"),
    });

    checks.extend(settings.as_ref().and_then(poll_interval_check));
    (
        Found {
            checks,
            config: config_shown,
            status: None,
            secrets,
        },
        settings,
    )
}

/// What only a running service can answer: its state, a pause, bans, the
/// poll URL as the service last saw it.
async fn service_checks(found: &mut Found, settings: &Settings) {
    let mut checks = std::mem::take(&mut found.checks);
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
            check("service", false, format!("failed: {last_error}")).coded("SERVICE_PORT_IN_USE")
        }
        Some("failed") => {
            check("service", false, format!("failed: {last_error}")).coded("SERVICE_FAILED")
        }
        _ => check("service", false, "not running").coded("SERVICE_NOT_RUNNING"),
    });
    checks.push(resources_check(
        settings,
        crate::resources::memory_total(),
        crate::resources::read(&crate::resources::disk_path(&settings.history_path)),
    ));
    checks.extend(certificate_check(settings));
    if let Some(warning) = &settings.config_warning {
        checks.push(soft("config-in-use", false, warning.clone()).coded("CONFIG_FALLBACK_IN_USE"));
    }

    // The flag itself, not the status file, which is up to a second behind.
    let paused = settings.history_path.exists()
        && crate::store::Store::open(&settings.history_path)
            .and_then(|store| store.draining())
            .unwrap_or(false);
    if paused {
        checks.push(
            soft("paused", false, "the worker is paused and takes no work").coded("WORKER_PAUSED"),
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
                    .coded("LOCAL_ADDRESS_BANNED")
                    .with(local.clone()),
                );
            } else if !ips.is_empty() {
                checks.push(
                    soft("bans", false, format!("banned: {}", ips.join(", ")))
                        .coded("ADDRESS_BANNED"),
                );
            }
        }
    }

    match &settings.poll_url {
        None => checks
            .push(soft("poll", false, "no poll URL: the worker is idle").coded("POLL_URL_UNSET")),
        Some(raw_url) => {
            let url = raw_url;
            let reach = reach(url).await;
            // The query pairs the machine with a person: never printed.
            let url = &config::mask_query(url);
            let poll_error = fresh.and_then(|s| s["poll_error"].as_str());
            checks.push(match (reach, poll_error) {
                (Err(err), _) => check("poll", false, format!("cannot reach {url}: {err}"))
                    .coded("POLL_UNREACHABLE"),
                (Ok(()), Some(err)) if err.contains("not approved") => soft(
                    "poll",
                    false,
                    format!("{url} answers, and has not approved this machine ({err})"),
                )
                .coded("POLL_NOT_APPROVED")
                .with(settings.worker_name.clone()),
                (Ok(()), Some(err)) => match http_status_in(err) {
                    Some(status) => poll_answer_check(url, status, false),
                    None => check("poll", false, format!("{url}: {err}")).coded("POLL_ERROR"),
                },
                // A running service polled and had nothing to complain about.
                (Ok(()), None) if fresh.is_some() => {
                    check("poll", true, format!("{url} answers the service"))
                }
                // No service to say so: ask once, as `config check` does.
                (Ok(()), None) => match probe(settings, raw_url).await {
                    Ok(status) => poll_answer_check(url, status, true),
                    Err(err) => check("poll", false, format!("cannot reach {url}: {err}"))
                        .coded("POLL_UNREACHABLE"),
                },
            });
        }
    }
    found.checks = checks;
    found.status = status;
}

/// Present only while passthrough is on: is there a certificate to give?
pub fn certificate_check(settings: &Settings) -> Option<Check> {
    if !settings.certificate_enabled {
        return None;
    }
    let found = crate::certificate::validate_imported(&settings.certificate_dir);
    Some(if found.ok {
        check(
            "certificate",
            true,
            format!(
                "passthrough is ON: every container gets the certificate (valid until {})",
                found.not_after.as_deref().unwrap_or("?")
            ),
        )
    } else {
        check("certificate", false, found.detail.clone()).coded(found.code)
    })
}

/// The limits in force, and whether the machine can take a job under them
/// right now. Never a hard failure: the worker runs, and waits.
pub fn resources_check(
    settings: &Settings,
    memory_total: Option<u64>,
    free: crate::resources::Free,
) -> Check {
    use crate::resources::human;
    let limits = crate::status::Limits::of(settings).line();
    if let Some(short) =
        crate::resources::shortage(free, settings.min_free_memory, settings.min_free_disk)
    {
        return soft(
            "resources",
            false,
            format!("{short}: no new job starts until it clears. {limits}"),
        )
        .coded(if short.resource == "disk" {
            "LOW_DISK"
        } else {
            "LOW_MEMORY"
        });
    }
    if let (Some(per_job), Some(total)) = (settings.max_job_memory, memory_total) {
        let fit = (total / per_job.max(1)).max(1);
        if settings.max_slots as u64 > fit {
            return soft(
                "resources",
                false,
                format!(
                    "{} jobs of {} each is more than the {} this machine has. {limits}",
                    settings.max_slots,
                    human(per_job),
                    human(total)
                ),
            )
            .coded("SLOTS_EXCEED_MEMORY")
            .with(fit.to_string());
        }
    }
    let free_now = match (free.memory, free.disk) {
        (Some(memory), Some(disk)) => {
            format!("; free now: memory {}, disk {}", human(memory), human(disk))
        }
        _ => String::new(),
    };
    check("resources", true, format!("{limits}{free_now}"))
}

/// Can a TCP connection be opened to the poll URL's host? Nothing is sent,
/// so the poll URL sees no request and enrolls nobody.
/// The status in the service's own "poll returned 404 Not Found".
pub fn http_status_in(poll_error: &str) -> Option<u16> {
    poll_error
        .strip_prefix("poll returned ")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// What an HTTP status of the poll URL means. `url` is already masked.
/// `probe` is the config check's own request, which carries no token, so
/// 401 and 403 are the right answer to it; from the service they mean the
/// machine is not approved, which the doctor says before it gets here.
pub fn poll_answer_check(url: &str, status: u16, probe: bool) -> Check {
    let line = |ok: bool, detail: String| {
        if probe {
            // The service out there, not the config in here: a warning.
            soft("poll", ok, detail)
        } else {
            check("poll", ok, detail)
        }
    };
    match status {
        200..=299 => check("poll", true, format!("{url} answers (HTTP {status})")),
        401 | 403 if probe => check(
            "poll",
            true,
            format!("{url} answers and asks for the token (HTTP {status})"),
        ),
        429 => soft(
            "poll",
            false,
            format!("{url} answered HTTP 429: it asks this machine to slow down"),
        )
        .coded("POLL_RATE_LIMITED"),
        _ => line(
            false,
            format!("{url} answered HTTP {status}: that is not a poll endpoint, or it is failing"),
        )
        .coded("POLL_HTTP_ERROR"),
    }
}

/// One request to the poll URL that cannot take a task away: no token, and
/// `free_slots` 0. A poll endpoint answers 401, 403 or 204; a wrong path
/// answers 404. Returns the HTTP status.
pub(crate) async fn probe(settings: &Settings, url: &str) -> Result<u16, String> {
    let response = crate::poll::client()?
        .post(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .json(&crate::poll::poll_body(settings, 0))
        .send()
        .await
        .map_err(|err| err.without_url().to_string())?;
    Ok(response.status().as_u16())
}

pub(crate) async fn reach(url: &str) -> Result<(), String> {
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

        // The query pairs the machine with a person: masked unless asked.
        let got = config_command(&path, &["get", "poll_url"], false);
        assert_eq!(got.text, "https://jobs.example/poll?user=***");
        assert_eq!(got.json["value"], "https://jobs.example/poll?user=***");
        let got = config_command(&path, &["get", "poll_url"], true);
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
            // Obviously made up: a pairing query with a tax id, an e-mail and a key.
            (
                "poll_url",
                "https://jobs.example/poll?cpf=00000000000&email=nobody%40example.com&chave=FAKE-PAIRING-KEY",
            ),
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
        // A check that quotes a secret, or the poll URL, by accident is
        // scrubbed as well.
        let checks = vec![
            check("docker", true, "answers"),
            check(
                "poll",
                false,
                "poll-token-0123456789abcdef was refused at \
                 https://jobs.example/poll?cpf=00000000000&email=nobody@example.com",
            ),
        ];
        let out = doctor_out(
            "linux x86_64",
            &checks,
            Some(&masked),
            None,
            &secrets,
            &[],
            true,
        );
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
        // The poll URL is in the report, and the values of its query are not:
        // they pair the machine with a person.
        let report = out.json["report"].as_str().unwrap();
        assert!(report.contains("https://jobs.example/poll?cpf=***&email=***&chave=***"));
        for private in ["00000000000", "nobody", "example.com", "FAKE-PAIRING-KEY"] {
            assert!(!everything.contains(private), "{private} leaked");
        }
        // `config show` masks the same way, and --reveal is the way to read it.
        let shown = config_command(&path, &["show"], false);
        assert_eq!(
            shown.json["config"]["poll_url"],
            "https://jobs.example/poll?cpf=***&email=***&chave=***"
        );
        assert!(!shown.text.contains("00000000000") && !shown.text.contains("nobody"));
        let revealed = config_command(&path, &["show"], true);
        assert!(revealed.text.contains("cpf=00000000000"));
        // The service still polls the whole URL.
        assert_eq!(
            Settings::load_json(&path).poll_url.as_deref(),
            Some("https://jobs.example/poll?cpf=00000000000&email=nobody%40example.com&chave=FAKE-PAIRING-KEY")
        );
        let url = out.json["issue_url"].as_str().unwrap();
        assert!(url.starts_with(
            "https://github.com/nailtongomes/shutup-and-run/issues/new?template=worker-problem.md&title=doctor%3A%20poll%20failed&body="
        ));
        assert!(url.len() < 8000 * 3);
        assert_eq!(out.json["checks"][1]["status"], "fail");
        // One next step, under `hint` and under its old name `fix`.
        let hint = out.json["checks"][1]["hint"].as_str().unwrap();
        assert!(!hint.is_empty());
        assert_eq!(out.json["checks"][1]["fix"], hint);
        assert!(out.text.contains(&format!("hint: {hint}")) && !out.text.contains("fix:"));
        assert_eq!(checks_from_json(&out.json["checks"])[1].hint(), hint);
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
    fn a_poll_interval_under_30_is_refused_with_exit_3_and_an_old_one_is_warned_about() {
        let path = temp("poll-min");
        let set = |words: &[&str]| config_command(&path, words, false);
        assert_eq!(set(&["set", "max_slots", "1"]).code, 0);
        // A Linux server on every platform: Windows defaults to shared-pc.
        assert_eq!(set(&["set", "worker_os", "linux"]).code, 0);
        let before = std::fs::read_to_string(&path).unwrap();
        for low in ["1", "2", "29"] {
            let out = set(&["set", "poll_interval_secs", low]);
            assert_eq!(out.code, EXIT_INVALID, "{low}");
            assert!(out.json["error"].as_str().unwrap().contains("at least 30"));
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert_eq!(set(&["set", "poll_interval_secs", "30"]).code, 0);

        // The default, and the limits, are visible though no key sets them.
        assert_eq!(set(&["unset", "poll_interval_secs"]).code, 0);
        let shown = set(&["show"]);
        assert_eq!(shown.json["effective"]["poll_interval_secs"], 30);
        assert_eq!(shown.json["effective"]["profile"], "none");
        assert!(shown.json["effective"]
            .get("max_job_memory_bytes")
            .is_some());
        assert!(shown.text.contains("in force, defaults filled in:"));
        assert!(shown.text.contains("poll_interval_secs 30"));
        assert!(shown.text.contains("per job: cpu"));

        // A config installed by an older release, with 2: valid, with a warning
        // and a soft check that names the fix.
        let mut old = config::read_json(&path).unwrap();
        old.insert("poll_interval_secs".into(), json!(2));
        std::fs::write(&path, config::to_text(&old)).unwrap();
        let valid = set(&["validate"]);
        assert_eq!(valid.code, 0);
        assert!(valid.json["warning"]
            .as_str()
            .unwrap()
            .contains("polls every 30 s"));
        assert_eq!(set(&["show"]).json["effective"]["poll_interval_secs"], 30);
        let warned = poll_interval_check(&Settings::load_json(&path)).unwrap();
        assert!(!warned.ok && warned.soft);
        assert_eq!(warned.code(), "POLL_INTERVAL_RAISED");
        assert!(warned
            .hint()
            .contains("config set poll_interval_secs 30 --apply"));
        assert!(report(&[warned]).1, "a warning must not fail the doctor");
        assert!(
            poll_interval_check(&Settings::from_json_map(&config::default_map()).unwrap())
                .is_none()
        );
    }

    #[test]
    fn the_language_of_the_hints_is_a_config_key_and_every_failure_has_a_hint() {
        use crate::hints::{hint_in, Lang};
        let path = temp("language");
        let set = |words: &[&str]| config_command(&path, words, false);
        assert_eq!(set(&["set", "language", "pt"]).code, 0);
        assert_eq!(Settings::load_json(&path).language, "pt");
        let before = std::fs::read_to_string(&path).unwrap();
        let refused = set(&["set", "language", "fr"]);
        assert_eq!(refused.code, EXIT_INVALID);
        assert!(refused.json["error"].as_str().unwrap().contains("en or pt"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert_eq!(set(&["unset", "language"]).code, 0);
        assert_eq!(Settings::load_json(&path).language, "en");

        // Each kind of CLI failure leaves with a code and a hint.
        for (mut out, code) in [
            (set(&["set", "max_slots", "0"]), "INVALID_VALUE"),
            (set(&["set", "no_such_key", "1"]), "USAGE"),
            (set(&["frobnicate"]), "USAGE"),
            (status_out(&path.with_file_name("status.json"), 0), "FAILED"),
        ] {
            assert_ne!(out.code, 0);
            crate::hints::attach(&mut out.json, out.code);
            assert_eq!(out.json["code"], code);
            assert_eq!(out.json["hint"], hint_in(code, Lang::En));
        }
        // The doctor's text carries the hint under the line that failed.
        let checks = [soft("paused", false, "the worker is paused").coded("WORKER_PAUSED")];
        assert!(report(&checks)
            .0
            .contains("hint: Run `shutup-and-run resume`."));
        assert_eq!(
            hint_in("WORKER_PAUSED", Lang::Pt),
            "Rode `shutup-and-run resume`."
        );
    }

    #[test]
    fn the_resource_profile_is_set_and_inspected_from_the_cli() {
        let path = temp("resources");
        let set = |words: &[&str]| config_command(&path, words, false);
        assert_eq!(set(&["set", "resource_profile", "shared-pc"]).code, 0);
        assert_eq!(set(&["set", "max_job_memory", "1g"]).code, 0);
        assert_eq!(set(&["set", "max_job_cpus", "0.5"]).code, 0);
        let before = std::fs::read_to_string(&path).unwrap();
        for (key, bad) in [
            ("resource_profile", "gaming"),
            ("max_job_cpus", "lots"),
            ("max_job_memory", "1m"),
            ("min_free_disk", "five"),
        ] {
            let out = set(&["set", key, bad]);
            assert_eq!(out.code, EXIT_INVALID, "{key}={bad}");
            assert!(out.json["error"].as_str().unwrap().contains(key));
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert_eq!(set(&["get", "max_job_memory"]).text, "1g");
        let keys = set(&["keys"]).text;
        for key in ["resource_profile", "max_job_cpus", "min_free_memory"] {
            assert!(keys.contains(key), "{key}");
        }
        let settings = Settings::from_json_map(&config::read_json(&path).unwrap()).unwrap();
        assert_eq!(settings.max_job_memory, Some(1 << 30));
        assert_eq!(set(&["unset", "max_job_memory"]).code, 0);

        // The doctor's line: fine, short of memory, short of disk, too many slots.
        const GIB: u64 = 1 << 30;
        let mut settings = settings;
        settings.max_job_memory = Some(2 * GIB);
        settings.min_free_memory = 2 * GIB;
        settings.min_free_disk = 5 * GIB;
        let free = |memory, disk| crate::resources::Free {
            memory: Some(memory),
            disk: Some(disk),
        };
        let fine = resources_check(&settings, Some(8 * GIB), free(4 * GIB, 50 * GIB));
        assert!(fine.ok, "{}", fine.detail);
        assert!(fine.detail.starts_with(
            "profile shared-pc; per job: cpu 0.5, memory 2.0 GiB; a new job needs free memory 2.0 GiB, free disk 5.0 GiB; free now: memory 4.0 GiB, disk 50.0 GiB"
        ));
        let low = resources_check(&settings, Some(8 * GIB), free(GIB, 50 * GIB));
        assert!(!low.ok && low.soft);
        assert!(low
            .detail
            .starts_with("low on memory: 1.0 GiB free, 2.0 GiB needed"));
        assert!(low.hint().contains("min_free_memory"));
        let full = resources_check(&settings, Some(8 * GIB), free(4 * GIB, GIB));
        assert!(full.detail.starts_with("low on disk"));
        assert!(full.hint().contains("docker image prune"));
        settings.max_slots = 3;
        let many = resources_check(&settings, Some(4 * GIB), free(4 * GIB, 50 * GIB));
        assert!(!many.ok && many.soft);
        assert!(
            many.hint().contains("config set max_slots 2 --apply"),
            "{}",
            many.hint()
        );
        // A soft line never makes the doctor exit 1.
        assert!(report(&[many]).1);
    }

    #[test]
    fn status_shows_the_limits_and_why_the_worker_waits() {
        let path = temp("status-limits").with_file_name("status.json");
        let now = 1_700_000_000;
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "state": "idle", "slots_total": 1, "slots_used": 0,
                "updated_at": crate::status::unix_to_iso(now),
                "waiting_for": "low on memory: 512 MiB free, 2.0 GiB needed",
                "limits": {"profile": "shared-pc", "max_job_cpus": 1.0,
                    "max_job_memory_bytes": 2147483648u64,
                    "min_free_memory_bytes": 2147483648u64, "min_free_disk_bytes": 0},
            }))
            .unwrap(),
        )
        .unwrap();
        let out = status_out(&path, now);
        assert_eq!(out.code, 0);
        assert!(out
            .text
            .contains("limits: profile shared-pc; per job: cpu 1, memory 2.0 GiB"));
        assert!(out.text.contains("waiting_for: low on memory"));
        assert_eq!(out.json["limits"]["max_job_cpus"], 1.0);
    }

    #[test]
    fn every_check_has_a_stable_code_and_the_old_fields_stay() {
        let checks = vec![
            check("docker", true, "answers"),
            check("docker", false, "no socket").coded("DOCKER_UNAVAILABLE"),
            soft("wsl-limits", false, "above"),
            check("service", false, "down"),
            // A code is kept only on a check that is not ok.
            check("config", true, "fine").coded("CONFIG_INVALID"),
        ];
        let json = checks_json(&checks);
        let codes: Vec<&str> = json
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["code"].as_str().unwrap())
            .collect();
        assert_eq!(
            codes,
            vec![
                "DOCKER_OK",
                "DOCKER_UNAVAILABLE",
                "WSL_LIMITS_WARNING",
                "SERVICE_FAILED",
                "CONFIG_OK"
            ]
        );
        // What a consumer written before the codes reads is unchanged.
        assert_eq!(
            json[1],
            json!({"name": "docker", "code": "DOCKER_UNAVAILABLE", "status": "fail",
                   "detail": "no socket",
                   "fix": crate::hints::hint_in("DOCKER_UNAVAILABLE", crate::hints::Lang::En),
                   "hint": crate::hints::hint_in("DOCKER_UNAVAILABLE", crate::hints::Lang::En)})
        );
        // A line that is fine has no hint; one that is not always has one.
        assert!(json[0]["hint"].is_null());
        for line in json.as_array().unwrap() {
            assert_eq!(line["hint"].is_string(), line["status"] != "ok", "{line}");
        }
        // A hint that came from another process, in its language, is kept.
        let mut inner = json.clone();
        inner[1]["hint"] = "Inicie o Docker.".into();
        assert_eq!(
            checks_json(&checks_from_json(&inner))[1]["hint"],
            "Inicie o Docker."
        );
        // The Windows doctor carries the inner doctor's codes through.
        let back = checks_from_json(&json);
        assert_eq!(back[1].code(), "DOCKER_UNAVAILABLE");
        assert_eq!(checks_json(&back), json);
        // The report names the failing codes, the build and the certificate state.
        let config = json!({"certificate_enabled": true, "token": "***"});
        let status = json!({"poll_error": "poll returned 500", "limits": {"profile": "shared-pc",
            "max_job_cpus": 1.0, "max_job_memory_bytes": 1073741824u64,
            "min_free_memory_bytes": 0, "min_free_disk_bytes": 0}});
        let text = diagnostic_report(
            "linux x86_64",
            &checks,
            Some(&config),
            Some(&status),
            &["hunter2hunter2".to_string()],
            &["WARN poll: hunter2hunter2 refused".to_string()],
        );
        assert!(text.contains("codes: DOCKER_UNAVAILABLE, WSL_LIMITS_WARNING, SERVICE_FAILED"));
        assert!(text.contains("build: ") && text.contains("api v1"));
        assert!(text.contains("limits: profile shared-pc"));
        assert!(text.contains("poll_error: poll returned 500"));
        assert!(text.contains("certificate passthrough: ON"));
        assert!(text.contains("recent warnings and errors:\nWARN poll: *** refused"));
        assert!(!text.contains("hunter2"));
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
