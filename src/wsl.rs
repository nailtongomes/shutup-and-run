//! Windows 11 supervisor. `shutup-and-run.exe wsl ...` puts the Linux
//! service inside a WSL2 distribution of its own and keeps it running.
//!
//! The .exe does not start containers itself. It imports an Ubuntu rootfs as
//! the distribution `shutup-and-run`, installs Docker Engine and the musl
//! binary inside it, writes a pull-only config there, and runs that binary
//! with `wsl.exe` as a scheduled task at logon. Docker Desktop is not used.
//!
//! Every command here is a thin shell-out to `wsl.exe` or `schtasks.exe`.
//! The functions that build command lines, parse `wsl.exe` output and render
//! config files are pure and tested on any platform. The commands that need
//! Windows say so when `wsl.exe` is not found.

use crate::cli::{check, report, soft, Check};
use crate::config::JsonMap;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

/// The WSL distribution this supervisor owns. Nothing else lives in it.
pub const DISTRO: &str = "shutup-and-run";
/// The scheduled task that runs `wsl serve --background` at logon.
pub const TASK_NAME: &str = "shutup-and-run";

const ROOTFS_FILE: &str = "ubuntu-noble-wsl-amd64-24.04lts.rootfs.tar.gz";
const ROOTFS_URL: &str =
    "https://cloud-images.ubuntu.com/wsl/releases/24.04/current/ubuntu-noble-wsl-amd64-24.04lts.rootfs.tar.gz";
const MUSL_ASSET: &str = "shutup-and-run-x86_64-unknown-linux-musl";
const RELEASE_URL: &str = "https://github.com/nailtongomes/shutup-and-run/releases/download";

const INNER_BIN: &str = "/usr/local/bin/shutup-and-run";
const INNER_SERVE: &str = "/usr/local/bin/shutup-and-run-serve";
const INNER_CONFIG: &str = "/etc/shutup-and-run/config.json";
/// What installs up to 0.2.4 wrote. Read once, to carry the settings over.
const OLD_CONFIG: &str = "/etc/shutup-and-run/config.toml";
const INNER_STATUS: &str = "/var/lib/shutup-and-run/status.json";
const HOST_BACKUP: &str = "config.backup.json";
const SHORTCUT_FILE: &str = "shutup-and-run.url";
const INNER_ENV: &str = "/etc/shutup-and-run/env";
const INNER_LISTEN: &str = "127.0.0.1:8787";

/// The service's process name inside the distribution, as `pgrep -x` sees it.
const SERVICE_PROCESS: &str = "shutup-and-run";
const SUPERVISOR_LOCK: &str = "supervisor.lock";

const DEFAULT_SMOKE_IMAGE: &str = "hello-world";
const LOG_FILE: &str = "supervisor.log";
const LOG_ROTATE_BYTES: u64 = 10 * 1024 * 1024;
const RESTART_MIN: Duration = Duration::from_secs(5);
const RESTART_MAX: Duration = Duration::from_secs(60);

/// Step 4 of the install, in order. Ubuntu's docker.io has a systemd unit and
/// no init script, so the daemon is started by systemd, which `/etc/wsl.conf`
/// turns on. Each script is safe to run again on a machine that has it all.
const DOCKER_STEPS: &[(&str, &str)] = &[
    (
        "apt-get update",
        "DEBIAN_FRONTEND=noninteractive apt-get update -q",
    ),
    (
        "apt-get install docker.io",
        "DEBIAN_FRONTEND=noninteractive apt-get install -y -q docker.io curl ca-certificates",
    ),
    (
        "creating the config and data directories",
        "mkdir -p /etc/shutup-and-run /var/lib/shutup-and-run",
    ),
    (
        "starting docker with systemd",
        "if [ ! -d /run/systemd/system ]; then \
         echo 'systemd is not running in this distribution, so nothing can start docker.' >&2; \
         echo 'It needs WSL 0.67.6 or newer: run `wsl --update`, then this install again.' >&2; \
         exit 1; fi; \
         systemctl enable --now docker 2>&1; \
         i=0; until docker info >/dev/null 2>&1; do i=$((i + 1)); \
         if [ \"$i\" -ge 60 ]; then \
         echo 'dockerd did not answer within 60s' >&2; \
         systemctl status docker --no-pager -l 2>&1 | tail -n 12; \
         journalctl -u docker --no-pager -n 12 2>&1; \
         exit 1; fi; sleep 1; done",
    ),
];

/// Run inside the distribution before the VM goes away: by `wsl stop` with
/// 10 seconds, and at sign-out or shutdown with the 3 that Windows leaves.
/// The service goes first so it takes no new task.
pub(crate) fn graceful_stop_script(grace_secs: u32) -> String {
    format!(
        "pkill -TERM -x shutup-and-run; \
         ids=$(docker ps -q --filter label=shutup-and-run=1 2>/dev/null); \
         [ -z \"$ids\" ] || docker stop -t {grace_secs} $ids >/dev/null"
    )
}

/// Run inside the distribution before a re-install writes the new config and
/// env: the service that read the old ones must be gone first, so the next
/// start is the only process there is and it reads the new files. SIGTERM
/// leaves the containers alone, and the service may take 30 seconds to settle
/// a job that is still pulling, so SIGKILL waits for 35.
pub(crate) fn stop_service_script(process: &str) -> String {
    format!(
        "pkill -TERM -x {process}; i=0; \
         while pgrep -x {process} >/dev/null; do i=$((i + 1)); \
         [ $i -eq 35 ] && pkill -KILL -x {process}; \
         [ $i -ge 40 ] && exit 1; sleep 1; done"
    )
}

const FAIL_LINES: usize = 20;
const SERVICE_WAIT_SECS: u32 = 20;
/// After an install the service must answer, so it gets longer than a doctor
/// run by hand: dockerd may still be starting.
const INSTALL_SERVICE_WAIT_SECS: u32 = 60;
const LOCK_TRIES: u32 = 20;
const SHORT: Duration = Duration::from_secs(30);
const DISTRO_START: Duration = Duration::from_secs(120);
const APT: Duration = Duration::from_secs(20 * 60);
const SMOKE: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct WslError(pub String);

type WslResult<T> = Result<T, WslError>;

fn err(msg: impl Into<String>) -> WslError {
    WslError(msg.into())
}

pub fn usage() -> &'static str {
    "\
shutup-and-run wsl doctor [--smoke-image IMAGE] [--json] [--report]
    check WSL2, the shutup-and-run distribution, Docker inside it, run one
    container as a smoke test, then the config, the service, a forgotten
    pause, bans and the poll URL. Every failed line has a `fix:`. --report
    adds a report without secrets and a link that opens a GitHub issue.
    Exit 1 when something is wrong. `shutup-and-run doctor` is the same.

shutup-and-run wsl install [--poll-url URL] [options]
    create the distribution, install Docker and the Linux binary in it, write
    config.json, register the logon task, put a shortcut to the local page on
    the desktop, and run the doctor. Safe to run again: a re-install keeps
    the config the machine has and changes only the options given here.
    Without a poll URL the worker is idle: only the doctor's smoke test runs.
    SHUTUP_AND_RUN_POLL_TOKEN is the token the poll URL knows this machine
    by. Unset, the machine keeps the one it has or makes its own, and the
    poll URL decides whether to serve it.
    Afterwards the config changes with `shutup-and-run config set`, not here.
      --max-slots N              containers at once (default 1)
      --poll-interval-secs N     default 15
      --worker-name NAME         default: the computer name
      --smoke-image IMAGE        default hello-world
      --rootfs PATH              a downloaded Ubuntu WSL rootfs (skips the download)
      --bin PATH                 the musl binary (skips the download)
      --wsl-memory SIZE          write memory=SIZE to %UserProfile%\\.wslconfig
      --wsl-processors N         write processors=N to %UserProfile%\\.wslconfig
      --no-task                  do not register the scheduled task

shutup-and-run wsl reset [--factory] [install options]
    factory reset for a machine that broke: delete the distribution with its
    containers, images and job history, and install it again. The config
    (poll URL, slots, this machine's poll token) is kept. --factory forgets
    it too: the machine comes back idle, with a new poll token that the poll
    URL has to approve again.

shutup-and-run wsl serve [--background]
    run the service inside the distribution and restart it if it exits.
    --background writes only to the log. The logon task runs it under a
    headless console host, so no window opens.

shutup-and-run wsl start | stop
    start or end the scheduled task. stop gives running containers SIGTERM
    and 10 seconds, then terminates the distribution.

shutup-and-run wsl logs [--lines N]
    print the end of the supervisor log (default 100 lines).

shutup-and-run wsl uninstall [--purge]
    delete the task and the distribution. --purge also removes the data folder.

shutup-and-run wsl task register | remove
    only the logon task: what install and uninstall do with it. Needs no WSL."
}

pub async fn cli(args: &[String]) -> ExitCode {
    let (sub, rest) = match args.split_first() {
        Some((sub, rest)) => (sub.as_str(), rest),
        None => {
            eprintln!("{}", usage());
            return ExitCode::from(2);
        }
    };
    let result = match sub {
        "doctor" => doctor_cli(rest),
        "install" => install_cli(rest).await,
        "reset" => reset_cli(rest).await,
        "serve" => serve_cli(rest),
        "start" => start_cli(),
        "stop" => stop_cli(),
        "logs" => logs_cli(rest),
        "uninstall" => uninstall_cli(rest),
        "task" => task_cli(rest),
        "-h" | "--help" | "help" => {
            println!("{}", usage());
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("{}", usage());
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("shutup-and-run wsl: {e}");
            ExitCode::from(1)
        }
    }
}

// ---------------------------------------------------------------------------
// Flags

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Flags {
    values: HashMap<String, String>,
    switches: Vec<String>,
}

impl Flags {
    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    pub(crate) fn has(&self, key: &str) -> bool {
        self.switches.iter().any(|s| s == key)
    }
}

/// `--key value` pairs for `valued`, bare `--key` for `switches`. Anything
/// else is an error, so a typo does not silently install with defaults.
pub(crate) fn parse_flags(args: &[String], valued: &[&str], switches: &[&str]) -> WslResult<Flags> {
    let mut flags = Flags::default();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let (key, inline) = match arg.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k, Some(v.to_string())),
            _ => (arg, None),
        };
        if switches.contains(&key) {
            if inline.is_some() {
                return Err(err(format!("{key} takes no value")));
            }
            flags.switches.push(key.to_string());
            i += 1;
        } else if valued.contains(&key) {
            let value = match inline {
                Some(v) => v,
                None => {
                    i += 1;
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| err(format!("{key} needs a value")))?
                }
            };
            if value.is_empty() {
                return Err(err(format!("{key} is empty")));
            }
            flags.values.insert(key.to_string(), value);
            i += 1;
        } else {
            return Err(err(format!("unknown argument {arg}")));
        }
    }
    Ok(flags)
}

