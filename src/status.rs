//! The local status page: `GET /` on the machine itself, off unless
//! `status_page = true`. It is for the person at the PC and for support
//! asking them for a screenshot: is it running, is a job running, may the PC
//! be turned off. It shows no token, no env and no logs.
//!
//! It is the one thing on the port without the bearer token, so it answers
//! only a loopback peer that also names a loopback host. Anything else is a
//! 404, and never an authentication failure: a browser must not get the
//! machine's own address banned.

use crate::launcher::Launcher;
use crate::store::JobRecord;
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::middleware::{from_fn, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn router(launcher: Arc<Launcher>) -> Router {
    Router::new()
        .route("/", get(page))
        // A browser asks for this by itself. Unanswered, it would reach the
        // API as a request without a token and count toward a ban.
        .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }))
        .route("/local/pause", post(pause))
        .route("/local/resume", post(resume))
        .layer(from_fn(local_only))
        .with_state(launcher)
}

async fn local_only(request: Request<Body>, next: Next) -> Response {
    // The socket peer, never X-Forwarded-For. The Host check keeps out a
    // proxy on this machine that forwards a public name, and DNS rebinding.
    let peer_is_local = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| match info.0.ip() {
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
            }
            std::net::IpAddr::V4(v4) => v4.is_loopback(),
        })
        .unwrap_or(false);
    if !peer_is_local || local_host(request.headers()).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    if request.method() == axum::http::Method::POST && !same_origin(request.headers()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

/// The Host header, when it names this machine's loopback.
fn local_host(headers: &HeaderMap) -> Option<&str> {
    let host = headers.get(header::HOST)?.to_str().ok()?;
    let name = host.rsplit_once(':').map_or(host, |(name, _port)| name);
    matches!(name, "127.0.0.1" | "localhost" | "[::1]").then_some(host)
}

/// A button on the page posts with `Origin: http://<this host>`. A form on
/// some other site the person has open posts with that site's origin.
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(host) = local_host(headers) else {
        return false;
    };
    headers
        .get(header::ORIGIN)
        .and_then(|origin| origin.to_str().ok())
        .is_some_and(|origin| origin == format!("http://{host}"))
}

async fn pause(State(launcher): State<Arc<Launcher>>) -> Response {
    set_draining(&launcher, true)
}

async fn resume(State(launcher): State<Arc<Launcher>>) -> Response {
    set_draining(&launcher, false)
}

