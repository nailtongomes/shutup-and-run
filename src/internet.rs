//! Lend only the internet connection. A PC that cannot run containers (no
//! WSL, too little memory) can still let the operator's containers reach
//! sites from its residential address.
//!
//! The PC dials out to a relay the operator runs (`shutup-and-run relay`,
//! `src/relay.rs`) and keeps a few idle TCP connections open to it. When a
//! container asks the relay's HTTP proxy for `host:port`, the relay sends that
//! line down one idle connection; this side dials the site and then copies
//! bytes both ways. No port is opened on the PC or its router, and nothing
//! else runs: no Docker, no WSL.
//!
//! The line protocol, one connection per proxied stream, no multiplexing:
//!
//! ```text
//! PC    -> relay  SAR1 <id> <secret>
//! relay -> PC     OK <proxy port>      | NO <reason>
//! PC    -> relay  P                    (every 20 s while idle; the relay echoes it)
//! relay -> PC     C <host>:<port>
//! PC    -> relay  OK                   | NO <reason>
//! ...the raw bytes of the stream, both ways, until either side closes
//! ```
//!
//! A mode of its own, apart from the worker: its own file (`internet.json`),
//! its own process (`internet serve`, a logon task on Windows, a systemd unit
//! on Linux) and its own read-only page on 127.0.0.1:8788. It must run on a
//! PC where there is no worker. Every new stream waits, like a new job, while
//! the worker is paused, outside `lend_hours`, on a low battery, while the
//! person presents and on a metered network (`src/lending.rs`). A stream
//! already open is never cut for any of them.
//!
//! Alpha: the link to the relay is plain TCP, and the relay accepts any PC.
//! See docs/INTERNET.md for what that costs and what comes next.

use crate::cli::{check, soft, Check};
use crate::lending::Hold;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::watch;

/// The first word a PC sends. A new protocol gets a new word.
pub(crate) const HELLO: &str = "SAR1";
/// The page of this mode. 8787 is the worker's.
pub const PAGE_LISTEN: &str = "127.0.0.1:8788";
/// Idle connections kept open to the relay, each ready for one stream.
const IDLE_CONNECTIONS: usize = 4;
/// Streams open at once. Above it no idle connection is replaced.
const MAX_STREAMS: usize = 256;
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const REPLY_TIMEOUT: Duration = Duration::from_secs(15);
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// How often the file and the waits are read again, and the status written.
const TICK: Duration = Duration::from_secs(5);
/// The status file is written at least this often, so a reader can tell a
/// process that stopped from one with nothing new.
const STATUS_EVERY: Duration = Duration::from_secs(30);
/// Older than this, the status file is from a process that is gone.
const STALE_SECS: i64 = 90;
/// The longest protocol line either side reads.
pub(crate) const LINE_MAX: usize = 512;

pub const TASK_NAME: &str = "shutup-and-run-internet";
const UNIT_PATH: &str = "/etc/systemd/system/shutup-and-run-internet.service";

const FILE: &str = "internet.json";
const STATUS_FILE: &str = "internet-status.json";
const LOG_FILE: &str = "internet.log";
const LOCK_FILE: &str = "internet.lock";

pub const NOT_RUNNING: &str = "INTERNET_NOT_RUNNING";
pub const RELAY_UNREACHABLE: &str = "INTERNET_RELAY_UNREACHABLE";
pub const REFUSED: &str = "INTERNET_REFUSED";
pub const WAITING: &str = "INTERNET_WAITING";

/// Where this mode keeps its files: beside the supervisor's on Windows,
/// beside the job history on Linux.
pub fn dir() -> PathBuf {
    if cfg!(windows) {
        crate::wsl::data_dir()
    } else {
        PathBuf::from("/var/lib/shutup-and-run")
    }
}

// ---------------------------------------------------------------------------
// The file

/// `internet.json`, mode 600. Written only by `internet on` and `internet off`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    /// The relay's address for PCs, `host:port`.
    pub relay: String,
    /// The proxy user name: which PC a container goes out through.
    pub id: String,
    /// The proxy password. Generated here; the operator gets it from the
    /// person at the PC (`internet credentials`, or the page).
    pub secret: String,
    /// When the PC lends its connection, in the syntax of `lend_hours`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lend_hours: Option<String>,
    /// The person accepted that streams spend a metered network's data.
    #[serde(default)]
    pub metered: bool,
    /// When it was turned on at this PC: the person's consent.
    pub consent_at: String,
}