fn parse_count(
    flags: &Flags,
    key: &str,
    default: u64,
    range: std::ops::RangeInclusive<u64>,
) -> WslResult<u64> {
    match flags.get(key) {
        None => Ok(default),
        Some(raw) => {
            let n: u64 = raw
                .parse()
                .map_err(|_| err(format!("{key} is not a number")))?;
            if !range.contains(&n) {
                return Err(err(format!(
                    "{key} must be between {} and {}",
                    range.start(),
                    range.end()
                )));
            }
            Ok(n)
        }
    }
}

// ---------------------------------------------------------------------------
// Paths

/// Where the supervisor keeps the rootfs, the vhd, the log and a copy of the .exe.
pub(crate) fn data_dir() -> PathBuf {
    if let Ok(base) = std::env::var("ProgramData") {
        if !base.trim().is_empty() {
            return PathBuf::from(base).join("shutup-and-run");
        }
    }
    if cfg!(windows) {
        PathBuf::from(r"C:\ProgramData\shutup-and-run")
    } else {
        // Not a Windows machine: only here so the paths exist in tests.
        std::env::temp_dir().join("shutup-and-run-wsl")
    }
}

fn log_path() -> PathBuf {
    data_dir().join(LOG_FILE)
}

fn wslconfig_path() -> Option<PathBuf> {
    std::env::var("USERPROFILE")
        .ok()
        .filter(|p| !p.trim().is_empty())
        .map(|p| PathBuf::from(p).join(".wslconfig"))
}

// ---------------------------------------------------------------------------
// Running commands

#[derive(Debug)]
struct Output {
    status: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Output {
    fn ok(&self) -> bool {
        self.status == Some(0)
    }

    fn text(&self) -> String {
        let mut text = decode_console(&self.stdout);
        let errors = decode_console(&self.stderr);
        if !errors.trim().is_empty() {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&errors);
        }
        text
    }

    fn one_line(&self) -> String {
        let text = self.text();
        let line = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .to_string();
        match self.status {
            Some(0) => line,
            Some(code) => format!("exit {code}: {line}"),
            None => format!("timed out: {line}"),
        }
    }

    /// For a step that failed: how it ended and the end of what it printed,
    /// stdout then stderr. The end, because the start is the part that worked.
    fn tail(&self, lines: usize) -> String {
        let text = self.text();
        let all: Vec<&str> = text
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.trim().is_empty())
            .collect();
        let ended = match self.status {
            Some(code) => format!("exit {code}"),
            None => "timed out".to_string(),
        };
        let mut out = ended;
        for line in &all[all.len().saturating_sub(lines)..] {
            out.push_str("\n    ");
            out.push_str(line);
        }
        out
    }
}

/// The error of a failed step: what it was, the command, and its last lines.
fn step_failed(what: &str, command: &str, out: &Output) -> WslError {
    err(format!(
        "{what} failed\n  command: {command}\n  {}",
        out.tail(FAIL_LINES)
    ))
}

/// `wsl.exe` writes UTF-16LE to a pipe for `--list` and `--status`, and UTF-8
/// for what a Linux process prints. A NUL byte in the first bytes tells them apart.
pub(crate) fn decode_console(bytes: &[u8]) -> String {
    let looks_utf16 = bytes.len() >= 2 && bytes.iter().take(64).any(|b| *b == 0);
    if looks_utf16 {
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        String::from_utf16_lossy(&units)
            .trim_start_matches('\u{feff}')
            .replace("\r\n", "\n")
    } else {
        String::from_utf8_lossy(bytes).replace("\r\n", "\n")
    }
}

/// Run a command with a deadline. Stdin, when given, is written on its own
/// thread, so a file can be piped into the distribution.
fn run(
    program: &str,
    args: &[&str],
    stdin: Option<Vec<u8>>,
    timeout: Duration,
) -> WslResult<Output> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    no_window(&mut command);
    let mut child = command.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            err(format!(
                "{program} was not found. This command runs on Windows 11 with WSL."
            ))
        } else {
            err(format!("{program}: {e}"))
        }
    })?;
    if let Some(bytes) = stdin {
        if let Some(mut pipe) = child.stdin.take() {
            std::thread::spawn(move || {
                let _ = pipe.write_all(&bytes);
            });
        }
    }
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_thread = std::thread::spawn(move || read_all(stdout));
    let err_thread = std::thread::spawn(move || read_all(stderr));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {
                if started.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(err(format!("{program}: {e}"))),
        }
    };
    Ok(Output {
        status,
        stdout: out_thread.join().unwrap_or_default(),
        stderr: err_thread.join().unwrap_or_default(),
    })
}

fn read_all<R: Read>(reader: Option<R>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(mut reader) = reader {
        let _ = reader.read_to_end(&mut buf);
    }
    buf
}

/// `wsl.exe -d shutup-and-run -u root -e <args>`. `-e` runs the program
/// directly, with no login shell in between to re-split the arguments.
pub(crate) fn wsl_exec_args<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut v = vec!["-d", DISTRO, "-u", "root", "-e"];
    v.extend_from_slice(args);
    v
}

fn in_distro(args: &[&str], stdin: Option<Vec<u8>>, timeout: Duration) -> WslResult<Output> {
    run("wsl.exe", &wsl_exec_args(args), stdin, timeout)
}

fn sh_in_distro(script: &str, stdin: Option<Vec<u8>>, timeout: Duration) -> WslResult<Output> {
    in_distro(&["sh", "-c", script], stdin, timeout)
}

/// Write `bytes` to `path` inside the distribution with `mode`, through stdin.
/// The Windows drives are not mounted in there, so this is the only way in.
fn put_file(path: &str, mode: &str, bytes: Vec<u8>) -> WslResult<()> {
    let script = format!(
        "umask 077 && mkdir -p $(dirname {path}) && cat > {path}.tmp && chmod {mode} {path}.tmp && mv {path}.tmp {path}"
    );
    let out = sh_in_distro(&script, Some(bytes), DISTRO_START)?;
    if !out.ok() {
        return Err(step_failed(&format!("writing {path}"), &script, &out));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// wsl.exe state

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Distro {
    pub name: String,
    pub state: String,
    pub version: String,
    pub default: bool,
}

/// Parse `wsl.exe --list --verbose`. The header and the state column are
/// localized ("Em Execução" on a Portuguese Windows), so a line is a
/// distribution when its last column is a WSL version, the name is the first
/// column, and the state is whatever lies between. A `*` marks the default.
pub(crate) fn parse_distros(text: &str) -> Vec<Distro> {
    text.lines()
        .filter_map(|line| {
            let default = line.trim_start().starts_with('*');
            let body = line.trim_start().trim_start_matches('*').trim_start();
            let parts: Vec<&str> = body.split_whitespace().collect();
            let (name, rest) = parts.split_first()?;
            let (version, state) = rest.split_last()?;
            if *version != "1" && *version != "2" {
                return None;
            }
            Some(Distro {
                name: (*name).to_string(),
                state: state.join(" "),
                version: (*version).to_string(),
                default,
            })
        })
        .collect()
}

fn wsl_status() -> WslResult<Output> {
    run("wsl.exe", &["--status"], None, SHORT)
}

fn list_distros() -> WslResult<Vec<Distro>> {
    // With no distribution at all wsl.exe prints a localized message and
    // exits non-zero. That parses as no distributions, which is the answer.
    // A broken WSL was already caught by `--status`.
    let out = run("wsl.exe", &["--list", "--verbose"], None, SHORT)?;
    Ok(parse_distros(&out.text()))
}

fn our_distro() -> WslResult<Option<Distro>> {
    Ok(list_distros()?
        .into_iter()
        .find(|d| d.name.eq_ignore_ascii_case(DISTRO)))
}

// ---------------------------------------------------------------------------
// Files rendered into the distribution

/// `/etc/wsl.conf`. Docker starts with the distribution. The Windows drives
/// are not mounted and Windows programs cannot be launched from inside, so a
/// container, and the token holder, see only this distribution.
pub(crate) fn render_wsl_conf() -> String {
    "[boot]\n\
     systemd = true\n\
     \n\
     [automount]\n\
     enabled = false\n\
     \n\
     [interop]\n\
     enabled = false\n\
     appendWindowsPath = false\n"
        .to_string()
}

/// The script `wsl serve` runs. It waits for dockerd, loads the env file,
/// and becomes the service, so the supervisor sees the service's exit code.
pub(crate) fn render_serve_script() -> String {
    format!(
        "#!/bin/sh\n\
         # Started by shutup-and-run.exe wsl serve. Keeps the distribution up while it runs.\n\
         i=0\n\
         until docker info >/dev/null 2>&1; do\n\
         \x20 [ \"$i\" -eq 0 ] && systemctl start docker >/dev/null 2>&1\n\
         \x20 i=$((i + 1))\n\
         \x20 if [ \"$i\" -ge 60 ]; then echo 'dockerd did not answer within 60s' >&2; exit 1; fi\n\
         \x20 sleep 1\n\
         done\n\
         exec {INNER_BIN} serve\n"
    )
}

/// What `wsl install` was asked for. None leaves the config as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct InstallOptions {
    pub poll_url: Option<String>,
    pub poll_interval_secs: Option<u64>,
    pub max_slots: Option<u64>,
    pub worker_name: Option<String>,
    pub poll_token: Option<String>,
}

/// config.json for this machine: the config it already has, or the factory
/// one, with the options of this install on top. The listener stays on
/// localhost inside the distribution, for the local page.
///
/// A poll URL given here turns `poll_any_image` on unless the config already
/// says otherwise: the poll URL decides the image. The factory allowlist is
/// the smoke image, because the service wants one entry.
pub(crate) fn install_config(
    old: Option<&JsonMap>,
    options: &InstallOptions,
    computer: &str,
    smoke_image: &str,
) -> JsonMap {
    use serde_json::{json, Value};
    let mut map = old.cloned().unwrap_or_default();
    let mut default = |key: &str, value: Value| {
        map.entry(key.to_string()).or_insert(value);
    };
    default("listen", json!(INNER_LISTEN));
    default("max_slots", json!(1));
    default("allowlist", json!([smoke_image]));
    default("worker_name", json!(computer));
    default("history_path", json!("/var/lib/shutup-and-run/jobs.sqlite"));
    default("token", json!(crate::launcher::random_id()));
    map.insert("http".into(), json!(true));
    map.insert("status_page".into(), json!(true));
    map.insert("worker_os".into(), json!("windows"));
    if let Some(n) = options.max_slots {
        map.insert("max_slots".into(), json!(n));
    }
    if let Some(n) = options.poll_interval_secs {
        map.insert("poll_interval_secs".into(), json!(n));
    }
    if let Some(name) = &options.worker_name {
        map.insert("worker_name".into(), json!(name));
    }
    if let Some(url) = &options.poll_url {
        map.insert("poll_url".into(), json!(url));
        map.entry("poll_any_image".to_string())
            .or_insert(json!(true));
    }
    if let Some(token) = &options.poll_token {
        map.insert("poll_token".into(), json!(token));
    }
    map
}

/// The settings of an install up to 0.2.4, which kept them in config.toml
/// and the tokens in an env file. The keys have the same names.
pub(crate) fn config_from_toml(text: &str, poll_token: Option<String>) -> Option<JsonMap> {
    let value: toml::Value = toml::from_str(text).ok()?;
    let mut map = match serde_json::to_value(value).ok()? {
        serde_json::Value::Object(map) => map,
        _ => return None,
    };
    if let Some(token) = poll_token {
        map.insert("poll_token".into(), serde_json::Value::String(token));
    }
    Some(map)
}

/// The desktop shortcut: a link the person clicks to see whether the PC is
/// in use. A wildcard listen address is opened as localhost.
pub(crate) fn shortcut_text(listen: &str) -> String {
    let port = listen.rsplit(':').next().unwrap_or("8787");
    format!("[InternetShortcut]\r\nURL=http://127.0.0.1:{port}/\r\n")
}

/// `%UserProfile%\.wslconfig` is per user and global to WSL2, so it is written
/// only when it does not exist. Otherwise the lines to add are printed.
pub(crate) fn render_wslconfig(memory: Option<&str>, processors: Option<u64>) -> String {
    let mut text = String::from("[wsl2]\n");
    if let Some(memory) = memory {
        text.push_str(&format!("memory={memory}\n"));
    }
    if let Some(processors) = processors {
        text.push_str(&format!("processors={processors}\n"));
    }
    text
}

fn validate_wsl_memory(raw: &str) -> WslResult<String> {
    let value = raw.trim();
    let digits = value.trim_end_matches(['G', 'M', 'g', 'm', 'B', 'b']);
    if digits.is_empty() || digits.parse::<u64>().is_err() || value.len() > 8 {
        return Err(err("--wsl-memory looks like 4GB or 512MB"));
    }
    Ok(value.to_string())
}

// ---------------------------------------------------------------------------
// Checksums and downloads

/// The hash for `asset` in a `sha256sum` listing. The name is matched by its
/// base name, as install.sh does, because v0.1.0 recorded `dist/<asset>`.
pub(crate) fn sha256_for(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
        (base == asset && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

fn sha256_file(path: &Path) -> WslResult<String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| err(format!("{}: {e}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| err(format!("{}: {e}", path.display())))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn http_client() -> WslResult<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(concat!("shutup-and-run/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| err(e.to_string()))
}

async fn fetch_text(client: &reqwest::Client, url: &str) -> WslResult<String> {
    let response = client
        .get(url)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(|e| err(format!("{url}: {e}")))?;
    if !response.status().is_success() {
        return Err(err(format!("{url}: {}", response.status())));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| err(format!("{url}: {e}")))?;
    if bytes.len() > 64 * 1024 {
        return Err(err(format!("{url}: larger than expected")));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Stream `url` to `dest`, through a `.tmp` file, and check its sha256 before
/// the rename. A file already there with the right hash is kept.
async fn download_verified(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected: &str,
) -> WslResult<()> {
    if dest.exists() && sha256_file(dest)? == expected {
        println!("    {} is already here and matches", dest.display());
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| err(format!("{}: {e}", parent.display())))?;
    }
    let tmp = dest.with_extension("tmp");
    let mut file =
        std::fs::File::create(&tmp).map_err(|e| err(format!("{}: {e}", tmp.display())))?;
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|e| err(format!("{url}: {e}")))?;
    if !response.status().is_success() {
        return Err(err(format!("{url}: {}", response.status())));
    }
    let total = response.content_length();
    let mut hasher = Sha256::new();
    let mut done: u64 = 0;
    let mut next_mark: u64 = 0;
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(e) => return Err(err(format!("{url}: {e}"))),
        };
        hasher.update(&chunk);
        file.write_all(&chunk)
            .map_err(|e| err(format!("{}: {e}", tmp.display())))?;
        done += chunk.len() as u64;
        if done >= next_mark {
            match total {
                Some(total) if total > 0 => {
                    println!("    {} MiB of {} MiB", done >> 20, total >> 20)
                }
                _ => println!("    {} MiB", done >> 20),
            }
            next_mark = done + (32 << 20);
        }
    }
    drop(file);
    let actual = hex(&hasher.finalize());
    if actual != expected {
        let _ = std::fs::remove_file(&tmp);
        return Err(err(format!(
            "{url}: sha256 mismatch (expected {expected}, got {actual}). Nothing was installed."
        )));
    }
    std::fs::rename(&tmp, dest).map_err(|e| err(format!("{}: {e}", dest.display())))?;
    Ok(())
}

/// The release this .exe was built from. The Linux binary inside WSL is the
/// same version, so the two never disagree about the poll contract.
fn release_url(asset: &str) -> String {
    format!("{RELEASE_URL}/v{}/{asset}", env!("CARGO_PKG_VERSION"))
}

fn sums_url_for(url: &str) -> String {
    match url.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/SHA256SUMS"),
        None => "SHA256SUMS".to_string(),
    }
}