fn set_draining(launcher: &Launcher, draining: bool) -> Response {
    match launcher.set_draining(draining) {
        Ok(_) => Redirect::to("/").into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn page(State(launcher): State<Arc<Launcher>>) -> Response {
    let settings = launcher.settings();
    let live = launcher.live_status().await;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let view = View {
        worker: &settings.worker_name,
        os: &settings.worker_os,
        slots: live.slots_for_page(),
        draining: live.draining,
        last: live.last.as_ref(),
        now,
    };
    ([(header::CACHE_CONTROL, "no-store")], Html(render(&view))).into_response()
}

pub(crate) struct View<'a> {
    pub worker: &'a str,
    pub os: &'a str,
    /// Used and total. None when Docker did not answer.
    pub slots: Option<(usize, usize)>,
    pub draining: bool,
    pub last: Option<&'a JobRecord>,
    pub now: i64,
}

pub(crate) fn render(view: &View) -> String {
    let (state, color) = match worker_state(
        view.slots.is_none(),
        view.draining,
        view.slots.map(|(used, _)| used).unwrap_or(0),
        false,
    ) {
        "failed" => ("DOCKER PAROU / DOCKER IS DOWN", "#b00020"),
        "draining" => ("PAUSADO / PAUSED", "#8a6d00"),
        _ => ("RODANDO / RUNNING", "#0a7a2f"),
    };
    let (slots, off) = match view.slots {
        Some((used, total)) => (
            format!("{used} / {total}"),
            if used == 0 {
                "SIM, pode desligar o PC. / YES, the PC can be turned off."
            } else {
                "NAO agora: ha tarefa rodando. / NOT now: a job is running."
            },
        ),
        None => ("?".to_string(), "?"),
    };
    let last = match view.last {
        Some(job) => format!(
            "{} &middot; {}{}{} &middot; {}",
            escape(&job.image),
            job.status(),
            job.reason
                .as_deref()
                .map(|r| format!(" ({})", escape(r)))
                .unwrap_or_default(),
            job.exit_code
                .map(|c| format!(", exit {c}"))
                .unwrap_or_default(),
            ago(view.now.saturating_sub(job.started_at)),
        ),
        None => "nenhum ainda / none yet".to_string(),
    };
    let (action, label) = if view.draining {
        ("/local/resume", "Voltar a aceitar tarefas / Resume")
    } else {
        ("/local/pause", "Pausar: nao pegar novas tarefas / Pause")
    };
    format!(
        "<!doctype html><html lang=\"pt-BR\"><head><meta charset=\"utf-8\">\
         <meta http-equiv=\"refresh\" content=\"10\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>shutup-and-run</title>\
         <style>body{{font:16px/1.5 system-ui,sans-serif;max-width:40em;margin:2em auto;padding:0 1em}}\
         h1{{font-size:1.1em}}.state{{font-size:1.6em;font-weight:700;color:{color}}}\
         td{{padding:.2em 1em .2em 0;vertical-align:top}}button{{font:inherit;padding:.4em 1em}}</style>\
         </head><body><h1>shutup-and-run {version}</h1>\
         <p class=\"state\">{state}</p>\
         <table>\
         <tr><td>Pode desligar? / Turn off?</td><td><b>{off}</b></td></tr>\
         <tr><td>Tarefas agora / Jobs now</td><td>{slots}</td></tr>\
         <tr><td>Ultima tarefa / Last job</td><td>{last}</td></tr>\
         <tr><td>Maquina / Machine</td><td>{worker} ({os})</td></tr>\
         </table>\
         <form method=\"post\" action=\"{action}\"><p><button>{label}</button></p></form>\
         <p><small>Esta pagina so abre neste computador e se atualiza a cada 10 s. / \
         This page opens only on this computer and reloads every 10 s.</small></p>\
         </body></html>",
        version = env!("CARGO_PKG_VERSION"),
        worker = escape(view.worker),
        os = escape(view.os),
    )
}

fn ago(secs: i64) -> String {
    match secs.max(0) {
        s if s < 120 => format!("{s} s"),
        s if s < 7200 => format!("{} min", s / 60),
        s if s < 172_800 => format!("{} h", s / 3600),
        s => format!("{} d", s / 86_400),
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ---------------------------------------------------------------------------
// status.json. The page and this file are both built from `Live`.

/// How often a healthy worker rewrites the file even when nothing changed.
pub const REWRITE_EVERY: std::time::Duration = std::time::Duration::from_secs(30);
/// A file older than this was not rewritten on time: the worker is dead.
pub const STALE_AFTER_SECS: i64 = 60;
/// Marker on the service's stdout. The Windows supervisor copies that line
/// into `C:\ProgramData\shutup-and-run\status.json`.
pub const LOG_MARK: &str = "status-file ";

/// What the page and the file both describe. `slots_*` is None when Docker
/// did not answer.
pub struct Live {
    pub slots_used: Option<u64>,
    pub slots_total: Option<u64>,
    pub draining: bool,
    pub job_ids: Vec<String>,
    pub last_job_at: Option<i64>,
    pub last_job_error: Option<String>,
    pub docker_error: Option<String>,
    pub last: Option<JobRecord>,
}

impl Live {
    pub fn slots_for_page(&self) -> Option<(usize, usize)> {
        Some((self.slots_used? as usize, self.slots_total? as usize))
    }

    /// State, slots, jobs, and the last error. `updated_at` is not part of it,
    /// so a heartbeat still rewrites the file on the interval.
    pub fn signature(&self) -> String {
        format!(
            "{}|{:?}|{:?}|{}|{}|{:?}|{:?}",
            self.state(false),
            self.slots_used,
            self.slots_total,
            self.draining,
            self.job_ids.join(","),
            self.last_job_at,
            self.last_error(false, None)
        )
    }

    pub fn state(&self, stopped: bool) -> &'static str {
        worker_state(
            self.docker_error.is_some(),
            self.draining,
            self.slots_used.unwrap_or(0) as usize,
            stopped,
        )
    }

    pub fn last_error(&self, stopped: bool, failed: Option<&str>) -> Option<String> {
        if let Some(msg) = failed {
            return Some(one_line(msg));
        }
        if stopped {
            return None;
        }
        self.docker_error
            .as_deref()
            .or(self.last_job_error.as_deref())
            .map(one_line)
    }
}

/// `active` while a job holds a slot, `idle` when none does, `draining` when
/// new work is refused, `failed` when Docker (or the whole service) is down,
/// `stopped` after a graceful stop.
pub fn worker_state(
    docker_down: bool,
    draining: bool,
    slots_used: usize,
    stopped: bool,
) -> &'static str {
    if stopped {
        "stopped"
    } else if docker_down {
        "failed"
    } else if draining {
        "draining"
    } else if slots_used > 0 {
        "active"
    } else {
        "idle"
    }
}

pub fn job_error(job: &JobRecord) -> Option<String> {
    if !job.is_finished() {
        return None;
    }
    let bad_code = job.exit_code.unwrap_or(0) != 0;
    let bad_reason = job.reason.as_deref().is_some_and(|r| r != "exited");
    if !bad_code && !bad_reason {
        return None;
    }
    let reason = job.reason.as_deref().unwrap_or("exited");
    Some(match job.exit_code {
        Some(code) => format!("{reason} exit {code}"),
        None => reason.to_string(),
    })
}

#[derive(serde::Serialize)]
struct FileBody<'a> {
    state: &'a str,
    slots_total: Option<u64>,
    slots_used: Option<u64>,
    job_ids: &'a [String],
    last_job_at: Option<String>,
    last_error: Option<String>,
    version: &'a str,
    pid: u32,
    updated_at: String,
}

