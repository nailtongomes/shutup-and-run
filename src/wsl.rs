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
        "DEBIAN_FRONTEND=noninteractive apt-get install -y -q docker.io curl ca-certificates openssl",
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
      --poll-interval-secs N     default 30, and not less
      --worker-name NAME         default: the computer name
      --smoke-image IMAGE        default hello-world
      --rootfs PATH              a downloaded Ubuntu WSL rootfs (skips the download)
      --bin PATH                 the musl binary (skips the download)
      --wsl-memory SIZE          the WSL2 memory limit, like 4GB (default: 40% of the RAM)
      --wsl-processors N         the WSL2 processor limit (default: half of them)
                                 Both go to %UserProfile%\\.wslconfig when that file does
                                 not exist. A file that exists is never changed here,
                                 and without these options none is written on a PC
                                 that has other WSL distributions: see `wsl limits`.
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

shutup-and-run wsl limits [--memory SIZE] [--processors N] [--apply] [--json]
    how much of this PC WSL2 may take. Prints the PC's RAM and processors,
    the budget (40% of the RAM, half the processors, or what the flags say)
    and what %UserProfile%\\.wslconfig says. Exit 1 when the file lets WSL2
    take more. --apply writes memory= and processors= under [wsl2], keeps
    every other line and saves the old file as .wslconfig.bak. .wslconfig
    covers every WSL2 distribution of this user, and a change takes effect
    when WSL restarts; this program never runs `wsl --shutdown`.

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
        "limits" => limits_cli(rest),
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
            eprintln!("hint: {}", crate::hints::hint("FAILED"));
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
    // An install is a write: an interval kept from a release that allowed
    // less than the minimum is raised here, once.
    if map
        .get("poll_interval_secs")
        .and_then(Value::as_u64)
        .is_some_and(|n| n < crate::config::MIN_POLL_INTERVAL_SECS)
    {
        map.insert(
            "poll_interval_secs".into(),
            json!(crate::config::MIN_POLL_INTERVAL_SECS),
        );
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

fn validate_wsl_memory(raw: &str) -> WslResult<String> {
    let value = raw.trim();
    let digits = value.trim_end_matches(['G', 'M', 'g', 'm', 'B', 'b']);
    if digits.is_empty() || digits.parse::<u64>().is_err() || value.len() > 8 {
        return Err(err("--wsl-memory looks like 4GB or 512MB"));
    }
    Ok(value.to_string())
}

// ---------------------------------------------------------------------------
// The WSL2 budget of a shared PC

/// What WSL2 may take of a PC its owner is working on: about 40% of the RAM
/// and half the logical processors. Below 5 GiB of RAM the 40% is too little
/// to run Docker and one job, so the floor is 2 GiB or half the RAM.
/// Returns megabytes and processors.
pub(crate) fn wsl_budget(ram_bytes: u64, cpus: u64) -> (u64, u64) {
    const MIB: u64 = 1024 * 1024;
    let ram_mb = ram_bytes / MIB;
    let memory = (ram_mb * 2 / 5).max(2048.min(ram_mb / 2));
    (memory, (cpus / 2).max(1))
}

/// Physical RAM of this PC. None when it cannot be read.
#[cfg(windows)]
fn host_memory_bytes() -> Option<u64> {
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }
    extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
    }
    // SAFETY: the struct is the documented MEMORYSTATUSEX, zeroed, with its
    // own size in the first field as the call requires.
    unsafe {
        let mut status: MemoryStatusEx = std::mem::zeroed();
        status.length = std::mem::size_of::<MemoryStatusEx>() as u32;
        (GlobalMemoryStatusEx(&mut status) != 0).then_some(status.total_phys)
    }
}

#[cfg(not(windows))]
fn host_memory_bytes() -> Option<u64> {
    crate::resources::memory_total()
}

fn host_cpus() -> u64 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u64)
        .unwrap_or(1)
}

/// `memory` and `processors` under `[wsl2]`, as written. Keys and the section
/// name are matched without case, as WSL does.
pub(crate) fn wslconfig_values(text: &str) -> (Option<String>, Option<String>) {
    let (mut memory, mut processors) = (None, None);
    let mut in_wsl2 = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_wsl2 = line.eq_ignore_ascii_case("[wsl2]");
        } else if in_wsl2 && !line.starts_with('#') {
            if let Some((key, value)) = line.split_once('=') {
                let value = value.trim().to_string();
                match key.trim().to_ascii_lowercase().as_str() {
                    "memory" => memory = Some(value),
                    "processors" => processors = Some(value),
                    _ => {}
                }
            }
        }
    }
    (memory, processors)
}