async fn obtain_rootfs(client: &reqwest::Client, flags: &Flags) -> WslResult<PathBuf> {
    if let Some(path) = flags.get("--rootfs") {
        let path = PathBuf::from(path);
        if !path.is_file() {
            return Err(err(format!("--rootfs {}: not a file", path.display())));
        }
        return Ok(path);
    }
    let url = std::env::var("SHUTUP_AND_RUN_ROOTFS_URL").unwrap_or_else(|_| ROOTFS_URL.to_string());
    let asset = url.rsplit('/').next().unwrap_or(ROOTFS_FILE);
    let sums = fetch_text(client, &sums_url_for(&url)).await?;
    let expected = sha256_for(&sums, asset).ok_or_else(|| {
        err(format!(
            "SHA256SUMS at {} does not list {asset}",
            sums_url_for(&url)
        ))
    })?;
    let dest = data_dir().join(asset);
    println!("    {url}");
    download_verified(client, &url, &dest, &expected).await?;
    Ok(dest)
}

async fn obtain_musl(client: &reqwest::Client, flags: &Flags) -> WslResult<PathBuf> {
    if let Some(path) = flags.get("--bin") {
        let path = PathBuf::from(path);
        if !path.is_file() {
            return Err(err(format!("--bin {}: not a file", path.display())));
        }
        return Ok(path);
    }
    let url = std::env::var("SHUTUP_AND_RUN_URL").unwrap_or_else(|_| release_url(MUSL_ASSET));
    let asset = url.rsplit('/').next().unwrap_or(MUSL_ASSET);
    let sums = fetch_text(client, &sums_url_for(&url)).await?;
    let expected = sha256_for(&sums, asset).ok_or_else(|| {
        err(format!(
            "SHA256SUMS at {} does not list {asset}",
            sums_url_for(&url)
        ))
    })?;
    let dest = data_dir().join(asset);
    println!("    {url}");
    download_verified(client, &url, &dest, &expected).await?;
    Ok(dest)
}

// ---------------------------------------------------------------------------
// doctor

fn doctor_cli(args: &[String]) -> WslResult<ExitCode> {
    let flags = parse_flags(args, &["--smoke-image"], &["--json", "--report"])?;
    let image = flags.get("--smoke-image").unwrap_or(DEFAULT_SMOKE_IMAGE);
    let (checks, inner) = doctor(image, SERVICE_WAIT_SECS)?;
    // The inner doctor masked its own secrets; the lines added here have none.
    let out = crate::cli::doctor_out(
        "windows (WSL2)",
        &checks,
        inner
            .as_ref()
            .map(|v| &v["config"])
            .filter(|v| !v.is_null()),
        inner
            .as_ref()
            .map(|v| &v["status"])
            .filter(|v| !v.is_null()),
        &[],
        flags.has("--report"),
    );
    if flags.has("--json") {
        println!("{}", out.json);
    } else {
        println!("{}", out.text);
    }
    Ok(ExitCode::from(out.code))
}

/// Each check needs the one before it. The first failure stops the list,
/// so a machine without WSL gets one line, not a page of errors.
///
/// The second value is what the doctor inside the distribution answered:
/// its checks are already in the list, and its masked config and status go
/// into the report.
fn doctor(
    smoke_image: &str,
    service_wait_secs: u32,
) -> WslResult<(Vec<Check>, Option<serde_json::Value>)> {
    let mut checks = Vec::new();
    const INSTALL: &str = "`shutup-and-run wsl install` (safe to run again; it keeps the config)";
    const RESET: &str = "`shutup-and-run wsl reset` rebuilds the distribution and keeps the config";

    let status = match wsl_status() {
        Ok(out) => out,
        Err(e) => {
            checks.push(
                check("wsl", false, e.to_string())
                    .fix("run `wsl --install --no-distribution` as administrator and reboot"),
            );
            return Ok((checks, None));
        }
    };
    if !status.ok() {
        checks.push(
            check("wsl", false, status.one_line())
                .fix("run `wsl --install --no-distribution` as administrator and reboot"),
        );
        return Ok((checks, None));
    }
    checks.push(check("wsl", true, status.one_line()));

    match our_distro()? {
        Some(d) if d.version == "2" => checks.push(check(
            "distribution",
            true,
            format!("{DISTRO} ({})", d.state),
        )),
        Some(d) => {
            checks.push(
                check(
                    "distribution",
                    false,
                    format!("{DISTRO} is WSL version {}; it must be 2", d.version),
                )
                .fix(RESET),
            );
            return Ok((checks, None));
        }
        None => {
            checks.push(
                check("distribution", false, format!("{DISTRO} is not registered")).fix(INSTALL),
            );
            return Ok((checks, None));
        }
    }

    // A stopped distribution boots here, and dockerd needs a few seconds
    // after that, so the first answer is awaited rather than failed.
    let docker = sh_in_distro(
        "i=0; until docker info --format {{.ServerVersion}} 2>/dev/null; do i=$((i + 1)); [ $i -ge 30 ] && { docker info; exit 1; }; sleep 1; done",
        None,
        DISTRO_START,
    )?;
    if !docker.ok() {
        checks.push(check("docker", false, docker.one_line()).fix(format!(
            "`shutup-and-run wsl stop`, then `shutup-and-run wsl start`. Still failing: {RESET}"
        )));
        return Ok((checks, None));
    }
    checks.push(check(
        "docker",
        true,
        format!("engine {}", docker.one_line()),
    ));

    // Not labeled shutup-and-run=1, so it never counts as one of our slots.
    let smoke = in_distro(
        &[
            "docker",
            "run",
            "--rm",
            "--label",
            "shutup-and-run.doctor=1",
            smoke_image,
        ],
        None,
        SMOKE,
    )?;
    let first = smoke.text();
    let first = first
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    checks.push(
        check(
            "container",
            smoke.ok(),
            if smoke.ok() {
                format!("{smoke_image} ran: {first}")
            } else {
                format!("{smoke_image}: {}", smoke.one_line())
            },
        )
        .fix(
            "the image could not be pulled or run: check the internet connection of this PC \
         (a restricted network may block the registry), then run the doctor again",
        ),
    );

    let bin = in_distro(&[INNER_BIN, "--help"], None, DISTRO_START)?;
    checks.push(
        check(
            "binary",
            bin.ok(),
            if bin.ok() {
                INNER_BIN.to_string()
            } else {
                format!("{INNER_BIN}: {}", bin.one_line())
            },
        )
        .fix(INSTALL),
    );

    // Right after an install the task has just started and the service is
    // still waiting for dockerd, so its first status file gets some seconds.
    let _ = sh_in_distro(
        &format!(
            "i=0; until [ -s {INNER_STATUS} ]; do i=$((i + 1)); [ $i -ge {service_wait_secs} ] && break; sleep 1; done"
        ),
        None,
        DISTRO_START,
    );
    // The rest is the same doctor a Linux machine runs, inside the distribution.
    let inner = in_distro(&[INNER_BIN, "doctor", "--json"], None, DISTRO_START)?;
    let inner: Option<serde_json::Value> = serde_json::from_str(inner.text().trim()).ok();
    match &inner {
        Some(answer) => checks.extend(
            crate::cli::checks_from_json(&answer["checks"])
                .into_iter()
                // Docker was checked above, from the outside.
                .filter(|c| c.name != "docker"),
        ),
        None => checks.push(
            check(
                "config",
                false,
                "the binary in the distribution has no doctor",
            )
            .fix(INSTALL),
        ),
    }

    let task = run("schtasks.exe", &["/Query", "/TN", TASK_NAME], None, SHORT);
    checks.push(match task {
        Ok(out) if out.ok() => soft("task", true, format!("{TASK_NAME} is registered")),
        Ok(_) => soft(
            "task",
            false,
            format!("{TASK_NAME} is not registered, so the worker does not start at logon"),
        )
        .fix("`shutup-and-run wsl task register`"),
        Err(e) => soft("task", false, e.to_string()),
    });

    Ok((checks, inner))
}