pub fn document(live: &Live, now_unix: i64, stopped: bool, failed: Option<&str>) -> Vec<u8> {
    let err = live.last_error(stopped, failed);
    let body = FileBody {
        state: if failed.is_some() {
            "failed"
        } else {
            live.state(stopped)
        },
        slots_total: live.slots_total,
        slots_used: live.slots_used,
        job_ids: &live.job_ids,
        last_job_at: live.last_job_at.map(unix_to_iso),
        last_error: err,
        version: env!("CARGO_PKG_VERSION"),
        pid: std::process::id(),
        updated_at: unix_to_iso(now_unix),
    };
    let mut bytes = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
    bytes.push(b'\n');
    bytes
}

/// A failure the Windows supervisor records when it cannot see the service.
pub fn failed_bytes(reason: &str) -> Vec<u8> {
    document(&empty_live(), now_unix(), false, Some(reason))
}

pub fn stopped_bytes() -> Vec<u8> {
    document(&empty_live(), now_unix(), true, None)
}

fn empty_live() -> Live {
    Live {
        slots_used: None,
        slots_total: None,
        draining: false,
        job_ids: Vec::new(),
        last_job_at: None,
        last_job_error: None,
        docker_error: None,
        last: None,
    }
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `true` when `updated_at` is missing, unreadable, or more than
/// [`STALE_AFTER_SECS`] behind `now_unix`.
pub fn is_stale(updated_at: &str, now_unix: i64) -> bool {
    match iso_to_unix(updated_at) {
        Some(then) => now_unix.saturating_sub(then) > STALE_AFTER_SECS,
        None => true,
    }
}

pub fn status_path(history: &std::path::Path) -> std::path::PathBuf {
    history.with_file_name("status.json")
}

/// The JSON object on a service log line, if this line carries one.
pub fn status_json_from_log(line: &str) -> Option<&str> {
    let rest = line.split(LOG_MARK).nth(1)?.trim();
    rest.starts_with('{').then_some(rest)
}

/// Write `bytes` by creating a sibling temp file and renaming it over `path`,
/// so a reader sees the previous file or the new one, never a short write.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let tmp = path.with_extension("json.tmp");
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    replace_file(&tmp, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644));
    }
    Ok(())
}

pub fn log_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    format!("{LOG_MARK}{}", text.trim_end())
}

fn one_line(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.trim();
    flat.chars().take(200).collect()
}