pub fn read_settings(dir: &Path) -> Option<Settings> {
    let text = std::fs::read_to_string(dir.join(FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_settings(dir: &Path, settings: &Settings) -> std::io::Result<()> {
    let path = dir.join(FILE);
    let text = serde_json::to_string_pretty(settings).map_err(std::io::Error::other)?;
    crate::status::write_atomic(&path, text.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// A name or a secret on a protocol line: no space, nothing to escape.
pub(crate) fn valid_word(text: &str, len: std::ops::RangeInclusive<usize>) -> bool {
    len.contains(&text.len())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

/// `host:port`, with a port that is a number and no character that could
/// end a protocol line.
pub(crate) fn valid_target(target: &str) -> bool {
    let Some((host, port)) = target.rsplit_once(':') else {
        return false;
    };
    !host.is_empty()
        && target.len() <= 300
        && port.parse::<u16>().is_ok_and(|p| p > 0)
        && target.chars().all(|c| c.is_ascii_graphic())
}

/// The PC's name, cleaned, and four random hex digits: two PCs with the same
/// name do not take each other's place on the relay.
fn default_id() -> String {
    let raw = ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|key| std::env::var(key).ok())
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .unwrap_or_default();
    let name: String = raw
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(40)
        .collect();
    let name = name.trim_matches('-');
    let suffix = &crate::launcher::random_id()[..4];
    if name.is_empty() {
        format!("pc-{suffix}")
    } else {
        format!("{name}-{suffix}")
    }
}

// ---------------------------------------------------------------------------
// The status the process writes

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub version: String,
    pub pid: u32,
    /// `connected`, `connecting`, `waiting` or `refused`.
    pub state: String,
    pub relay: String,
    pub id: String,
    /// The relay's proxy port, as the relay said in its answer.
    pub proxy_port: Option<u16>,
    /// Idle connections open to the relay.
    pub idle: usize,
    /// Streams open now.
    pub streams: usize,
    /// Bytes from the sites to the PC, since `counting_since`, across restarts.
    pub bytes_received: u64,
    /// Bytes from the PC to the sites.
    pub bytes_sent: u64,
    pub counting_since: String,
    pub connected_since: Option<String>,
    pub last_error: Option<String>,
    pub hold: Option<Hold>,
    pub updated_at: String,
}

pub fn read_status(dir: &Path) -> Option<Status> {
    let text = std::fs::read_to_string(dir.join(STATUS_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// What the streams and the connections share with the status writer.
#[derive(Default)]
pub(crate) struct Shared {
    idle: AtomicUsize,
    streams: AtomicUsize,
    received: AtomicU64,
    sent: AtomicU64,
    connected_since: Mutex<Option<i64>>,
    proxy_port: Mutex<Option<u16>>,
    last_error: Mutex<Option<String>>,
    refused: Mutex<Option<String>>,
}

impl Shared {
    fn error(&self, text: String) {
        *self.last_error.lock().unwrap() = Some(text);
    }

    pub(crate) fn bytes(&self) -> (u64, u64) {
        (
            self.received.load(Ordering::Relaxed),
            self.sent.load(Ordering::Relaxed),
        )
    }

    pub(crate) fn idle(&self) -> usize {
        self.idle.load(Ordering::Relaxed)
    }
}

/// Counts down when dropped.
struct Count<'a>(&'a AtomicUsize);

impl<'a> Count<'a> {
    fn up(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Count(counter)
    }
}

impl Drop for Count<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn state_of(shared: &Shared, hold: &Option<Hold>) -> &'static str {
    if hold.is_some() {
        "waiting"
    } else if shared.refused.lock().unwrap().is_some() {
        "refused"
    } else if shared.idle() > 0 || shared.streams.load(Ordering::Relaxed) > 0 {
        "connected"
    } else {
        "connecting"
    }
}

// ---------------------------------------------------------------------------
// The PC's side

/// What a test changes. The defaults are what runs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Opts {
    /// A stream to this PC's own network or loopback. Only tests: on a PC it
    /// would hand the operator the router's page and the worker's.
    pub private_ok: bool,
    pub ping: Duration,
    /// No word from the relay for this long: the link is dead (the PC slept,
    /// the network changed), and a new one is dialed.
    pub silent: Duration,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            private_ok: false,
            ping: Duration::from_secs(20),
            silent: Duration::from_secs(60),
        }
    }
}

/// Starts the idle connections. They follow `settings` and `hold` as those
/// change, and end with the runtime.
pub(crate) fn spawn_links(
    settings: watch::Receiver<Settings>,
    hold: watch::Receiver<Option<Hold>>,
    shared: Arc<Shared>,
    opts: Opts,
) {
    for _ in 0..IDLE_CONNECTIONS {
        tokio::spawn(slot(settings.clone(), hold.clone(), shared.clone(), opts));
    }
}

enum Ended {
    /// It carries a stream now; a new idle connection takes its place.
    Claimed,
    /// It was closed on purpose: a wait began or the file changed.
    Closed,
}

async fn slot(
    settings: watch::Receiver<Settings>,
    mut hold: watch::Receiver<Option<Hold>>,
    shared: Arc<Shared>,
    opts: Opts,
) {
    let mut backoff = BACKOFF_MIN;
    loop {
        while hold.borrow().is_some() {
            if hold.changed().await.is_err() {
                return;
            }
        }
        if shared.streams.load(Ordering::Relaxed) >= MAX_STREAMS {
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }
        let current = settings.borrow().clone();
        let began = tokio::time::Instant::now();
        match link(&current, &settings, &hold, &shared, opts).await {
            Ok(Ended::Claimed) => backoff = BACKOFF_MIN,
            Ok(Ended::Closed) => {
                backoff = BACKOFF_MIN;
                tokio::time::sleep(BACKOFF_MIN).await;
            }
            Err(e) => {
                shared.error(e);
                // A link that worked for a while starts over from the shortest wait.
                if began.elapsed() > BACKOFF_MAX {
                    backoff = BACKOFF_MIN;
                }
                // Apart in time, so four connections do not knock together.
                let roll = crate::launcher::random_id();
                let jitter = u64::from_str_radix(&roll[..3], 16).unwrap_or(0) % 1000;
                tokio::time::sleep(backoff + Duration::from_millis(jitter)).await;
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

/// One idle connection, from the dial to its claim or its end.
async fn link(
    current: &Settings,
    settings: &watch::Receiver<Settings>,
    hold: &watch::Receiver<Option<Hold>>,
    shared: &Arc<Shared>,
    opts: Opts,
) -> Result<Ended, String> {
    let stream = tokio::time::timeout(DIAL_TIMEOUT, TcpStream::connect(&current.relay))
        .await
        .map_err(|_| format!("the relay {} did not answer in 10s", current.relay))?
        .map_err(|e| format!("the relay {}: {e}", current.relay))?;
    let _ = stream.set_nodelay(true);
    let mut s = tokio::io::BufReader::new(stream);
    s.write_all(format!("{HELLO} {} {}\n", current.id, current.secret).as_bytes())
        .await
        .map_err(|e| format!("the relay {}: {e}", current.relay))?;
    let reply = tokio::time::timeout(REPLY_TIMEOUT, read_line(&mut s))
        .await
        .map_err(|_| format!("the relay {} did not greet back", current.relay))??;
    if let Some(reason) = reply.strip_prefix("NO") {
        let reason = reason.trim().to_string();
        *shared.refused.lock().unwrap() = Some(reason.clone());
        return Err(format!("the relay refused this PC: {reason}"));
    }
    let Some(port) = reply.strip_prefix("OK") else {
        return Err(format!("the relay answered {reply:?}"));
    };
    *shared.refused.lock().unwrap() = None;
    *shared.last_error.lock().unwrap() = None;
    *shared.proxy_port.lock().unwrap() = port.trim().parse().ok();
    shared
        .connected_since
        .lock()
        .unwrap()
        .get_or_insert_with(crate::status::now_unix);
    let idle = Count::up(&shared.idle);

    let mut buf = Vec::new();
    let mut heard = tokio::time::Instant::now();
    loop {
        match tokio::time::timeout(opts.ping, read_until_capped(&mut s, &mut buf)).await {
            Err(_) => {
                if heard.elapsed() >= opts.silent {
                    return Err("the relay stopped answering".to_string());
                }
                if hold.borrow().is_some() || *settings.borrow() != *current {
                    return Ok(Ended::Closed);
                }
                s.write_all(b"P\n")
                    .await
                    .map_err(|e| format!("the relay: {e}"))?;
            }
            Ok(Err(e)) => return Err(e),
            Ok(Ok(false)) => return Err("the relay closed the connection".to_string()),
            Ok(Ok(true)) => {
                heard = tokio::time::Instant::now();
                let line = take_line(&mut buf);
                if line == "P" {
                    continue;
                }
                let Some(target) = line.strip_prefix("C ").map(str::to_string) else {
                    return Err(format!("the relay sent {line:?}"));
                };
                drop(idle);
                let shared = shared.clone();
                let waiting = hold.borrow().clone();
                tokio::spawn(async move { carry(s, target, waiting, shared, opts).await });
                return Ok(Ended::Claimed);
            }
        }
    }
}

/// One stream: dial the site, say so, copy both ways.
async fn carry(
    mut relay: tokio::io::BufReader<TcpStream>,
    target: String,
    waiting: Option<Hold>,
    shared: Arc<Shared>,
    opts: Opts,
) {
    let site = match waiting {
        Some(hold) => Err(format!("this PC waits: {}", hold.detail)),
        None => dial(&target, opts.private_ok).await,
    };
    let site = match site {
        Ok(site) => site,
        Err(reason) => {
            let reason: String = reason.chars().filter(|c| !c.is_control()).collect();
            let _ = relay.write_all(format!("NO {reason}\n").as_bytes()).await;
            return;
        }
    };
    if relay.write_all(b"OK\n").await.is_err() {
        return;
    }
    let _open = Count::up(&shared.streams);
    let mut site = Counted {
        inner: site,
        read: &shared.received,
        written: &shared.sent,
    };
    let _ = tokio::io::copy_bidirectional(&mut relay, &mut site).await;
}

/// The site's first address that is on the internet, connected. One on this
/// PC's own network is refused, and the address checked is the one dialed,
/// so a name that resolves twice cannot swap it.
async fn dial(target: &str, private_ok: bool) -> Result<TcpStream, String> {
    if !valid_target(target) {
        return Err("not host:port".to_string());
    }
    let addrs: Vec<SocketAddr> =
        tokio::time::timeout(DIAL_TIMEOUT, tokio::net::lookup_host(target))
            .await
            .map_err(|_| format!("{target}: no DNS answer in 10s"))?
            .map_err(|e| format!("{target}: {e}"))?
            .collect();
    let allowed: Vec<SocketAddr> = addrs
        .iter()
        .copied()
        .filter(|a| private_ok || is_public(a.ip()))
        .collect();
    if allowed.is_empty() {
        return Err(if addrs.is_empty() {
            format!("{target}: no address")
        } else {
            format!("{target} is on this PC's own network, which is not lent")
        });
    }
    let mut last = String::new();
    for addr in allowed {
        match tokio::time::timeout(DIAL_TIMEOUT, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Ok(Err(e)) => last = format!("{target}: {e}"),
            Err(_) => last = format!("{target}: no answer in 10s"),
        }
    }
    Err(last)
}

/// An address on the internet: not loopback, not a private or link-local
/// network, not the carrier's NAT range, not multicast or reserved.
pub(crate) fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || a == 0
                || a >= 240
                || (a == 100 && (64..128).contains(&b))
                || (a == 198 && (18..20).contains(&b))
                || (a == 192 && b == 0 && v4.octets()[2] == 0))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80)
        }
    }
}

/// One side of a stream, counting what is read from it and written to it.
pub(crate) struct Counted<'a, S> {
    pub inner: S,
    pub read: &'a AtomicU64,
    pub written: &'a AtomicU64,
}