// ---------------------------------------------------------------------------
// install

async fn install_cli(args: &[String]) -> WslResult<ExitCode> {
    let flags = parse_flags(
        args,
        &[
            "--poll-url",
            "--max-slots",
            "--poll-interval-secs",
            "--worker-name",
            "--smoke-image",
            "--rootfs",
            "--bin",
            "--wsl-memory",
            "--wsl-processors",
        ],
        &["--no-task"],
    )?;

    let poll_url = flags
        .get("--poll-url")
        .map(str::to_string)
        .or_else(|| std::env::var("SHUTUP_AND_RUN_POLL_URL").ok())
        .filter(|u| !u.trim().is_empty());
    // No URL: install everything, poll nothing. The doctor is the only work.
    // A URL and no token: step 6 keeps the token this machine already has, or
    // makes one, and the poll URL enrolls the machine the first time it asks.
    let given_token = std::env::var("SHUTUP_AND_RUN_POLL_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    let count = |key: &str, range| {
        flags
            .get(key)
            .map(|_| parse_count(&flags, key, 1, range))
            .transpose()
    };
    let mut options = InstallOptions {
        poll_url: poll_url.clone(),
        max_slots: count("--max-slots", 1..=256)?,
        poll_interval_secs: count("--poll-interval-secs", 1..=3600)?,
        worker_name: flags.get("--worker-name").map(str::to_string),
        poll_token: given_token.clone(),
    };
    let computer = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "windows-worker".to_string());
    let smoke_image = flags.get("--smoke-image").unwrap_or(DEFAULT_SMOKE_IMAGE);
    let wsl_memory = flags
        .get("--wsl-memory")
        .map(validate_wsl_memory)
        .transpose()?;
    let wsl_processors = flags
        .get("--wsl-processors")
        .map(|_| parse_count(&flags, "--wsl-processors", 1, 1..=256))
        .transpose()?;

    // The same checks the service applies, before anything is downloaded.
    crate::config::Settings::from_json_map(&install_config(None, &options, &computer, smoke_image))
        .map_err(|e| err(format!("the config these options make is invalid: {e}")))?;

    let data = data_dir();
    std::fs::create_dir_all(&data).map_err(|e| err(format!("{}: {e}", data.display())))?;

    println!("[1/8] WSL");
    let status = wsl_status()?;
    if !status.ok() {
        return Err(err(format!(
            "{}. Run `wsl --install --no-distribution` as administrator, reboot, and run this again.",
            status.one_line()
        )));
    }
    println!("    {}", status.one_line());

    println!("[2/8] distribution {DISTRO}");
    let client = http_client()?;
    match our_distro()? {
        Some(d) if d.version == "2" => println!("    already registered ({})", d.state),
        Some(d) => {
            return Err(err(format!(
                "{DISTRO} is registered as WSL version {}. Run `shutup-and-run wsl uninstall` and install again.",
                d.version
            )))
        }
        None => {
            let rootfs = obtain_rootfs(&client, &flags).await?;
            let vhd = data.join("vhd");
            std::fs::create_dir_all(&vhd).map_err(|e| err(format!("{}: {e}", vhd.display())))?;
            let out = run(
                "wsl.exe",
                &[
                    "--import",
                    DISTRO,
                    &vhd.to_string_lossy(),
                    &rootfs.to_string_lossy(),
                    "--version",
                    "2",
                ],
                None,
                APT,
            )?;
            if !out.ok() {
                return Err(step_failed("wsl --import", "wsl --import", &out));
            }
            println!("    imported into {}", vhd.display());
        }
    }

    // On a re-install the task's supervisor would restart the old service in
    // the middle of apt, on the old config, and that service would still be
    // the one running at the end. End the task, then hold the supervisor's
    // own lock until step 7, so none can start in between.
    let _ = run("schtasks.exe", &["/End", "/TN", TASK_NAME], None, SHORT);
    let no_supervisor = wait_for_lock(
        &data.join(SUPERVISOR_LOCK),
        LOCK_TRIES,
        Duration::from_secs(1),
        kill_other_supervisors,
    )
    .map_err(|e| {
        err(format!(
            "a `shutup-and-run wsl serve` is still running and could not be ended ({e}). \
             Close it, or sign out and in again, and run this install again."
        ))
    })?;

    println!("[3/8] /etc/wsl.conf (no Windows drives, no interop, docker at boot)");
    put_file("/etc/wsl.conf", "644", render_wsl_conf().into_bytes())?;
    // A changed wsl.conf is read at the next start, and Microsoft documents
    // an eight second wait after terminate before that start.
    let _ = run("wsl.exe", &["--terminate", DISTRO], None, SHORT);
    std::thread::sleep(Duration::from_secs(8));

    println!("[4/8] docker engine inside the distribution (apt, a few minutes)");
    // One command per thing that can fail, so the error names the right one.
    for (what, script) in DOCKER_STEPS {
        let out = sh_in_distro(script, None, APT)?;
        if !out.ok() {
            return Err(step_failed(
                &format!("{what} inside {DISTRO}"),
                script,
                &out,
            ));
        }
    }
    println!("    installed and running");

    println!("[5/8] the Linux binary");
    let musl = obtain_musl(&client, &flags).await?;
    let bytes = std::fs::read(&musl).map_err(|e| err(format!("{}: {e}", musl.display())))?;
    put_file(INNER_BIN, "755", bytes)?;
    let help = in_distro(&[INNER_BIN, "--help"], None, DISTRO_START)?;
    if !help.ok() {
        return Err(step_failed(
            "running the Linux binary",
            &format!("{INNER_BIN} --help"),
            &help,
        ));
    }
    println!("    {INNER_BIN}");

    println!("[6/8] config.json and the serve script");
    // Step 3 terminated the distribution, so a service from an earlier
    // install should be gone. Make sure, before the file it read changes: a
    // survivor would keep running on the old config.
    let stop = stop_service_script(SERVICE_PROCESS);
    let out = sh_in_distro(&stop, None, DISTRO_START)?;
    if !out.ok() {
        return Err(step_failed("stopping the previous service", &stop, &out));
    }
    // The config this machine already has: config.json in the distribution,
    // else the settings of an install up to 0.2.4, else the copy kept on the
    // Windows side, which is what survives a `wsl reset`.
    let usable = |map: &JsonMap| crate::config::Settings::from_json_map(map).is_ok();
    let old = inner_text(INNER_CONFIG)
        .and_then(|text| crate::config::parse_json(&text).ok())
        .or_else(|| config_from_toml(&inner_text(OLD_CONFIG)?, existing_poll_token()))
        .or_else(host_backup);
    let has_poll_token = |map: Option<&JsonMap>| {
        map.and_then(|m| m.get("poll_token"))
            .and_then(|t| t.as_str())
            .is_some_and(is_token)
    };
    let mut config = install_config(old.as_ref(), &options, &computer, smoke_image);
    if old.is_some() && usable(&config) {
        println!("    kept the config this machine had");
    } else {
        if old.is_some() {
            println!(
                "    the config this machine had is not usable; starting from the factory one"
            );
        }
        config = install_config(None, &options, &computer, smoke_image);
    }
    let mut enrolling = false;
    if config.contains_key("poll_url") && !has_poll_token(Some(&config)) {
        enrolling = true;
        println!("    made a poll token for this machine");
        options.poll_token = Some(format!(
            "{}{}",
            crate::launcher::random_id(),
            crate::launcher::random_id()
        ));
        config = install_config(Some(&config), &options, &computer, smoke_image);
    }
    crate::config::Settings::from_json_map(&config)
        .map_err(|e| err(format!("the config is invalid: {e}")))?;
    let text = crate::config::to_text(&config);
    put_file(INNER_CONFIG, "600", text.clone().into_bytes())?;
    put_file(
        &format!("{INNER_CONFIG}.bak"),
        "600",
        text.clone().into_bytes(),
    )?;
    put_file(INNER_SERVE, "755", render_serve_script().into_bytes())?;
    // config.json is now the one source: the files of an older install go,
    // and so does the status the old service left, so the doctor below sees
    // only what the new service writes.
    let _ = sh_in_distro(
        &format!("rm -f {OLD_CONFIG} {INNER_ENV} {INNER_STATUS}"),
        None,
        SHORT,
    );
    save_host_backup(&text);
    println!("    {INNER_CONFIG} (600), {INNER_SERVE}");
    // An earlier install may have banned this machine's own address by
    // sending a new token to an old service, and a ban outlives a restart.
    // This is `bans lift` as root inside the distribution, where the listener
    // is on localhost: it changes nothing for a caller from anywhere else.
    if let Ok(out) = in_distro(&[INNER_BIN, "bans", "lift", "127.0.0.1"], None, SHORT) {
        if out.ok() && out.text().contains("lifted") {
            println!("    lifted a ban on 127.0.0.1 left by an earlier install");
        }
    }

    if wsl_memory.is_some() || wsl_processors.is_some() {
        let text = render_wslconfig(wsl_memory.as_deref(), wsl_processors);
        match wslconfig_path() {
            Some(path) if !path.exists() => {
                std::fs::write(&path, &text)
                    .map_err(|e| err(format!("{}: {e}", path.display())))?;
                println!("    wrote {}", path.display());
            }
            Some(path) => {
                println!(
                    "    {} exists and was not changed. Add under [wsl2]:",
                    path.display()
                );
                for line in text.lines().skip(1) {
                    println!("        {line}");
                }
            }
            None => println!("    USERPROFILE is unset; .wslconfig was not written"),
        }
    }

    println!("[7/8] scheduled task {TASK_NAME} (at logon)");
    let exe = install_exe_copy(&data)?;
    // The new supervisor needs the lock this install has been holding.
    drop(no_supervisor);
    if flags.has("--no-task") {
        println!(
            "    skipped (--no-task). Run: \"{}\" wsl serve",
            exe.display()
        );
    } else {
        register_task(&exe)?;
        println!("    registered and started");
        std::thread::sleep(Duration::from_secs(5));
    }
    let listen = config["listen"]
        .as_str()
        .unwrap_or(INNER_LISTEN)
        .to_string();
    match write_shortcut(&listen) {
        Ok(path) => println!("    shortcut to the local page: {}", path.display()),
        Err(e) => println!("    no desktop shortcut ({e}); the page is http://{listen}/"),
    }

    println!("[8/8] doctor");
    let (checks, _) = doctor(smoke_image, INSTALL_SERVICE_WAIT_SECS)?;
    let (text, healthy) = report(&checks);
    print!("{text}");
    if !healthy {
        return Err(err(
            "the doctor found a problem. Run the `fix:` lines, then `shutup-and-run doctor`.",
        ));
    }
    // Step 6 removed the old status file, so a service the doctor saw
    // running was started after it and read this install's config.
    if !flags.has("--no-task") && !service_answers(&checks) {
        return Err(err(
            "the service is not running with the config this install wrote. \
             See `shutup-and-run wsl logs`, then run this install again.",
        ));
    }
    println!();
    let slots = &config["max_slots"];
    let name = config["worker_name"].as_str().unwrap_or("?");
    match config.get("poll_url").and_then(|u| u.as_str()) {
        Some(url) => {
            println!(
                "done. The worker polls {} as {name} with {slots} slot(s).",
                host_of(url)
            );
            if enrolling {
                println!(
                    "This machine has a new token. It gets no work until the poll URL approves it."
                );
            }
        }
        None => println!(
            "done. No poll URL: the worker is idle. Give it one with\n    shutup-and-run config set poll_url URL --apply"
        ),
    }
    println!("page: http://{listen}/   config: shutup-and-run config show");
    println!("logs: shutup-and-run wsl logs");
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn service_answers(checks: &[Check]) -> bool {
    checks.iter().any(|c| c.name == "service" && c.ok)
}

