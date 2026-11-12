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
const INNER_CONFIG: &str = "/etc/shutup-and-run/config.toml";
const INNER_ENV: &str = "/etc/shutup-and-run/env";
const INNER_LISTEN: &str = "127.0.0.1:8787";

const DEFAULT_SMOKE_IMAGE: &str = "hello-world";
const LOG_FILE: &str = "supervisor.log";
const LOG_ROTATE_BYTES: u64 = 10 * 1024 * 1024;
const RESTART_MIN: Duration = Duration::from_secs(5);
const RESTART_MAX: Duration = Duration::from_secs(60);

/// Run inside the distribution by `wsl stop`, before the VM is terminated.
const GRACEFUL_STOP: &str = "pkill -TERM -x shutup-and-run; \
     ids=$(docker ps -q --filter label=shutup-and-run=1 2>/dev/null); \
     [ -z \"$ids\" ] || docker stop -t 10 $ids >/dev/null";

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
shutup-and-run wsl doctor [--smoke-image IMAGE]
    check WSL2, the shutup-and-run distribution, Docker inside it, and run
    one container as a smoke test. Exit 1 when something is missing.

shutup-and-run wsl install [--poll-url URL] [options]
    create the distribution, install Docker and the Linux binary in it, write a
    pull-only config, register the logon task, and run the doctor.
    Without --poll-url the worker is installed idle: only the doctor's smoke
    test runs. SHUTUP_AND_RUN_POLL_TOKEN is the token the poll URL knows this
    machine by. Unset, the machine keeps the one it has or makes its own, and
    the poll URL decides whether to serve it.
      --max-slots N              containers at once (default 1)
      --poll-interval-secs N     default 15
      --worker-name NAME         default: the computer name
      --smoke-image IMAGE        default hello-world
      --rootfs PATH              a downloaded Ubuntu WSL rootfs (skips the download)
      --bin PATH                 the musl binary (skips the download)
      --wsl-memory SIZE          write memory=SIZE to %UserProfile%\\.wslconfig
      --wsl-processors N         write processors=N to %UserProfile%\\.wslconfig
      --no-task                  do not register the scheduled task

shutup-and-run wsl serve [--background]
    run the service inside the distribution and restart it if it exits.
    --background detaches from the console and writes only to the log.

shutup-and-run wsl start | stop
    start or end the scheduled task. stop gives running containers SIGTERM
    and 10 seconds, then terminates the distribution.

shutup-and-run wsl logs [--lines N]
    print the end of the supervisor log (default 100 lines).