impl<S: AsyncRead + Unpin> AsyncRead for Counted<'_, S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let poll = Pin::new(&mut self.inner).poll_read(cx, buf);
        let read = buf.filled().len() - before;
        self.read.fetch_add(read as u64, Ordering::Relaxed);
        poll
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Counted<'_, S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let poll = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &poll {
            self.written.fetch_add(*n as u64, Ordering::Relaxed);
        }
        poll
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// `512 B`, `12.3 KiB`, `4.0 MiB`, `1.2 GiB`.
pub fn bytes_text(n: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Reads up to a newline into `buf`, keeping what came before a cancel:
/// `read_until` appends as it goes. False at the end of the stream.
pub(crate) async fn read_until_capped<R>(reader: &mut R, buf: &mut Vec<u8>) -> Result<bool, String>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    loop {
        if buf.contains(&b'\n') {
            return Ok(true);
        }
        let room = LINE_MAX.saturating_sub(buf.len());
        if room == 0 {
            return Err("a line too long".to_string());
        }
        let n = (&mut *reader)
            .take(room as u64)
            .read_until(b'\n', buf)
            .await
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(false);
        }
    }
}

/// The first line of `buf`, without its end, removed from `buf`.
pub(crate) fn take_line(buf: &mut Vec<u8>) -> String {
    let end = buf.iter().position(|&b| b == b'\n').unwrap_or(buf.len());
    let line: Vec<u8> = buf.drain(..(end + 1).min(buf.len())).collect();
    String::from_utf8_lossy(&line)
        .trim_end_matches(['\r', '\n'])
        .to_string()
}