/// Days since the Unix epoch to a UTC `YYYY-MM-DDThh:mm:ssZ`.
pub fn unix_to_iso(secs: i64) -> String {
    let secs = secs.max(0) as u64;
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    // Howard Hinnant, civil_from_days. Public domain.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

fn iso_to_unix(text: &str) -> Option<i64> {
    let text = text.trim().strip_suffix('Z')?;
    let (date, time) = text.split_once('T')?;
    let mut date = date.split('-');
    let y: i64 = date.next()?.parse().ok()?;
    let m: u64 = date.next()?.parse().ok()?;
    let d: u64 = date.next()?.parse().ok()?;
    if date.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let mut time = time.split(':');
    let hh: u64 = time.next()?.parse().ok()?;
    let mm: u64 = time.next()?.parse().ok()?;
    let ss: u64 = time.next()?.parse().ok()?;
    if time.next().is_some() || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    let days = days_from_civil(y, m, d)?;
    Some(days * 86_400 + (hh * 3600 + mm * 60 + ss) as i64)
}

fn days_from_civil(y: i64, m: u64, d: u64) -> Option<i64> {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe as i64 - 719_468)
}

fn replace_file(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        extern "system" {
            fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
        }
        const MOVEFILE_REPLACE_EXISTING: u32 = 1;
        fn wide(path: &std::path::Path) -> Vec<u16> {
            path.as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        }
        let from = wide(from);
        let to = wide(to);
        // SAFETY: both pointers are NUL-terminated paths that outlive the call.
        let ok = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING) };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(from, to)
    }
}

/// Rewrites `status.json` beside the history file. Stops when [`StatusTask::finish`]
/// runs, and that call is the last write.
pub struct StatusTask {
    path: std::path::PathBuf,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    lock: std::sync::Arc<tokio::sync::Mutex<()>>,
}

pub fn spawn(launcher: Arc<Launcher>) -> StatusTask {
    let path = status_path(&launcher.settings().history_path);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
    let task = StatusTask {
        path: path.clone(),
        stop: stop.clone(),
        lock: lock.clone(),
    };
    tokio::spawn(async move {
        let mut ticks = tokio::time::interval(std::time::Duration::from_secs(1));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut since = REWRITE_EVERY;
        let mut previous = String::new();
        loop {
            ticks.tick().await;
            if stop.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            let _guard = lock.lock().await;
            if stop.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            since += std::time::Duration::from_secs(1);
            let live = launcher.live_status().await;
            let signature = live.signature();
            if signature == previous && since < REWRITE_EVERY {
                continue;
            }
            since = std::time::Duration::ZERO;
            previous = signature;
            publish(&path, &live, false, None);
        }
    });
    task
}

impl StatusTask {
    pub async fn finish(&self, launcher: &Launcher, end: End) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _guard = self.lock.lock().await;
        let live = launcher.live_status().await;
        match end {
            End::Stopped => publish(&self.path, &live, true, None),
            End::Failed(msg) => publish(&self.path, &live, false, Some(&msg)),
        }
    }
}

pub enum End {
    Stopped,
    Failed(String),
}