/// The file with `memory` and `processors` set under `[wsl2]`. Every other
/// line, comment and section stays where it was.
pub(crate) fn merge_wslconfig(existing: &str, memory: &str, processors: u64) -> String {
    let newline = if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let wanted = [
        ("memory", memory.to_string()),
        ("processors", processors.to_string()),
    ];
    let mut done = [false, false];
    let mut out: Vec<String> = Vec::new();
    let mut in_wsl2 = false;
    let mut seen_wsl2 = false;
    // Where the missing keys go: right under the [wsl2] line.
    let mut header_at = None;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_wsl2 = trimmed.eq_ignore_ascii_case("[wsl2]");
            if in_wsl2 && !seen_wsl2 {
                seen_wsl2 = true;
                header_at = Some(out.len() + 1);
            }
        } else if in_wsl2 && !trimmed.starts_with('#') {
            if let Some((key, _)) = trimmed.split_once('=') {
                let key = key.trim().to_ascii_lowercase();
                if let Some(i) = wanted.iter().position(|(name, _)| *name == key) {
                    out.push(format!("{}={}", wanted[i].0, wanted[i].1));
                    done[i] = true;
                    continue;
                }
            }
        }
        out.push(line.to_string());
    }
    let missing: Vec<String> = wanted
        .iter()
        .zip(done)
        .filter(|(_, done)| !done)
        .map(|((key, value), _)| format!("{key}={value}"))
        .collect();
    match header_at {
        Some(at) => {
            for (i, line) in missing.into_iter().enumerate() {
                out.insert(at + i, line);
            }
        }
        None => {
            out.push("[wsl2]".to_string());
            out.extend(missing);
        }
    }
    let mut text = out.join(newline);
    text.push_str(newline);
    text
}

/// How the PC's `.wslconfig` stands against the budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WslLimits {
    pub host_memory_bytes: Option<u64>,
    pub host_cpus: u64,
    /// The budget: megabytes and processors.
    pub budget: (u64, u64),
    /// What `.wslconfig` says now.
    pub memory: Option<String>,
    pub processors: Option<String>,
    /// Empty when the file keeps WSL2 inside the budget.
    pub problems: Vec<String>,
}

impl WslLimits {
    fn within(&self) -> bool {
        self.problems.is_empty()
    }
}

/// `wanted` is what the operator asked for with flags; without it the budget
/// is worked out from the PC. A value in the file at or below the budget is
/// fine: the owner may have chosen less.
pub(crate) fn wsl_limits(
    ram_bytes: Option<u64>,
    cpus: u64,
    wslconfig: Option<&str>,
    wanted: (Option<u64>, Option<u64>),
) -> WslLimits {
    let worked_out = wsl_budget(ram_bytes.unwrap_or(8 * crate::resources::GIB), cpus);
    let budget = (
        wanted.0.unwrap_or(worked_out.0),
        wanted.1.unwrap_or(worked_out.1),
    );
    let (memory, processors) = wslconfig.map(wslconfig_values).unwrap_or_default();
    let mut problems = Vec::new();
    let memory_mb = memory
        .as_deref()
        .and_then(|m| crate::bytesize::parse_qty(m).ok())
        .map(|bytes| bytes as u64 / (1024 * 1024));
    match (&memory, memory_mb) {
        (None, _) => problems.push(format!(
            "no memory limit: WSL2 may take half of this PC's RAM; the budget is {}MB",
            budget.0
        )),
        (Some(text), None) => problems.push(format!("memory={text} is not a size WSL reads")),
        (Some(text), Some(mb)) if mb > budget.0 => problems.push(format!(
            "memory={text} is above the budget of {}MB",
            budget.0
        )),
        _ => {}
    }
    match (&processors, processors.as_deref().map(str::parse::<u64>)) {
        (None, _) => problems.push(format!(
            "no processor limit: WSL2 may use every processor; the budget is {}",
            budget.1
        )),
        (Some(text), Some(Ok(n))) if n > budget.1 => problems.push(format!(
            "processors={text} is above the budget of {}",
            budget.1
        )),
        (Some(text), Some(Err(_))) => problems.push(format!("processors={text} is not a number")),
        _ => {}
    }
    WslLimits {
        host_memory_bytes: ram_bytes,
        host_cpus: cpus,
        budget,
        memory,
        processors,
        problems,
    }
}

const LIMITS_APPLY: &str = "`shutup-and-run wsl limits --apply` (it keeps every other line of \
     .wslconfig and saves the old file as .wslconfig.bak)";
/// Never run by this program: it stops every WSL distribution of the user.
const WSL_RESTART: &str = "the limits take effect when WSL restarts: restart the PC, or run \
     `wsl --shutdown` at a moment when no other WSL work and no job is running";

fn read_wslconfig() -> Option<String> {
    wslconfig_path().and_then(|path| std::fs::read_to_string(path).ok())
}

fn parse_wsl_memory_mb(flags: &Flags, key: &str) -> WslResult<Option<u64>> {
    flags
        .get(key)
        .map(|raw| {
            let text = validate_wsl_memory(raw)?;
            crate::bytesize::parse_qty(&text)
                .map(|bytes| (bytes as u64 / (1024 * 1024)).max(1))
                .map_err(|e| err(format!("{key}: {e}")))
        })
        .transpose()
}