pub(crate) async fn read_line<R>(reader: &mut R) -> Result<String, String>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut buf = Vec::new();
    if !read_until_capped(reader, &mut buf).await? {
        return Err("the connection closed".to_string());
    }
    Ok(take_line(&mut buf))
}

// ---------------------------------------------------------------------------
// The waits

/// The first reason a new stream waits, or None.
fn hold_now(settings: &Settings, worker_status: Option<&Path>, now: i64) -> Option<Hold> {
    if worker_status.is_some_and(|path| worker_paused(path, now)) {
        return Some(Hold {
            code: "WORKER_PAUSED".to_string(),
            detail: "the worker is paused".to_string(),
            resumes_at: None,
        });
    }
    let hours = settings
        .lend_hours
        .as_deref()
        .and_then(|text| crate::lending::LendHours::parse(text).ok());
    // Windows says what only it knows; a Linux machine reads its own clock.
    let feed = cfg!(windows).then(|| crate::wsl::host_feed(None));
    crate::lending::hold(hours.as_ref(), feed, now, settings.metered)
}

/// The worker's status file says it is paused, and is fresh.
fn worker_paused(path: &Path, now: i64) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .is_some_and(|doc| {
            doc["state"] == "draining"
                && !crate::status::is_stale(doc["updated_at"].as_str().unwrap_or(""), now)
        })
}

fn worker_status_path() -> Option<PathBuf> {
    if cfg!(windows) {
        Some(crate::wsl::data_dir().join("status.json"))
    } else {
        crate::config::Settings::history_path()
            .ok()
            .map(|history| crate::status::status_path(&history))
    }
}

// ---------------------------------------------------------------------------
// serve: the process

pub async fn serve(dir: &Path, background: bool) -> Result<ExitCode, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let _lock = crate::lock::acquire(&dir.join(LOCK_FILE))?;
    let log = crate::wsl::Logger::open(&dir.join(LOG_FILE), !background).map_err(|e| e.0)?;
    let Some(first) = read_settings(dir).filter(|s| s.enabled) else {
        log.raw("internet: off; nothing to do");
        return Ok(ExitCode::SUCCESS);
    };
    log.raw(&format!(
        "internet: {} starting; relay {}, id {}",
        env!("CARGO_PKG_VERSION"),
        first.relay,
        first.id
    ));
    let worker_status = worker_status_path();
    let now = crate::status::now_unix();
    let (settings_tx, settings_rx) = watch::channel(first.clone());
    let (hold_tx, hold_rx) = watch::channel(hold_now(&first, worker_status.as_deref(), now));
    let shared = Arc::new(Shared::default());
    // The totals go on from where the last run left them.
    let before = read_status(dir);
    let counting_since = before
        .as_ref()
        .filter(|s| s.id == first.id)
        .map(|s| s.counting_since.clone())
        .unwrap_or_else(|| crate::status::unix_to_iso(now));
    let (base_received, base_sent) = before
        .filter(|s| s.id == first.id)
        .map_or((0, 0), |s| (s.bytes_received, s.bytes_sent));

    let page = Page {
        dir: dir.to_path_buf(),
    };
    match tokio::net::TcpListener::bind(PAGE_LISTEN).await {
        Ok(listener) => {
            tokio::spawn(async move {
                let app = page.router();
                let _ = axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await;
            });
        }
        Err(e) => log.raw(&format!("internet: no page on {PAGE_LISTEN}: {e}")),
    }

    spawn_links(settings_rx, hold_rx, shared.clone(), Opts::default());

    let mut written: Option<(Status, std::time::Instant)> = None;
    let mut last_state = String::new();
    loop {
        tokio::time::sleep(TICK).await;
        let now = crate::status::now_unix();
        let Some(current) = read_settings(dir).filter(|s| s.enabled) else {
            log.raw("internet: turned off; exiting");
            let _ = std::fs::remove_file(dir.join(STATUS_FILE));
            return Ok(ExitCode::SUCCESS);
        };
        if *settings_tx.borrow() != current {
            log.raw(&format!(
                "internet: settings changed; relay {}, id {}",
                current.relay, current.id
            ));
            if settings_tx.borrow().id != current.id {
                *shared.connected_since.lock().unwrap() = None;
            }
            settings_tx.send_replace(current.clone());
        }
        let hold = hold_now(&current, worker_status.as_deref(), now);
        if *hold_tx.borrow() != hold {
            hold_tx.send_replace(hold.clone());
        }
        if shared.idle() == 0 && shared.streams.load(Ordering::Relaxed) == 0 {
            *shared.connected_since.lock().unwrap() = None;
        }
        let (received, sent) = shared.bytes();
        let status = Status {
            version: env!("CARGO_PKG_VERSION").to_string(),
            pid: std::process::id(),
            state: state_of(&shared, &hold).to_string(),
            relay: current.relay.clone(),
            id: current.id.clone(),
            proxy_port: *shared.proxy_port.lock().unwrap(),
            idle: shared.idle(),
            streams: shared.streams.load(Ordering::Relaxed),
            bytes_received: base_received + received,
            bytes_sent: base_sent + sent,
            counting_since: counting_since.clone(),
            connected_since: shared
                .connected_since
                .lock()
                .unwrap()
                .map(crate::status::unix_to_iso),
            last_error: shared.last_error.lock().unwrap().clone(),
            hold,
            updated_at: String::new(),
        };
        if status.state != last_state {
            let why = status
                .hold
                .as_ref()
                .map(|h| h.detail.clone())
                .or_else(|| status.last_error.clone())
                .unwrap_or_default();
            log.raw(&format!("internet: {} {why}", status.state));
            last_state = status.state.clone();
        }
        let due = written
            .as_ref()
            .is_none_or(|(old, at)| *old != status || at.elapsed() >= STATUS_EVERY);
        if due {
            let mut out = status.clone();
            out.updated_at = crate::status::unix_to_iso(now);
            if let Ok(bytes) = serde_json::to_vec_pretty(&out) {
                if let Err(e) = crate::status::write_atomic(&dir.join(STATUS_FILE), &bytes) {
                    log.raw(&format!("internet: status file: {e}"));
                }
            }
            written = Some((status, std::time::Instant::now()));
        }
    }
}