fn publish(path: &std::path::Path, live: &Live, stopped: bool, failed: Option<&str>) {
    let bytes = document(live, now_unix(), stopped, failed);
    if let Err(err) = write_atomic(path, &bytes) {
        tracing::warn!(error = %err, path = %path.display(), "status file");
        return;
    }
    tracing::info!("{}", log_line(&bytes));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_says_the_state_and_whether_the_pc_can_be_turned_off() {
        let idle = render(&View {
            worker: "DESK<07>",
            os: "windows",
            slots: Some((0, 2)),
            draining: false,
            last: None,
            now: 1_700_000_000,
        });
        assert!(idle.contains("RODANDO / RUNNING"));
        assert!(idle.contains("SIM, pode desligar"));
        assert!(idle.contains("0 / 2"));
        assert!(idle.contains("DESK&lt;07&gt; (windows)"));
        assert!(idle.contains(env!("CARGO_PKG_VERSION")));
        assert!(idle.contains("action=\"/local/pause\""));

        let busy = render(&View {
            worker: "d",
            os: "linux",
            slots: Some((1, 2)),
            draining: true,
            last: None,
            now: 0,
        });
        assert!(busy.contains("PAUSADO / PAUSED"));
        assert!(busy.contains("NAO agora"));
        assert!(busy.contains("action=\"/local/resume\""));

        let down = render(&View {
            worker: "d",
            os: "linux",
            slots: None,
            draining: false,
            last: None,
            now: 0,
        });
        assert!(down.contains("DOCKER PAROU"));
        assert_eq!(ago(90), "90 s");
        assert_eq!(ago(600), "10 min");
        assert_eq!(ago(7200), "2 h");
    }

    #[test]
    fn the_file_uses_the_same_state_as_the_page() {
        assert_eq!(worker_state(false, false, 0, false), "idle");
        assert_eq!(worker_state(false, false, 2, false), "active");
        assert_eq!(worker_state(false, true, 2, false), "draining");
        assert_eq!(worker_state(true, false, 0, false), "failed");
        assert_eq!(worker_state(false, false, 1, true), "stopped");

        let idle = render(&View {
            worker: "d",
            os: "linux",
            slots: Some((0, 2)),
            draining: false,
            last: None,
            now: 0,
        });
        assert!(idle.contains("RODANDO / RUNNING"));
        let down = render(&View {
            worker: "d",
            os: "linux",
            slots: None,
            draining: false,
            last: None,
            now: 0,
        });
        assert!(down.contains("DOCKER PAROU"));
    }

    #[test]
    fn status_json_has_the_fields_and_a_utc_timestamp() {
        assert_eq!(unix_to_iso(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(iso_to_unix("2023-11-14T22:13:20Z"), Some(1_700_000_000));
        assert!(!is_stale("2023-11-14T22:13:20Z", 1_700_000_000 + 60));
        assert!(is_stale("2023-11-14T22:13:20Z", 1_700_000_000 + 61));
        assert!(is_stale("not-a-time", 1_700_000_000));

        let live = Live {
            slots_used: Some(1),
            slots_total: Some(2),
            draining: false,
            job_ids: vec!["abc".into(), "def".into()],
            last_job_at: Some(1_700_000_000),
            last_job_error: None,
            docker_error: None,
            last: None,
        };
        let bytes = document(&live, 1_700_000_100, false, None);
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["state"], "active");
        assert_eq!(value["slots_total"], 2);
        assert_eq!(value["slots_used"], 1);
        assert_eq!(value["job_ids"], serde_json::json!(["abc", "def"]));
        assert_eq!(value["last_job_at"], "2023-11-14T22:13:20Z");
        assert!(value["last_error"].is_null());
        assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
        assert!(value["pid"].is_number());
        assert_eq!(value["updated_at"], "2023-11-14T22:15:00Z");
        let line = format!("INFO shutup_and_run::status: {}", log_line(&bytes));
        let parsed: serde_json::Value =
            serde_json::from_str(status_json_from_log(&line).unwrap()).unwrap();
        assert_eq!(parsed["state"], "active");
        assert!(status_json_from_log("supervisor: restarting").is_none());

        let failed: serde_json::Value =
            serde_json::from_slice(&failed_bytes("service exited")).unwrap();
        assert_eq!(failed["state"], "failed");
        assert_eq!(failed["last_error"], "service exited");
        assert!(failed["slots_total"].is_null());
        let stopped: serde_json::Value = serde_json::from_slice(&stopped_bytes()).unwrap();
        assert_eq!(stopped["state"], "stopped");
        assert!(stopped["last_error"].is_null());
    }

    #[test]
    fn the_status_file_is_replaced_whole() {
        let dir = std::env::temp_dir().join(format!(
            "shutup-and-run-status-{}-{}",
            std::process::id(),
            unix_to_iso(now_unix()).replace(':', "")
        ));
        let path = dir.join("status.json");
        let first = br#"{"state":"idle","slots_total":2,"slots_used":0,"job_ids":[],"last_job_at":null,"last_error":null,"version":"0","pid":1,"updated_at":"2023-11-14T22:13:20Z"}"#;
        let second = br#"{"state":"active","slots_total":2,"slots_used":1,"job_ids":["abc"],"last_job_at":"2023-11-14T22:13:20Z","last_error":null,"version":"0","pid":1,"updated_at":"2023-11-14T22:13:40Z"}"#;
        write_atomic(&path, first).unwrap();
        write_atomic(&path, second).unwrap();
        let got = std::fs::read(&path).unwrap();
        assert_eq!(got, second);
        let parsed: serde_json::Value = serde_json::from_slice(&got).unwrap();
        assert_eq!(parsed["state"], "active");
        assert_eq!(parsed["job_ids"][0], "abc");
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