/// Take the supervisor's lock, waiting for whoever holds it to end. Halfway
/// through, `on_stuck` runs once: ending the task did not end the holder.
/// The caller keeps the file; dropping it lets a supervisor start again.
pub(crate) fn wait_for_lock(
    path: &Path,
    tries: u32,
    pause: Duration,
    mut on_stuck: impl FnMut(),
) -> Result<std::fs::File, String> {
    let mut last = String::new();
    for attempt in 0..tries.max(1) {
        match crate::lock::acquire(path) {
            Ok(file) => return Ok(file),
            Err(e) => last = e,
        }
        if attempt == tries / 2 {
            on_stuck();
        }
        std::thread::sleep(pause);
    }
    Err(last)
}

/// `taskkill` for every `shutup-and-run.exe` except this one: a supervisor
/// started by hand in a terminal, or one the ended task left behind.
pub(crate) fn taskkill_args(own_pid: u32) -> Vec<String> {
    vec![
        "/F".to_string(),
        "/FI".to_string(),
        "IMAGENAME eq shutup-and-run.exe".to_string(),
        "/FI".to_string(),
        format!("PID ne {own_pid}"),
    ]
}

fn kill_other_supervisors() {
    let args = taskkill_args(std::process::id());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let _ = run("taskkill.exe", &args, None, SHORT);
}

/// The poll token a previous install left in the distribution, so a
/// re-install is still the machine the poll URL already knows. Never printed.
fn existing_poll_token() -> Option<String> {
    let script =
        format!("[ -r {INNER_ENV} ] && . {INNER_ENV}; printf %s \"$SHUTUP_AND_RUN_POLL_TOKEN\"");
    let out = sh_in_distro(&script, None, SHORT).ok()?;
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    is_token(&token).then_some(token)
}

/// A file inside the distribution, when it is there and has text.
fn inner_text(path: &str) -> Option<String> {
    let out = in_distro(&["cat", path], None, SHORT).ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    (out.ok() && !text.trim().is_empty()).then_some(text)
}

/// A copy of config.json beside the Windows-side data. The distribution can
/// be lost (a power cut, a full disk); this is what `wsl reset` and the next
/// install put back, so the machine keeps its poll URL and its poll token.
fn save_host_backup(text: &str) {
    let _ = crate::status::write_atomic(&data_dir().join(HOST_BACKUP), text.as_bytes());
}

fn host_backup() -> Option<JsonMap> {
    let text = std::fs::read_to_string(data_dir().join(HOST_BACKUP)).ok()?;
    crate::config::parse_json(&text).ok()
}

/// Copy the distribution's config.json to the Windows side, when it is valid.
fn refresh_host_backup() -> Option<JsonMap> {
    let text = inner_text(INNER_CONFIG)?;
    let map = crate::config::parse_json(&text).ok()?;
    crate::config::Settings::from_json_map(&map).ok()?;
    save_host_backup(&text);
    Some(map)
}

fn desktop_dir() -> WslResult<PathBuf> {
    let out = run(
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Environment]::GetFolderPath('Desktop')",
        ],
        None,
        SHORT,
    )?;
    let dir = out.text().trim().to_string();
    if !out.ok() || dir.is_empty() {
        return Err(err("the desktop folder was not found"));
    }
    Ok(PathBuf::from(dir))
}

fn write_shortcut(listen: &str) -> WslResult<PathBuf> {
    let path = desktop_dir()?.join(SHORTCUT_FILE);
    std::fs::write(&path, shortcut_text(listen))
        .map_err(|e| err(format!("{}: {e}", path.display())))?;
    Ok(path)
}

fn remove_shortcut() {
    if let Ok(dir) = desktop_dir() {
        let _ = std::fs::remove_file(dir.join(SHORTCUT_FILE));
    }
}

/// `config`, `pause`, `resume` and `bans` run inside the distribution, where
/// the file, the job history and the service are. Output and exit code are
/// the inner command's own.
pub fn forward(args: &[String]) -> ExitCode {
    let as_json = args.iter().any(|a| a == "--json");
    let wants_help = args.iter().any(|a| a == "--help" || a == "-h");
    let fail = |msg: String| {
        if as_json {
            println!("{}", serde_json::json!({"ok": false, "error": msg}));
        } else {
            eprintln!("shutup-and-run: {msg}");
        }
        ExitCode::from(1)
    };
    if wants_help {
        println!("{}", crate::cli::usage());
        return ExitCode::SUCCESS;
    }
    match our_distro() {
        Ok(Some(_)) => {}
        Ok(None) => {
            return fail(format!(
                "the worker is not installed on this PC ({DISTRO} is not a WSL distribution). \
                 Run `shutup-and-run wsl install`."
            ))
        }
        Err(e) => return fail(e.to_string()),
    }
    let mut inner: Vec<&str> = vec![INNER_BIN];
    inner.extend(args.iter().map(String::as_str));
    let out = match in_distro(&inner, None, DISTRO_START) {
        Ok(out) => out,
        Err(e) => return fail(e.to_string()),
    };
    print!("{}", String::from_utf8_lossy(&out.stdout));
    eprint!("{}", String::from_utf8_lossy(&out.stderr));
    if out.ok() && args.first().map(String::as_str) == Some("config") {
        let changes = args
            .iter()
            .any(|a| matches!(a.as_str(), "set" | "unset" | "apply" | "reset"));
        if args.iter().any(|a| a == "reset") {
            // The factory config of a Linux machine says linux.
            let _ = in_distro(
                &[
                    INNER_BIN,
                    "config",
                    "set",
                    "worker_os",
                    "windows",
                    "--apply",
                ],
                None,
                DISTRO_START,
            );
        }
        if changes {
            if let Some(map) = refresh_host_backup() {
                if let Some(listen) = map.get("listen").and_then(|l| l.as_str()) {
                    let _ = write_shortcut(listen);
                }
            }
        }
    }
    ExitCode::from(out.status.unwrap_or(1).clamp(0, 255) as u8)
}

/// `wsl reset`: the distribution is deleted and installed again. The config
/// comes back from the Windows-side copy, unless `--factory` forgets it.
async fn reset_cli(args: &[String]) -> WslResult<ExitCode> {
    let factory = args.iter().any(|a| a == "--factory");
    let rest: Vec<String> = args.iter().filter(|a| *a != "--factory").cloned().collect();
    if factory {
        let _ = std::fs::remove_file(data_dir().join(HOST_BACKUP));
        println!("reset: forgetting the config (--factory)");
    } else if our_distro()?.is_some() && refresh_host_backup().is_some() {
        println!("reset: kept a copy of the config");
    } else if host_backup().is_some() {
        println!("reset: the distribution's config is unreadable; using the last copy kept");
    } else {
        println!("reset: no config to keep; the worker comes back with the factory one");
    }
    remove_task();
    if our_distro()?.is_some() {
        let _ = sh_in_distro(&graceful_stop_script(10), None, DISTRO_START);
        let _ = run("wsl.exe", &["--terminate", DISTRO], None, SHORT);
        let out = run("wsl.exe", &["--unregister", DISTRO], None, APT)?;
        if !out.ok() {
            return Err(step_failed("wsl --unregister", "wsl --unregister", &out));
        }
        println!("reset: deleted {DISTRO}; installing it again");
    }
    mark_stopped();
    install_cli(&rest).await
}

/// What a token we wrote looks like. Anything else in that file is not reused.
pub(crate) fn is_token(s: &str) -> bool {
    (16..=256).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "the poll URL".to_string())
}

/// The task must not point at a file in Downloads. The .exe is copied next
/// to the data, unless it already runs from there.
fn install_exe_copy(data: &Path) -> WslResult<PathBuf> {
    let current = std::env::current_exe().map_err(|e| err(format!("current exe: {e}")))?;
    let target = data.join("shutup-and-run.exe");
    if current == target {
        return Ok(target);
    }
    match std::fs::copy(&current, &target) {
        Ok(_) => Ok(target),
        // Windows refuses to overwrite a running .exe. The copy already there
        // is then the one the task runs, which is fine for a re-install.
        Err(e) if target.exists() => {
            println!("    kept {} ({e})", target.display());
            Ok(target)
        }
        Err(e) => Err(err(format!("copy to {}: {e}", target.display()))),
    }
}