// ---------------------------------------------------------------------------
// Checks: `internet status`, and a line of each doctor

/// The lines of this mode, or none when it was never turned on here.
pub fn checks_if_configured() -> Vec<Check> {
    let dir = dir();
    match read_settings(&dir) {
        Some(settings) => checks(
            &settings,
            read_status(&dir).as_ref(),
            crate::status::now_unix(),
        ),
        None => Vec::new(),
    }
}

pub fn checks(settings: &Settings, status: Option<&Status>, now: i64) -> Vec<Check> {
    const NAME: &str = "internet";
    if !settings.enabled {
        return vec![check(NAME, true, "off")];
    }
    let fresh = status.filter(|s| {
        crate::status::iso_to_unix(&s.updated_at).is_some_and(|at| now - at <= STALE_SECS)
            && s.id == settings.id
    });
    let Some(status) = fresh else {
        return vec![check(
            NAME,
            false,
            format!(
                "on, but its process is not running (relay {})",
                settings.relay
            ),
        )
        .coded(NOT_RUNNING)];
    };
    let line = match status.state.as_str() {
        "connected" => check(
            NAME,
            true,
            format!(
                "connected to {} as {}: {} idle, {} open; {} received, {} sent",
                status.relay,
                status.id,
                status.idle,
                status.streams,
                bytes_text(status.bytes_received),
                bytes_text(status.bytes_sent)
            ),
        ),
        "waiting" => {
            let detail = status
                .hold
                .as_ref()
                .map_or(String::new(), |h| format!("{}: {}", h.code, h.detail));
            soft(NAME, false, format!("waits: {detail}")).coded(WAITING)
        }
        "refused" => check(
            NAME,
            false,
            status
                .last_error
                .clone()
                .unwrap_or_else(|| "the relay refused this PC".to_string()),
        )
        .coded(REFUSED),
        _ => check(
            NAME,
            false,
            status
                .last_error
                .clone()
                .unwrap_or_else(|| format!("connecting to {}", status.relay)),
        )
        .coded(RELAY_UNREACHABLE)
        .with(settings.relay.clone()),
    };
    vec![line]
}

/// `http://ID:SECRET@HOST:PORT`, with the relay's host and the proxy port
/// it gave. `***` for the secret unless `reveal`.
pub fn proxy_url(settings: &Settings, proxy_port: Option<u16>, reveal: bool) -> String {
    let host = settings
        .relay
        .rsplit_once(':')
        .map_or(settings.relay.as_str(), |(host, _)| host);
    let port = proxy_port.map_or("PROXY_PORT".to_string(), |p| p.to_string());
    let secret = if reveal {
        settings.secret.as_str()
    } else {
        crate::config::MASK
    };
    format!("http://{}:{secret}@{host}:{port}", settings.id)
}

// ---------------------------------------------------------------------------
// CLI

pub fn usage() -> &'static str {
    "\
shutup-and-run internet on --relay HOST:PORT [--id NAME] [--hours \"mon-fri 19-07\"] [--metered]
    lend this PC's internet connection to the operator's containers, through
    the relay the operator runs. No WSL, no Docker. It starts now and at
    every logon (Windows: task shutup-and-run-internet; Linux: a systemd
    unit). Turning it on here is the PC owner's consent. New connections
    wait while the worker is paused, outside --hours, on a battery under
    60%, while the person presents, and on a metered network unless
    --metered. One already open is never cut.
shutup-and-run internet off [--forget]
    stop, and remove the task or unit. --forget also deletes the id and the
    password, so the next `on` makes new ones.
shutup-and-run internet status [--json]
    on or off, connected or why not, bytes carried. Exit 1 when it is on and
    not working.
shutup-and-run internet credentials [--json]
    the proxy address with its password, for the operator.
shutup-and-run internet serve [--background]
    the process itself, which the task or the unit runs. Its page is
    http://127.0.0.1:8788/.
docs/INTERNET.md has the relay, the risks and a test."
}

pub async fn cli(args: &[String]) -> ExitCode {
    let (sub, rest) = match args.split_first() {
        Some((sub, rest)) => (sub.as_str(), rest),
        None => {
            eprintln!("{}", usage());
            return ExitCode::from(2);
        }
    };
    let dir = dir();
    let result = match sub {
        "on" => on_cli(&dir, rest),
        "off" => off_cli(&dir, rest),
        "status" => status_cli(&dir, rest),
        "credentials" => credentials_cli(&dir, rest),
        "serve" => match crate::wsl::parse_flags(rest, &[], &["--background"]) {
            Ok(flags) => serve(&dir, flags.has("--background")).await,
            Err(e) => Err(e.0),
        },
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
            eprintln!("shutup-and-run internet: {e}");
            eprintln!("hint: {}", crate::hints::hint("FAILED"));
            ExitCode::from(1)
        }
    }
}

fn print(json_out: bool, value: Value, text: String) {
    if json_out {
        println!("{value}");
    } else {
        println!("{text}");
    }
}