/// Write the budget into `.wslconfig`, keeping the rest of the file. An
/// existing file is copied to `.wslconfig.bak` first.
fn apply_wsl_limits(limits: &WslLimits) -> WslResult<PathBuf> {
    let path = wslconfig_path().ok_or_else(|| err("USERPROFILE is unset; no .wslconfig"))?;
    let old = std::fs::read_to_string(&path).ok();
    if old.is_some() {
        let backup = path.with_file_name(".wslconfig.bak");
        std::fs::copy(&path, &backup).map_err(|e| err(format!("{}: {e}", backup.display())))?;
    }
    let text = merge_wslconfig(
        old.as_deref().unwrap_or(""),
        &format!("{}MB", limits.budget.0),
        limits.budget.1,
    );
    std::fs::write(&path, text).map_err(|e| err(format!("{}: {e}", path.display())))?;
    Ok(path)
}

/// The doctor's line. `vm` is what the running VM has: bytes and processors.
pub(crate) fn wsl_limits_check(limits: &WslLimits, vm: Option<(u64, u64)>) -> Check {
    use crate::resources::human;
    let host = format!(
        "this PC: {}, {} processors; budget for WSL2: {}MB, {} processors",
        limits
            .host_memory_bytes
            .map(human)
            .unwrap_or_else(|| "RAM unknown".to_string()),
        limits.host_cpus,
        limits.budget.0,
        limits.budget.1
    );
    let now = vm
        .map(|(bytes, cpus)| format!("; the VM now has {}, {cpus} processors", human(bytes)))
        .unwrap_or_default();
    if !limits.within() {
        return soft(
            "wsl-limits",
            false,
            format!(".wslconfig: {}. {host}{now}", limits.problems.join("; ")),
        )
        .coded("WSL_LIMITS_ABOVE_BUDGET")
        .fix(format!("{LIMITS_APPLY}. Then {WSL_RESTART}"));
    }
    // The file is right. Is the VM still running on what it had before?
    let budget_bytes = limits.budget.0 * 1024 * 1024;
    if let Some((bytes, cpus)) = vm {
        if bytes > budget_bytes + budget_bytes / 10 || cpus > limits.budget.1 {
            return soft(
                "wsl-limits",
                false,
                format!(".wslconfig is within the budget, but not in effect yet. {host}{now}"),
            )
            .coded("WSL_LIMITS_NOT_IN_EFFECT")
            .fix(WSL_RESTART);
        }
    }
    soft("wsl-limits", true, format!("{host}{now}"))
}

fn vm_size() -> Option<(u64, u64)> {
    let out = sh_in_distro("grep MemTotal: /proc/meminfo; nproc", None, SHORT).ok()?;
    let text = out.text();
    let bytes = crate::resources::meminfo_field(&text, "MemTotal:")?;
    let cpus = text.lines().last()?.trim().parse().ok()?;
    Some((bytes, cpus))
}

fn limits_cli(args: &[String]) -> WslResult<ExitCode> {
    let flags = parse_flags(args, &["--memory", "--processors"], &["--apply", "--json"])?;
    let wanted = (
        parse_wsl_memory_mb(&flags, "--memory")?,
        flags
            .get("--processors")
            .map(|_| parse_count(&flags, "--processors", 1, 1..=256))
            .transpose()?,
    );
    let read = || {
        wsl_limits(
            host_memory_bytes(),
            host_cpus(),
            read_wslconfig().as_deref(),
            wanted,
        )
    };
    let mut limits = read();
    let mut applied = None;
    // With a size given, --apply sets exactly that, even downwards or upwards.
    let explicit = wanted.0.is_some() || wanted.1.is_some();
    if flags.has("--apply") && (!limits.within() || explicit) {
        applied = Some(apply_wsl_limits(&limits)?);
        limits = read();
    }
    let path = wslconfig_path();
    if flags.has("--json") {
        println!(
            "{}",
            serde_json::json!({
                "ok": limits.within(),
                "error": (!limits.within()).then(|| limits.problems.join("; ")),
                "host": {"memory_bytes": limits.host_memory_bytes, "cpus": limits.host_cpus},
                "budget": {"memory_mb": limits.budget.0, "processors": limits.budget.1},
                "wslconfig": {
                    "path": path,
                    "memory": limits.memory,
                    "processors": limits.processors,
                },
                "applied": applied.is_some(),
                "restart_needed": applied.is_some(),
                "fix": (!limits.within()).then_some(LIMITS_APPLY),
                "note": WSL_RESTART,
            })
        );
    } else {
        let check = wsl_limits_check(&limits, None);
        println!("{}", report(std::slice::from_ref(&check)).0.trim_end());
        println!(
            ".wslconfig now: memory={}, processors={}",
            limits.memory.as_deref().unwrap_or("(not set)"),
            limits.processors.as_deref().unwrap_or("(not set)")
        );
        if let Some(path) = &applied {
            println!("wrote {}. {WSL_RESTART}", path.display());
        }
    }
    Ok(ExitCode::from(if limits.within() { 0 } else { 1 }))
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
        &recent_log_problems(),
        flags.has("--report"),
    );
    let mut out = out;
    crate::hints::attach(&mut out.json, out.code);
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
                    .coded("WSL_UNAVAILABLE")
                    .fix("run `wsl --install --no-distribution` as administrator and reboot"),
            );
            return Ok((checks, None));
        }
    };
    if !status.ok() {
        checks.push(
            check("wsl", false, status.one_line())
                .coded("WSL_UNAVAILABLE")
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
                check("distribution", false, format!("{DISTRO} is not registered"))
                    .coded("DISTRIBUTION_MISSING")
                    .fix(INSTALL),
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
        checks.push(
            check("docker", false, docker.one_line())
                .coded("DOCKER_UNAVAILABLE")
                .fix(format!(
            "`shutup-and-run wsl stop`, then `shutup-and-run wsl start`. Still failing: {RESET}"
        )),
        );
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
        .coded("TASK_NOT_REGISTERED")
        .fix("`shutup-and-run wsl task register`"),
        Err(e) => soft("task", false, e.to_string()),
    });
    checks.push(wsl_limits_check(
        &wsl_limits(
            host_memory_bytes(),
            host_cpus(),
            read_wslconfig().as_deref(),
            (None, None),
        ),
        vm_size(),
    ));

    Ok((checks, inner))
}