/// The PowerShell that registers the logon task. `schtasks /Create /SC
/// ONLOGON` needs an administrator even with `/RL LIMITED`, because its
/// trigger is "any user logs on". A trigger tied to one user does not, and
/// `Register-ScheduledTask` can say that. The task runs as the user who
/// installed, which is the user the distribution belongs to, with no
/// elevation, and only while that user is logged on.
///
/// The settings undo defaults meant for a short job: do not start on battery,
/// stop on battery, end after 72 hours. A second run of the task is ignored.
///
/// The action is `conhost.exe --headless <exe> wsl serve --background`. The
/// `.exe` is a console program, so started directly it gets a console
/// window, and whoever closes that window kills the worker. Under a headless
/// console host there is no window, and the process still has a console to
/// receive the sign-out and shutdown events on.
///
/// No double quote in the script: it travels as one command-line argument.
/// The quotes around the path are `[char]34`.
pub(crate) fn task_register_script(exe: &Path) -> String {
    let exe = exe.display().to_string().replace('\'', "''");
    format!(
        "$ErrorActionPreference = 'Stop'; \
         $u = $env:USERDOMAIN + '\\' + $env:USERNAME; \
         $q = [char]34; \
         $a = New-ScheduledTaskAction -Execute ($env:WINDIR + '\\System32\\conhost.exe') \
         -Argument ('--headless ' + $q + '{exe}' + $q + ' wsl serve --background'); \
         $t = New-ScheduledTaskTrigger -AtLogOn -User $u; \
         $s = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries \
         -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew; \
         Register-ScheduledTask -TaskName '{TASK_NAME}' -Action $a -Trigger $t -Settings $s \
         -RunLevel Limited -Force | Out-Null"
    )
}

fn register_task(exe: &Path) -> WslResult<()> {
    let script = task_register_script(exe);
    let out = run(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", &script],
        None,
        SHORT,
    )?;
    if !out.ok() {
        return Err(step_failed(
            "registering the logon task",
            &format!("powershell -Command {script}"),
            &out,
        ));
    }
    let out = run("schtasks.exe", &["/Run", "/TN", TASK_NAME], None, SHORT)?;
    if !out.ok() {
        return Err(step_failed(
            "schtasks /Run",
            &format!("schtasks /Run /TN {TASK_NAME}"),
            &out,
        ));
    }
    Ok(())
}

/// End and delete the logon task. A task that is not there is not an error.
fn remove_task() {
    let _ = run("schtasks.exe", &["/End", "/TN", TASK_NAME], None, SHORT);
    match run(
        "schtasks.exe",
        &["/Delete", "/F", "/TN", TASK_NAME],
        None,
        SHORT,
    ) {
        Ok(out) if out.ok() => println!("deleted task {TASK_NAME}"),
        Ok(out) => println!("task {TASK_NAME}: {}", out.one_line()),
        Err(e) => println!("task {TASK_NAME}: {e}"),
    }
}

/// `wsl task register|remove`: step 7 of the install and the first step of
/// the uninstall, alone. It needs no WSL, so a machine without it (CI, a
/// Windows Sandbox) can test the one part that depends on the user's rights.
fn task_cli(args: &[String]) -> WslResult<ExitCode> {
    match args.first().map(String::as_str) {
        Some("register") if args.len() == 1 => {
            let data = data_dir();
            std::fs::create_dir_all(&data).map_err(|e| err(format!("{}: {e}", data.display())))?;
            let exe = install_exe_copy(&data)?;
            register_task(&exe)?;
            println!("registered and started task {TASK_NAME}");
            Ok(ExitCode::SUCCESS)
        }
        Some("remove") if args.len() == 1 => {
            remove_task();
            Ok(ExitCode::SUCCESS)
        }
        _ => Err(err("usage: shutup-and-run wsl task register|remove")),
    }
}

// ---------------------------------------------------------------------------
// serve

fn serve_cli(args: &[String]) -> WslResult<ExitCode> {
    let flags = parse_flags(args, &[], &["--background"])?;
    let background = flags.has("--background");
    let data = data_dir();
    std::fs::create_dir_all(&data).map_err(|e| err(format!("{}: {e}", data.display())))?;
    // One supervisor per machine. A second one would only fight over the
    // service inside the distribution, so it says so and leaves.
    let _lock = crate::lock::acquire(&data.join(SUPERVISOR_LOCK)).map_err(err)?;
    let log = Logger::open(&log_path(), !background)?;
    stop_jobs_when_the_session_ends(&log);
    log.line(&format!(
        "supervisor {} starting; distribution {DISTRO}",
        env!("CARGO_PKG_VERSION")
    ));
    let mut wait = RESTART_MIN;
    loop {
        let started = Instant::now();
        // What the page changed since the last start is copied to the
        // Windows side here, for a reset to put back.
        let _ = refresh_host_backup();
        let reason = match run_service(&log) {
            Ok(code) => {
                log.line(&format!("service exited with {code:?}"));
                format!("service exited with {code:?}")
            }
            Err(e) => {
                log.line(&format!("service could not start: {e}"));
                format!("service could not start: {e}")
            }
        };
        // The service is gone, so its last status line is no longer true.
        // A later line from the next start replaces this.
        if let Err(e) = crate::status::write_atomic(
            &data_dir().join("status.json"),
            &crate::status::failed_bytes(&reason),
        ) {
            log.line(&format!("status file: {e}"));
        }
        // A service that stayed up for a while gets a quick restart. One that
        // dies at once backs off, so a broken config does not spin.
        wait = if started.elapsed() > RESTART_MAX {
            RESTART_MIN
        } else {
            (wait * 2).min(RESTART_MAX)
        };
        log.line(&format!("restarting in {}s", wait.as_secs()));
        std::thread::sleep(wait);
    }
}

/// One run of the service. Its output goes to the log as it is written.
fn run_service(log: &Logger) -> WslResult<Option<i32>> {
    let args = wsl_exec_args(&[INNER_SERVE]);
    let mut command = Command::new("wsl.exe");
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    no_window(&mut command);
    let mut child = command.spawn().map_err(|e| err(format!("wsl.exe: {e}")))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let a = log.clone();
    let b = log.clone();
    let t1 = std::thread::spawn(move || pump(stdout, &a));
    let t2 = std::thread::spawn(move || pump(stderr, &b));
    let status = child.wait().map_err(|e| err(format!("wsl.exe: {e}")))?;
    let _ = t1.join();
    let _ = t2.join();
    Ok(status.code())
}

fn pump<R: Read>(reader: Option<R>, log: &Logger) {
    let Some(reader) = reader else { return };
    let mut reader = std::io::BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        match std::io::BufRead::read_line(&mut reader, &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let text = line.trim_end_matches(['\r', '\n']);
                if let Some(json) = crate::status::status_json_from_log(text) {
                    publish_host_status(json);
                }
                log.raw(text);
            }
        }
    }
}

#[derive(Clone)]
struct Logger {
    file: std::sync::Arc<std::sync::Mutex<std::fs::File>>,
    echo: bool,
}

impl Logger {
    fn open(path: &Path, echo: bool) -> WslResult<Self> {
        rotate(path);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| err(format!("{}: {e}", path.display())))?;
        Ok(Self {
            file: std::sync::Arc::new(std::sync::Mutex::new(file)),
            echo,
        })
    }

    fn line(&self, text: &str) {
        self.raw(&format!("supervisor: {text}"));
    }

    fn raw(&self, text: &str) {
        if self.echo {
            println!("{text}");
        }
        if let Ok(mut file) = self.file.lock() {
            let _ = writeln!(file, "{} {text}", stamp());
        }
    }
}

/// `supervisor.log` over 10 MiB becomes `supervisor.log.1`, replacing the
/// last one. Two files, so the log cannot fill the disk.
fn rotate(path: &Path) {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > LOG_ROTATE_BYTES {
            let old = path.with_extension("log.1");
            let _ = std::fs::rename(path, old);
        }
    }
}

fn stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

/// Every child is `wsl.exe`, `schtasks.exe` or `powershell.exe`: console
/// programs. Their output is piped, so they need no console, and without
/// this flag Windows gives a console program that has none a new window.
/// That window was the worker: closing it killed the service.
#[cfg(windows)]
fn no_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_window(_command: &mut Command) {}

static SESSION_LOG: std::sync::OnceLock<Logger> = std::sync::OnceLock::new();

/// Sign-out, shutdown and restart reach a console process as a control
/// event, and Windows then gives it about five seconds. That is spent on the
/// jobs: stop the service, SIGTERM to each container, three seconds, kill.
/// The task runs under a headless console host, so the event arrives though
/// there is no window.
#[cfg(windows)]
fn stop_jobs_when_the_session_ends(log: &Logger) {
    extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }
    unsafe extern "system" fn handler(event: u32) -> i32 {
        const CTRL_CLOSE: u32 = 2;
        const CTRL_LOGOFF: u32 = 5;
        const CTRL_SHUTDOWN: u32 = 6;
        if !matches!(event, CTRL_CLOSE | CTRL_LOGOFF | CTRL_SHUTDOWN) {
            // Ctrl+C in a terminal: the default, which ends the supervisor.
            return 0;
        }
        if let Some(log) = SESSION_LOG.get() {
            log.line("the session is ending; stopping the jobs");
        }
        let _ = sh_in_distro(&graceful_stop_script(3), None, Duration::from_secs(8));
        mark_stopped();
        1
    }
    let _ = SESSION_LOG.set(log.clone());
    // SAFETY: the handler is a plain function that lives for the whole
    // process, and it only reads a OnceLock and starts a child process.
    unsafe {
        SetConsoleCtrlHandler(Some(handler), 1);
    }
}

#[cfg(not(windows))]
fn stop_jobs_when_the_session_ends(log: &Logger) {
    let _ = SESSION_LOG.set(log.clone());
}

// ---------------------------------------------------------------------------
// start, stop, logs, uninstall

fn start_cli() -> WslResult<ExitCode> {
    let out = run("schtasks.exe", &["/Run", "/TN", TASK_NAME], None, SHORT)?;
    if !out.ok() {
        return Err(step_failed("schtasks /Run", "schtasks /Run", &out));
    }
    println!("started {TASK_NAME}");
    Ok(ExitCode::SUCCESS)
}