fn on_cli(dir: &Path, args: &[String]) -> Result<ExitCode, String> {
    let flags = crate::wsl::parse_flags(
        args,
        &["--relay", "--id", "--hours"],
        &["--metered", "--json"],
    )
    .map_err(|e| e.0)?;
    let Some(relay) = flags.get("--relay") else {
        eprintln!("{}", usage());
        return Ok(ExitCode::from(2));
    };
    if !valid_target(relay) {
        return Err(format!("--relay {relay}: give HOST:PORT"));
    }
    let old = read_settings(dir);
    let id = match flags.get("--id") {
        Some(id) if valid_word(id, 1..=64) => id.to_string(),
        Some(id) => {
            return Err(format!(
                "--id {id}: 1 to 64 letters, digits, '-', '_' or '.'"
            ))
        }
        None => old.as_ref().map_or_else(default_id, |o| o.id.clone()),
    };
    let hours = match flags.get("--hours") {
        Some(text) => {
            crate::lending::LendHours::parse(text).map_err(|e| format!("--hours: {e}"))?;
            Some(text.to_string())
        }
        None => None,
    };
    let settings = Settings {
        enabled: true,
        relay: relay.to_string(),
        id,
        secret: old
            .as_ref()
            .map_or_else(crate::launcher::random_id, |o| o.secret.clone()),
        lend_hours: hours,
        metered: flags.has("--metered"),
        consent_at: crate::status::unix_to_iso(crate::status::now_unix()),
    };
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    write_settings(dir, &settings).map_err(|e| format!("{}: {e}", dir.join(FILE).display()))?;
    let started = start_process()?;
    let proxy = proxy_url(&settings, None, false);
    let text = format!(
        "internet sharing is on: relay {}, id {}\n{started}\n\
         give the operator the proxy address with its password: `shutup-and-run internet credentials`\n\
         status: `shutup-and-run internet status`, or http://127.0.0.1:8788/",
        settings.relay, settings.id
    );
    print(
        flags.has("--json"),
        json!({"ok": true, "enabled": true, "relay": settings.relay, "id": settings.id,
               "proxy": proxy, "started": started}),
        text,
    );
    Ok(ExitCode::SUCCESS)
}

fn off_cli(dir: &Path, args: &[String]) -> Result<ExitCode, String> {
    let flags = crate::wsl::parse_flags(args, &[], &["--forget", "--json"]).map_err(|e| e.0)?;
    let mut text = Vec::new();
    if let Some(mut settings) = read_settings(dir) {
        settings.enabled = false;
        write_settings(dir, &settings).map_err(|e| e.to_string())?;
    }
    text.push(stop_process());
    if flags.has("--forget") {
        for file in [FILE, STATUS_FILE, LOG_FILE, LOCK_FILE] {
            let _ = std::fs::remove_file(dir.join(file));
        }
        let _ = std::fs::remove_file(dir.join(LOG_FILE).with_extension("log.1"));
        text.push("deleted the id, the password, the status and the log".to_string());
    }
    text.insert(0, "internet sharing is off".to_string());
    print(
        flags.has("--json"),
        json!({"ok": true, "enabled": false, "forgot": flags.has("--forget")}),
        text.join("\n"),
    );
    Ok(ExitCode::SUCCESS)
}

fn status_cli(dir: &Path, args: &[String]) -> Result<ExitCode, String> {
    let flags = crate::wsl::parse_flags(args, &[], &["--json"]).map_err(|e| e.0)?;
    let (value, text, code) = status_out(
        read_settings(dir).as_ref(),
        read_status(dir).as_ref(),
        crate::status::now_unix(),
    );
    print(flags.has("--json"), value, text);
    Ok(ExitCode::from(code))
}

pub fn status_out(
    settings: Option<&Settings>,
    status: Option<&Status>,
    now: i64,
) -> (Value, String, u8) {
    let Some(settings) = settings else {
        return (
            json!({"ok": true, "enabled": false, "checks": []}),
            "internet sharing: off (never turned on here)".to_string(),
            0,
        );
    };
    let checks = checks(settings, status, now);
    let (report, healthy) = crate::cli::report(&checks);
    let mut text = report.trim_end().to_string();
    let port = status.and_then(|s| s.proxy_port);
    if settings.enabled {
        text.push_str(&format!(
            "\nproxy for the operator: {} (`shutup-and-run internet credentials` shows the password)",
            proxy_url(settings, port, false)
        ));
        if let Some(hours) = &settings.lend_hours {
            text.push_str(&format!("\nhours: {hours}"));
        }
    }
    let value = json!({
        "ok": healthy,
        "enabled": settings.enabled,
        "relay": settings.relay,
        "id": settings.id,
        "proxy": proxy_url(settings, port, false),
        "lend_hours": settings.lend_hours,
        "metered": settings.metered,
        "consent_at": settings.consent_at,
        "status": status,
        "checks": crate::cli::checks_json(&checks),
    });
    (value, text, if healthy { 0 } else { 1 })
}

fn credentials_cli(dir: &Path, args: &[String]) -> Result<ExitCode, String> {
    let flags = crate::wsl::parse_flags(args, &[], &["--json"]).map_err(|e| e.0)?;
    let Some(settings) = read_settings(dir) else {
        return Err("internet sharing was never turned on here: `shutup-and-run internet on --relay HOST:PORT`".to_string());
    };
    let port = read_status(dir).and_then(|s| s.proxy_port);
    let url = proxy_url(&settings, port, true);
    print(
        flags.has("--json"),
        json!({"ok": true, "id": settings.id, "password": settings.secret, "proxy": url}),
        format!(
            "user: {}\npassword: {}\nproxy: {url}",
            settings.id, settings.secret
        ),
    );
    Ok(ExitCode::SUCCESS)
}