// ---------------------------------------------------------------------------
// install

/// `--poll-interval-secs`: a number from the minimum to an hour. `Err` is
/// an invalid value, which is exit 3.
pub(crate) fn install_interval(raw: Option<&str>) -> Result<Option<u64>, String> {
    let min = crate::config::MIN_POLL_INTERVAL_SECS;
    match raw {
        None => Ok(None),
        Some(raw) => match raw.parse::<u64>() {
            Ok(n) if (min..=3600).contains(&n) => Ok(Some(n)),
            _ => Err(format!(
                "--poll-interval-secs must be at least {min} (and at most 3600), not {raw}: \
                 nothing was installed or changed"
            )),
        },
    }
}

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
    // The same answer as `config set poll_interval_secs`: exit 3, and
    // nothing is installed or changed.
    let poll_interval_secs = match install_interval(flags.get("--poll-interval-secs")) {
        Ok(n) => n,
        Err(msg) => {
            eprintln!("shutup-and-run wsl: {msg}");
            eprintln!("hint: {}", crate::hints::hint("POLL_INTERVAL_TOO_LOW"));
            return Ok(ExitCode::from(crate::cli::EXIT_INVALID));
        }
    };
    let mut options = InstallOptions {
        poll_url: poll_url.clone(),
        max_slots: count("--max-slots", 1..=256)?,
        poll_interval_secs,
        worker_name: flags.get("--worker-name").map(str::to_string),
        poll_token: given_token.clone(),
    };
    let computer = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "windows-worker".to_string());
    let smoke_image = flags.get("--smoke-image").unwrap_or(DEFAULT_SMOKE_IMAGE);
    let wsl_wanted = (
        parse_wsl_memory_mb(&flags, "--wsl-memory")?,
        flags
            .get("--wsl-processors")
            .map(|_| parse_count(&flags, "--wsl-processors", 1, 1..=256))
            .transpose()?,
    );

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
    // Before the distribution starts, so a PC with no WSL running yet gets
    // the limits from its first boot. `.wslconfig` is the user's and covers
    // every WSL2 distribution: a file that exists is reported on, not edited.
    let limits = wsl_limits(
        host_memory_bytes(),
        host_cpus(),
        read_wslconfig().as_deref(),
        wsl_wanted,
    );
    // Someone else's distribution (Docker Desktop, a developer's Ubuntu)
    // would be capped by the same file, so then it is the owner's call.
    let others: Vec<String> = list_distros()?
        .into_iter()
        .map(|d| d.name)
        .filter(|name| name != DISTRO)
        .collect();
    match wslconfig_path() {
        // Unless the operator named a size: then it was asked for.
        Some(path) if !path.exists() && !others.is_empty() && wsl_wanted == (None, None) => {
            println!(
                "    {} does not exist and was not written, because it would also limit {}: {}",
                path.display(),
                others.join(", "),
                limits.problems.join("; ")
            );
            println!("    to set the limits: {LIMITS_APPLY}");
        }
        Some(path) if !path.exists() => {
            apply_wsl_limits(&limits)?;
            println!(
                "    wrote {}: memory={}MB, processors={}",
                path.display(),
                limits.budget.0,
                limits.budget.1
            );
        }
        Some(path) if limits.within() => {
            println!("    {} keeps WSL2 inside the budget", path.display())
        }
        Some(path) => {
            println!(
                "    {} exists and was not changed: {}",
                path.display(),
                limits.problems.join("; ")
            );
            println!("    to set the limits: {LIMITS_APPLY}");
        }
        None => println!("    USERPROFILE is unset; .wslconfig was not written"),
    }

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

    // A config kept from before a reset may say passthrough is on while the
    // certificate went with the old distribution. Every job would then fail
    // to start, so it is turned off, and the owner imports the file again.
    let enabled = text.contains("\"certificate_enabled\": true");
    if enabled
        && !sh_in_distro(
            "test -f /etc/shutup-and-run/certificate/client.pfx",
            None,
            SHORT,
        )
        .is_ok_and(|out| out.ok())
    {
        let _ = in_distro(&[INNER_BIN, "certificate", "disable"], None, SHORT);
        let _ = refresh_host_backup();
        println!(
            "    certificate passthrough was on, and the certificate is not in this distribution: \
             turned off. Import it again with `shutup-and-run certificate set`."
        );
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
    let (text, _) = report(&checks);
    print!("{text}");
    for check in checks
        .iter()
        .filter(|c| !c.ok && !c.soft && c.name == "poll")
    {
        println!(
            "note: the install is complete. {} The worker keeps trying and needs nothing done here.",
            check.detail
        );
    }
    if !install_healthy(&checks) {
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

/// What makes an install fail. The poll URL is someone else's server: one
/// that does not answer at this minute is reported, and the install, which
/// did everything it does on this PC, still ends with 0.
pub(crate) fn install_healthy(checks: &[Check]) -> bool {
    checks.iter().all(|c| c.ok || c.soft || c.name == "poll")
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

/// The language of the hints, from the host's copy of the config.
pub fn load_language() {
    if let Some(language) = host_backup().and_then(|map| {
        map.get("language")
            .and_then(|v| v.as_str().map(String::from))
    }) {
        crate::hints::set_language(&language);
    }
}

/// Copy the distribution's config.json to the Windows side, when it is valid.
fn refresh_host_backup() -> Option<JsonMap> {
    let text = inner_text(INNER_CONFIG)?;
    let map = crate::config::parse_json(&text).ok()?;
    crate::config::Settings::from_json_map(&map).ok()?;
    save_host_backup(&text);
    Some(map)
}

/// The desktop folder, asked of PowerShell as base64 of UTF-8. Printed as
/// text it comes through the console's code page, and "Área de Trabalho"
/// arrives as a folder that does not exist (error 3).
fn desktop_dir() -> WslResult<PathBuf> {
    let out = run(
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes([Environment]::GetFolderPath('Desktop')))",
        ],
        None,
        SHORT,
    )?;
    out.ok()
        .then(|| path_from_base64(&out.text()))
        .flatten()
        .ok_or_else(|| err("the desktop folder was not found"))
}

/// A path PowerShell printed as base64 of its UTF-8 bytes.
pub(crate) fn path_from_base64(text: &str) -> Option<PathBuf> {
    use base64::Engine;
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(line)
        .ok()?;
    let path = String::from_utf8(bytes).ok()?;
    (!path.trim().is_empty()).then(|| PathBuf::from(path.trim()))
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
            let code = if msg.contains("is not installed") {
                "NOT_INSTALLED"
            } else {
                "FAILED"
            };
            println!("{}", crate::cli::failure_json(code, &msg));
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

/// `job validate FILE`, `demo sign` and `certificate set --file` on Windows.
/// The service is inside the distribution, which mounts no Windows drive, so
/// a file named on the Windows side is read here and handed over on stdin,
/// and the signed PDF of the demo is written here from the answer.
pub fn forward_with_files(args: &[String]) -> ExitCode {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let as_json = args.iter().any(|a| a == "--json");
    let fail = |msg: String| {
        if as_json {
            let code = if msg.contains("is not installed") {
                "NOT_INSTALLED"
            } else {
                "FAILED"
            };
            println!("{}", crate::cli::failure_json(code, &msg));
        } else {
            eprintln!("shutup-and-run: {msg}");
        }
        ExitCode::from(1)
    };
    let value_of = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|at| args.get(at + 1))
            .cloned()
    };
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut inner: Vec<String> = vec![INNER_BIN.to_string()];
    let mut stdin = None;
    let mut pdf_out = None;
    match words[..] {
        ["job", "validate", file, ..] if file != "-" && !file.starts_with("--") => {
            match std::fs::read(file) {
                Ok(bytes) => stdin = Some(bytes),
                Err(e) => return fail(format!("{file}: {e}")),
            }
            inner.extend(args.iter().map(|a| {
                if a == file {
                    "-".to_string()
                } else {
                    a.clone()
                }
            }));
        }
        ["demo", "sign", ..] if !args.iter().any(|a| a == "--help" || a == "-h") => {
            pdf_out =
                Some(PathBuf::from(value_of("--out").unwrap_or_else(|| {
                    "shutup-and-run-demo-signed.pdf".to_string()
                })));
            let mut skip = false;
            for arg in args {
                if std::mem::take(&mut skip) {
                    continue;
                }
                match arg.as_str() {
                    "--out" => skip = true,
                    "--json" | "--inline" => {}
                    _ => inner.push(arg.clone()),
                }
            }
            inner.extend(["--json".to_string(), "--inline".to_string()]);
        }
        ["certificate", "set", ..] if value_of("--file").is_some() => {
            let file = value_of("--file").unwrap_or_default();
            let pfx = match std::fs::read(&file) {
                Ok(bytes) => bytes,
                Err(e) => return fail(format!("{file}: {e}")),
            };
            let password = if args.iter().any(|a| a == "--password-stdin") {
                let mut line = String::new();
                if let Err(e) = std::io::stdin().read_line(&mut line) {
                    return fail(format!("could not read the password from stdin: {e}"));
                }
                line.trim_end_matches(['\n', '\r']).to_string()
            } else {
                match std::env::var("SHUTUP_AND_RUN_CERT_PASSWORD") {
                    Ok(password) => password,
                    Err(_) => return fail(
                        "the certificate password is not given: set SHUTUP_AND_RUN_CERT_PASSWORD, \
                             or pass --password-stdin and write it on stdin"
                            .to_string(),
                    ),
                }
            };
            let name = Path::new(&file)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            stdin = Some(
                serde_json::json!({"name": name, "pfx_base64": b64.encode(pfx), "password": password})
                    .to_string()
                    .into_bytes(),
            );
            let mut skip = false;
            for arg in args {
                if std::mem::take(&mut skip) {
                    continue;
                }
                match arg.as_str() {
                    "--file" => skip = true,
                    "--password-stdin" => {}
                    _ => inner.push(arg.clone()),
                }
            }
            inner.push("--stdin-json".to_string());
        }
        _ => inner.extend(args.iter().cloned()),
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
    let inner: Vec<&str> = inner.iter().map(String::as_str).collect();
    // A demo pulls an image the first time.
    let out = match in_distro(&inner, stdin, APT) {
        Ok(out) => out,
        Err(e) => return fail(e.to_string()),
    };
    let code = ExitCode::from(out.status.unwrap_or(1).clamp(0, 255) as u8);
    let Some(pdf_out) = pdf_out else {
        print!("{}", String::from_utf8_lossy(&out.stdout));
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
        if out.ok() && words.first() == Some(&"certificate") {
            let _ = refresh_host_backup();
        }
        return code;
    };
    // The signed PDF comes back inside the JSON and is written on this side.
    let Ok(mut answer) = serde_json::from_str::<serde_json::Value>(out.text().trim()) else {
        print!("{}", String::from_utf8_lossy(&out.stdout));
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
        return code;
    };
    let pdf = answer
        .as_object_mut()
        .and_then(|o| o.remove("signed_pdf_base64"))
        .and_then(|v| v.as_str().and_then(|s| b64.decode(s).ok()));
    let mut wrote = String::new();
    if let Some(pdf) = pdf {
        match std::fs::write(&pdf_out, pdf) {
            Ok(()) => {
                answer["output"] = serde_json::json!(pdf_out);
                wrote = format!("wrote {}\n", pdf_out.display());
            }
            Err(e) => wrote = format!("could not write {}: {e}\n", pdf_out.display()),
        }
    }
    if as_json {
        println!("{answer}");
    } else {
        let checks = crate::cli::checks_from_json(&answer["checks"]);
        print!("{}{wrote}", report(&checks).0);
        println!(
            "{}",
            if answer["ok"] == true {
                "signing demo passed"
            } else {
                "signing demo failed: run the `fix:` under each FAIL"
            }
        );
    }
    code
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

/// The last warnings and errors of the supervisor log, for the report. The
/// log never holds a token or a job's env, and the report is scrubbed again.
fn recent_log_problems() -> Vec<String> {
    let text = std::fs::read_to_string(log_path()).unwrap_or_default();
    problems_in(&text, 400, 12)
}

pub(crate) fn problems_in(text: &str, window: usize, keep: usize) -> Vec<String> {
    let found: Vec<&str> = tail(text, window)
        .into_iter()
        .filter(|line| line.contains("WARN") || line.contains("ERROR") || line.contains("failed"))
        .collect();
    found[found.len().saturating_sub(keep)..]
        .iter()
        .map(|line| line.chars().take(300).collect())
        .collect()
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
            poll_interval_secs: Some(40),
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
        assert_eq!(settings.poll_interval, Duration::from_secs(40));
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
        assert_eq!(
            settings.poll_token.as_ref().unwrap().as_str(),
            "c".repeat(32)
        );
        assert!(config_from_toml("not = [toml", None).is_none());

        // Migration of an old config whose interval is under the minimum (the
        // 15 above): as it was left on the disk it still loads, polls every
        // 30 s and says so; the install then writes 30 and the warning ends.
        let mut left = carried.clone();
        left.insert("token".into(), map["token"].clone());
        left.insert("allowlist".into(), map["allowlist"].clone());
        let as_left = crate::config::Settings::from_json_map(&left).unwrap();
        assert_eq!(as_left.poll_interval, Duration::from_secs(30));
        assert_eq!(as_left.poll_interval_raised_from, Some(15));
        let note = crate::cli::poll_interval_note(&as_left).unwrap();
        assert!(note.contains("15") && note.contains("polls every 30 s"));
        assert_eq!(map["poll_interval_secs"], 30);
        assert_eq!(settings.poll_interval, Duration::from_secs(30));
        assert_eq!(settings.poll_interval_raised_from, None);
        assert!(crate::cli::poll_interval_note(&settings).is_none());

        // `wsl install --poll-interval-secs 5` is an invalid value (exit 3),
        // like `config set poll_interval_secs 5`.
        assert_eq!(install_interval(None), Ok(None));
        assert_eq!(install_interval(Some("30")), Ok(Some(30)));
        assert_eq!(install_interval(Some("3600")), Ok(Some(3600)));
        for bad in ["5", "29", "0", "3601", "soon", ""] {
            let said = install_interval(Some(bad)).unwrap_err();
            assert!(said.contains("at least 30"), "{bad}: {said}");
        }

        // A pairing query in the poll URL goes into the config whole.
        let paired = install_config(
            None,
            &InstallOptions {
                poll_url: Some(
                    "https://jobs.example/poll?cpf=00000000000&email=nobody%40example.com".into(),
                ),
                ..InstallOptions::default()
            },
            "X",
            "hello-world",
        );
        assert_eq!(
            crate::config::Settings::from_json_map(&paired)
                .unwrap()
                .poll_url
                .as_deref(),
            Some("https://jobs.example/poll?cpf=00000000000&email=nobody%40example.com")
        );
        assert_eq!(
            crate::config::masked(&paired)["poll_url"],
            "https://jobs.example/poll?cpf=***&email=***"
        );

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
        assert_eq!(validate_wsl_memory("4GB").unwrap(), "4GB");
        assert_eq!(validate_wsl_memory("512MB").unwrap(), "512MB");
        assert!(validate_wsl_memory("lots").is_err());
        assert!(validate_wsl_memory("").is_err());
    }

    const GIB: u64 = crate::resources::GIB;

    #[test]
    fn the_wsl2_budget_is_about_40_percent_of_the_ram_and_half_the_processors() {
        assert_eq!(wsl_budget(16 * GIB, 8), (6553, 4));
        assert_eq!(wsl_budget(8 * GIB, 4), (3276, 2));
        assert_eq!(wsl_budget(32 * GIB, 16), (13107, 8));
        // Too small for 40% to hold Docker and a job: 2 GiB, or half the RAM.
        assert_eq!(wsl_budget(4 * GIB, 2), (2048, 1));
        assert_eq!(wsl_budget(3 * GIB, 1), (1536, 1));
        // What the service then gives one job inside such a VM.
        let job =
            |ram| crate::resources::shared_pc_memory(Some(wsl_budget(ram, 4).0 * 1024 * 1024));
        assert_eq!(job(16 * GIB), 2 * GIB);
        assert_eq!(job(8 * GIB), 3276 * 1024 * 1024 / 2);
        assert_eq!(job(4 * GIB), GIB);
    }

    #[test]
    fn an_existing_wslconfig_keeps_everything_that_is_not_ours() {
        let theirs = "# my settings\r\n[wsl2]\r\nswap=0\r\nMemory = 12GB\r\n# processors=99\r\n\r\n[experimental]\r\nsparseVhd=true\r\nmemory=1GB\r\n";
        assert_eq!(wslconfig_values(theirs), (Some("12GB".to_string()), None));
        let merged = merge_wslconfig(theirs, "6553MB", 4);
        assert_eq!(
            merged,
            "# my settings\r\n[wsl2]\r\nprocessors=4\r\nswap=0\r\nmemory=6553MB\r\n# processors=99\r\n\r\n[experimental]\r\nsparseVhd=true\r\nmemory=1GB\r\n"
        );
        // Merging again changes nothing.
        assert_eq!(merge_wslconfig(&merged, "6553MB", 4), merged);
        assert_eq!(
            wslconfig_values(&merged),
            (Some("6553MB".to_string()), Some("4".to_string()))
        );

        // No [wsl2] section: it is added after what is there.
        assert_eq!(
            merge_wslconfig("[experimental]\nsparseVhd=true\n", "4GB", 2),
            "[experimental]\nsparseVhd=true\n[wsl2]\nmemory=4GB\nprocessors=2\n"
        );
        // No file.
        assert_eq!(
            merge_wslconfig("", "3276MB", 2),
            "[wsl2]\nmemory=3276MB\nprocessors=2\n"
        );
    }

    #[test]
    fn wsl_limits_report_a_file_that_lets_wsl2_take_more() {
        let none = (None, None);
        // No file: both limits are missing.
        let limits = wsl_limits(Some(16 * GIB), 8, None, none);
        assert_eq!(limits.budget, (6553, 4));
        assert_eq!(limits.problems.len(), 2);
        let check = wsl_limits_check(&limits, None);
        assert!(!check.ok && check.soft);
        assert!(check.fix.contains("shutup-and-run wsl limits --apply"));
        assert!(check.fix.contains("wsl --shutdown"));

        // A file above the budget, and one the owner set lower.
        let limits = wsl_limits(
            Some(16 * GIB),
            8,
            Some("[wsl2]\nmemory=12GB\nprocessors=8\n"),
            none,
        );
        assert_eq!(
            limits.problems,
            vec![
                "memory=12GB is above the budget of 6553MB",
                "processors=8 is above the budget of 4"
            ]
        );
        let lower = wsl_limits(
            Some(16 * GIB),
            8,
            Some("[wsl2]\nmemory=4GB\nprocessors=2\n"),
            none,
        );
        assert!(lower.within());
        assert!(wsl_limits_check(&lower, Some((4 * GIB, 2))).ok);
        let junk = wsl_limits(
            Some(16 * GIB),
            8,
            Some("[wsl2]\nmemory=lots\nprocessors=x\n"),
            none,
        );
        assert_eq!(junk.problems.len(), 2);

        // The file is right but the VM booted before it was written.
        let stale = wsl_limits_check(&lower, Some((8 * GIB, 8)));
        assert!(!stale.ok && stale.soft);
        assert!(stale.detail.contains("not in effect yet"));
        assert!(!stale.fix.contains("--apply"));

        // What the operator asked for replaces the worked-out budget.
        let asked = wsl_limits(
            Some(16 * GIB),
            8,
            Some("[wsl2]\nmemory=8GB\nprocessors=6\n"),
            (Some(8192), Some(6)),
        );
        assert!(asked.within());
        // A PC whose RAM cannot be read is treated as 8 GiB.
        assert_eq!(wsl_limits(None, 4, None, none).budget, (3276, 2));
    }

    #[test]
    fn the_desktop_path_survives_accents_and_the_install_survives_a_silent_poll_url() {
        use base64::Engine;
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        // What PowerShell prints for a Portuguese OneDrive desktop.
        let desktop = "C:\\Users\\José\\OneDrive\\Área de Trabalho";
        let printed = format!("{}\r\n", b64(desktop));
        let path = path_from_base64(&printed).unwrap();
        assert_eq!(path, PathBuf::from(desktop));
        assert!(path
            .join(SHORTCUT_FILE)
            .to_string_lossy()
            .contains("Área de Trabalho"));
        assert_eq!(
            path_from_base64(&b64("C:\\Users\\Ana\\Desktop")).unwrap(),
            PathBuf::from("C:\\Users\\Ana\\Desktop")
        );
        for bad in ["", "\r\n", "not base64 !!", &b64("   ")] {
            assert!(path_from_base64(bad).is_none(), "{bad:?}");
        }

        // An upgrade whose poll URL does not answer this minute is complete.
        let up = check("service", true, "running (idle)");
        let silent = check(
            "poll",
            false,
            "cannot reach https://q.example/poll: no answer in 8 s",
        )
        .coded("POLL_UNREACHABLE");
        assert!(install_healthy(&[
            up.clone(),
            silent.clone(),
            soft("task", false, "")
        ]));
        assert!(service_answers(&[up.clone(), silent]));
        // Anything else that failed still fails it.
        assert!(!install_healthy(&[up, check("docker", false, "down")]));

        // A kept config with the old 2 s interval is raised by the install.
        let mut old = JsonMap::new();
        old.insert("poll_interval_secs".into(), serde_json::json!(2));
        let options = InstallOptions {
            poll_url: None,
            max_slots: None,
            poll_interval_secs: None,
            worker_name: None,
            poll_token: None,
        };
        let map = install_config(Some(&old), &options, "PC", "hello-world");
        assert_eq!(map["poll_interval_secs"], 30);
        assert_eq!(
            install_config(None, &options, "PC", "hello-world").get("poll_interval_secs"),
            None
        );
    }

    #[test]
    fn the_report_takes_the_last_problems_of_the_log() {
        let log =
            "a INFO up\nb WARN poll: refused\nc INFO ok\nd ERROR docker\ne the service failed\n";
        assert_eq!(
            problems_in(log, 400, 2),
            vec!["d ERROR docker", "e the service failed"]
        );
        assert!(problems_in("INFO fine\n", 400, 12).is_empty());
        assert_eq!(
            problems_in(&format!("WARN {}", "x".repeat(900)), 5, 5)[0].len(),
            300
        );
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