fn stop_cli() -> WslResult<ExitCode> {
    let ended = run("schtasks.exe", &["/End", "/TN", TASK_NAME], None, SHORT)?;
    if !ended.ok() {
        println!("schtasks /End: {}", ended.one_line());
    }
    // Terminating the VM would kill the containers with no warning. Stop the
    // service first so it takes no new task, then give each job SIGTERM and
    // 10 seconds. The rows are closed when the service next starts.
    if our_distro()?.is_some() {
        match sh_in_distro(&graceful_stop_script(10), None, DISTRO_START) {
            Ok(out) if out.ok() => {}
            Ok(out) => println!("graceful stop: {}", out.one_line()),
            Err(e) => println!("graceful stop: {e}"),
        }
    }
    let out = run("wsl.exe", &["--terminate", DISTRO], None, SHORT)?;
    if !out.ok() {
        return Err(step_failed("wsl --terminate", "wsl --terminate", &out));
    }
    mark_stopped();
    println!("stopped {TASK_NAME} and terminated {DISTRO}; its containers were stopped");
    Ok(ExitCode::SUCCESS)
}

/// `status.json` on the Windows side. The bytes are the service's own
/// document, copied from its log line, so the host file and the page match.
fn publish_host_status(json: &str) {
    let path = data_dir().join("status.json");
    let mut bytes = json.as_bytes().to_vec();
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    if let Err(e) = crate::status::write_atomic(&path, &bytes) {
        if let Some(log) = SESSION_LOG.get() {
            log.line(&format!("status file: {e}"));
        }
    }
}

fn mark_stopped() {
    let path = data_dir().join("status.json");
    let _ = crate::status::write_atomic(&path, &crate::status::stopped_bytes());
}