/// Starts `internet serve` now and at every logon or boot. What it did, in
/// one line.
fn start_process() -> Result<String, String> {
    if cfg!(windows) {
        let data = crate::wsl::data_dir();
        std::fs::create_dir_all(&data).map_err(|e| format!("{}: {e}", data.display()))?;
        let exe = crate::wsl::install_exe_copy(&data).map_err(|e| e.0)?;
        crate::wsl::register_named_task(&exe, TASK_NAME, "internet serve --background")
            .map_err(|e| e.0)?;
        return Ok(format!(
            "started; task {TASK_NAME} starts it at every logon"
        ));
    }
    if !Path::new("/run/systemd/system").exists() {
        return Ok("no systemd here: keep `shutup-and-run internet serve` running".to_string());
    }
    let exe = std::env::current_exe().map_err(|e| format!("current exe: {e}"))?;
    std::fs::write(UNIT_PATH, unit_text(&exe)).map_err(|e| format!("{UNIT_PATH}: {e}"))?;
    for args in [
        &["daemon-reload"][..],
        &["enable", "--now", "shutup-and-run-internet"][..],
    ] {
        let out = std::process::Command::new("systemctl")
            .args(args)
            .output()
            .map_err(|e| format!("systemctl: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "systemctl {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    }
    Ok("started; systemd unit shutup-and-run-internet starts it at boot".to_string())
}

fn stop_process() -> String {
    if cfg!(windows) {
        return crate::wsl::remove_named_task(TASK_NAME);
    }
    if !Path::new(UNIT_PATH).exists() {
        return "a running `internet serve` stops within 5 seconds".to_string();
    }
    let _ = std::process::Command::new("systemctl")
        .args(["disable", "--now", "shutup-and-run-internet"])
        .output();
    let _ = std::fs::remove_file(UNIT_PATH);
    let _ = std::process::Command::new("systemctl")
        .arg("daemon-reload")
        .output();
    "removed systemd unit shutup-and-run-internet".to_string()
}

pub(crate) fn unit_text(exe: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=shutup-and-run: lend this machine's internet connection\n\
         After=network-online.target\n\
         Wants=network-online.target\n\n\
         [Service]\n\
         ExecStart={} internet serve\n\
         Restart=always\n\
         RestartSec=5\n\n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        exe.display()
    )
}

// ---------------------------------------------------------------------------
// The page: read-only, loopback only

#[derive(Clone)]
struct Page {
    dir: PathBuf,
}

impl Page {
    fn router(self) -> axum::Router {
        use axum::routing::get;
        axum::Router::new()
            .route("/", get(page))
            .layer(axum::middleware::from_fn(crate::status::local_only))
            .with_state(self)
    }
}

async fn page(
    axum::extract::State(page): axum::extract::State<Page>,
) -> axum::response::Html<String> {
    let pt = crate::hints::lang() == crate::hints::Lang::Pt;
    axum::response::Html(render(
        read_settings(&page.dir).as_ref(),
        read_status(&page.dir).as_ref(),
        crate::status::now_unix(),
        pt,
    ))
}

fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub(crate) fn render(
    settings: Option<&Settings>,
    status: Option<&Status>,
    now: i64,
    pt: bool,
) -> String {
    let t = |en: &'static str, pt_text: &'static str| if pt { pt_text } else { en };
    let mut body = String::new();
    match settings {
        None => body.push_str(&format!(
            "<p>{}</p>",
            t(
                "Off. To turn it on: <code>shutup-and-run internet on --relay HOST:PORT</code>",
                "Desligado. Para ligar: <code>shutup-and-run internet on --relay HOST:PORTA</code>"
            )
        )),
        Some(settings) => {
            for c in checks(settings, status, now) {
                let mark = if c.ok { "ok" } else { "!" };
                body.push_str(&format!(
                    "<p class=\"{}\"><b>{mark}</b> {}</p>",
                    if c.ok { "ok" } else { "bad" },
                    esc(&c.detail)
                ));
                if !c.ok {
                    body.push_str(&format!("<p class=\"hint\">{}</p>", esc(&c.hint())));
                }
            }
            if settings.enabled {
                let port = status.and_then(|s| s.proxy_port);
                body.push_str(&format!(
                    "<table>\
                     <tr><th>{}</th><td><code>{}</code></td></tr>\
                     <tr><th>{}</th><td><code>{}</code></td></tr>\
                     <tr><th>{}</th><td><details><summary>{}</summary><code>{}</code></details></td></tr>\
                     <tr><th>{}</th><td><details><summary>{}</summary><code>{}</code></details></td></tr>",
                    t("Relay", "Relay"),
                    esc(&settings.relay),
                    t("User (id)", "Usuário (id)"),
                    esc(&settings.id),
                    t("Password", "Senha"),
                    t("show", "mostrar"),
                    esc(&settings.secret),
                    t("Proxy for the operator", "Proxy para o operador"),
                    t("show", "mostrar"),
                    esc(&proxy_url(settings, port, true)),
                ));
                if let Some(status) = status {
                    body.push_str(&format!(
                        "<tr><th>{}</th><td>{} / {}</td></tr>\
                         <tr><th>{}</th><td>{} {}</td></tr>",
                        t("Received / sent", "Recebido / enviado"),
                        bytes_text(status.bytes_received),
                        bytes_text(status.bytes_sent),
                        t("Counting since", "Contando desde"),
                        esc(&status.counting_since),
                        t("(UTC)", "(UTC)"),
                    ));
                }
                if let Some(hours) = &settings.lend_hours {
                    body.push_str(&format!(
                        "<tr><th>{}</th><td><code>{}</code></td></tr>",
                        t("Hours", "Horário"),
                        esc(hours)
                    ));
                }
                body.push_str("</table>");
            }
            body.push_str(&format!(
                "<p class=\"hint\">{}</p>",
                t(
                    "To turn it off: <code>shutup-and-run internet off</code>",
                    "Para desligar: <code>shutup-and-run internet off</code>"
                )
            ));
        }
    }
    format!(
        "<!doctype html><html lang=\"{}\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta http-equiv=\"refresh\" content=\"10\">\
         <title>shutup-and-run: {}</title>\
         <style>body{{font:15px system-ui,sans-serif;max-width:46em;margin:2em auto;padding:0 1em}}\
         .bad b{{color:#b00}}.ok b{{color:#070}}.hint{{color:#555}}th{{text-align:left;padding-right:1em}}\
         code{{word-break:break-all}}</style></head><body><h1>{}</h1>{body}</body></html>",
        if pt { "pt-BR" } else { "en" },
        t("internet sharing", "compartilhar internet"),
        t("Internet sharing", "Compartilhar internet"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(relay: &str) -> Settings {
        Settings {
            enabled: true,
            relay: relay.to_string(),
            id: "pc-test".to_string(),
            secret: "0123456789abcdef0123456789abcdef".to_string(),
            lend_hours: None,
            metered: false,
            consent_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn only_an_address_on_the_internet_is_lent() {
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "fe80::1",
            "fd00::1",
            "::ffff:192.168.0.1",
            "198.18.0.1",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
        for public in [
            "8.8.8.8",
            "200.160.2.3",
            "2001:4860:4860::8888",
            "100.128.0.1",
        ] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn a_target_is_host_and_port_with_nothing_that_ends_a_line() {
        assert!(valid_target("example.com:443"));
        assert!(valid_target("[2001:db8::1]:443"));
        assert!(!valid_target("example.com"));
        assert!(!valid_target("example.com:0"));
        assert!(!valid_target("example.com:443\nC x:1"));
        assert!(!valid_target("exa mple.com:443"));
        assert!(valid_word("pc-natal-01", 1..=64));
        assert!(!valid_word("pc natal", 1..=64));
    }

    #[test]
    fn the_status_says_what_the_process_wrote_and_when_it_stopped() {
        let s = settings("relay.example.com:7000");
        let now = 1_800_000_000;
        let mut status = Status {
            state: "connected".to_string(),
            id: s.id.clone(),
            relay: s.relay.clone(),
            idle: 4,
            bytes_received: 2048,
            updated_at: crate::status::unix_to_iso(now - 10),
            ..Status::default()
        };
        let lines = checks(&s, Some(&status), now);
        assert!(lines[0].ok, "{:?}", lines[0].detail);
        assert!(lines[0].detail.contains("4 idle"));

        status.updated_at = crate::status::unix_to_iso(now - 600);
        assert_eq!(checks(&s, Some(&status), now)[0].code(), NOT_RUNNING);
        assert_eq!(checks(&s, None, now)[0].code(), NOT_RUNNING);

        status.updated_at = crate::status::unix_to_iso(now);
        status.state = "connecting".to_string();
        status.last_error = Some("the relay relay.example.com:7000: refused".to_string());
        let line = &checks(&s, Some(&status), now)[0];
        assert_eq!(line.code(), RELAY_UNREACHABLE);
        assert!(
            line.hint().contains("relay.example.com:7000"),
            "{}",
            line.hint()
        );

        status.state = "waiting".to_string();
        status.hold = Some(Hold {
            code: "METERED_NETWORK".to_string(),
            detail: "metered".to_string(),
            resumes_at: None,
        });
        let line = &checks(&s, Some(&status), now)[0];
        assert_eq!(line.code(), WAITING);
        assert!(line.soft);

        let off = Settings {
            enabled: false,
            ..s.clone()
        };
        assert!(checks(&off, None, now)[0].ok);

        let (value, text, code) = status_out(Some(&s), Some(&status), now);
        assert_eq!(code, 0, "a wait is not a failure");
        assert!(!text.contains(&s.secret) && !value.to_string().contains(&s.secret));
        assert!(text.contains("http://pc-test:***@relay.example.com:"));
        let (_, _, code) = status_out(None, None, now);
        assert_eq!(code, 0);
    }

    #[test]
    fn the_proxy_address_is_the_relays_host_and_the_port_it_gave() {
        let s = settings("relay.example.com:7000");
        assert_eq!(
            proxy_url(&s, Some(8080), true),
            "http://pc-test:0123456789abcdef0123456789abcdef@relay.example.com:8080"
        );
        assert_eq!(
            proxy_url(&s, None, false),
            "http://pc-test:***@relay.example.com:PROXY_PORT"
        );
    }

    #[test]
    fn the_file_round_trips_and_an_unknown_field_is_refused() {
        let dir = std::env::temp_dir().join(format!("sar-internet-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let s = settings("relay.example.com:7000");
        write_settings(&dir, &s).unwrap();
        assert_eq!(read_settings(&dir), Some(s));
        std::fs::write(dir.join(FILE), r#"{"enabled":true,"extra":1}"#).unwrap();
        assert_eq!(read_settings(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_paused_worker_holds_new_streams() {
        let dir = std::env::temp_dir().join(format!("sar-internet-p-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("status.json");
        let now = 1_800_000_000;
        let doc = json!({"state": "draining", "updated_at": crate::status::unix_to_iso(now)});
        std::fs::write(&path, doc.to_string()).unwrap();
        let s = settings("relay.example.com:7000");
        assert_eq!(
            hold_now(&s, Some(&path), now).unwrap().code,
            "WORKER_PAUSED"
        );
        let doc = json!({"state": "idle", "updated_at": crate::status::unix_to_iso(now)});
        std::fs::write(&path, doc.to_string()).unwrap();
        if !cfg!(windows) {
            assert_eq!(hold_now(&s, Some(&path), now), None);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_page_names_the_state_and_hides_the_password_until_asked() {
        let s = settings("relay.example.com:7000");
        let html = render(Some(&s), None, 1_800_000_000, true);
        assert!(html.contains("Compartilhar internet"));
        assert!(html.contains("<details><summary>mostrar</summary>"));
        let off = render(None, None, 1_800_000_000, false);
        assert!(off.contains("internet on --relay"));
    }

    #[test]
    fn bytes_read_as_a_person_says_them() {
        assert_eq!(bytes_text(0), "0 B");
        assert_eq!(bytes_text(7040), "6.9 KiB");
        assert_eq!(bytes_text(1_010_749), "987.1 KiB");
        assert_eq!(bytes_text(3 << 30), "3.0 GiB");
    }

    #[test]
    fn the_unit_runs_the_process_again_when_it_stops() {
        let unit = unit_text(Path::new("/usr/local/bin/shutup-and-run"));
        assert!(unit.contains("ExecStart=/usr/local/bin/shutup-and-run internet serve"));
        assert!(unit.contains("Restart=always"));
    }
}
