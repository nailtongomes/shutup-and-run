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
    let slots = launcher.slots().await.ok();
    let draining = launcher.draining().unwrap_or(false);
    let last = launcher.last_job().ok().flatten();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let view = View {
        worker: &settings.worker_name,
        os: &settings.worker_os,
        slots: slots.map(|s| (s.used, s.total)),
        draining,
        last: last.as_ref(),
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
    let (state, color) = match (view.slots, view.draining) {
        (None, _) => ("DOCKER PAROU / DOCKER IS DOWN", "#b00020"),
        (Some(_), true) => ("PAUSADO / PAUSED", "#8a6d00"),
        (Some(_), false) => ("RODANDO / RUNNING", "#0a7a2f"),
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
}