fn logs_cli(args: &[String]) -> WslResult<ExitCode> {
    let flags = parse_flags(args, &["--lines"], &[])?;
    let lines = parse_count(&flags, "--lines", 100, 1..=100_000)? as usize;
    let path = log_path();
    let text =
        std::fs::read_to_string(&path).map_err(|e| err(format!("{}: {e}", path.display())))?;
    for line in tail(&text, lines) {
        println!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn tail(text: &str, lines: usize) -> Vec<&str> {
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    all[start..].to_vec()
}

fn uninstall_cli(args: &[String]) -> WslResult<ExitCode> {
    let flags = parse_flags(args, &[], &["--purge"])?;
    remove_task();
    remove_shortcut();
    if our_distro()?.is_some() {
        let _ = run("wsl.exe", &["--terminate", DISTRO], None, SHORT);
        let out = run("wsl.exe", &["--unregister", DISTRO], None, APT)?;
        if !out.ok() {
            return Err(step_failed("wsl --unregister", "wsl --unregister", &out));
        }
        println!("unregistered {DISTRO}; its containers, images and job history are gone");
    } else {
        println!("{DISTRO} was not registered");
    }
    if flags.has("--purge") {
        let data = data_dir();
        match std::fs::remove_dir_all(&data) {
            Ok(()) => println!("removed {}", data.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            // The running .exe may be inside; Windows keeps it until it exits.
            Err(e) => println!(
                "{}: {e} (delete it after this command ends)",
                data.display()
            ),
        }
    } else {
        println!(
            "kept {} with a copy of the config, which the next install uses (pass --purge to delete it)",
            data_dir().display()
        );
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flags_take_values_switches_and_refuse_typos() {
        let flags = parse_flags(
            &args(&["--poll-url", "https://x/poll", "--no-task", "--max-slots=2"]),
            &["--poll-url", "--max-slots"],
            &["--no-task"],
        )
        .unwrap();
        assert_eq!(flags.get("--poll-url"), Some("https://x/poll"));
        assert_eq!(flags.get("--max-slots"), Some("2"));
        assert!(flags.has("--no-task"));
        assert!(!flags.has("--purge"));
        assert_eq!(parse_count(&flags, "--max-slots", 1, 1..=256).unwrap(), 2);
        assert_eq!(parse_count(&flags, "--missing", 7, 1..=256).unwrap(), 7);

        assert!(parse_flags(&args(&["--pol-url", "x"]), &["--poll-url"], &[]).is_err());
        assert!(parse_flags(&args(&["--poll-url"]), &["--poll-url"], &[]).is_err());
        assert!(parse_flags(&args(&["--poll-url="]), &["--poll-url"], &[]).is_err());
        assert!(parse_flags(&args(&["--no-task=1"]), &[], &["--no-task"]).is_err());
        assert!(parse_flags(&args(&["extra"]), &[], &[]).is_err());
        let flags = parse_flags(&args(&["--max-slots", "0"]), &["--max-slots"], &[]).unwrap();
        assert!(parse_count(&flags, "--max-slots", 1, 1..=256).is_err());
        let flags = parse_flags(&args(&["--max-slots", "x"]), &["--max-slots"], &[]).unwrap();
        assert!(parse_count(&flags, "--max-slots", 1, 1..=256).is_err());
    }

    #[test]
    fn wsl_list_output_is_utf16_and_parses() {
        let text = "  NAME              STATE           VERSION\r\n* Ubuntu            Running         2\r\n  shutup-and-run    Stopped         2\r\n  docker-desktop    Stopped         1\r\n";
        let mut bytes = vec![0xff, 0xfe];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let decoded = decode_console(&bytes);
        assert!(decoded.starts_with("  NAME"));
        assert!(!decoded.contains('\r'));
        let distros = parse_distros(&decoded);
        assert_eq!(distros.len(), 3);
        assert_eq!(
            distros[0],
            Distro {
                name: "Ubuntu".into(),
                state: "Running".into(),
                version: "2".into(),
                default: true
            }
        );
        assert_eq!(distros[1].name, DISTRO);
        assert_eq!(distros[1].version, "2");
        assert!(!distros[1].default);
        assert_eq!(distros[2].version, "1");

        // A Portuguese Windows localizes the header and the state.
        let pt = "  NOME              ESTADO           VERSÃO\n* Ubuntu            Em Execução      2\n  shutup-and-run    Parado           2\nO Subsistema do Windows para Linux não tem distribuições instaladas.\n";
        let distros = parse_distros(pt);
        assert_eq!(distros.len(), 2);
        assert_eq!(distros[0].state, "Em Execução");
        assert_eq!(distros[1].name, DISTRO);
        assert_eq!(distros[1].version, "2");

        // UTF-8 from a Linux process is left as it is, minus CRLF.
        assert_eq!(decode_console(b"28.0.1\r\n"), "28.0.1\n");
        assert_eq!(decode_console(b""), "");
        assert!(parse_distros("").is_empty());
        assert!(parse_distros("  NAME  STATE  VERSION\n").is_empty());
    }

    #[test]
    fn exec_args_run_the_program_directly_as_root() {
        assert_eq!(
            wsl_exec_args(&["docker", "info"]),
            vec!["-d", "shutup-and-run", "-u", "root", "-e", "docker", "info"]
        );
    }

    #[test]
    fn the_install_config_is_one_the_service_accepts() {
        let options = InstallOptions {
            poll_url: Some("https://orchestrator.example/poll?user=ana".into()),
            poll_interval_secs: Some(20),
            max_slots: Some(2),
            worker_name: None,
            poll_token: Some("b".repeat(32)),
        };
        let map = install_config(None, &options, "DESK-07", "hello-world");
        let settings = crate::config::Settings::from_json_map(&map).unwrap();
        assert!(settings.http_plain);
        assert_eq!(settings.listen.to_string(), INNER_LISTEN);
        assert_eq!(settings.max_slots, 2);
        assert!(settings.poll_any_image);
        assert_eq!(
            settings.poll_url.as_deref(),
            Some("https://orchestrator.example/poll?user=ana")
        );
        assert_eq!(settings.poll_interval, Duration::from_secs(20));
        assert_eq!(settings.worker_name, "DESK-07");
        assert_eq!(settings.worker_os, "windows");
        assert!(settings.poll_token.is_some());
        assert!(settings.status_page);

        // No poll URL: idle, and the smoke image is the one allowed image.
        let idle = install_config(None, &InstallOptions::default(), "DESK-07", "hello-world");
        let settings = crate::config::Settings::from_json_map(&idle).unwrap();
        assert!(settings.poll_url.is_none() && !settings.poll_any_image);
        assert_eq!(settings.allowlist, vec!["hello-world".to_string()]);
        assert!(settings.listen.ip().is_loopback());
    }

    #[test]
    fn a_reinstall_keeps_the_config_and_changes_only_what_was_asked() {
        let first = install_config(
            None,
            &InstallOptions {
                poll_url: Some("https://jobs.example/poll?user=ana".into()),
                max_slots: Some(3),
                poll_token: Some("b".repeat(32)),
                ..InstallOptions::default()
            },
            "DESK-07",
            "hello-world",
        );
        // The person changed two things on the page since.
        let mut edited = first.clone();
        edited.insert("poll_any_image".into(), serde_json::json!(false));
        edited.insert("admin_password".into(), serde_json::json!("changed"));

        let again = install_config(
            Some(&edited),
            &InstallOptions::default(),
            "OTHER",
            "hello-world",
        );
        assert_eq!(again, edited, "an install with no options changes nothing");

        let moved = install_config(
            Some(&edited),
            &InstallOptions {
                poll_url: Some("https://jobs.example/poll?user=bia".into()),
                ..InstallOptions::default()
            },
            "OTHER",
            "hello-world",
        );
        assert_eq!(moved["poll_url"], "https://jobs.example/poll?user=bia");
        assert_eq!(moved["poll_any_image"], false);
        assert_eq!(moved["max_slots"], 3);
        assert_eq!(moved["token"], first["token"]);
        assert_eq!(moved["poll_token"], first["poll_token"]);
        assert_eq!(moved["worker_name"], "DESK-07");

        // What 0.2.4 wrote, carried over with the token from its env file.
        let old = "listen = \"127.0.0.1:8787\"\nhttp = true\nmax_slots = 2\nallowlist = []\n\
                   poll_url = \"https://jobs.example/poll\"\npoll_interval_secs = 15\n\
                   poll_any_image = true\nworker_name = \"DESK-07\"\nworker_os = \"windows\"\n\
                   status_page = true\nhistory_path = \"/var/lib/shutup-and-run/jobs.sqlite\"\n";
        let carried = config_from_toml(old, Some("c".repeat(32))).unwrap();
        let map = install_config(
            Some(&carried),
            &InstallOptions::default(),
            "X",
            "hello-world",
        );
        let settings = crate::config::Settings::from_json_map(&map).unwrap();
        assert_eq!(
            settings.poll_url.as_deref(),
            Some("https://jobs.example/poll")
        );
        assert_eq!(settings.max_slots, 2);
        assert_eq!(settings.poll_token.unwrap().as_str(), "c".repeat(32));
        assert!(config_from_toml("not = [toml", None).is_none());

        assert_eq!(
            shortcut_text("127.0.0.1:8788"),
            "[InternetShortcut]\r\nURL=http://127.0.0.1:8788/\r\n"
        );
        assert!(shortcut_text("0.0.0.0:8787").contains("http://127.0.0.1:8787/"));
    }

    #[test]
    fn env_wsl_conf_and_serve_script_have_the_expected_shape() {
        let conf = render_wsl_conf();
        assert!(conf.contains("[automount]\nenabled = false"));
        assert!(conf.contains("[interop]\nenabled = false"));
        assert!(conf.contains("[boot]\nsystemd = true"));
        assert!(!conf.contains("service docker"));
        assert!(DOCKER_STEPS
            .iter()
            .any(|(_, s)| s.contains("systemctl enable --now docker")));
        let script = render_serve_script();
        assert!(script.contains("systemctl start docker"));
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(!script.contains("/etc/shutup-and-run/env"));
        assert!(script.ends_with("exec /usr/local/bin/shutup-and-run serve\n"));
        assert_eq!(
            render_wslconfig(Some("4GB"), Some(2)),
            "[wsl2]\nmemory=4GB\nprocessors=2\n"
        );
        assert_eq!(render_wslconfig(None, Some(2)), "[wsl2]\nprocessors=2\n");
        assert_eq!(validate_wsl_memory("4GB").unwrap(), "4GB");
        assert_eq!(validate_wsl_memory("512MB").unwrap(), "512MB");
        assert!(validate_wsl_memory("lots").is_err());
        assert!(validate_wsl_memory("").is_err());
    }

    #[test]
    fn sha256_lines_match_by_base_name() {
        let sums = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  dist/shutup-and-run-x86_64-unknown-linux-musl\n\
                    fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210 *ubuntu-noble-wsl-amd64-24.04lts.rootfs.tar.gz\n\
                    nothash  other\n";
        assert_eq!(
            sha256_for(sums, MUSL_ASSET).as_deref(),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
        assert_eq!(
            sha256_for(sums, ROOTFS_FILE).as_deref(),
            Some("fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210")
        );
        assert_eq!(sha256_for(sums, "other"), None);
        assert_eq!(sha256_for(sums, "missing"), None);
        assert_eq!(
            sums_url_for("https://x.example/a/b/file.tar.gz"),
            "https://x.example/a/b/SHA256SUMS"
        );
        assert!(release_url(MUSL_ASSET)
            .ends_with(&format!("/v{}/{MUSL_ASSET}", env!("CARGO_PKG_VERSION"))));

        let dir = std::env::temp_dir().join(format!("shutup-and-run-sha-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("x");
        std::fs::write(&file, b"abc").unwrap();
        assert_eq!(
            sha256_file(&file).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn task_command_line_and_report() {
        assert!(is_token(&"a".repeat(64)));
        assert!(!is_token("short"));
        assert!(!is_token("has space in it, and is long enough"));

        let script = task_register_script(Path::new(r"C:\ProgramData\shutup-and-run\it's.exe"));
        // No window: the console host is headless and the .exe is its argument.
        assert!(script.contains(r"-Execute ($env:WINDIR + '\System32\conhost.exe')"));
        assert!(script.contains(
            r"-Argument ('--headless ' + $q + 'C:\ProgramData\shutup-and-run\it''s.exe' + $q + ' wsl serve --background')"
        ));
        assert!(script.contains("$q = [char]34"));
        assert!(graceful_stop_script(3).contains("docker stop -t 3 $ids"));
        assert!(graceful_stop_script(10).starts_with("pkill -TERM -x shutup-and-run;"));
        // Tied to the current user, which is what a non-administrator may register.
        assert!(script.contains(r"$u = $env:USERDOMAIN + '\' + $env:USERNAME"));
        assert!(script.contains("New-ScheduledTaskTrigger -AtLogOn -User $u"));
        assert!(script.contains("-TaskName 'shutup-and-run'"));
        assert!(script.contains("-RunLevel Limited -Force"));
        assert!(script.contains("-AllowStartIfOnBatteries"));
        assert!(script.contains("-DontStopIfGoingOnBatteries"));
        assert!(script.contains("[TimeSpan]::Zero"));
        assert!(script.contains("IgnoreNew"));
        assert!(!script.contains('"'));
        assert!(!script.contains("ONLOGON"));

        let checks = vec![
            check("wsl", true, "ok"),
            check("docker", false, "no"),
            soft("task", false, "absent"),
        ];
        let (text, healthy) = report(&checks);
        assert!(!healthy);
        assert!(text.contains("FAIL docker"));
        assert!(text.contains("--   task"));
        let (_, healthy) = report(&[check("wsl", true, ""), soft("task", false, "")]);
        assert!(healthy);

        assert_eq!(tail("a\nb\nc\n", 2), vec!["b", "c"]);
        assert_eq!(tail("a\n", 5), vec!["a"]);
        assert!(tail("", 5).is_empty());
    }

    #[test]
    fn output_lines_and_console_absence() {
        let out = Output {
            status: Some(1),
            stdout: b"\n  first\nsecond\n".to_vec(),
            stderr: b"oops\n".to_vec(),
        };
        assert_eq!(out.one_line(), "exit 1: first");
        // A failed step shows how it ended and the end of its output.
        let failed = step_failed("apt-get update", "apt-get update -q", &out);
        assert!(failed
            .to_string()
            .starts_with("apt-get update failed\n  command: apt-get update -q\n  exit 1\n"));
        assert!(failed.to_string().ends_with("    second\n    oops"));
        let long = Output {
            status: Some(2),
            stdout: (1..=50)
                .map(|i| format!("line {i}\n"))
                .collect::<String>()
                .into_bytes(),
            stderr: b"the real reason\n".to_vec(),
        };
        let tail = long.tail(FAIL_LINES);
        assert_eq!(tail.lines().count(), FAIL_LINES + 1);
        assert!(!tail.contains("line 31\n"));
        assert!(tail.contains("line 32\n"));
        assert!(tail.ends_with("the real reason"));
        assert!(out.text().contains("oops"));
        let out = Output {
            status: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        assert_eq!(out.one_line(), "timed out: ");

        // On a machine without wsl.exe the doctor says so in its first line
        // and exits 1. On Windows with WSL this test still passes when the
        // distribution is absent, because the second check fails instead.
        if cfg!(not(windows)) {
            let (checks, _) = doctor(DEFAULT_SMOKE_IMAGE, 1).unwrap();
            assert_eq!(checks.len(), 1);
            assert!(!checks[0].ok);
            assert!(checks[0].detail.contains("wsl.exe"), "{}", checks[0].detail);
            let (_, healthy) = report(&checks);
            assert!(!healthy);
        }
    }

    #[test]
    fn a_reinstall_waits_for_the_supervisor_lock_and_knows_what_the_service_said() {
        let dir = std::env::temp_dir().join(format!("sar-wsl-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SUPERVISOR_LOCK);

        // A supervisor that ends while the install waits: the lock is taken,
        // and while the install holds it no supervisor can start.
        let held = crate::lock::acquire(&path).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            drop(held);
        });
        let mut stuck = 0;
        let ours = wait_for_lock(&path, 40, Duration::from_millis(25), || stuck += 1).unwrap();
        release.join().unwrap();
        assert!(crate::lock::acquire(&path).is_err());
        drop(ours);

        // One that never ends: `on_stuck` runs once, then the install stops
        // with an error instead of writing a config nothing will read.
        let held = crate::lock::acquire(&path).unwrap();
        let mut stuck = 0;
        let refused = wait_for_lock(&path, 6, Duration::from_millis(5), || stuck += 1);
        assert!(refused.unwrap_err().contains("already running"));
        assert_eq!(stuck, 1);
        drop(held);
        let _ = std::fs::remove_dir_all(&dir);

        // Never this process, which is also named shutup-and-run.exe.
        assert_eq!(
            taskkill_args(4242),
            [
                "/F",
                "/FI",
                "IMAGENAME eq shutup-and-run.exe",
                "/FI",
                "PID ne 4242"
            ]
        );

        // The install insists on a service the doctor saw running.
        let up = check("service", true, "running (idle)");
        let down = check("service", false, "not running").fix("`shutup-and-run wsl start`");
        assert_eq!(down.fix, "`shutup-and-run wsl start`");
        assert!(!service_answers(&[check("wsl", true, ""), down]));
        assert!(service_answers(&[check("wsl", true, ""), up]));
    }

    /// The first defect: the old service kept running, on the old config,
    /// after the install wrote a new one. The stop script returns only when
    /// no process of that name is left.
    #[test]
    fn the_stop_script_returns_only_when_the_old_service_is_gone() {
        let script = stop_service_script(SERVICE_PROCESS);
        assert!(script.starts_with("pkill -TERM -x shutup-and-run;"));
        assert!(script.contains("while pgrep -x shutup-and-run >/dev/null"));
        assert!(script.contains("exit 1"));
        assert!(!script.contains('"'));

        let have = |program: &str| run(program, &["-V"], None, SHORT).is_ok_and(|o| o.ok());
        if !cfg!(target_os = "linux") || !have("pgrep") || !have("pkill") {
            return;
        }
        // A stand-in for the old service: `sleep` under a name of its own,
        // short enough for the 15 characters `pgrep -x` compares.
        let name = format!("sar-old-{}", std::process::id() % 100_000);
        let dir = std::env::temp_dir().join(format!("{name}-dir"));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join(&name);
        let sleep = ["/usr/bin/sleep", "/bin/sleep"]
            .iter()
            .find(|p| Path::new(p).exists())
            .unwrap();
        std::fs::copy(sleep, &fake).unwrap();
        let mut old = Command::new(&fake).arg("300").spawn().unwrap();
        let pid = old.id().to_string();
        // Reaped as it dies, as init does in the distribution. Unreaped, it
        // would stay in the process table and the script would wait for it.
        let reaper = std::thread::spawn(move || old.wait());
        let seen = run("pgrep", &["-x", &name], None, SHORT).unwrap();
        assert!(seen.ok(), "the stand-in is not running");

        let out = run("sh", &["-c", &stop_service_script(&name)], None, SHORT).unwrap();
        assert!(out.ok(), "{}", out.tail(5));
        let after = run("pgrep", &["-x", &name], None, SHORT).unwrap();
        assert_eq!(after.status, Some(1), "the old process survived");
        // Nothing to stop is not an error: a first install has no service.
        let out = run("sh", &["-c", &stop_service_script(&name)], None, SHORT).unwrap();
        assert!(out.ok());
        let _ = reaper.join();
        assert!(!Path::new("/proc").join(pid).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_reports_a_timeout_and_a_missing_program() {
        let missing = run("definitely-not-a-program-xyz", &[], None, SHORT).unwrap_err();
        assert!(missing.to_string().contains("not found"));
        if cfg!(unix) {
            let out = run("sh", &["-c", "sleep 5"], None, Duration::from_millis(200)).unwrap();
            assert_eq!(out.status, None);
            let out = run("sh", &["-c", "cat"], Some(b"piped".to_vec()), SHORT).unwrap();
            assert!(out.ok());
            assert_eq!(out.text(), "piped");
        }
    }
}