shutup-and-run wsl uninstall [--purge]
    delete the task and the distribution. --purge also removes the data folder."
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
        "serve" => serve_cli(rest),
        "start" => start_cli(),
        "stop" => stop_cli(),
        "logs" => logs_cli(rest),
        "uninstall" => uninstall_cli(rest),
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
        return Err(err(format!("could not write {path}: {}", out.one_line())));
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
     command = \"service docker start\"\n\
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
         set -a\n\
         . {INNER_ENV}\n\
         set +a\n\
         i=0\n\
         until docker info >/dev/null 2>&1; do\n\
         \x20 [ \"$i\" -eq 0 ] && service docker start >/dev/null 2>&1\n\
         \x20 i=$((i + 1))\n\
         \x20 if [ \"$i\" -ge 60 ]; then echo 'dockerd did not answer within 60s' >&2; exit 1; fi\n\
         \x20 sleep 1\n\
         done\n\
         exec {INNER_BIN} serve\n"
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InnerConfig {
    /// None: nothing to poll. The service idles and only the doctor runs a container.
    pub poll_url: Option<String>,
    pub smoke_image: String,
    pub poll_interval_secs: u64,
    pub max_slots: u64,
    pub worker_name: String,
}

/// The pull-only config. No allowlist: the poll URL decides the image, which
/// is what `poll_any_image` means. The listener stays on localhost inside the
/// distribution, for the doctor and for `GET /v1/jobs/{id}` from a shell.
///
/// Without a poll URL nothing is polled and `poll_any_image` cannot be set, so
/// the allowlist holds the smoke image, because the service wants one entry.
pub(crate) fn render_config(c: &InnerConfig) -> String {
    let work = match &c.poll_url {
        Some(url) => format!(
            "allowlist = []\n\
             poll_url = {}\n\
             poll_interval_secs = {}\n\
             poll_any_image = true\n",
            toml_string(url),
            c.poll_interval_secs,
        ),
        None => format!("allowlist = [{}]\n", toml_string(&c.smoke_image)),
    };
    format!(
        "# Written by shutup-and-run.exe wsl install. Edit and restart the task.\n\
         listen = \"{INNER_LISTEN}\"\n\
         http = true\n\
         max_slots = {}\n\
         {work}\
         worker_name = {}\n\
         worker_os = \"windows\"\n\
         history_path = \"/var/lib/shutup-and-run/jobs.sqlite\"\n",
        c.max_slots,
        toml_string(&c.worker_name),
    )
}

fn toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `/etc/shutup-and-run/env`, mode 600. Both tokens live only here.
pub(crate) fn render_env(api_token: &str, poll_token: Option<&str>) -> String {
    let mut text = format!("SHUTUP_AND_RUN_TOKEN={api_token}\n");
    if let Some(poll_token) = poll_token {
        text.push_str(&format!("SHUTUP_AND_RUN_POLL_TOKEN={poll_token}\n"));
    }
    text
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Check {
    pub name: &'static str,
    pub ok: bool,
    /// A failed check that does not fail the doctor: the service is simply
    /// not running yet, or the task was not registered on purpose.
    pub soft: bool,
    pub detail: String,
}

fn check(name: &'static str, ok: bool, detail: impl Into<String>) -> Check {
    Check {
        name,
        ok,
        soft: false,
        detail: detail.into(),
    }
}

fn soft(name: &'static str, ok: bool, detail: impl Into<String>) -> Check {
    Check {
        name,
        ok,
        soft: true,
        detail: detail.into(),
    }
}

pub(crate) fn report(checks: &[Check]) -> (String, bool) {
    let mut text = String::new();
    let mut healthy = true;
    for c in checks {
        let mark = if c.ok {
            "ok  "
        } else if c.soft {
            "--  "
        } else {
            healthy = false;
            "FAIL"
        };
        text.push_str(&format!("{mark} {:<22} {}\n", c.name, c.detail));
    }
    (text, healthy)
}

fn doctor_cli(args: &[String]) -> WslResult<ExitCode> {
    let flags = parse_flags(args, &["--smoke-image"], &[])?;
    let image = flags.get("--smoke-image").unwrap_or(DEFAULT_SMOKE_IMAGE);
    let checks = doctor(image)?;
    let (text, healthy) = report(&checks);
    print!("{text}");
    if healthy {
        println!("the Windows worker is ready");
        Ok(ExitCode::SUCCESS)
    } else {
        println!("something is missing. `shutup-and-run wsl install` sets up what FAIL names.");
        Ok(ExitCode::from(1))
    }
}

/// Each check needs the one before it. The first failure stops the list,
/// so a machine without WSL gets one line, not a page of errors.
fn doctor(smoke_image: &str) -> WslResult<Vec<Check>> {
    let mut checks = Vec::new();

    let status = match wsl_status() {
        Ok(out) => out,
        Err(e) => {
            checks.push(check("wsl", false, e.to_string()));
            return Ok(checks);
        }
    };
    if !status.ok() {
        checks.push(check(
            "wsl",
            false,
            format!(
                "{}. Run `wsl --install --no-distribution` as administrator and reboot.",
                status.one_line()
            ),
        ));
        return Ok(checks);
    }
    checks.push(check("wsl", true, status.one_line()));

    match our_distro()? {
        Some(d) if d.version == "2" => checks.push(check(
            "distribution",
            true,
            format!("{DISTRO} ({})", d.state),
        )),
        Some(d) => {
            checks.push(check(
                "distribution",
                false,
                format!("{DISTRO} is WSL version {}; it must be 2", d.version),
            ));
            return Ok(checks);
        }
        None => {
            checks.push(check(
                "distribution",
                false,
                format!("{DISTRO} is not registered"),
            ));
            return Ok(checks);
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
        checks.push(check("docker", false, docker.one_line()));
        return Ok(checks);
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
    checks.push(check(
        "container",
        smoke.ok(),
        if smoke.ok() {
            format!("{smoke_image} ran: {first}")
        } else {
            format!("{smoke_image}: {}", smoke.one_line())
        },
    ));

    let bin = in_distro(&[INNER_BIN, "--help"], None, DISTRO_START)?;
    checks.push(check(
        "binary",
        bin.ok(),
        if bin.ok() {
            INNER_BIN.to_string()
        } else {
            format!("{INNER_BIN}: {}", bin.one_line())
        },
    ));

    let config = sh_in_distro(
        &format!("test -s {INNER_CONFIG} && test -s {INNER_ENV} && test -x {INNER_SERVE}"),
        None,
        DISTRO_START,
    )?;
    checks.push(check(
        "config",
        config.ok(),
        if config.ok() {
            INNER_CONFIG.to_string()
        } else {
            format!("{INNER_CONFIG}, {INNER_ENV} or {INNER_SERVE} is missing")
        },
    ));

    // No double quotes in the script: the token is hex, so it needs none,
    // and one fewer quoting layer between here and sh is one fewer surprise.
    let health = sh_in_distro(
        &format!(
            ". {INNER_ENV} && curl -fsS -m 5 --oauth2-bearer $SHUTUP_AND_RUN_TOKEN http://{INNER_LISTEN}/health"
        ),
        None,
        DISTRO_START,
    )?;
    checks.push(soft(
        "service",
        health.ok(),
        if health.ok() {
            format!("answers on {INNER_LISTEN}: {}", health.one_line())
        } else {
            "not running (start it with `shutup-and-run wsl start`)".to_string()
        },
    ));

    let task = run("schtasks.exe", &["/Query", "/TN", TASK_NAME], None, SHORT);
    checks.push(match task {
        Ok(out) if out.ok() => soft("task", true, format!("{TASK_NAME} is registered")),
        Ok(_) => soft(
            "task",
            false,
            format!("{TASK_NAME} is not registered (--no-task?)"),
        ),
        Err(e) => soft("task", false, e.to_string()),
    });

    Ok(checks)
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
    let max_slots = parse_count(&flags, "--max-slots", 1, 1..=256)?;
    let poll_interval_secs = parse_count(&flags, "--poll-interval-secs", 15, 1..=3600)?;
    let worker_name = flags
        .get("--worker-name")
        .map(str::to_string)
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| "windows-worker".to_string());
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
    let inner = InnerConfig {
        poll_url: poll_url.clone(),
        smoke_image: smoke_image.to_string(),
        poll_interval_secs,
        max_slots,
        worker_name: worker_name.clone(),
    };
    let api_token = crate::launcher::random_id();
    let probe = render_config(&inner);
    let mut probe_env = HashMap::from([("SHUTUP_AND_RUN_TOKEN".to_string(), api_token.clone())]);
    if let Some(token) = &given_token {
        probe_env.insert("SHUTUP_AND_RUN_POLL_TOKEN".to_string(), token.clone());
    }
    crate::config::Settings::from_toml_and_env(&probe, &probe_env)
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
                return Err(err(format!("wsl --import: {}", out.one_line())));
            }
            println!("    imported into {}", vhd.display());
        }
    }

    // On a re-install the task's supervisor would restart the old service in
    // the middle of apt. End it first; step 7 starts it again.
    let _ = run("schtasks.exe", &["/End", "/TN", TASK_NAME], None, SHORT);

    println!("[3/8] /etc/wsl.conf (no Windows drives, no interop, docker at boot)");
    put_file("/etc/wsl.conf", "644", render_wsl_conf().into_bytes())?;
    // A changed wsl.conf is read at the next start, and Microsoft documents
    // an eight second wait after terminate before that start.
    let _ = run("wsl.exe", &["--terminate", DISTRO], None, SHORT);
    std::thread::sleep(Duration::from_secs(8));

    println!("[4/8] docker engine inside the distribution (apt, a few minutes)");
    let apt = sh_in_distro(
        "export DEBIAN_FRONTEND=noninteractive; apt-get update -q && apt-get install -y -q docker.io curl ca-certificates && mkdir -p /etc/shutup-and-run /var/lib/shutup-and-run && (service docker status >/dev/null 2>&1 || service docker start)",
        None,
        APT,
    )?;
    if !apt.ok() {
        return Err(err(format!("apt inside {DISTRO}: {}", apt.one_line())));
    }
    println!("    installed");

    println!("[5/8] the Linux binary");
    let musl = obtain_musl(&client, &flags).await?;
    let bytes = std::fs::read(&musl).map_err(|e| err(format!("{}: {e}", musl.display())))?;
    put_file(INNER_BIN, "755", bytes)?;
    let help = in_distro(&[INNER_BIN, "--help"], None, DISTRO_START)?;
    if !help.ok() {
        return Err(err(format!(
            "{INNER_BIN} does not run: {}",
            help.one_line()
        )));
    }
    println!("    {INNER_BIN}");

    println!("[6/8] config, env and the serve script");
    let mut enrolling = false;
    let poll_token = match (&poll_url, given_token) {
        (None, _) => None,
        (Some(_), Some(token)) => Some(token),
        (Some(_), None) => match existing_poll_token() {
            Some(token) => {
                println!("    kept this machine's poll token");
                Some(token)
            }
            None => {
                enrolling = true;
                println!("    made a poll token for this machine");
                Some(format!(
                    "{}{}",
                    crate::launcher::random_id(),
                    crate::launcher::random_id()
                ))
            }
        },
    };
    put_file(INNER_CONFIG, "644", render_config(&inner).into_bytes())?;
    put_file(
        INNER_ENV,
        "600",
        render_env(&api_token, poll_token.as_deref()).into_bytes(),
    )?;
    put_file(INNER_SERVE, "755", render_serve_script().into_bytes())?;
    println!("    {INNER_CONFIG}, {INNER_ENV} (600), {INNER_SERVE}");

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

    println!("[8/8] doctor");
    let checks = doctor(smoke_image)?;
    let (text, healthy) = report(&checks);
    print!("{text}");
    if !healthy {
        return Err(err(
            "the doctor found a problem. Fix it and run `shutup-and-run wsl doctor`.",
        ));
    }
    println!();
    match &poll_url {
        Some(url) => {
            println!(
                "done. The worker polls {} as {worker_name} with {max_slots} slot(s).",
                host_of(url)
            );
            if enrolling {
                println!(
                    "This machine has a new token. It gets no work until the poll URL approves it."
                );
            }
        }
        None => println!(
            "done. No poll URL: the worker is installed and idle. Run install again with --poll-url to give it work."
        ),
    }
    println!("logs: shutup-and-run wsl logs");
    Ok(ExitCode::SUCCESS)
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

/// The schtasks command line. `/RL LIMITED`: no elevation. No `/RU`: the task
/// runs as the user who installed, which is the user the distribution
/// belongs to.
pub(crate) fn schtasks_create_args(exe: &Path) -> Vec<String> {
    let action = format!("\"{}\" wsl serve --background", exe.display());
    vec![
        "/Create".into(),
        "/F".into(),
        "/SC".into(),
        "ONLOGON".into(),
        "/TN".into(),
        TASK_NAME.into(),
        "/TR".into(),
        action,
        "/RL".into(),
        "LIMITED".into(),
    ]
}

/// `schtasks /Create` leaves defaults meant for a short job: the task does
/// not start on battery, stops when the machine goes on battery, and is
/// ended after 72 hours. This worker runs for as long as the user is logged
/// on, so those are turned off. A second run of the task is ignored.
pub(crate) fn task_settings_script() -> String {
    format!(
        "$s = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries \
         -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew; \
         Set-ScheduledTask -TaskName '{TASK_NAME}' -Settings $s | Out-Null"
    )
}

fn register_task(exe: &Path) -> WslResult<()> {
    let args = schtasks_create_args(exe);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run("schtasks.exe", &refs, None, SHORT)?;
    if !out.ok() {
        return Err(err(format!("schtasks /Create: {}", out.one_line())));
    }
    // Soft: the task works without this, with the defaults described above.
    let script = task_settings_script();
    match run(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", &script],
        None,
        SHORT,
    ) {
        Ok(out) if out.ok() => {}
        Ok(out) => println!(
            "    could not change the task settings ({}). It will not start on battery and ends after 72 hours.",
            out.one_line()
        ),
        Err(e) => println!("    could not change the task settings ({e})."),
    }
    let out = run("schtasks.exe", &["/Run", "/TN", TASK_NAME], None, SHORT)?;
    if !out.ok() {
        return Err(err(format!("schtasks /Run: {}", out.one_line())));
    }
    Ok(())
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
    let _lock = crate::lock::acquire(&data.join("supervisor.lock")).map_err(err)?;
    let log = Logger::open(&log_path(), !background)?;
    // Only after the log is open: with no console, a print would have
    // nowhere to go, and this loop never returns to print anything.
    if background {
        detach_console();
    }
    log.line(&format!(
        "supervisor {} starting; distribution {DISTRO}",
        env!("CARGO_PKG_VERSION")
    ));
    let mut wait = RESTART_MIN;
    loop {
        let started = Instant::now();
        match run_service(&log) {
            Ok(code) => log.line(&format!("service exited with {code:?}")),
            Err(e) => log.line(&format!("service could not start: {e}")),
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
    let mut child = Command::new("wsl.exe")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| err(format!("wsl.exe: {e}")))?;
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
            Ok(_) => log.raw(line.trim_end_matches(['\r', '\n'])),
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

#[cfg(windows)]
fn detach_console() {
    extern "system" {
        fn FreeConsole() -> i32;
    }
    // SAFETY: FreeConsole takes no arguments and only detaches this process
    // from its console. Output after this goes to the log file alone.
    unsafe {
        FreeConsole();
    }
}

#[cfg(not(windows))]
fn detach_console() {}

// ---------------------------------------------------------------------------
// start, stop, logs, uninstall

fn start_cli() -> WslResult<ExitCode> {
    let out = run("schtasks.exe", &["/Run", "/TN", TASK_NAME], None, SHORT)?;
    if !out.ok() {
        return Err(err(format!("schtasks /Run: {}", out.one_line())));
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
        match sh_in_distro(GRACEFUL_STOP, None, DISTRO_START) {
            Ok(out) if out.ok() => {}
            Ok(out) => println!("graceful stop: {}", out.one_line()),
            Err(e) => println!("graceful stop: {e}"),
        }
    }
    let out = run("wsl.exe", &["--terminate", DISTRO], None, SHORT)?;
    if !out.ok() {
        return Err(err(format!("wsl --terminate: {}", out.one_line())));
    }
    println!("stopped {TASK_NAME} and terminated {DISTRO}; its containers were stopped");
    Ok(ExitCode::SUCCESS)
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
    if our_distro()?.is_some() {
        let _ = run("wsl.exe", &["--terminate", DISTRO], None, SHORT);
        let out = run("wsl.exe", &["--unregister", DISTRO], None, APT)?;
        if !out.ok() {
            return Err(err(format!("wsl --unregister: {}", out.one_line())));
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
        println!("kept {} (pass --purge to delete it)", data_dir().display());
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
    fn rendered_config_is_one_the_service_accepts() {
        let text = render_config(&InnerConfig {
            poll_url: Some("https://orchestrator.example/poll?x=\"1\"".into()),
            smoke_image: "hello-world".into(),
            poll_interval_secs: 20,
            max_slots: 2,
            worker_name: "DESK-07".into(),
        });
        let env = HashMap::from([
            ("SHUTUP_AND_RUN_TOKEN".to_string(), "a".repeat(32)),
            ("SHUTUP_AND_RUN_POLL_TOKEN".to_string(), "b".repeat(32)),
        ]);
        let settings = crate::config::Settings::from_toml_and_env(&text, &env).unwrap();
        assert!(settings.http_plain);
        assert_eq!(settings.listen.to_string(), INNER_LISTEN);
        assert!(settings.listen.ip().is_loopback());
        assert_eq!(settings.max_slots, 2);
        assert!(settings.allowlist.is_empty());
        assert!(settings.poll_any_image);
        assert_eq!(
            settings.poll_url.as_deref(),
            Some("https://orchestrator.example/poll?x=\"1\"")
        );
        assert_eq!(settings.poll_interval, Duration::from_secs(20));
        assert_eq!(settings.worker_name, "DESK-07");
        assert_eq!(settings.worker_os, "windows");
        assert!(settings.poll_token.is_some());

        // Without the poll token in the env the service refuses, and so does install.
        let no_poll_token = HashMap::from([("SHUTUP_AND_RUN_TOKEN".to_string(), "a".repeat(32))]);
        let settings = crate::config::Settings::from_toml_and_env(&text, &no_poll_token).unwrap();
        assert!(settings.poll_token.is_none());

        assert_eq!(toml_string("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
    }

    #[test]
    fn without_a_poll_url_the_config_is_idle_and_still_accepted() {
        let text = render_config(&InnerConfig {
            poll_url: None,
            smoke_image: "hello-world".into(),
            poll_interval_secs: 15,
            max_slots: 1,
            worker_name: "DESK-07".into(),
        });
        let env = HashMap::from([("SHUTUP_AND_RUN_TOKEN".to_string(), "a".repeat(32))]);
        let settings = crate::config::Settings::from_toml_and_env(&text, &env).unwrap();
        assert!(settings.poll_url.is_none());
        assert!(!settings.poll_any_image);
        assert_eq!(settings.allowlist, vec!["hello-world".to_string()]);
        assert!(settings.listen.ip().is_loopback());
        assert_eq!(render_env("api", None), "SHUTUP_AND_RUN_TOKEN=api\n");
    }

    #[test]
    fn env_wsl_conf_and_serve_script_have_the_expected_shape() {
        let env = render_env("api-token-value-ok", Some("poll-token-value-ok"));
        assert_eq!(
            env,
            "SHUTUP_AND_RUN_TOKEN=api-token-value-ok\nSHUTUP_AND_RUN_POLL_TOKEN=poll-token-value-ok\n"
        );
        let conf = render_wsl_conf();
        assert!(conf.contains("[automount]\nenabled = false"));
        assert!(conf.contains("[interop]\nenabled = false"));
        assert!(conf.contains("command = \"service docker start\""));
        let script = render_serve_script();
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(script.contains(". /etc/shutup-and-run/env"));
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
        let args = schtasks_create_args(Path::new(
            r"C:\ProgramData\shutup-and-run\shutup-and-run.exe",
        ));
        assert_eq!(args[0], "/Create");
        assert!(args.contains(&"ONLOGON".to_string()));
        assert!(args.contains(&TASK_NAME.to_string()));
        assert!(args.contains(
            &r#""C:\ProgramData\shutup-and-run\shutup-and-run.exe" wsl serve --background"#
                .to_string()
        ));

        assert!(is_token(&"a".repeat(64)));
        assert!(!is_token("short"));
        assert!(!is_token("has space in it, and is long enough"));

        let settings = task_settings_script();
        assert!(settings.contains("-TaskName 'shutup-and-run'"));
        assert!(settings.contains("-AllowStartIfOnBatteries"));
        assert!(settings.contains("[TimeSpan]::Zero"));
        assert!(settings.contains("IgnoreNew"));
        assert!(!settings.contains('"'));

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
            let checks = doctor(DEFAULT_SMOKE_IMAGE).unwrap();
            assert_eq!(checks.len(), 1);
            assert!(!checks[0].ok);
            assert!(checks[0].detail.contains("wsl.exe"), "{}", checks[0].detail);
            let (_, healthy) = report(&checks);
            assert!(!healthy);
        }
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
