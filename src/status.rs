//! The local page: `GET /` on the machine itself, off unless
//! `status_page = true`. It is for the person at the PC and for support
//! asking them for a screenshot: is it running, is a job running, may the PC
//! be turned off. On a machine whose config is config.json it also edits
//! that file: the everyday keys directly, the dangerous ones behind the
//! admin login. It shows no token, no env and no logs.
//!
//! It is the one thing on the port without the bearer token, so it answers
//! only a loopback peer that also names a loopback host. Anything else is a
//! 404, and never an authentication failure: a browser must not get the
//! machine's own address banned.

use crate::config::{self, JsonMap, Kind, OnPage};
use crate::launcher::Launcher;
use crate::store::JobRecord;
use axum::body::Body;
use axum::extract::{ConnectInfo, Form, Query, State};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::middleware::{from_fn, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::Engine;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
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
        .route("/local/lang", get(set_lang))
        .route("/local/safety", get(safety_page).post(safety_save))
        .route("/local/config", get(config_page).post(config_save))
        .route("/local/advanced", get(advanced_page).post(advanced_save))
        .route(
            "/local/certificate",
            get(certificate_page).post(certificate_save),
        )
        .route("/local/managed", get(managed_page).post(managed_save))
        .route("/local/managed/edit", get(managed_edit))
        .route("/local/logs", get(logs_page))
        .route("/local/logs/{id}", get(log_file))
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

/// `?for=1h`, `3h` or `day` (to midnight on the PC's clock) pauses for a
/// while; nothing pauses until someone resumes.
async fn pause(
    State(launcher): State<Arc<Launcher>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let secs = match query.get("for").map(String::as_str) {
        None => return set_draining(&launcher, true),
        Some("1h") => 3600,
        Some("3h") => 3 * 3600,
        Some("day") => crate::lending::secs_to_midnight(crate::host::current(), now_unix()),
        Some(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    match launcher.pause_for(secs) {
        Ok(_) => Redirect::to("/").into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
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

// ---------------------------------------------------------------------------
// Language. One at a time on the screen, never both.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lang {
    En,
    Pt,
}

impl Lang {
    fn t(self, en: &'static str, pt: &'static str) -> &'static str {
        match self {
            Lang::En => en,
            Lang::Pt => pt,
        }
    }
}

/// The cookie the language button sets, kept by the browser for ten years.
/// Without it, `language = "pt"` in the config decides, then the browser's
/// own language.
pub(crate) fn lang_of(headers: &HeaderMap, configured: &str) -> Lang {
    let text = |name| {
        headers
            .get(name)
            .and_then(|v: &header::HeaderValue| v.to_str().ok())
    };
    let cookie = text(header::COOKIE).and_then(|cookies| {
        cookies
            .split(';')
            .filter_map(|pair| pair.trim().strip_prefix("lang="))
            .next()
    });
    match cookie {
        Some("pt") => Lang::Pt,
        Some("en") => Lang::En,
        // `language` of the config, the same key the CLI's hints follow.
        _ if configured == "pt" => Lang::Pt,
        _ if text(header::ACCEPT_LANGUAGE)
            .is_some_and(|accept| accept.trim_start().to_ascii_lowercase().starts_with("pt")) =>
        {
            Lang::Pt
        }
        _ => Lang::En,
    }
}

async fn set_lang(Query(query): Query<HashMap<String, String>>) -> Response {
    let lang = if query.get("set").map(String::as_str) == Some("pt") {
        "pt"
    } else {
        "en"
    };
    // Only back to one of this page's own paths.
    let to = query
        .get("to")
        .map(String::as_str)
        .filter(|to| matches!(*to, "/local/config" | "/local/advanced" | "/local/managed"))
        .unwrap_or("/");
    (
        [(
            header::SET_COOKIE,
            format!("lang={lang}; Path=/; Max-Age=315360000; SameSite=Lax"),
        )],
        Redirect::to(to),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// The status page

async fn page(State(launcher): State<Arc<Launcher>>, headers: HeaderMap) -> Response {
    let settings = launcher.settings();
    let live = launcher.live_status().await;
    let recent: Vec<(JobRecord, Option<u64>)> = launcher
        .recent_jobs(5)
        .unwrap_or_default()
        .into_iter()
        .map(|job| {
            let bytes = crate::joblogs::info(&settings.history_path, &job.id).map(|i| i.bytes);
            (job, bytes)
        })
        .collect();
    let programs = crate::host::programs();
    let view = View {
        memory: Some(&live.memory),
        programs: &programs,
        lang: lang_of(&headers, &settings.language),
        worker: &settings.worker_name,
        os: &settings.worker_os,
        slots: live.slots_for_page(),
        draining: live.draining,
        paused_until: live.paused_until,
        last: live.last.as_ref(),
        polls: settings.poll_url.is_some(),
        poll_error: live.poll_error.as_deref(),
        warning: live.config_warning.as_deref(),
        editable: settings.config_json.is_some(),
        computer: Some(&live.computer),
        limits: Some(&live.limits),
        waiting_for: live.waiting_for.as_deref(),
        safety: Some(&live.safety),
        managed: Some((
            live.managed_mode,
            live.managed.iter().filter(|m| m.state == "running").count(),
            live.managed.len(),
        )),
        last_poll: live.last_poll,
        recent: &recent,
        needs_secret: live
            .managed
            .iter()
            .filter(|m| !m.secrets_missing.is_empty())
            .map(|m| (m.definition.name.clone(), m.secrets_missing.join(", ")))
            .collect(),
    };
    html(StatusCode::OK, render(&view))
}

// ---------------------------------------------------------------------------
// Safety: what stands between this computer and a new job, and the one
// question only its owner answers. Not behind the admin login: that password
// is the operator's, and the yes has to be the person's at the PC.

fn findings_list(found: &[&crate::safety::Finding], lang: Lang) -> String {
    found
        .iter()
        .map(|f| {
            format!(
                "<li>{} <small>({})</small></li>",
                crate::safety::label(&f.code, lang == Lang::Pt),
                escape(&f.detail)
            )
        })
        .collect()
}

/// The safety page. `note` is the answer to the last button.
pub(crate) fn safety_view(
    report: &crate::safety::Report,
    lang: Lang,
    note: Option<(bool, &str)>,
) -> String {
    use crate::safety::Admission;
    let note = note
        .map(|(ok, text)| {
            format!(
                "<p class=\"callout tone-{}\"><b>{}</b></p>",
                if ok { "ok" } else { "bad" },
                escape(text)
            )
        })
        .unwrap_or_default();
    let back = format!(
        "<p class=\"note\"><a href=\"/\">{}</a></p>",
        lang.t("Back to the status", "Voltar ao status")
    );
    if !report.applies {
        return format!(
            "<h1>{}</h1>{note}<p>{}</p>{back}",
            lang.t("Safety of this computer", "Segurança deste computador"),
            lang.t(
                "This machine is dedicated to jobs (resource_profile = none), so it is not assessed.",
                "Esta máquina é dedicada a tarefas (resource_profile = none), então não é avaliada.",
            )
        );
    }
    let state = match report.admission {
        Admission::Open if report.reasons.is_empty() => lang.t(
            "Healthy: nothing stands in the way of a new job.",
            "Saudável: nada impede uma tarefa nova.",
        ),
        Admission::Open => lang.t(
            "There is a risk, and you authorized jobs under it.",
            "Há um risco, e você autorizou tarefas nessa condição.",
        ),
        Admission::AuthorizationRequired => lang.t(
            "New jobs are waiting for your decision.",
            "As tarefas novas estão esperando a sua decisão.",
        ),
        Admission::Blocked => lang.t(
            "New jobs are blocked to protect this computer. This clears by itself; no authorization lifts it.",
            "As tarefas novas estão bloqueadas para proteger este computador. Isso passa sozinho; nenhuma autorização libera.",
        ),
    };
    let section = |title: &str, found: Vec<&crate::safety::Finding>| {
        if found.is_empty() {
            String::new()
        } else {
            format!("<h2>{title}</h2><ul>{}</ul>", findings_list(&found, lang))
        }
    };
    let blocked = section(
        lang.t(
            "Critical now (blocks every new job)",
            "Crítico agora (bloqueia toda tarefa nova)",
        ),
        report.blocked(),
    );
    let asked = report.unauthorized();
    let covered: Vec<&crate::safety::Finding> = report
        .reasons
        .iter()
        .filter(|f| f.level == crate::safety::Level::Warning && !asked.contains(f))
        .collect();
    let form = if asked.is_empty() {
        String::new()
    } else {
        format!(
            "<h2>{t_risks}</h2><ul>{risks}</ul>{notice}\
             <form method=\"post\" action=\"/local/safety\" class=\"callout tone-warn\">\
             <label class=\"check\"><input type=\"checkbox\" name=\"confirm\" required> {t_confirm}</label>\
             <p><button class=\"primary\" name=\"action\" value=\"authorize\">{t_yes}</button></p>\
             <p><small>{t_no}</small></p></form>",
            t_risks = lang.t(
                "Risks that need your authorization",
                "Riscos que precisam da sua autorização"
            ),
            risks = findings_list(&asked, lang),
            notice = crate::safety::notice(lang == Lang::Pt)
                .split("\n\n")
                .map(|p| format!("<p>{}</p>", escape(p)))
                .collect::<String>(),
            t_confirm = lang.t(
                "I own or answer for this computer, I read the text above, and I authorize jobs to start under the conditions listed.",
                "Sou dono ou responsável por este computador, li o texto acima e autorizo que tarefas comecem nas condições listadas.",
            ),
            t_yes = lang.t("Authorize", "Autorizar"),
            t_no = lang.t(
                "To refuse, do nothing: no job starts under these conditions.",
                "Para recusar, não faça nada: nenhuma tarefa começa nessas condições.",
            ),
        )
    };
    let authorization = match &report.authorization {
        Some(consent) => format!(
            "<h2>{}</h2><p>{} {}{} ({} v{}): {}</p>{}\
             <form method=\"post\" action=\"/local/safety\"><p>\
             <button name=\"action\" value=\"revoke\">{}</button></p></form>",
            lang.t("Your authorization", "Sua autorização"),
            lang.t("Given on", "Dada em"),
            when(consent.at),
            if consent.channel == "contract" {
                lang.t(
                    ", in the contract, recorded when this PC was installed",
                    ", no contrato, registrada na instalação deste PC",
                )
            } else {
                ""
            },
            lang.t("notice", "aviso"),
            consent.notice_version,
            consent
                .codes
                .iter()
                .map(|code| crate::safety::label(code, lang == Lang::Pt))
                .collect::<Vec<_>>()
                .join("; "),
            if covered.is_empty() {
                String::new()
            } else {
                format!(
                    "<p>{}</p><ul>{}</ul>",
                    lang.t("In effect now for:", "Valendo agora para:"),
                    findings_list(&covered, lang)
                )
            },
            lang.t("Withdraw the authorization", "Retirar a autorização"),
        ),
        None => String::new(),
    };
    let incidents = if report.incidents.is_empty() {
        String::new()
    } else {
        format!(
            "<h2>{}</h2><div class=\"scroll\"><table>{}</table></div>",
            lang.t("Last incidents", "Últimos incidentes"),
            report
                .incidents
                .iter()
                .map(|i| format!(
                    "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
                    when(i.at),
                    escape(&i.kind),
                    escape(&i.detail)
                ))
                .collect::<String>()
        )
    };
    let tone = match report.admission {
        Admission::Open if report.reasons.is_empty() => "ok",
        Admission::Open | Admission::AuthorizationRequired => "warn",
        Admission::Blocked => "bad",
    };
    format!(
        "<h1>{title}</h1>{note}<section class=\"hero tone-{tone}\"><p class=\"sub\"><b>{state}</b></p></section>\
         {blocked}{form}{authorization}{incidents}{back}",
        title = lang.t("Safety of this computer", "Segurança deste computador"),
    )
}

async fn safety_page(State(launcher): State<Arc<Launcher>>, headers: HeaderMap) -> Response {
    let lang = lang_of(&headers, &launcher.settings().language);
    let report = launcher.live_status().await.safety;
    html(
        StatusCode::OK,
        shell(
            lang,
            "/local/safety",
            None,
            &safety_view(&report, lang, None),
        ),
    )
}

async fn safety_save(
    State(launcher): State<Arc<Launcher>>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let lang = lang_of(&headers, &launcher.settings().language);
    let note = match form.get("action").map(String::as_str) {
        // The box is the yes. A post without it, however it was made, is not.
        Some("authorize") if form.get("confirm").map(String::as_str) != Some("on") => (
            false,
            lang.t(
                "Not authorized: tick the box to confirm.",
                "Não autorizado: marque a caixa para confirmar.",
            ),
        ),
        Some("authorize") => match launcher.authorize("page") {
            Ok(_) => (true, lang.t("Authorized.", "Autorizado.")),
            Err(crate::safety::GrantError::NothingToAuthorize) => (
                true,
                lang.t(
                    "Nothing is waiting for an authorization.",
                    "Nada está esperando autorização.",
                ),
            ),
            Err(_) => (
                false,
                lang.t("Could not be saved.", "Não foi possível salvar."),
            ),
        },
        Some("revoke") => match launcher.revoke_authorization("page") {
            Ok(_) => (
                true,
                lang.t("Authorization withdrawn.", "Autorização retirada."),
            ),
            Err(_) => (
                false,
                lang.t("Could not be saved.", "Não foi possível salvar."),
            ),
        },
        _ => (false, lang.t("Unknown action.", "Ação desconhecida.")),
    };
    let report = launcher.live_status().await.safety;
    html(
        if note.0 {
            StatusCode::OK
        } else {
            StatusCode::BAD_REQUEST
        },
        shell(
            lang,
            "/local/safety",
            None,
            &safety_view(&report, lang, Some(note)),
        ),
    )
}

/// The logs of the last jobs: one row each, with a link that opens the
/// file in the browser and one that saves it.
async fn logs_page(State(launcher): State<Arc<Launcher>>, headers: HeaderMap) -> Response {
    let lang = lang_of(&headers, &launcher.settings().language);
    let history = launcher.settings().history_path.clone();
    let jobs: HashMap<String, JobRecord> = launcher
        .recent_jobs(crate::launcher::MAX_HISTORY_ROWS)
        .unwrap_or_default()
        .into_iter()
        .map(|job| (job.id.clone(), job))
        .collect();
    // The jobs of the history, newest first, then any log whose job the
    // history no longer has. Every log on disk is listed.
    let mut recent: Vec<&JobRecord> = jobs.values().collect();
    recent.sort_by_key(|job| std::cmp::Reverse(job.started_at));
    let mut rows: Vec<JobRow> = recent
        .iter()
        .take(100)
        .map(|job| JobRow {
            id: job.id.clone(),
            job: Some(job),
            log_bytes: crate::joblogs::info(&history, &job.id).map(|i| i.bytes),
            log_written_at: None,
        })
        .collect();
    let mut orphans: Vec<crate::joblogs::Info> = crate::joblogs::ids(&history)
        .iter()
        .filter(|id| !jobs.contains_key(*id))
        .filter_map(|id| crate::joblogs::info(&history, id))
        .collect();
    orphans.sort_by_key(|info| std::cmp::Reverse(info.written_at));
    rows.extend(orphans.iter().take(100).map(|info| {
        JobRow {
            id: info
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
            job: None,
            log_bytes: Some(info.bytes),
            log_written_at: Some(info.written_at),
        }
    }));
    let table = if rows.is_empty() {
        format!(
            "<p>{}</p>",
            lang.t("No job has run yet.", "Nenhuma tarefa rodou ainda.")
        )
    } else {
        jobs_table(&rows, lang)
    };
    let body = format!(
        "<h1>{title}</h1><p>{note}</p>{table}<p class=\"note\">{cli}</p>",
        title = lang.t("Jobs and logs", "Tarefas e logs"),
        note = lang.t(
            "What each job printed is kept on this computer for 7 days: its last 256 KiB. Nothing in it is masked. Open shows it here; download saves the file.",
            "O que cada tarefa imprimiu fica guardado neste computador por 7 dias: os últimos 256 KiB. Nada é mascarado. Abrir mostra aqui; baixar salva o arquivo.",
        ),
        cli = lang.t(
            "In a terminal: <code>shutup-and-run jobs</code> and <code>shutup-and-run jobs logs --last</code>.",
            "Num terminal: <code>shutup-and-run jobs</code> e <code>shutup-and-run jobs logs --last</code>.",
        ),
    );
    html(StatusCode::OK, shell(lang, "/local/logs", None, &body))
}

/// One log, as text: shown in the browser, or saved with `?download=1`.
async fn log_file(
    State(launcher): State<Arc<Launcher>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
) -> Response {
    // The id names a file: worker ids are hex, and nothing else is let through.
    if !crate::launcher::valid_container_id(&id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = crate::joblogs::path(&launcher.settings().history_path, &id);
    let Ok(bytes) = std::fs::read(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let disposition = if query.contains_key("download") {
        format!("attachment; filename=\"shutup-and-run-{id}.log\"")
    } else {
        "inline".to_string()
    };
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "text/plain; charset=utf-8".to_string(),
            ),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CACHE_CONTROL, "no-store".to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        bytes,
    )
        .into_response()
}

fn html(status: StatusCode, body: String) -> Response {
    (status, [(header::CACHE_CONTROL, "no-store")], Html(body)).into_response()
}

pub(crate) struct View<'a> {
    pub lang: Lang,
    pub worker: &'a str,
    pub os: &'a str,
    /// Used and total. None when Docker did not answer.
    pub slots: Option<(usize, usize)>,
    pub draining: bool,
    /// A pause for a while ends by itself then.
    pub paused_until: Option<i64>,
    pub last: Option<&'a JobRecord>,
    pub polls: bool,
    pub poll_error: Option<&'a str>,
    pub warning: Option<&'a str>,
    /// The config is config.json, so the page can edit it.
    pub editable: bool,
    /// The "Computer resources" section. None leaves it out.
    pub computer: Option<&'a Computer>,
    pub limits: Option<&'a Limits>,
    pub waiting_for: Option<&'a str>,
    /// What stands between this computer and a new job. None leaves it out.
    pub safety: Option<&'a crate::safety::Report>,
    /// Managed mode, and how many managed containers run, of how many.
    pub managed: Option<(bool, usize, usize)>,
    /// When the poll URL was last asked, and how it answered.
    pub last_poll: Option<(i64, &'static str)>,
    /// The last jobs, newest first, and the size of each one's log.
    pub recent: &'a [(JobRecord, Option<u64>)],
    /// Managed containers that cannot start: a secret has no value.
    pub needs_secret: Vec<(String, String)>,
    /// What memory keeps from starting, and how long it did.
    pub memory: Option<&'a crate::memory::View>,
    /// The programs holding the most of the PC's memory, while it is tight.
    pub programs: &'a [crate::memory::Program],
}

/// The one stylesheet of the local page. Embedded, no font, no script of
/// its own: the page works with no network. Colours follow the system's
/// light or dark theme. A surface is set apart by a step in tone; lines are
/// only where a table or a field needs them.
const PAGE_CSS: &str = "\
:root{color-scheme:light dark;--bg:#f3f3f1;--surface:#fff;--sunk:#e6e6e3;--ink:#16181b;--muted:#4b5058;\
--line:#d3d3cf;--btn:#16181b;--btn-ink:#fff;--focus:#0a5bb0;\
--ok:#0d6a33;--ok-bg:#dff1e5;--busy:#0a5bb0;--busy-bg:#dfebf8;--warn:#7a4f00;--warn-bg:#fbedcf;--bad:#a5121d;--bad-bg:#fbe2e2}\
@media (prefers-color-scheme:dark){:root{--bg:#111316;--surface:#1a1d21;--sunk:#262a30;--ink:#eceef1;--muted:#b3b8bf;\
--line:#383d44;--btn:#eceef1;--btn-ink:#111316;--focus:#8ec3f6;\
--ok:#82d9a3;--ok-bg:#15301f;--busy:#8ec3f6;--busy-bg:#142739;--warn:#f0c46c;--warn-bg:#352811;--bad:#ff9d9d;--bad-bg:#3b1619}}\
*{box-sizing:border-box}html{-webkit-text-size-adjust:100%}\
body{margin:0;background:var(--bg);color:var(--ink);font:16px/1.55 system-ui,-apple-system,\"Segoe UI\",Roboto,\"Noto Sans\",sans-serif}\
a{color:inherit;text-underline-offset:.18em}\
:focus-visible{outline:3px solid var(--focus);outline-offset:2px}\
.bar{display:flex;flex-wrap:wrap;align-items:center;gap:.4rem 1.4rem;padding:.8rem max(1.25rem,calc((100% - 64rem)/2));background:var(--surface)}\
.brand{font-weight:750;font-size:1.05rem;text-decoration:none;margin-right:auto}\
.brand span{font-weight:400;color:var(--muted);margin-left:.35em}\
.bar nav{display:flex;flex-wrap:wrap;gap:.2rem 1.1rem}@media (max-width:40rem){.brand{width:100%}}\
.bar nav a{text-decoration:none;padding:.2rem 0;border-bottom:2px solid transparent}\
.bar nav a:hover{border-bottom-color:var(--line)}\
.bar nav a[aria-current]{border-bottom-color:currentColor;font-weight:650}\
main{max-width:64rem;margin:0 auto;padding:1.75rem 1.25rem 3rem}\
h1{font-size:1.9rem;line-height:1.15;letter-spacing:-.015em;margin:0 0 1rem}\
h2{font-size:1.25rem;line-height:1.25;margin:2.5rem 0 .8rem}\
h3{font-size:1.05rem;margin:1.5rem 0 .4rem}\
p{margin:.6rem 0}ul{margin:.5rem 0;padding-left:1.25rem}li{margin:.25rem 0}\
small,.muted{color:var(--muted)}small{font-size:.875rem}\
.bad{color:var(--bad)}\
code{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:.875em;overflow-wrap:anywhere}\
pre{white-space:pre-wrap;background:var(--surface);padding:1rem;overflow-x:auto}\
.tone-ok{--tone:var(--ok);--tone-bg:var(--ok-bg)}.tone-busy{--tone:var(--busy);--tone-bg:var(--busy-bg)}\
.tone-warn{--tone:var(--warn);--tone-bg:var(--warn-bg)}.tone-bad{--tone:var(--bad);--tone-bg:var(--bad-bg)}\
.hero{background:var(--tone-bg);padding:1.6rem 1.6rem 1.3rem;margin:0 0 1rem}\
.hero .state{color:var(--tone);font-size:2.4rem;font-weight:800;line-height:1.05;letter-spacing:-.02em;margin:0}\
.hero .sub{font-size:1.15rem;margin:.6rem 0 .2rem}.hero .sub:first-child{margin-top:0}.hero .meta{color:var(--muted);margin:.2rem 0 0}\
.callout{background:var(--tone-bg);padding:1.1rem 1.3rem;margin:0 0 1rem}\
.callout h2{color:var(--tone);margin:0 0 .4rem;font-size:1.2rem}\
.callout p:last-child,.callout form:last-child p{margin-bottom:0}\
.facts{display:grid;grid-template-columns:repeat(auto-fill,minmax(17rem,1fr));gap:.5rem;margin:1rem 0}\
.facts div{background:var(--surface);padding:.8rem 1rem}\
.facts dt{color:var(--muted);font-size:.875rem}.facts dd{margin:.15rem 0 0;font-weight:600;overflow-wrap:anywhere}\
.actions{display:flex;flex-wrap:wrap;gap:.6rem;align-items:center;margin:1rem 0}\
.actions form,.actions p,form.inline{display:inline;margin:0}\
button,.button{display:inline-block;font:inherit;font-weight:600;line-height:1.3;padding:.6rem 1rem;border:0;border-radius:2px;\
background:var(--sunk);color:var(--ink);cursor:pointer;text-decoration:none;margin:.1rem 0}\
button:hover,.button:hover{background:var(--line)}\
button.primary,.button.primary{background:var(--btn);color:var(--btn-ink)}\
button.primary:hover,.button.primary:hover{opacity:.88}\
table{border-collapse:collapse;width:100%;background:var(--surface)}\
th,td{text-align:left;vertical-align:top;padding:.6rem .8rem;border-bottom:1px solid var(--line)}\
th{font-size:.875rem;font-weight:600;color:var(--muted)}\
td.num,th.num{text-align:right;font-variant-numeric:tabular-nums;white-space:nowrap}\
.scroll{overflow-x:auto;margin:.5rem 0}\
.kv td:first-child{color:var(--muted);width:40%}\
.pill{display:inline-block;padding:.1rem .55rem;border-radius:999px;font-size:.875rem;font-weight:650;\
background:var(--tone-bg);color:var(--tone);white-space:nowrap}\
fieldset{border:0;margin:0 0 1rem;padding:1.1rem 1.3rem;background:var(--surface)}\
legend{float:left;width:100%;padding:0;margin:0 0 .2rem;font-weight:700;font-size:1.1rem}\
legend+*{clear:both}\
.field{margin:1rem 0}.field:last-child{margin-bottom:.2rem}\
.field.locked{padding-left:.9rem;border-left:4px solid var(--warn)}\
.field label{display:block;font-weight:600;margin:0 0 .3rem}\
.help{color:var(--muted);font-size:.875rem;margin:.3rem 0 0}\
.tag{display:inline-block;margin-left:.5rem;padding:0 .45rem;font-size:.8rem;font-weight:650;color:var(--warn);background:var(--warn-bg)}\
input[type=text],input[type=number],input[type=password],input[type=file],textarea,select{font:inherit;width:100%;padding:.55rem .65rem;\
border:1px solid var(--line);border-radius:2px;background:var(--bg);color:var(--ink)}\
label{display:block;margin:.8rem 0 .3rem}\
label.check{display:flex;gap:.6rem;align-items:flex-start;font-weight:400}\
label.check input{margin-top:.3rem;width:1.1rem;height:1.1rem;flex:none}\
details{background:var(--surface);padding:.8rem 1rem;margin:1rem 0}summary{cursor:pointer;font-weight:600}\
.note{margin-top:2.5rem;color:var(--muted);font-size:.875rem}\
@media (max-width:40rem){main{padding:1.1rem 1rem 2.5rem}.hero{padding:1.2rem 1rem}.hero .state{font-size:1.85rem}\
h1{font-size:1.6rem}.kv td{display:block;width:auto;border:0;padding:.05rem .9rem}\
.kv td:first-child{width:auto;padding-top:.65rem;font-size:.875rem}.kv tr{display:block;border-bottom:1px solid var(--line);padding-bottom:.6rem}\
.jobs tr:first-child{display:none}.jobs tr{display:block;padding:.6rem .9rem;border-bottom:1px solid var(--line)}\
.jobs td{display:flex;gap:.75rem;border:0;padding:.15rem 0;text-align:left}\
.jobs td::before{content:attr(data-label);color:var(--muted);font-size:.875rem;min-width:5.5rem;padding-top:.1rem}\
fieldset{padding:1rem}}";

/// Turns each `<time datetime>` into the browser's local date and time.
/// Without it the page still reads: the text is UTC.
const LOCAL_TIME_JS: &str = "(function(){var l=document.documentElement.lang;\
document.querySelectorAll('time[datetime]').forEach(function(t){var d=new Date(t.getAttribute('datetime'));\
if(!isNaN(d))t.textContent=d.toLocaleString(l,{dateStyle:'short',timeStyle:'short'})})})();";

/// A moment as `<time>`: UTC in the markup, local time once the page loads.
fn when(unix: i64) -> String {
    let iso = unix_to_iso(unix);
    let text = iso.replacen('T', " ", 1).trim_end_matches('Z').to_string();
    format!(
        "<time datetime=\"{iso}\">{} UTC</time>",
        &text[..text.len().min(16)]
    )
}

/// The colour of a job's status, and the word for it.
fn job_status(job: &JobRecord, lang: Lang) -> String {
    let status = crate::jobs::status_of(job);
    let (tone, word) = match status {
        "ok" => ("ok", "ok"),
        "running" => ("busy", lang.t("running", "rodando")),
        "interrupted" => ("warn", lang.t("interrupted", "interrompida")),
        "timeout" => ("bad", lang.t("timeout", "tempo esgotado")),
        "killed" if job.oom_killed => ("bad", lang.t("out of memory", "sem memória")),
        "killed" => ("bad", lang.t("killed", "encerrada")),
        _ => ("bad", lang.t("failed", "falhou")),
    };
    format!("<span class=\"pill tone-{tone}\">{word}</span>")
}

/// A log's size in the unit that keeps it readable: 812 B, 14 KiB, 2.1 MiB.
fn log_size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{} KiB", b.div_ceil(1024)),
        b => format!("{:.1} MiB", b as f64 / 1048576.0),
    }
}

/// `registry.example/team/worker:tag` as `worker:tag`; the whole name is
/// in the title.
fn short_image(image: &str) -> String {
    let short = image.rsplit('/').next().unwrap_or(image);
    format!("<span title=\"{}\">{}</span>", escape(image), escape(short))
}

/// A table of jobs, newest first, with the links to each one's log.
/// One row of [`jobs_table`]: the job, when the history still has it, and
/// its log, when there is one.
pub(crate) struct JobRow<'a> {
    pub id: String,
    pub job: Option<&'a JobRecord>,
    pub log_bytes: Option<u64>,
    pub log_written_at: Option<i64>,
}

fn jobs_table(rows: &[JobRow], lang: Lang) -> String {
    let (l_status, l_image, l_started, l_duration, l_exit, l_log) = (
        lang.t("Status", "Status"),
        lang.t("Image", "Imagem"),
        lang.t("Started", "Início"),
        lang.t("Duration", "Duração"),
        lang.t("Exit", "Saída"),
        lang.t("Log", "Log"),
    );
    let l_used = lang.t("CPU / memory", "CPU / memória");
    let body: String = rows
        .iter()
        .map(|row| {
            let (job, id) = (row.job, &row.id);
            let links = match row.log_bytes {
                Some(bytes) => format!(
                    "<a href=\"/local/logs/{id}\">{open}</a> &middot; \
                     <a href=\"/local/logs/{id}?download=1\" download>{save}</a> <small>{size}</small>",
                    id = escape(id),
                    open = lang.t("open", "abrir"),
                    save = lang.t("download", "baixar"),
                    size = log_size(bytes),
                ),
                None => "<span class=\"muted\">&ndash;</span>".to_string(),
            };
            match job {
                Some(job) => format!(
                    "<tr><td data-label=\"{l_status}\">{status}</td><td data-label=\"{l_image}\"><span>{image}<br><small><code>{short}</code></small></span></td>\
                     <td data-label=\"{l_started}\">{started}</td><td class=\"num\" data-label=\"{l_duration}\">{duration}</td>\
                     <td class=\"num\" data-label=\"{l_used}\">{used}</td>\
                     <td class=\"num\" data-label=\"{l_exit}\">{exit}</td><td data-label=\"{l_log}\"><span>{links}</span></td></tr>",
                    status = job_status(job, lang),
                    used = used_cell(job),
                    image = short_image(&job.image),
                    short = escape(&id[..id.len().min(12)]),
                    started = when(job.started_at),
                    duration = job
                        .duration_secs()
                        .map(ago)
                        .unwrap_or_else(|| "&ndash;".to_string()),
                    exit = job
                        .exit_code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "&ndash;".to_string()),
                ),
                None => format!(
                    "<tr><td data-label=\"{l_status}\"><span class=\"pill tone-warn\">?</span></td>\
                     <td data-label=\"{l_image}\"><small><code>{short}</code></small></td>\
                     <td data-label=\"{l_started}\">{written}</td><td class=\"num\" data-label=\"{l_duration}\">&ndash;</td>\
                     <td class=\"num\" data-label=\"{l_used}\">&ndash;</td>\
                     <td class=\"num\" data-label=\"{l_exit}\">&ndash;</td><td data-label=\"{l_log}\"><span>{links}</span></td></tr>",
                    short = escape(&id[..id.len().min(12)]),
                    written = row.log_written_at.map(when).unwrap_or_default(),
                ),
            }
        })
        .collect();
    format!(
        "<div class=\"scroll\"><table class=\"jobs\"><tr><th>{l_status}</th><th>{l_image}</th><th>{l_started}</th>\
         <th class=\"num\">{l_duration}</th><th class=\"num\">{l_used}</th><th class=\"num\">{l_exit}</th><th>{l_log}</th></tr>{body}</table></div>"
    )
}

/// `5.5 s / 300 MiB`: the CPU time a job used and the most memory it held,
/// from readings every 30 seconds. A dash for what was not read.
fn used_cell(job: &JobRecord) -> String {
    if job.cpu_ms.is_none() && job.memory_peak.is_none() {
        return "&ndash;".to_string();
    }
    format!(
        "{} / {}",
        job.cpu_ms
            .map(|ms| format!("{:.1} s", ms as f64 / 1000.0))
            .unwrap_or_else(|| "&ndash;".into()),
        job.memory_peak
            .map(|bytes| escape(&crate::jobs::memory_text(bytes)))
            .unwrap_or_else(|| "&ndash;".into())
    )
}

/// Everything around a page's body: one language, and a link to the other.
fn shell(lang: Lang, here: &str, refresh: Option<(u32, &str)>, body: &str) -> String {
    let refresh = refresh
        .map(|(secs, url)| format!("<meta http-equiv=\"refresh\" content=\"{secs};url={url}\">"))
        .unwrap_or_default();
    let (other, other_name) = match lang {
        Lang::En => ("pt", "Português"),
        Lang::Pt => ("en", "English"),
    };
    let nav: String = [
        ("/", lang.t("Status", "Status")),
        ("/local/logs", lang.t("Jobs and logs", "Tarefas e logs")),
        ("/local/safety", lang.t("Safety", "Segurança")),
        ("/local/managed", lang.t("Managed", "Gerenciados")),
        // What to expect, in short answers. The one page that is not here.
        (
            lang.t(
                "https://github.com/nailtongomes/shutup-and-run/blob/main/docs/FAQ.md",
                "https://github.com/nailtongomes/shutup-and-run/blob/main/docs/FAQ.pt-BR.md",
            ),
            lang.t("Questions", "Dúvidas"),
        ),
    ]
    .iter()
    .map(|(path, name)| {
        let current = if *path == here || (here.starts_with(path) && *path != "/") {
            " aria-current=\"page\""
        } else {
            ""
        };
        format!("<a{current} href=\"{path}\">{name}</a>")
    })
    .collect();
    format!(
        "<!doctype html><html lang=\"{code}\"><head><meta charset=\"utf-8\">{refresh}\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>shutup-and-run</title><style>{css}</style></head><body>\
         <header class=\"bar\"><a class=\"brand\" href=\"/\">shutup-and-run<span>{version}</span></a>\
         <nav aria-label=\"{t_nav}\">{nav}</nav>\
         <a lang=\"{other_code}\" href=\"/local/lang?set={other}&amp;to={here}\">{other_name}</a></header>\
         <main>{body}</main><script>{js}</script></body></html>",
        code = lang.t("en", "pt-BR"),
        other_code = lang.t("pt-BR", "en"),
        version = env!("CARGO_PKG_VERSION"),
        css = PAGE_CSS,
        js = LOCAL_TIME_JS,
        t_nav = lang.t("Pages", "Páginas"),
    )
}

pub(crate) fn render(view: &View) -> String {
    use crate::safety::Admission;
    let lang = view.lang;
    let (used, total) = match view.slots {
        Some((used, total)) => (used, total),
        None => (0, 0),
    };
    let admission = view.safety.filter(|s| s.applies).map(|s| s.admission);
    let paused_until = view
        .paused_until
        .map(|until| crate::lending::hour_at(view.computer.and_then(|c| c.host), until));
    // One word for the whole state, its colour, and one sentence under it.
    let (state, tone, sub) = match worker_state(view.slots.is_none(), view.draining, used, false) {
        "failed" => (
            lang.t("DOCKER IS DOWN", "DOCKER PAROU"),
            "bad",
            lang.t(
                "Docker is not answering, so no job can start.",
                "O Docker não está respondendo, então nenhuma tarefa pode começar.",
            )
            .to_string(),
        ),
        "draining" => (
            lang.t("PAUSED", "PAUSADO"),
            "warn",
            match &paused_until {
                Some(at) => format!(
                    "{} {at}. {}",
                    lang.t("No new job starts until", "Nenhuma tarefa nova começa até"),
                    lang.t(
                        "Then it takes jobs again by itself. A job already running finishes.",
                        "Depois ele volta a aceitar sozinho. Uma tarefa que já roda termina.",
                    )
                ),
                None => lang
                    .t(
                        "No new job starts until you resume. A job already running finishes.",
                        "Nenhuma tarefa nova começa até você retomar. Uma tarefa que já roda termina.",
                    )
                    .to_string(),
            },
        ),
        _ => match admission {
            Some(Admission::AuthorizationRequired) => (
                lang.t(
                    "WAITING FOR YOUR AUTHORIZATION",
                    "ESPERANDO A SUA AUTORIZAÇÃO",
                ),
                "warn",
                lang.t(
                    "No job starts until the owner of this computer decides.",
                    "Nenhuma tarefa começa até o dono deste computador decidir.",
                )
                .to_string(),
            ),
            Some(Admission::Blocked) => (
                lang.t(
                    "NEW JOBS BLOCKED FOR SAFETY",
                    "TAREFAS NOVAS BLOQUEADAS POR SEGURANÇA",
                ),
                "bad",
                lang.t(
                    "To protect this computer. It clears by itself.",
                    "Para proteger este computador. Passa sozinho.",
                )
                .to_string(),
            ),
            _ if used > 0 => (
                lang.t("RUNNING", "RODANDO"),
                "busy",
                format!(
                    "{} {used} / {total}",
                    lang.t("Working now: jobs", "Trabalhando agora: tarefas")
                ),
            ),
            _ => match view.computer.and_then(|c| c.hold.as_ref()) {
                // Outside lend_hours, a low battery, a presentation: it
                // clears by itself and nobody has to do anything.
                Some(hold) => (
                    lang.t("WAITING", "ESPERANDO"),
                    "warn",
                    format!(
                        "{}. {}",
                        crate::lending::label(&hold.code, lang == Lang::Pt),
                        lang.t(
                            "New jobs start again by themselves.",
                            "As tarefas novas voltam sozinhas."
                        )
                    ),
                ),
                None => (
                    lang.t("RUNNING", "RODANDO"),
                    "ok",
                    lang.t(
                        "Idle and ready: no job is running.",
                        "Livre e pronto: nenhuma tarefa rodando.",
                    )
                    .to_string(),
                ),
            },
        },
    };
    let managed_on = view.managed.is_some_and(|(on, _, _)| on);
    let poll_line = match (view.polls, view.last_poll) {
        _ if managed_on => String::new(),
        (false, _) => lang.t("no poll URL", "sem URL de poll").to_string(),
        (true, None) => lang
            .t("not asked for work yet", "ainda não pediu trabalho")
            .to_string(),
        (true, Some((at, how))) => format!(
            "{} {} &middot; {}",
            lang.t("last poll", "último poll"),
            when(at),
            match how {
                "task" => lang.t("got a job", "recebeu tarefa"),
                "nothing" => lang.t("no job waiting", "nenhuma tarefa esperando"),
                "task_rejected" => lang.t("refused a task", "recusou uma tarefa"),
                "not_approved" => lang.t("machine not approved yet", "máquina ainda não aprovada"),
                _ => lang.t("error", "erro"),
            }
        ),
    };
    let meta = [
        format!("{} ({})", escape(view.worker), escape(view.os)),
        poll_line,
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" &middot; ");
    let hero = format!(
        "<section class=\"hero tone-{tone}\"><h1 class=\"state\">{state}</h1>\
         <p class=\"sub\">{sub}</p><p class=\"meta\">{meta}</p></section>"
    );

    // What needs a person, first, each with its one next step.
    let mut todo: Vec<String> = Vec::new();
    let callout = |tone: &str, title: &str, body: String| {
        format!("<section class=\"callout tone-{tone}\"><h2>{title}</h2>{body}</section>")
    };
    if view.slots.is_none() {
        todo.push(callout(
            "bad",
            lang.t("Docker is not answering", "O Docker não está respondendo"),
            format!(
                "<p>{} <code>shutup-and-run doctor</code></p>",
                lang.t(
                    "In a terminal on this computer, run this; each line that failed says what to do:",
                    "Num terminal deste computador, rode isto; cada linha que falhou diz o que fazer:",
                )
            ),
        ));
    }
    if let Some(report) = view.safety.filter(|s| s.applies) {
        match report.admission {
            Admission::AuthorizationRequired => todo.push(callout(
                "warn",
                lang.t("Your authorization is needed", "A sua autorização é necessária"),
                format!(
                    "<p>{}</p><ul>{}</ul><p><a class=\"button primary\" href=\"/local/safety\">{}</a></p>",
                    lang.t(
                        "No job starts until you decide. Running jobs on this computer now has a risk:",
                        "Nenhuma tarefa começa até você decidir. Rodar tarefas neste computador agora tem um risco:",
                    ),
                    findings_list(&report.unauthorized(), lang),
                    lang.t("Read the risks and decide", "Ler os riscos e decidir"),
                ),
            )),
            Admission::Blocked => todo.push(callout(
                "bad",
                lang.t("New jobs are blocked", "Tarefas novas estão bloqueadas"),
                format!(
                    "<p>{}</p><ul>{}</ul><p><a href=\"/local/safety\">{}</a></p>",
                    lang.t(
                        "No new job starts now, to protect this computer. It clears by itself:",
                        "Nenhuma tarefa nova começa agora, para proteger este computador. Passa sozinho:",
                    ),
                    findings_list(&report.blocked(), lang),
                    lang.t("Safety of this computer", "Segurança deste computador"),
                ),
            )),
            Admission::Open => {}
        }
    }
    if let Some(percent) = crate::lending::on_battery(view.computer.and_then(|c| c.host)) {
        todo.push(callout(
            "warn",
            lang.t("This PC is on its battery", "Este PC está na bateria"),
            format!(
                "<p>{percent}%. {}</p>",
                lang.t(
                    "Jobs still start, and use the battery. Below 60% no new job starts until the PC is plugged in.",
                    "As tarefas ainda começam, e gastam a bateria. Abaixo de 60% nenhuma tarefa nova começa até o PC ir para a tomada.",
                )
            ),
        ));
    }
    if let Some(need) = view.memory.and_then(|m| m.need) {
        let amount = crate::resources::human(need.to_free_bytes);
        let body = match need.place {
            "wsl2" => format!(
                "<p>{} {amount}. {}</p>",
                lang.t(
                    "WSL2, where the jobs run, needs about",
                    "O WSL2, onde as tarefas rodam, precisa de mais",
                ),
                lang.t(
                    "What a running job holds is freed when it ends; closing programs on Windows does not help here.",
                    "O que uma tarefa em execução ocupa é liberado quando ela termina; fechar programas no Windows não ajuda aqui.",
                )
            ),
            _ => format!(
                "<p>{} {amount} {}</p>{}",
                lang.t(
                    "For the next job to start, free about",
                    "Para a próxima tarefa começar, libere cerca de",
                ),
                lang.t(
                    "of memory: close a program you do not need now. Or just wait: jobs start again by themselves.",
                    "de memória: feche um programa que não precisa agora. Ou só espere: as tarefas voltam sozinhas.",
                ),
                if view.programs.is_empty() {
                    String::new()
                } else {
                    format!(
                        "<p>{} {}</p>",
                        lang.t("Using the most memory:", "Usando mais memória:"),
                        escape(&crate::memory::programs_in(
                view.programs,
                lang.t("processes", "processos"),
            ))
                    )
                }
            ),
        };
        todo.push(callout(
            "warn",
            lang.t("Memory is short", "Falta memória"),
            body,
        ));
    }
    if view.draining {
        todo.push(callout(
            "warn",
            lang.t("The worker is paused", "O worker está pausado"),
            format!(
                "<p>{}</p><form method=\"post\" action=\"/local/resume\"><p>\
                 <button class=\"primary\">{}</button></p></form>",
                match &paused_until {
                    Some(at) => format!(
                        "{} {at}.",
                        lang.t(
                            "It takes no new job, and takes them again by itself at",
                            "Ele não pega tarefa nova, e volta a aceitar sozinho às",
                        )
                    ),
                    None => lang
                        .t(
                            "It takes no new job. A pause ends when the PC restarts.",
                            "Ele não pega tarefa nova. A pausa termina quando o PC reinicia.",
                        )
                        .to_string(),
                },
                lang.t("Resume taking jobs", "Voltar a aceitar tarefas"),
            ),
        ));
    }
    for (name, missing) in &view.needs_secret {
        todo.push(callout(
            "warn",
            lang.t("A managed container needs a secret", "Um container gerenciado precisa de um segredo"),
            format!(
                "<p><b>{name}</b>: {} <code>{missing}</code></p><p>{}</p>\
                 <p><a class=\"button primary\" href=\"/local/managed/edit?name={name}\">{}</a></p>",
                lang.t("no value for", "sem valor para"),
                lang.t(
                    "It does not start until each secret has a value. The value is typed once and never shown again.",
                    "Ele não começa até cada segredo ter um valor. O valor é digitado uma vez e nunca mais aparece.",
                ),
                lang.t("Give it the value", "Informar o valor"),
                name = escape(name),
                missing = escape(missing),
            ),
        ));
    }
    if let Some(warning) = view.warning {
        todo.push(callout(
            "bad",
            lang.t(
                "The config file has a problem",
                "O arquivo de configuração tem um problema",
            ),
            format!("<p>{}</p>", escape(warning)),
        ));
    }
    if let (true, false, Some(error)) = (view.polls, managed_on, view.poll_error) {
        todo.push(callout(
            "warn",
            lang.t("The poll URL did not answer well", "A URL de poll não respondeu bem"),
            format!(
                "<p><span class=\"bad\">{}</span></p><p>{}</p>",
                escape(error),
                lang.t(
                    "The worker keeps trying by itself. If this stays, check the poll URL in the settings or with whoever runs it.",
                    "O worker continua tentando sozinho. Se isso continuar, confira a URL de poll na configuração ou com quem a mantém.",
                ),
            ),
        ));
    }

    let off = match view.slots {
        Some((0, _)) => lang.t("YES, the PC can be turned off.", "SIM, pode desligar o PC."),
        Some(_) => lang.t(
            "NOT now: a job is running.",
            "NÃO agora: há tarefa rodando.",
        ),
        None => "?",
    };
    let slots = match view.slots {
        Some((used, total)) => format!("{used} / {total}"),
        None => "?".to_string(),
    };
    let last = match view.last {
        Some(job) => format!(
            "{} {} <small>{} {}</small>",
            job_status(job, lang),
            short_image(&job.image),
            lang.t("started", "início"),
            when(job.started_at),
        ),
        None => lang.t("none yet", "nenhuma ainda").to_string(),
    };
    let queue = match (view.polls, view.poll_error) {
        _ if managed_on => lang
            .t(
                "off: managed mode takes no job from outside",
                "desligada: o modo gerenciado não pega tarefa de fora",
            )
            .to_string(),
        (false, _) => lang
            .t(
                "no poll URL: nothing to do",
                "sem URL de poll: nada a fazer",
            )
            .to_string(),
        (true, None) => lang.t("answering", "respondendo").to_string(),
        (true, Some(_)) => format!(
            "<span class=\"bad\">{}</span>",
            lang.t("not answering well", "não está respondendo bem")
        ),
    };
    let managed = match view.managed {
        None => String::new(),
        Some((on, running, total)) => format!(
            "<div><dt><a href=\"/local/managed\">{}</a></dt><dd>{}</dd></div>",
            lang.t("Managed containers", "Containers gerenciados"),
            if on {
                format!(
                    "{} &middot; {running} / {total} {}",
                    lang.t("on", "ligado"),
                    lang.t("running", "rodando"),
                )
            } else {
                format!("{} &middot; {total}", lang.t("off", "desligado"))
            },
        ),
    };
    let fact = |name: &str, value: &str| format!("<div><dt>{name}</dt><dd>{value}</dd></div>");
    let facts = format!(
        "<dl class=\"facts\">{}{}{}{}{managed}{}</dl>",
        fact(lang.t("Turn off?", "Pode desligar?"), off),
        fact(lang.t("Jobs now", "Tarefas agora"), &slots),
        fact(lang.t("Last job", "Última tarefa"), &last),
        fact(lang.t("Job queue", "Fila de tarefas"), &queue),
        fact(
            lang.t("Machine", "Máquina"),
            &format!("{} ({})", escape(view.worker), escape(view.os))
        ),
    );
    let pause = if view.draining {
        String::new()
    } else {
        let mut forms = format!(
            "<form method=\"post\" action=\"/local/pause\"><button>{}</button></form>",
            lang.t("Pause: take no new jobs", "Pausar: não pegar novas tarefas")
        );
        for (span, en, pt) in [
            ("1h", "for 1 hour", "por 1 hora"),
            ("3h", "for 3 hours", "por 3 horas"),
            ("day", "for the rest of the day", "pelo resto do dia"),
        ] {
            forms.push_str(&format!(
                "<form method=\"post\" action=\"/local/pause?for={span}\"><button>{}</button></form>",
                lang.t(en, pt)
            ));
        }
        forms
    };
    let settings = if view.editable {
        format!(
            "<a class=\"button\" href=\"/local/config\">{}</a>",
            lang.t("Settings", "Configuração")
        )
    } else {
        String::new()
    };
    let recent = if view.recent.is_empty() {
        String::new()
    } else {
        let rows: Vec<JobRow> = view
            .recent
            .iter()
            .map(|(job, log_bytes)| JobRow {
                id: job.id.clone(),
                job: Some(job),
                log_bytes: *log_bytes,
                log_written_at: None,
            })
            .collect();
        format!(
            "<h2>{}</h2>{}<p><a href=\"/local/logs\">{}</a></p>",
            lang.t("Last jobs", "Últimas tarefas"),
            jobs_table(&rows, lang),
            lang.t("All jobs and their logs", "Todas as tarefas e os logs"),
        )
    };
    let body = format!(
        "{hero}{todo}{facts}<div class=\"actions\">{pause}{settings}</div>{recent}{computer}<p class=\"note\">{note}</p>",
        todo = todo.concat(),
        computer = computer_section(view),
        note = lang.t(
            "This page opens only on this computer and reloads every 10 s. A pause with no end ends when the PC restarts. Times are in this computer's time zone.",
            "Esta página só abre neste computador e se atualiza a cada 10 s. Uma pausa sem prazo termina quando o PC reinicia. Os horários estão no fuso deste computador.",
        ),
    );
    shell(lang, "/", Some((10, "/")), &body)
}

/// "Computer resources": what the machine has, what a job may take, and
/// whether a new job could start. Readings only: a figure that was not read
/// is left out or shown as not reported, never guessed.
fn computer_section(view: &View) -> String {
    use crate::resources::human;
    let (Some(computer), Some(limits)) = (view.computer, view.limits) else {
        return String::new();
    };
    let lang = view.lang;
    let windows = view.os == "windows";
    let mut rows: Vec<(&str, String)> = Vec::new();
    rows.push((
        lang.t("Operating system", "Sistema operacional"),
        if windows {
            lang.t(
                "Windows (jobs run inside WSL2)",
                "Windows (as tarefas rodam dentro do WSL2)",
            )
            .to_string()
        } else {
            escape(view.os)
        },
    ));
    let machine = |cpus: Option<u64>, total: Option<u64>, available: Option<u64>| {
        let mut parts = Vec::new();
        if let Some(cpus) = cpus {
            parts.push(format!("{cpus} {}", lang.t("processors", "processadores")));
        }
        if let Some(total) = total {
            parts.push(format!(
                "{} {}",
                human(total),
                lang.t("of memory", "de memória")
            ));
        }
        if let Some(available) = available {
            parts.push(format!(
                "{} {}",
                human(available),
                lang.t("available", "disponível")
            ));
        }
        if parts.is_empty() {
            lang.t("not reported", "não informado").to_string()
        } else {
            parts.join(", ")
        }
    };
    let inner = machine(
        computer.cpus,
        computer.memory_total,
        computer.memory_available,
    );
    if windows {
        let host = computer.host.unwrap_or_default();
        rows.push((
            lang.t("This PC (Windows)", "Este PC (Windows)"),
            machine(host.cpus, host.memory_total, host.memory_available),
        ));
        rows.push((
            lang.t(
                "WSL2 (its limit, and what is free)",
                "WSL2 (o limite dele, e o que está livre)",
            ),
            inner,
        ));
    } else {
        rows.push((lang.t("This machine", "Esta máquina"), inner));
    }
    let no_limit = lang.t("no limit", "sem limite");
    rows.push((
        lang.t("Limit of each job", "Limite de cada tarefa"),
        format!(
            "CPU: {}, {}: {}",
            limits
                .max_job_cpus
                .map(|c| c.to_string())
                .unwrap_or_else(|| no_limit.to_string()),
            lang.t("memory", "memória"),
            limits
                .max_job_memory_bytes
                .map(human)
                .unwrap_or_else(|| no_limit.to_string()),
        ),
    ));
    rows.push((
        lang.t("Jobs running / at most", "Tarefas rodando / máximo"),
        match view.slots {
            Some((used, total)) => format!("{used} / {total}"),
            None => "?".to_string(),
        },
    ));
    if windows {
        let idle = match computer.host.and_then(|host| host.idle_secs) {
            Some(secs) => ago(secs as i64),
            None => lang.t("not reported", "não informado").to_string(),
        };
        let rule = match computer.min_idle_secs {
            0 => lang
                .t("jobs start at any time", "tarefas começam a qualquer hora")
                .to_string(),
            secs => format!(
                "{} {}",
                lang.t("a job starts after", "uma tarefa começa depois de"),
                ago(secs as i64)
            ),
        };
        rows.push((
            lang.t("PC without use for", "PC sem uso há"),
            format!("{idle} ({rule})"),
        ));
        let host = computer.host.unwrap_or_default();
        rows.push((
            lang.t("Power", "Energia"),
            match (host.on_battery, host.battery_percent) {
                (Some(true), Some(percent)) => format!(
                    "{} {percent}% ({})",
                    lang.t("on the battery,", "na bateria,"),
                    lang.t(
                        "below 60% no new job starts",
                        "abaixo de 60% nenhuma tarefa nova começa"
                    )
                ),
                (Some(true), None) => lang.t("on the battery", "na bateria").to_string(),
                (Some(false), _) => lang.t("plugged in", "na tomada").to_string(),
                (None, _) => lang.t("not reported", "não informado").to_string(),
            },
        ));
    }
    rows.push((
        lang.t("Lends itself", "Empresta"),
        match &computer.lend_hours {
            None => lang.t("at any time", "a qualquer hora").to_string(),
            Some(hours) => format!(
                "<code>{}</code> &middot; {} {}",
                escape(hours),
                lang.t("now", "agora"),
                escape(&computer.clock)
            ),
        },
    ));
    if !view.programs.is_empty() {
        rows.push((
            lang.t("Using the most memory", "Usando mais memória"),
            escape(&crate::memory::programs_in(
                view.programs,
                lang.t("processes", "processos"),
            )),
        ));
    }
    if let Some(memory) = view.memory.filter(|m| m.waited_week_secs > 0) {
        rows.push((
            lang.t("Waited for memory", "Esperou por memória"),
            format!(
                "{} {} &middot; {} {}",
                lang.t("today", "hoje"),
                crate::memory::duration_text(memory.waited_today_secs),
                lang.t("7 days", "7 dias"),
                crate::memory::duration_text(memory.waited_week_secs)
            ),
        ));
    }
    if let Some(hours) = view.memory.and_then(|m| m.short_hours.as_ref()) {
        rows.push((
            lang.t("Memory short, 7 days", "Memória curta, 7 dias"),
            format!("<code>{}</code>", escape(&hours.when)),
        ));
    }
    let admission = match (view.slots, view.draining, view.waiting_for) {
        (None, _, _) => lang
            .t("no: Docker is down", "não: o Docker parou")
            .to_string(),
        (_, true, _) => lang.t("no: paused", "não: pausado").to_string(),
        (Some((used, total)), _, _) if used >= total => lang
            .t(
                "no: busy, every slot is taken",
                "não: ocupado, todas as vagas em uso",
            )
            .to_string(),
        _ if view
            .safety
            .is_some_and(|s| s.admission == crate::safety::Admission::Blocked) =>
        {
            lang.t(
                "no: blocked to protect this computer",
                "não: bloqueado para proteger este computador",
            )
            .to_string()
        }
        _ if view
            .safety
            .is_some_and(|s| s.admission == crate::safety::Admission::AuthorizationRequired) =>
        {
            lang.t(
                "no: waiting for your authorization",
                "não: esperando a sua autorização",
            )
            .to_string()
        }
        (_, _, Some(_)) if computer.user_active => lang
            .t("waiting: the PC is in use", "esperando: o PC está em uso")
            .to_string(),
        (_, _, Some(_)) if computer.hold.is_some() => format!(
            "{}: {}",
            lang.t("waiting", "esperando"),
            computer
                .hold
                .as_ref()
                .map(|h| crate::lending::label(&h.code, lang == Lang::Pt))
                .unwrap_or_default()
        ),
        (_, _, Some(why)) => format!(
            "{}: {}",
            lang.t(
                "waiting, not enough resources",
                "esperando, recursos insuficientes"
            ),
            escape(why)
        ),
        _ => lang.t("yes", "sim").to_string(),
    };
    rows.push((
        lang.t("Can a new job start?", "Pode começar tarefa nova?"),
        admission,
    ));
    let delivery = computer.delivery;
    rows.push((
        lang.t("Interrupted jobs", "Tarefas interrompidas"),
        format!(
            "{} ({} {})",
            delivery.interrupted,
            delivery.interrupted_unnotified,
            lang.t("not notified", "sem aviso entregue")
        ),
    ));
    rows.push((
        lang.t("Callback delivery", "Entrega de callbacks"),
        format!(
            "{} {}, {} {}",
            delivery.pending,
            lang.t("waiting for a new try", "aguardando nova tentativa"),
            delivery.failed,
            lang.t("given up", "desistidos")
        ),
    ));
    let rows: String = rows
        .iter()
        .map(|(name, value)| format!("<tr><td>{name}</td><td>{value}</td></tr>"))
        .collect();
    format!(
        "<h2>{}</h2><table class=\"kv\">{rows}</table>",
        lang.t("Computer resources", "Recursos do computador")
    )
}

// ---------------------------------------------------------------------------
// The config editor. config.json is the source; these pages read it and
// write it through the same validation as the CLI.

/// A key's name on the page, and one short line of help under its field.
fn field_text(name: &str, lang: Lang) -> (&'static str, &'static str) {
    match name {
        "poll_url" => (
            lang.t("Poll URL", "URL de poll"),
            lang.t(
                "Where this computer asks for work, with its ?user= filter. The values of the filter are hidden here and kept when saved unchanged.",
                "Onde este computador pede trabalho, com o filtro ?user=. Os valores do filtro ficam ocultos aqui e são mantidos se salvos sem mudança.",
            ),
        ),
        "poll_interval_secs" => (
            lang.t("Poll interval, seconds", "Intervalo de poll, segundos"),
            lang.t("How often it asks for work: 30 or more.", "De quanto em quanto tempo pede trabalho: 30 ou mais."),
        ),
        "worker_name" => (
            lang.t("Machine name", "Nome da máquina"),
            lang.t("How this computer is named to the poll URL.", "Como este computador é chamado na URL de poll."),
        ),
        "max_slots" => (
            lang.t("Containers at once", "Containers ao mesmo tempo"),
            lang.t("How many jobs may run at the same time.", "Quantas tarefas podem rodar ao mesmo tempo."),
        ),
        "min_idle_secs" => (
            lang.t("Wait for the PC to be idle, seconds", "Esperar o PC ficar sem uso, segundos"),
            lang.t(
                "Windows: start a job only after nobody used the PC for this long. 0: at any time.",
                "Windows: só começar uma tarefa depois de o PC ficar sem uso por esse tempo. 0: a qualquer hora.",
            ),
        ),
        "lend_hours" => (
            lang.t("Lend only at these hours", "Emprestar só nestes horários"),
            lang.t(
                "Local time, like mon-fri 19-07; sat,sun. Outside them no new job starts. Empty: at any time.",
                "Hora local, como mon-fri 19-07; sat,sun. Fora deles nenhuma tarefa nova começa. Vazio: a qualquer hora.",
            ),
        ),
        "allowlist" => (
            lang.t("Allowed images", "Imagens permitidas"),
            lang.t("One per line. A job with any other image is refused.", "Uma por linha. Uma tarefa com outra imagem é recusada."),
        ),
        "poll_any_image" => (
            lang.t("Run any image the poll URL sends", "Rodar qualquer imagem que a URL de poll mandar"),
            lang.t(
                "Skips the list above for jobs from the poll URL. Only for a poll URL you trust.",
                "Ignora a lista acima para tarefas da URL de poll. Só para uma URL de poll em que você confia.",
            ),
        ),
        "default_timeout_secs" => (
            lang.t("Job time limit, seconds", "Tempo limite da tarefa, segundos"),
            lang.t("Used when a job does not say how long it may run.", "Usado quando a tarefa não diz quanto tempo pode rodar."),
        ),
        "max_timeout_secs" => (
            lang.t("Longest time limit, seconds", "Maior tempo limite, segundos"),
            lang.t("The most a job may ask for.", "O máximo que uma tarefa pode pedir."),
        ),
        "resource_profile" => (
            lang.t("Resource profile", "Perfil de recursos"),
            lang.t(
                "shared-pc protects the person using this computer; none gives jobs the whole machine.",
                "shared-pc protege quem usa este computador; none dá a máquina inteira às tarefas.",
            ),
        ),
        "max_job_cpus" => (
            lang.t("CPUs per job", "CPUs por tarefa"),
            lang.t("Blank: the profile's default, shown in grey.", "Vazio: o padrão do perfil, em cinza."),
        ),
        "max_job_memory" => (
            lang.t("Memory per job", "Memória por tarefa"),
            lang.t("Like 2g or 512m.", "Como 2g ou 512m."),
        ),
        "min_free_memory" => (
            lang.t("Free memory a new job needs", "Memória livre que uma tarefa nova exige"),
            lang.t("Like 2g. 0: not checked.", "Como 2g. 0: não confere."),
        ),
        "min_free_disk" => (
            lang.t("Free disk a new job needs", "Disco livre que uma tarefa nova exige"),
            lang.t("Like 5g. 0: not checked.", "Como 5g. 0: não confere."),
        ),
        "token" => (
            lang.t("API token", "Token da API"),
            lang.t("Blank keeps the one in use.", "Vazio mantém o atual."),
        ),
        "poll_token" => (
            lang.t("Poll token", "Token de poll"),
            lang.t(
                "Who this machine is to the poll URL. Blank keeps it.",
                "Quem esta máquina é para a URL de poll. Vazio mantém.",
            ),
        ),
        "admin_user" => (lang.t("Admin user", "Usuário admin"), ""),
        "admin_password" => (
            lang.t("Admin password", "Senha admin"),
            lang.t("Blank keeps it.", "Vazio mantém."),
        ),
        _ => ("", ""),
    }
}

/// The sections of the settings forms, in order. A key in none of them
/// goes in the last one, so nothing a group has is left out.
fn field_groups(lang: Lang) -> [(&'static str, &'static [&'static str]); 6] {
    [
        (
            lang.t("Where the work comes from", "De onde vem o trabalho"),
            &["poll_url", "poll_interval_secs", "worker_name"],
        ),
        (
            lang.t(
                "How much this computer lends",
                "Quanto este computador empresta",
            ),
            &["max_slots", "min_idle_secs", "lend_hours"],
        ),
        (
            lang.t("Which images may run", "Quais imagens podem rodar"),
            &["allowlist", "poll_any_image"],
        ),
        (
            lang.t("Time limits", "Limites de tempo"),
            &["default_timeout_secs", "max_timeout_secs"],
        ),
        (
            lang.t("Resources of each job", "Recursos de cada tarefa"),
            &[
                "resource_profile",
                "max_job_cpus",
                "max_job_memory",
                "min_free_memory",
                "min_free_disk",
            ],
        ),
        (
            lang.t("Tokens and the admin login", "Tokens e o login admin"),
            &["token", "poll_token", "admin_user", "admin_password"],
        ),
    ]
}

/// The form for the keys of one group, filled from the file.
/// What runs for a key the file does not set, as the grey text of its field.
pub(crate) fn in_force(name: &str, settings: &config::Settings) -> String {
    let size = |bytes: u64| match bytes {
        0 => "0".to_string(),
        bytes => format!("{}m", bytes / (1024 * 1024)),
    };
    match name {
        "poll_interval_secs" => settings.poll_interval.as_secs().to_string(),
        "resource_profile" => Limits::of(settings).profile,
        "max_job_cpus" => settings
            .max_job_cpus
            .map(|c| c.to_string())
            .unwrap_or_else(|| "-".to_string()),
        "max_job_memory" => settings
            .max_job_memory
            .map(size)
            .unwrap_or_else(|| "-".to_string()),
        "min_free_memory" => size(settings.min_free_memory),
        "min_free_disk" => size(settings.min_free_disk),
        _ => String::new(),
    }
}

fn form(map: &JsonMap, group: OnPage, action: &str, lang: Lang) -> String {
    let running = config::Settings::from_json_map(map).ok();
    let locked = group == OnPage::Locked;
    let field = |key: &config::Key| -> String {
        let name = key.name;
        let (text, help) = field_text(name, lang);
        let value = map.get(name);
        let tag = if locked {
            format!(
                "<span class=\"tag\">{}</span>",
                lang.t("protected", "protegido")
            )
        } else {
            String::new()
        };
        let help = if help.is_empty() {
            String::new()
        } else {
            format!("<p class=\"help\" id=\"{name}-help\">{help}</p>")
        };
        let control = match key.kind {
            Kind::Bool => format!(
                "<label class=\"check\"><input type=\"checkbox\" name=\"{name}\"{}> <span>{text}{tag}</span></label>",
                if value.and_then(|v| v.as_bool()).unwrap_or(false) {
                    " checked"
                } else {
                    ""
                }
            ),
            Kind::List => format!(
                "<label for=\"{name}\">{text}{tag}</label><textarea id=\"{name}\" name=\"{name}\" rows=\"4\" aria-describedby=\"{name}-help\">{}</textarea>",
                escape(
                    &value
                        .and_then(|v| v.as_array())
                        .map(|list| list
                            .iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join("\n"))
                        .unwrap_or_default()
                )
            ),
            _ if key.secret => format!(
                "<label for=\"{name}\">{text}{tag}</label><input type=\"password\" id=\"{name}\" name=\"{name}\" autocomplete=\"new-password\" aria-describedby=\"{name}-help\">"
            ),
            kind => format!(
                "<label for=\"{name}\">{text}{tag}</label><input type=\"{}\" id=\"{name}\" name=\"{name}\" placeholder=\"{}\"{} value=\"{}\" aria-describedby=\"{name}-help\">",
                if kind == Kind::Int { "number" } else { "text" },
                escape(&running.as_ref().map(|s| in_force(name, s)).unwrap_or_default()),
                if name == "poll_interval_secs" {
                    format!(" min=\"{}\" max=\"3600\"", config::MIN_POLL_INTERVAL_SECS)
                } else {
                    String::new()
                },
                escape(&match value {
                    // The query pairs the machine with a person: masked.
                    // Saving it back unchanged keeps the URL (save_fields).
                    Some(serde_json::Value::String(s)) if name == "poll_url" => {
                        config::mask_query(s)
                    }
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                })
            ),
        };
        format!(
            "<div class=\"field{}\">{control}{help}</div>",
            if locked { " locked" } else { "" }
        )
    };
    let keys: Vec<&config::Key> = config::KEYS.iter().filter(|k| k.page == group).collect();
    let mut placed: Vec<&str> = Vec::new();
    let mut sections = String::new();
    for (title, names) in field_groups(lang) {
        let fields: String = names
            .iter()
            .filter_map(|n| keys.iter().find(|k| k.name == *n))
            .map(|k| {
                placed.push(k.name);
                field(k)
            })
            .collect();
        if !fields.is_empty() {
            sections += &format!("<fieldset><legend>{title}</legend>{fields}</fieldset>");
        }
    }
    let rest: String = keys
        .iter()
        .filter(|k| !placed.contains(&k.name))
        .map(|k| field(k))
        .collect();
    if !rest.is_empty() {
        sections += &format!(
            "<fieldset><legend>{}</legend>{rest}</fieldset>",
            lang.t("Other", "Outros")
        );
    }
    format!(
        "<form method=\"post\" action=\"{action}\">{sections}<p><button class=\"primary\">{}</button></p></form>",
        lang.t("Save and apply", "Salvar e aplicar")
    )
}

fn editor(path: &Path, group: OnPage, lang: Lang, error: Option<&str>) -> String {
    let (here, title, other) = match group {
        OnPage::Locked => (
            "/local/advanced",
            lang.t("Advanced settings", "Configuração avançada"),
            format!(
                "<a href=\"/local/config\">{}</a>",
                lang.t("Everyday settings", "Configuração do dia a dia")
            ),
        ),
        _ => (
            "/local/config",
            lang.t("Settings", "Configuração"),
            format!(
                "<a href=\"/local/advanced\">{}</a>",
                lang.t(
                    "Advanced settings (asks for the admin user and password)",
                    "Configuração avançada (pede o usuário e a senha admin)"
                )
            ),
        ),
    };
    let error = error
        .map(|e| {
            format!(
                "<section class=\"callout tone-bad\"><h2>{}</h2><p>{}</p></section>",
                lang.t("Not saved.", "Não foi salvo."),
                escape(e)
            )
        })
        .unwrap_or_default();
    let intro = if group == OnPage::Locked {
        format!(
            "<section class=\"callout tone-warn\"><p>{}</p></section>",
            lang.t(
                "These settings decide what this computer accepts and how much of it a job takes. A wrong value can let in an image nobody chose, or slow the PC down. Change one only when you know why.",
                "Estas configurações decidem o que este computador aceita e quanto dele uma tarefa usa. Um valor errado pode deixar entrar uma imagem que ninguém escolheu, ou deixar o PC lento. Mude só quando souber o porquê.",
            )
        )
    } else {
        String::new()
    };
    let body = match config::read_json(path) {
        Ok(map) => {
            let rows: String = config::masked(&map)
                .iter()
                .map(|(k, v)| {
                    format!(
                        "<tr><td><code>{}</code></td><td><code>{}</code></td></tr>",
                        escape(k),
                        escape(&v.to_string())
                    )
                })
                .collect();
            let more = if group == OnPage::Locked {
                format!(
                    "<p><a class=\"button\" href=\"/local/certificate\">{}</a></p>",
                    lang.t(
                        "Certificate (advanced, optional)",
                        "Certificado (avançado, opcional)"
                    )
                )
            } else {
                String::new()
            };
            format!(
                "{}{more}<details><summary>{}</summary><div class=\"scroll\"><table class=\"kv\">{rows}</table></div></details>\
                 <p class=\"note\">{}</p>",
                form(&map, group, here, lang),
                lang.t(
                    "The whole config file, secrets hidden",
                    "O arquivo de configuração inteiro, sem os segredos"
                ),
                lang.t(
                    "Saving restarts the service; running jobs are not stopped. An empty field goes back to its default. The other keys change with <code>shutup-and-run config set KEY VALUE</code>.",
                    "Salvar reinicia o serviço; as tarefas em execução não param. Campo vazio volta ao padrão. As outras chaves mudam com <code>shutup-and-run config set CHAVE VALOR</code>.",
                ),
            )
        }
        Err(err) => format!(
            "<p class=\"bad\">{}</p><p>{}</p>",
            escape(&err.0),
            lang.t(
                "Repair it with <code>shutup-and-run config reset</code>.",
                "Conserte com <code>shutup-and-run config reset</code>.",
            )
        ),
    };
    shell(
        lang,
        here,
        None,
        &format!(
            "<h1>{title}</h1>{error}{intro}{body}<p><a href=\"/\">{}</a> &middot; {other}</p>",
            lang.t("Back to status", "Voltar ao status")
        ),
    )
}

/// Put one group's form fields into the file. Only that group's keys are
/// read from the form, so the open form cannot carry a dangerous key.
pub(crate) fn save_fields(
    path: &Path,
    group: OnPage,
    form: &HashMap<String, String>,
) -> Result<(), String> {
    let mut map = config::read_json(path).map_err(|e| e.0)?;
    for key in config::KEYS.iter().filter(|k| k.page == group) {
        let raw = form.get(key.name).map(|v| v.trim());
        match (key.kind, raw) {
            (Kind::Bool, raw) => {
                map.insert(key.name.to_string(), serde_json::Value::Bool(raw.is_some()));
            }
            (_, None) => {}
            (Kind::List, Some(raw)) => {
                map.insert(
                    key.name.to_string(),
                    config::parse_value(key, raw).map_err(|e| e.0)?,
                );
            }
            // The poll URL is shown with its query masked. Sent back as it
            // was shown, it stays as it is.
            (_, Some(raw))
                if key.name == "poll_url"
                    && map
                        .get("poll_url")
                        .and_then(|v| v.as_str())
                        .map(config::mask_query)
                        == Some(raw.to_string()) => {}
            // Blank keeps a secret, and returns any other key to its default.
            (_, Some("")) if key.secret => {}
            (_, Some("")) => {
                map.remove(key.name);
            }
            (_, Some(raw)) => {
                map.insert(
                    key.name.to_string(),
                    config::parse_value(key, raw).map_err(|e| e.0)?,
                );
            }
        }
    }
    config::save_json(path, &map).map_err(|e| e.0)
}

/// `Authorization: Basic` against the admin user and password. It keeps a
/// curious person out of the dangerous keys. It is not strong and is not
/// meant to be: the person at the machine can read the file.
fn admin(headers: &HeaderMap, launcher: &Launcher) -> bool {
    let settings = launcher.settings();
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
        .and_then(|b64| {
            base64::engine::general_purpose::STANDARD
                .decode(b64.trim())
                .ok()
        })
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .is_some_and(|pair| pair == format!("{}:{}", settings.admin_user, settings.admin_password))
}

fn ask_login() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE,
            "Basic realm=\"shutup-and-run advanced settings\", charset=\"UTF-8\"",
        )],
        "admin user and password needed",
    )
        .into_response()
}

fn not_editable(lang: Lang) -> Response {
    html(
        StatusCode::NOT_FOUND,
        shell(
            lang,
            "/",
            None,
            lang.t(
                "<p>This machine is set up with config.toml. The page edits config.json only.</p>",
                "<p>Esta máquina usa config.toml. A página só edita o config.json.</p>",
            ),
        ),
    )
}

async fn config_page(State(launcher): State<Arc<Launcher>>, headers: HeaderMap) -> Response {
    edit_page(&launcher, &headers, OnPage::Open)
}

async fn advanced_page(State(launcher): State<Arc<Launcher>>, headers: HeaderMap) -> Response {
    edit_page(&launcher, &headers, OnPage::Locked)
}

fn edit_page(launcher: &Launcher, headers: &HeaderMap, group: OnPage) -> Response {
    let lang = lang_of(headers, &launcher.settings().language);
    let Some(path) = &launcher.settings().config_json else {
        return not_editable(lang);
    };
    if group == OnPage::Locked && !admin(headers, launcher) {
        return ask_login();
    }
    html(StatusCode::OK, editor(path, group, lang, None))
}

async fn config_save(
    State(launcher): State<Arc<Launcher>>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    save_page(&launcher, &headers, OnPage::Open, &form)
}

async fn advanced_save(
    State(launcher): State<Arc<Launcher>>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    save_page(&launcher, &headers, OnPage::Locked, &form)
}

fn save_page(
    launcher: &Launcher,
    headers: &HeaderMap,
    group: OnPage,
    form: &HashMap<String, String>,
) -> Response {
    let lang = lang_of(headers, &launcher.settings().language);
    let Some(path) = &launcher.settings().config_json else {
        return not_editable(lang);
    };
    if group == OnPage::Locked && !admin(headers, launcher) {
        return ask_login();
    }
    if let Err(error) = save_fields(path, group, form) {
        return html(
            StatusCode::BAD_REQUEST,
            editor(path, group, lang, Some(&error)),
        );
    }
    // Answer first, then restart: the page below reloads into the new service.
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        request_restart();
    });
    html(
        StatusCode::OK,
        shell(
            lang,
            "/",
            Some((4, "/")),
            lang.t(
                "<section class=\"hero tone-ok\"><h1 class=\"state\">Saved.</h1><p class=\"sub\">The service is restarting on the new settings. This page goes back to the status in a few seconds.</p></section>",
                "<section class=\"hero tone-ok\"><h1 class=\"state\">Salvo.</h1><p class=\"sub\">O serviço está reiniciando com a nova configuração. Esta página volta ao status em alguns segundos.</p></section>",
            ),
        ),
    )
}

// ---------------------------------------------------------------------------
// Certificate passthrough. Behind the admin login, like the other dangerous
// settings, and like them only on the machine itself.

fn risk(lang: Lang) -> &'static str {
    lang.t(
        crate::devx::RISK,
        "Quando ativado, todo container executado por este worker pode acessar o certificado \
         configurado e a senha dele. A montagem somente leitura protege o arquivo original \
         contra alteração, mas não impede copiar, exportar ou usar a chave privada. Ative este \
         recurso apenas se você confia em toda carga de trabalho que pode rodar nesta máquina.",
    )
}

/// The certificate section. `note` is the answer to the last button.
pub(crate) fn certificate_view(path: &Path, lang: Lang, note: Option<(bool, &str)>) -> String {
    let status = crate::devx::certificate_status(path);
    let enabled = status["enabled"] == true;
    let imported = status["imported"] == true;
    let state = match (enabled, imported) {
        (true, _) => lang.t(
            "ON: every container gets the certificate and its password.",
            "ATIVADO: todo container recebe o certificado e a senha.",
        ),
        (false, true) => lang.t(
            "Off. A certificate is imported and is not given to containers.",
            "Desativado. Há um certificado importado, que não é entregue aos containers.",
        ),
        (false, false) => lang.t("Off. No certificate.", "Desativado. Sem certificado."),
    };
    let note = note
        .map(|(ok, text)| {
            format!(
                "<pre{}>{}</pre>",
                if ok { "" } else { " class=\"bad\"" },
                escape(text)
            )
        })
        .unwrap_or_default();
    let name = status["name"]
        .as_str()
        .map(|n| format!(" (<code>{}</code>)", escape(n)))
        .unwrap_or_default();
    let body = format!(
        "<h1>{title}</h1>{note}<section class=\"hero tone-{tone}\"><p class=\"sub\"><b>{state}</b>{name}</p></section>\
         <section class=\"callout tone-bad\"><p>{risk}</p></section>\
         <form method=\"post\" action=\"/local/certificate\" id=\"cert\">\
         <label for=\"file\">{t_file}</label><input type=\"file\" id=\"file\" accept=\".pfx,.p12\">\
         <input type=\"hidden\" name=\"pfx_base64\" id=\"pfx_base64\">\
         <input type=\"hidden\" name=\"file_name\" id=\"file_name\">\
         <label for=\"password\">{t_password}</label>\
         <input type=\"password\" id=\"password\" name=\"password\" autocomplete=\"new-password\">\
         <label class=\"check\"><input type=\"checkbox\" name=\"confirm\"> {t_confirm}</label>\
         <p><button class=\"primary\" name=\"action\" value=\"save\">{t_save}</button></p>\
         <p><button name=\"action\" value=\"validate\">{t_validate}</button> \
         <button name=\"action\" value=\"sign\">{t_sign}</button></p>\
         <p><button name=\"action\" value=\"disable\">{t_disable}</button> \
         <button name=\"action\" value=\"forget\">{t_forget}</button></p></form>\
         <script>document.getElementById('file').addEventListener('change',function(){{\
         var f=this.files[0];if(!f)return;var r=new FileReader();r.onload=function(){{\
         document.getElementById('pfx_base64').value=r.result.split(',')[1]||'';\
         document.getElementById('file_name').value=f.name}};r.readAsDataURL(f)}});</script>\
         <p><small>{t_where} <code>{cert}</code>, <code>{pw}</code>. {t_note}</small></p>\
         <p><a href=\"/local/advanced\">{t_back}</a> &middot; <a href=\"/\">{t_status}</a></p>",
        title = lang.t("Certificate (advanced, optional)", "Certificado (avançado, opcional)"),
        tone = if enabled { "warn" } else { "ok" },
        risk = risk(lang),
        t_file = lang.t(
            "Certificate file (.pfx or .p12). It is copied into the worker; the original is only read.",
            "Arquivo do certificado (.pfx ou .p12). Ele é copiado para o worker; o original é só lido.",
        ),
        t_password = lang.t(
            "Certificate password. It is never shown again.",
            "Senha do certificado. Ela nunca é mostrada de novo.",
        ),
        t_confirm = lang.t(
            "I have read the warning above and I trust every job this machine may run.",
            "Li o aviso acima e confio em toda tarefa que esta máquina pode rodar.",
        ),
        t_save = lang.t("Enable certificate passthrough", "Ativar o repasse do certificado"),
        t_validate = lang.t("Validate certificate", "Validar o certificado"),
        t_sign = lang.t("Test PDF signing", "Testar assinatura de PDF"),
        t_disable = lang.t("Disable certificate passthrough", "Desativar o repasse do certificado"),
        t_forget = lang.t("Disable and delete the certificate", "Desativar e apagar o certificado"),
        t_where = lang.t("A container finds them at", "O container os encontra em"),
        cert = crate::certificate::CONTAINER_CERT,
        pw = crate::certificate::CONTAINER_PASSWORD,
        t_note = lang.t(
            "Enabling or disabling restarts the service; running jobs keep what they had. The signing test takes a while the first time, because it pulls an image.",
            "Ativar ou desativar reinicia o serviço; as tarefas em execução ficam com o que tinham. O teste de assinatura demora na primeira vez, porque baixa uma imagem.",
        ),
        t_back = lang.t("Advanced settings", "Configuração avançada"),
        t_status = lang.t("Back to status", "Voltar ao status"),
    );
    shell(lang, "/local/certificate", None, &body)
}

async fn certificate_page(State(launcher): State<Arc<Launcher>>, headers: HeaderMap) -> Response {
    let lang = lang_of(&headers, &launcher.settings().language);
    let Some(path) = &launcher.settings().config_json else {
        return not_editable(lang);
    };
    if !admin(&headers, &launcher) {
        return ask_login();
    }
    html(StatusCode::OK, certificate_view(path, lang, None))
}

/// What one button of the certificate form did. Restart: the config changed.
pub(crate) enum CertificateDone {
    Restart,
    Note(bool, String),
}

/// The buttons that need no container. `sign` is handled by the caller.
pub(crate) fn certificate_action(
    path: &Path,
    form: &HashMap<String, String>,
    lang: Lang,
) -> CertificateDone {
    let dir = config::certificate_dir_for(path);
    let field = |name: &str| form.get(name).map(String::as_str).unwrap_or("");
    match field("action") {
        "validate" => {
            let found = crate::certificate::validate_imported(&dir);
            CertificateDone::Note(
                found.ok,
                format!(
                    "{}: {}{}",
                    found.code,
                    found.detail,
                    found
                        .not_after
                        .as_deref()
                        .map(|until| format!(" (valid until {until})"))
                        .unwrap_or_default()
                ),
            )
        }
        "disable" | "forget" => {
            match crate::devx::certificate_disable(path, field("action") == "forget") {
                Ok(()) => CertificateDone::Restart,
                Err(err) => CertificateDone::Note(false, err),
            }
        }
        "save" => {
            if !form.contains_key("confirm") {
                return CertificateDone::Note(
                    false,
                    lang.t(
                        "Not enabled: tick the box to confirm you have read the warning.",
                        "Não ativado: marque a caixa para confirmar que leu o aviso.",
                    )
                    .to_string(),
                );
            }
            // No new file: the one already imported, with the password typed.
            let pfx = if field("pfx_base64").is_empty() {
                std::fs::read(crate::certificate::pfx_path(&dir)).map_err(|_| {
                    lang.t(
                        "Choose a certificate file.",
                        "Escolha um arquivo de certificado.",
                    )
                    .to_string()
                })
            } else {
                base64::engine::general_purpose::STANDARD
                    .decode(field("pfx_base64").trim())
                    .map_err(|_| "the file did not arrive whole".to_string())
            };
            let name = match field("file_name") {
                "" => "certificate",
                name => name,
            };
            match pfx.and_then(|pfx| {
                crate::devx::certificate_set(path, name, &pfx, field("password"), true)
                    .map_err(|out| out.text)
            }) {
                Ok(_) => CertificateDone::Restart,
                Err(err) => CertificateDone::Note(false, err),
            }
        }
        _ => CertificateDone::Note(false, "unknown action".to_string()),
    }
}

async fn certificate_save(
    State(launcher): State<Arc<Launcher>>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let lang = lang_of(&headers, &launcher.settings().language);
    let Some(path) = &launcher.settings().config_json else {
        return not_editable(lang);
    };
    if !admin(&headers, &launcher) {
        return ask_login();
    }
    let done = if form.get("action").map(String::as_str) == Some("sign") {
        let out = crate::devx::demo_sign(
            launcher.settings().clone(),
            None,
            crate::devx::SignOptions {
                configured: true,
                image: crate::devx::SIGN_IMAGE,
                out: None,
                inline: false,
            },
        )
        .await;
        CertificateDone::Note(out.code == 0, out.text)
    } else {
        certificate_action(path, &form, lang)
    };
    match done {
        CertificateDone::Note(ok, text) => html(
            if ok {
                StatusCode::OK
            } else {
                StatusCode::BAD_REQUEST
            },
            certificate_view(path, lang, Some((ok, &text))),
        ),
        CertificateDone::Restart => {
            tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                request_restart();
            });
            html(
                StatusCode::OK,
                shell(
                    lang,
                    "/",
                    Some((4, "/local/certificate")),
                    lang.t(
                        "<section class=\"hero tone-ok\"><h1 class=\"state\">Saved.</h1><p class=\"sub\">The service is restarting. Running jobs keep what they had; the next ones get the new setting.</p></section>",
                        "<section class=\"hero tone-ok\"><h1 class=\"state\">Salvo.</h1><p class=\"sub\">O serviço está reiniciando. As tarefas em execução ficam com o que tinham; as próximas recebem a nova configuração.</p></section>",
                    ),
                ),
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Managed containers. The list and its start, stop, enable and disable are
// for the person at the machine, like the pause. What a container is, and
// the mode of the machine, are behind the admin login, like the other
// dangerous settings. A secret's value is never sent to the browser.

const MANAGED_FIELDS: &[&str] = &[
    "name",
    "image",
    "command",
    "env",
    "secrets",
    "cpu",
    "memory",
    "shm",
    "timeout_secs",
    "policy",
    "interval_secs",
    "enabled",
];

fn managed_state(view: &crate::managed::View, now: i64, lang: Lang) -> String {
    let wait = |at: Option<i64>| ago(at.unwrap_or(now).saturating_sub(now));
    let state = match view.state {
        "running" => lang.t("running", "rodando").to_string(),
        "starting" => lang.t("starting", "iniciando").to_string(),
        "scheduled" => format!(
            "{} {}",
            lang.t("next run in", "próxima execução em"),
            wait(view.next_start_at)
        ),
        "backoff" => format!(
            "<span class=\"bad\">{}</span>: {} {}",
            lang.t("it keeps ending early", "está terminando cedo demais"),
            lang.t("next try in", "nova tentativa em"),
            wait(view.next_start_at)
        ),
        "waiting" => format!(
            "<span class=\"bad\">{}</span>: {}",
            lang.t("waiting", "esperando"),
            escape(view.note.as_deref().unwrap_or(""))
        ),
        "needs_secret" => format!(
            "<span class=\"bad\">{}</span>: {}",
            lang.t("a secret has no value", "falta o valor de um segredo"),
            escape(&view.secrets_missing.join(", "))
        ),
        "stopped" => lang
            .t("stopped by a person", "parado por uma pessoa")
            .to_string(),
        "idle" => lang
            .t("ready: runs when started", "pronto: roda ao iniciar")
            .to_string(),
        "disabled" => lang.t("disabled", "desativado").to_string(),
        _ => lang
            .t("managed mode is off", "modo gerenciado desligado")
            .to_string(),
    };
    if view.failures > 0 {
        format!(
            "{state}<br><small>{}: {}</small>",
            lang.t("failures in a row", "falhas seguidas"),
            view.failures
        )
    } else {
        state
    }
}

/// The list. `note` is the answer to the last button.
pub(crate) fn managed_view(
    mode: bool,
    views: &[crate::managed::View],
    now: i64,
    lang: Lang,
    note: Option<(bool, &str)>,
) -> String {
    let button = |name: &str, action: &str, label: &str| {
        format!(
            "<form method=\"post\" action=\"/local/managed\" class=\"inline\">\
             <input type=\"hidden\" name=\"name\" value=\"{}\">\
             <button name=\"action\" value=\"{action}\">{label}</button></form> ",
            escape(name)
        )
    };
    let mut rows = String::new();
    for view in views {
        let d = &view.definition;
        let policy = match (d.policy, d.interval_secs) {
            (crate::managed::Policy::Interval, Some(secs)) => {
                format!("{} {}", lang.t("every", "a cada"), ago(secs as i64))
            }
            (crate::managed::Policy::Always, _) => {
                lang.t("always running", "sempre rodando").to_string()
            }
            _ => lang
                .t("once, when started", "uma vez, ao iniciar")
                .to_string(),
        };
        let last = match &view.last {
            Some(job) => format!(
                "{} &middot; {}",
                crate::jobs::status_of(job),
                ago(now.saturating_sub(job.ended_at.unwrap_or(job.started_at)))
            ),
            None => lang.t("never ran", "nunca rodou").to_string(),
        };
        let mut controls = String::new();
        if view.running_job.is_some() {
            controls += &button(&d.name, "stop", lang.t("Stop", "Parar"));
        } else if view.enabled && mode {
            controls += &button(&d.name, "start", lang.t("Start now", "Iniciar agora"));
        }
        controls += &if view.enabled {
            button(&d.name, "disable", lang.t("Disable", "Desativar"))
        } else {
            button(&d.name, "enable", lang.t("Enable", "Ativar"))
        };
        rows += &format!(
            "<tr><td><b>{name}</b><br><small><code>{image}</code></small></td><td>{policy}</td>\
             <td>{state}</td><td>{last}</td><td>{controls}\
             <a href=\"/local/managed/edit?name={name}\">{edit}</a></td></tr>",
            name = escape(&d.name),
            image = escape(&d.image),
            state = managed_state(view, now, lang),
            edit = lang.t("Edit", "Editar"),
        );
    }
    let table = if views.is_empty() {
        format!(
            "<p>{}</p>",
            lang.t(
                "No managed container yet.",
                "Nenhum container gerenciado ainda."
            )
        )
    } else {
        format!(
            "<div class=\"scroll\"><table><tr><th>{}</th><th>{}</th><th>{}</th><th>{}</th><th></th></tr>{rows}</table></div>",
            lang.t("Container", "Container"),
            lang.t("Runs", "Roda"),
            lang.t("State", "Estado"),
            lang.t("Last run", "Última execução"),
        )
    };
    let mode_text = if mode {
        lang.t(
            "<b>Managed mode is ON.</b> This worker starts the containers below by itself. It takes no job from the poll URL and none from the API.",
            "<b>O modo gerenciado está LIGADO.</b> Este worker inicia sozinho os containers abaixo. Não pega tarefa da URL de poll nem da API.",
        )
    } else {
        lang.t(
            "<b>Managed mode is OFF.</b> This worker takes jobs from the poll URL and the API, and starts none of the containers below.",
            "<b>O modo gerenciado está DESLIGADO.</b> Este worker pega tarefas da URL de poll e da API, e não inicia nenhum dos containers abaixo.",
        )
    };
    let (mode_action, mode_label) = if mode {
        (
            "mode_off",
            lang.t("Turn managed mode off", "Desligar o modo gerenciado"),
        )
    } else {
        (
            "mode_on",
            lang.t("Turn managed mode on", "Ligar o modo gerenciado"),
        )
    };
    let note = note
        .map(|(ok, text)| {
            format!(
                "<p class=\"callout tone-{}\"><b>{}</b></p>",
                if ok { "ok" } else { "bad" },
                escape(text)
            )
        })
        .unwrap_or_default();
    let body = format!(
        "<h1>{title}</h1>{note}<section class=\"hero tone-{mode_tone}\"><p class=\"sub\">{mode_text}</p>\
         <form method=\"post\" action=\"/local/managed\"><p>\
         <button name=\"action\" value=\"{mode_action}\">{mode_label}</button> <small>{mode_note}</small></p></form></section>\
         {table}<p><a class=\"button\" href=\"/local/managed/edit\">{new}</a> <small>{admin}</small></p>\
         <p class=\"note\">{rules} <a href=\"/\">{back}</a></p>",
        mode_tone = if mode { "busy" } else { "ok" },
        back = lang.t("Status", "Status"),
        title = lang.t("Managed containers", "Containers gerenciados"),
        mode_note = lang.t(
            "Admin login. Only while nothing is running.",
            "Login de administrador. Só quando nada está rodando.",
        ),
        new = lang.t("New managed container", "Novo container gerenciado"),
        admin = lang.t("(admin login)", "(login de administrador)"),
        rules = lang.t(
            "Stop ends the container now, and it stays stopped until someone starts it. Disable starts nothing new: a run in progress finishes. Every run obeys this machine's allowlist, slots, limits and safety checks; when the machine is short of memory or in use, a start waits.",
            "Parar encerra o container agora, e ele fica parado até alguém iniciar. Desativar não inicia mais nada: a execução em andamento termina. Toda execução obedece à allowlist, aos slots, aos limites e às checagens de segurança desta máquina; se faltar memória ou a máquina estiver em uso, o início espera.",
        ),
    );
    shell(lang, "/local/managed", Some((10, "/local/managed")), &body)
}

async fn managed_list(
    launcher: &Launcher,
    lang: Lang,
    status: StatusCode,
    note: Option<(bool, &str)>,
) -> Response {
    let now = launcher.now();
    match crate::managed::views(launcher.store(), now) {
        Ok((mode, views)) => html(status, managed_view(mode, &views, now, lang, note)),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn managed_page(State(launcher): State<Arc<Launcher>>, headers: HeaderMap) -> Response {
    let lang = lang_of(&headers, &launcher.settings().language);
    managed_list(&launcher, lang, StatusCode::OK, None).await
}

fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(str::trim).filter(|line| !line.is_empty())
}

/// The editor's fields as a definition. A secret line is `NAME=value` to
/// set it, or `NAME` alone to keep the value this machine has.
pub(crate) fn managed_input(
    form: &HashMap<String, String>,
) -> Result<crate::managed::Input, String> {
    let field = |name: &str| form.get(name).map(|v| v.trim()).unwrap_or("");
    let optional = |name: &str| Some(field(name).to_string()).filter(|v| !v.is_empty());
    let number = |name: &str| -> Result<Option<u64>, String> {
        match field(name) {
            "" => Ok(None),
            text => text
                .parse()
                .map(Some)
                .map_err(|_| format!("{name} must be a whole number")),
        }
    };
    let mut env = std::collections::BTreeMap::new();
    for line in lines(field("env")) {
        let (key, value) = line.split_once('=').ok_or_else(|| {
            format!(
                "env: `{}` is not NAME=value",
                line.chars().take(40).collect::<String>()
            )
        })?;
        env.insert(key.trim().to_string(), value.to_string());
    }
    let mut secret_env = std::collections::BTreeMap::new();
    for line in lines(field("secrets")) {
        match line.split_once('=') {
            Some((key, value)) => {
                secret_env.insert(key.trim().to_string(), Some(value.to_string()))
            }
            None => secret_env.insert(line.to_string(), None),
        };
    }
    let command: Vec<String> = lines(field("command")).map(str::to_string).collect();
    Ok(crate::managed::Input {
        name: field("name").to_string(),
        image: field("image").to_string(),
        command: Some(command).filter(|c| !c.is_empty()),
        env,
        secret_env,
        cpu: match field("cpu") {
            "" => None,
            text => Some(
                text.replace(',', ".")
                    .parse()
                    .map_err(|_| "cpu must be a number".to_string())?,
            ),
        },
        memory: optional("memory"),
        shm: optional("shm"),
        timeout_secs: number("timeout_secs")?,
        policy: match field("policy") {
            "once" => crate::managed::Policy::Once,
            "interval" => crate::managed::Policy::Interval,
            "always" => crate::managed::Policy::Always,
            _ => return Err("policy must be once, interval or always".to_string()),
        },
        interval_secs: number("interval_secs")?,
        enabled: form.contains_key("enabled"),
    })
}

/// A stored definition as the editor's fields. Secrets by name only.
fn managed_fields(
    definition: &crate::managed::Definition,
    enabled: bool,
) -> HashMap<String, String> {
    let text = |value: Option<String>| value.unwrap_or_default();
    let mut fields = HashMap::from([
        ("name".to_string(), definition.name.clone()),
        ("image".to_string(), definition.image.clone()),
        (
            "command".to_string(),
            definition.command.clone().unwrap_or_default().join("\n"),
        ),
        (
            "env".to_string(),
            definition
                .env
                .iter()
                .map(|(k, v)| format!("{k}={v}\n"))
                .collect(),
        ),
        ("secrets".to_string(), definition.secret_env.join("\n")),
        (
            "cpu".to_string(),
            text(definition.cpu.map(|c| c.to_string())),
        ),
        ("memory".to_string(), text(definition.memory.clone())),
        ("shm".to_string(), text(definition.shm.clone())),
        (
            "timeout_secs".to_string(),
            text(definition.timeout_secs.map(|t| t.to_string())),
        ),
        ("policy".to_string(), definition.policy.name().to_string()),
        (
            "interval_secs".to_string(),
            text(definition.interval_secs.map(|t| t.to_string())),
        ),
    ]);
    if enabled {
        fields.insert("enabled".to_string(), "on".to_string());
    }
    fields
}

/// The editor. `existing` fixes the name. On an error the fields come back
/// as they were typed, except the secret values: those are typed again.
pub(crate) fn managed_editor(
    fields: &HashMap<String, String>,
    existing: bool,
    limits: &Limits,
    lang: Lang,
    error: Option<&str>,
) -> String {
    let value = |name: &str| escape(fields.get(name).map(String::as_str).unwrap_or(""));
    let input = |name: &str, label: &str, hint: &str, extra: &str| {
        format!(
            "<label for=\"{name}\">{label}</label>\
             <input type=\"text\" id=\"{name}\" name=\"{name}\" value=\"{}\" {extra}>\
             <small>{hint}</small>",
            value(name)
        )
    };
    let area = |name: &str, label: &str, hint: &str, text: String| {
        format!(
            "<label for=\"{name}\">{label}</label>\
             <textarea id=\"{name}\" name=\"{name}\" rows=\"4\" spellcheck=\"false\" autocomplete=\"off\">{text}</textarea>\
             <small>{hint}</small>"
        )
    };
    // Only the names go back to the browser, whatever was typed.
    let secret_names: String = lines(fields.get("secrets").map(String::as_str).unwrap_or(""))
        .map(|line| format!("{}\n", escape(line.split('=').next().unwrap_or("").trim())))
        .collect();
    let policy = fields
        .get("policy")
        .map(String::as_str)
        .unwrap_or("interval");
    let option = |name: &str, label: &str| {
        format!(
            "<option value=\"{name}\"{}>{label}</option>",
            if policy == name { " selected" } else { "" }
        )
    };
    let per_job = |value: Option<String>| {
        value.unwrap_or_else(|| lang.t("no limit", "sem limite").to_string())
    };
    let error = error
        .map(|e| {
            format!(
                "<p class=\"bad\"><b>{}</b> {}</p>",
                lang.t("Not saved.", "Não salvo."),
                escape(e)
            )
        })
        .unwrap_or_default();
    let delete = if existing {
        format!(
            "<form method=\"post\" action=\"/local/managed\"><input type=\"hidden\" name=\"name\" value=\"{}\">\
             <p><button name=\"action\" value=\"delete\">{}</button></p></form>",
            value("name"),
            lang.t("Remove this container", "Remover este container"),
        )
    } else {
        String::new()
    };
    let body = format!(
        "<p><a href=\"/local/managed\">&larr; {back}</a></p><h1>{title}</h1>{error}\
         <form method=\"post\" action=\"/local/managed\">\
         <input type=\"hidden\" name=\"action\" value=\"save\">\
         {name}{image}{command}{env}{secrets}{cpu}{memory}{shm}{timeout}\
         <label for=\"policy\">{t_policy}</label><select id=\"policy\" name=\"policy\">{once}{interval}{always}</select>\
         <small>{policy_hint}</small>{interval_secs}\
         <label class=\"check\"><input type=\"checkbox\" name=\"enabled\"{checked}> {t_enabled}</label>\
         <p><button class=\"primary\">{save}</button></p></form>{delete}",
        back = lang.t("Managed containers", "Containers gerenciados"),
        title = if existing {
            lang.t("Edit managed container", "Editar container gerenciado")
        } else {
            lang.t("New managed container", "Novo container gerenciado")
        },
        name = input(
            "name",
            lang.t("Name", "Nome"),
            lang.t(
                "Lowercase letters, digits, - and _. It cannot be changed later.",
                "Letras minúsculas, dígitos, - e _. Não pode ser mudado depois.",
            ),
            if existing { "readonly" } else { "" },
        ),
        image = input(
            "image",
            lang.t("Image", "Imagem"),
            lang.t(
                "Must be in this machine's allowlist.",
                "Precisa estar na allowlist desta máquina.",
            ),
            "",
        ),
        command = area(
            "command",
            lang.t("Command (optional)", "Comando (opcional)"),
            lang.t(
                "One argument per line. Empty: the image's own command.",
                "Um argumento por linha. Vazio: o comando da própria imagem.",
            ),
            value("command"),
        ),
        env = area(
            "env",
            lang.t("Environment", "Variáveis de ambiente"),
            lang.t(
                "One NAME=value per line. Not for passwords or tokens: use the next box.",
                "Um NOME=valor por linha. Não para senhas ou tokens: use a caixa seguinte.",
            ),
            value("env"),
        ),
        secrets = area(
            "secrets",
            lang.t("Secret environment", "Variáveis secretas"),
            lang.t(
                "NAME=value sets a secret. A NAME alone keeps the value this machine already has. A stored value is never shown again, here or anywhere.",
                "NOME=valor define um segredo. Só o NOME mantém o valor que esta máquina já tem. Um valor guardado nunca é mostrado de novo, aqui ou em outro lugar.",
            ),
            secret_names,
        ),
        cpu = input(
            "cpu",
            lang.t("CPUs", "CPUs"),
            &format!(
                "{} {}",
                lang.t("Empty: this machine's limit per job,", "Vazio: o limite desta máquina por tarefa,"),
                per_job(limits.max_job_cpus.map(|c| c.to_string()))
            ),
            "",
        ),
        memory = input(
            "memory",
            lang.t("Memory", "Memória"),
            &format!(
                "{} {}",
                lang.t("Like 2g. Empty: this machine's limit per job,", "Como 2g. Vazio: o limite desta máquina por tarefa,"),
                per_job(limits.max_job_memory_bytes.map(crate::resources::human))
            ),
            "",
        ),
        shm = input(
            "shm",
            lang.t("Shared memory (/dev/shm)", "Memória compartilhada (/dev/shm)"),
            lang.t(
                "Like 2g. A browser needs it. Empty: Docker's 64 MiB.",
                "Como 2g. Um navegador precisa. Vazio: os 64 MiB do Docker.",
            ),
            "",
        ),
        timeout = input(
            "timeout_secs",
            lang.t("Time limit of one run, in seconds", "Tempo limite de uma execução, em segundos"),
            lang.t(
                "A run that passes it is stopped. Empty: this machine's default; for \"always\", the longest this machine allows, after which the container is started again.",
                "A execução que passa disso é encerrada. Vazio: o padrão desta máquina; para \"sempre\", o maior que esta máquina permite, e depois o container é iniciado de novo.",
            ),
            "inputmode=\"numeric\"",
        ),
        t_policy = lang.t("When it runs", "Quando roda"),
        once = option("once", lang.t("Once, when someone starts it", "Uma vez, quando alguém inicia")),
        interval = option("interval", lang.t("Again every interval", "De novo a cada intervalo")),
        always = option("always", lang.t("Always running", "Sempre rodando")),
        policy_hint = lang.t(
            "Interval: start to start, never two at a time; a run still going when the next is due is followed by it, not joined. Always: started again whenever it ends, after a wait that grows from 30 s to 15 min while it keeps ending in under a minute.",
            "Intervalo: de início a início, nunca dois ao mesmo tempo; se a execução ainda não acabou quando chega a hora, a próxima vem depois dela. Sempre: iniciado de novo toda vez que termina, depois de uma espera que cresce de 30 s a 15 min enquanto ele terminar em menos de um minuto.",
        ),
        interval_secs = input(
            "interval_secs",
            lang.t("Interval, in seconds", "Intervalo, em segundos"),
            lang.t("Only for the interval. 60 or more.", "Só para o intervalo. 60 ou mais."),
            "inputmode=\"numeric\"",
        ),
        checked = if fields.contains_key("enabled") { " checked" } else { "" },
        t_enabled = lang.t(
            "Enabled: the worker may start it (in managed mode)",
            "Ativado: o worker pode iniciar (no modo gerenciado)",
        ),
        save = lang.t("Save", "Salvar"),
    );
    shell(lang, "/local/managed", None, &body)
}

/// The editor of a stored definition, as the admin gets it.
#[cfg(test)]
pub(crate) fn managed_editor_for_test(definition: &crate::managed::Definition) -> String {
    managed_editor(
        &managed_fields(definition, true),
        true,
        &Limits::default(),
        Lang::En,
        None,
    )
}

async fn managed_edit(
    State(launcher): State<Arc<Launcher>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let lang = lang_of(&headers, &launcher.settings().language);
    if !admin(&headers, &launcher) {
        return ask_login();
    }
    let limits = Limits::of(launcher.settings());
    let stored = query
        .get("name")
        .and_then(|name| launcher.store().managed_row(name).ok().flatten())
        .and_then(|row| {
            let definition = serde_json::from_str(&row.spec).ok()?;
            Some(managed_fields(&definition, row.enabled))
        });
    let fresh = HashMap::from([("policy".to_string(), "interval".to_string())]);
    html(
        StatusCode::OK,
        managed_editor(
            stored.as_ref().unwrap_or(&fresh),
            stored.is_some(),
            &limits,
            lang,
            None,
        ),
    )
}

async fn managed_save(
    State(launcher): State<Arc<Launcher>>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let lang = lang_of(&headers, &launcher.settings().language);
    let store = launcher.store().clone();
    let action = form.get("action").map(String::as_str).unwrap_or("");
    let name = form.get("name").map(String::as_str).unwrap_or("");
    if matches!(action, "mode_on" | "mode_off" | "save" | "delete") && !admin(&headers, &launcher) {
        return ask_login();
    }
    let done: Result<(), crate::managed::Failure> = match action {
        "start" => crate::managed::start(&store, name).map(|_| ()),
        "stop" => crate::managed::stop(&store, name).map(|_| ()),
        "enable" => crate::managed::set_enabled(&store, name, true),
        "disable" => crate::managed::set_enabled(&store, name, false),
        "delete" => crate::managed::remove(&store, name),
        "mode_on" | "mode_off" => match launcher.set_managed_mode(action == "mode_on").await {
            Ok(true) => Ok(()),
            Ok(false) => Err(crate::managed::busy(
                store.running_ids().map(|ids| ids.len()).unwrap_or(0),
            )),
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        },
        "save" => {
            let saved = managed_input(&form)
                .map_err(|message| crate::managed::Failure {
                    code: "MANAGED_INVALID",
                    message,
                })
                .and_then(|input| crate::managed::save(&store, launcher.settings(), input));
            if let Err(failure) = saved {
                let existing = store.managed_row(name).ok().flatten().is_some();
                let mut typed = form.clone();
                typed.retain(|key, _| MANAGED_FIELDS.contains(&key.as_str()));
                return html(
                    StatusCode::BAD_REQUEST,
                    managed_editor(
                        &typed,
                        existing,
                        &Limits::of(launcher.settings()),
                        lang,
                        Some(&failure.message),
                    ),
                );
            }
            Ok(())
        }
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    launcher.managed_changed.notify_one();
    match done {
        Ok(()) => Redirect::to("/local/managed").into_response(),
        Err(failure) => {
            let hint = crate::hints::hint_in(
                failure.code,
                match lang {
                    Lang::En => crate::hints::Lang::En,
                    Lang::Pt => crate::hints::Lang::Pt,
                },
            );
            let status = if failure.code == "MANAGED_BUSY" {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            let text = format!("{}. {hint}", failure.message.trim_end_matches('.'));
            managed_list(&launcher, lang, status, Some((false, &text))).await
        }
    }
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
    /// When a pause for a while ends by itself.
    pub paused_until: Option<i64>,
    pub job_ids: Vec<String>,
    /// What memory keeps from starting, and how long it did.
    pub memory: crate::memory::View,
    pub last_job_at: Option<i64>,
    pub last_job_error: Option<String>,
    pub docker_error: Option<String>,
    pub last: Option<JobRecord>,
    /// Why the last poll failed. None when it was answered or nothing is polled.
    pub poll_error: Option<String>,
    /// The last request sent to the poll URL: when, and how it ended.
    pub last_poll: Option<(i64, &'static str)>,
    /// Why no new job starts right now, though a slot is free: low memory
    /// or disk. None when a job could start.
    pub waiting_for: Option<String>,
    pub limits: Limits,
    pub config_warning: Option<String>,
    pub computer: Computer,
    pub safety: crate::safety::Report,
    /// True while the machine runs its own managed containers and takes no
    /// job from outside.
    pub managed_mode: bool,
    pub managed: Vec<crate::managed::View>,
}

/// Readings for the page's "Computer resources". On Windows `host` is the
/// PC, from the supervisor, and the rest is the WSL2 VM the service runs in.
#[derive(Clone, Debug, Default)]
pub struct Computer {
    pub host: Option<crate::host::Feed>,
    pub cpus: Option<u64>,
    pub memory_total: Option<u64>,
    pub memory_available: Option<u64>,
    pub min_idle_secs: u64,
    /// `waiting_for` is the idle wait, not memory or disk.
    pub user_active: bool,
    /// Outside `lend_hours`, a low battery, or a presentation.
    pub hold: Option<crate::lending::Hold>,
    /// `lend_hours` as written. None: at any time.
    pub lend_hours: Option<String>,
    /// The time on the clock `lend_hours` is read on: `wed 18:30, UTC-03:00`.
    pub clock: String,
    pub delivery: crate::store::Delivery,
}

impl Computer {
    pub fn read(launcher: &Launcher, machine: &crate::safety::Readings) -> Self {
        Self {
            host: machine.host.or_else(crate::host::current),
            cpus: machine.inner.cpus,
            memory_total: machine.inner.memory_total,
            memory_available: machine.inner.memory_available,
            min_idle_secs: launcher.settings().min_idle_secs,
            user_active: launcher.shortage().is_none() && launcher.user_active().is_some(),
            hold: launcher.lending_hold(),
            lend_hours: launcher
                .settings()
                .lend_hours
                .as_ref()
                .map(|h| h.text.clone()),
            clock: crate::lending::clock(machine.host.or_else(crate::host::current), now_unix()),
            delivery: launcher.delivery(),
        }
    }
}

/// The resource limits in force, defaults filled in. No secret is in here.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Limits {
    /// `shared-pc` or `none`.
    pub profile: String,
    /// Per job: the most it may ask for, and what it gets when it asks for
    /// nothing. Null: no limit.
    pub max_job_cpus: Option<f64>,
    pub max_job_memory_bytes: Option<u64>,
    /// Below these a new job waits. Zero: not checked.
    pub min_free_memory_bytes: u64,
    pub min_free_disk_bytes: u64,
}

impl Limits {
    pub fn of(settings: &crate::config::Settings) -> Self {
        Self {
            profile: if settings.shared_pc {
                crate::config::PROFILE_SHARED_PC
            } else {
                crate::config::PROFILE_NONE
            }
            .to_string(),
            max_job_cpus: settings.max_job_cpus,
            max_job_memory_bytes: settings.max_job_memory,
            min_free_memory_bytes: settings.min_free_memory,
            min_free_disk_bytes: settings.min_free_disk,
        }
    }

    /// One line for the doctor and `status`.
    pub fn line(&self) -> String {
        use crate::resources::human;
        let or_none = |text: Option<String>| text.unwrap_or_else(|| "no limit".to_string());
        let or_off = |bytes: u64| match bytes {
            0 => "not checked".to_string(),
            bytes => human(bytes),
        };
        format!(
            "profile {}; per job: cpu {}, memory {}; a new job needs free memory {}, free disk {}",
            self.profile,
            or_none(self.max_job_cpus.map(|c| c.to_string())),
            or_none(self.max_job_memory_bytes.map(human)),
            or_off(self.min_free_memory_bytes),
            or_off(self.min_free_disk_bytes),
        )
    }
}

impl Live {
    pub fn slots_for_page(&self) -> Option<(usize, usize)> {
        Some((self.slots_used? as usize, self.slots_total? as usize))
    }

    /// State, slots, jobs, and the last error. `updated_at` is not part of it,
    /// so a heartbeat still rewrites the file on the interval.
    pub fn signature(&self) -> String {
        format!(
            "{}|{:?}|{:?}|{}|{}|{:?}|{:?}|{:?}|{}|{:?}",
            self.state(false),
            self.slots_used,
            self.slots_total,
            self.draining,
            self.job_ids.join(","),
            self.last_job_at,
            self.last_error(false, None),
            self.poll_error,
            // Not the text: its byte counts change every second.
            self.waiting_for.is_some() || self.memory.need.is_some(),
            self.computer.delivery
        ) + &self.safety.signature()
            + &format!("|{}", self.managed_mode)
            + &self
                .managed
                .iter()
                .map(|m| {
                    format!(
                        "|{}:{}:{}:{}",
                        m.definition.name, m.state, m.enabled, m.failures
                    )
                })
                .collect::<String>()
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

fn memory_json(memory: &crate::memory::View) -> serde_json::Value {
    serde_json::json!({
        "short_of": memory.need.map(|n| n.place),
        "to_free_bytes": memory.need.map(|n| n.to_free_bytes),
        "waiting_since": memory.waiting_since.map(unix_to_iso),
        "waited_today_secs": memory.waited_today_secs,
        "waited_week_secs": memory.waited_week_secs,
        "short_hours": memory.short_hours.as_ref().map(|h| &h.when),
        "suggested_lend_hours": memory.short_hours.as_ref().and_then(|h| h.suggested_lend_hours.as_ref()),
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
    poll_error: Option<String>,
    /// The last request to the poll URL. Null until one was sent: nothing
    /// is polled, or the worker is full, paused or short of memory.
    last_poll_at: Option<String>,
    /// `task`, `nothing`, `task_rejected`, `not_approved` or `error`.
    last_poll_status: Option<&'a str>,
    waiting_for: Option<&'a str>,
    /// When a pause for a while ends by itself. Null for no pause, or one
    /// until someone resumes.
    paused_until: Option<String>,
    /// When the machine lends itself: `lend_hours`, the time on the clock it
    /// is read on (`wed 18:30, UTC-03:00`), the wait there is now
    /// (`OUTSIDE_LEND_HOURS`, `BATTERY_LOW`, `USER_PRESENTING`), and the
    /// battery's charge while a Windows PC runs on it and jobs still start.
    lending: serde_json::Value,
    /// Memory: how much to free before the next job (`short_of`, `pc`,
    /// `wsl2` or `machine`, and `to_free_bytes`), since when and how long
    /// new jobs waited for it, and the hours it ran short in 7 days. Never
    /// which programs hold it: that stays on the page.
    memory: serde_json::Value,
    /// Interrupted jobs and callbacks not answered, over the history.
    callbacks: &'a crate::store::Delivery,
    /// Whether a new job may start for the sake of this computer's owner:
    /// the level, why, the owner's authorization, and the last incidents.
    safety: &'a crate::safety::Report,
    /// `jobs`: work comes from the poll URL and the API. `managed`: the
    /// worker starts its own containers and takes nothing from outside.
    mode: &'a str,
    /// The managed containers: name, policy, state, the last run. Never
    /// their env or secrets.
    managed: Vec<serde_json::Value>,
    limits: &'a Limits,
    config_warning: Option<String>,
    version: &'a str,
    pid: u32,
    /// When this process started. It changes when the service restarts on a
    /// new config, which keeps the pid.
    started_at: String,
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
        poll_error: live.poll_error.as_deref().map(one_line),
        last_poll_at: live.last_poll.map(|(at, _)| unix_to_iso(at)),
        last_poll_status: live.last_poll.map(|(_, status)| status),
        waiting_for: live.waiting_for.as_deref(),
        paused_until: live.paused_until.map(unix_to_iso),
        lending: serde_json::json!({
            "lend_hours": live.computer.lend_hours,
            "clock": live.computer.clock,
            "hold": live.computer.hold,
            "on_battery_percent": crate::lending::on_battery(live.computer.host),
        }),
        memory: memory_json(&live.memory),
        callbacks: &live.computer.delivery,
        safety: &live.safety,
        mode: if live.managed_mode { "managed" } else { "jobs" },
        managed: live.managed.iter().map(crate::managed::view_json).collect(),
        limits: &live.limits,
        config_warning: live.config_warning.as_deref().map(one_line),
        version: env!("CARGO_PKG_VERSION"),
        pid: std::process::id(),
        started_at: unix_to_iso(*STARTED.get_or_init(self::now_unix)),
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
        paused_until: None,
        job_ids: Vec::new(),
        memory: Default::default(),
        last_job_at: None,
        last_job_error: None,
        docker_error: None,
        last: None,
        poll_error: None,
        last_poll: None,
        waiting_for: None,
        limits: Limits::default(),
        config_warning: None,
        computer: Computer::default(),
        safety: crate::safety::Report::default(),
        managed_mode: false,
        managed: Vec::new(),
    }
}

static RESTART: tokio::sync::Notify = tokio::sync::Notify::const_new();
static WANT_RESTART: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Stop serving and start again on the config file. `main` does the restart.
pub fn request_restart() {
    WANT_RESTART.store(true, std::sync::atomic::Ordering::SeqCst);
    RESTART.notify_one();
}

pub async fn restart_requested() {
    RESTART.notified().await;
}

pub fn restart_wanted() -> bool {
    WANT_RESTART.load(std::sync::atomic::Ordering::SeqCst)
}

static STARTED: std::sync::OnceLock<i64> = std::sync::OnceLock::new();

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

    fn view(lang: Lang, slots: Option<(usize, usize)>, draining: bool) -> View<'static> {
        View {
            lang,
            worker: "DESK<07>",
            os: "windows",
            slots,
            draining,
            paused_until: None,
            last: None,
            polls: true,
            poll_error: None,
            warning: None,
            editable: true,
            computer: None,
            limits: None,
            waiting_for: None,
            safety: None,
            managed: None,
            last_poll: None,
            recent: &[],
            needs_secret: Vec::new(),
            memory: None,
            programs: &[],
        }
    }

    /// Not a check. `scripts/screenshots.sh` runs it to render a state a
    /// test machine does not reach by itself: a small PC waiting for its
    /// owner. Writes `auth-en.html` and `auth-pt.html` into the folder named
    /// by SHUTUP_AND_RUN_SHOTS_DIR, and does nothing without it.
    #[test]
    #[ignore]
    fn render_states_for_screenshots() {
        use crate::safety::{Admission, Finding, Level, Report};
        let Ok(dir) = std::env::var("SHUTUP_AND_RUN_SHOTS_DIR") else {
            return;
        };
        let asked = Report {
            applies: true,
            level: Level::Warning,
            admission: Admission::AuthorizationRequired,
            reasons: vec![Finding {
                code: crate::safety::SMALL_MACHINE.into(),
                level: Level::Warning,
                detail: "7.6 GiB of memory, 2 processors".into(),
            }],
            needs_authorization: vec![crate::safety::SMALL_MACHINE.into()],
            ..Default::default()
        };
        for (lang, tag) in [(Lang::En, "en"), (Lang::Pt, "pt")] {
            let mut v = view(lang, Some((0, 1)), false);
            v.worker = "DESK-07";
            v.safety = Some(&asked);
            v.last_poll = Some((1_794_960_000, "nothing"));
            std::fs::write(Path::new(&dir).join(format!("auth-{tag}.html")), render(&v)).unwrap();
        }
    }

    #[test]
    fn the_page_says_the_state_and_whether_the_pc_can_be_turned_off() {
        let idle = render(&view(Lang::Pt, Some((0, 2)), false));
        assert!(idle.contains("RODANDO"));
        assert!(idle.contains("SIM, pode desligar"));
        assert!(idle.contains("0 / 2"));
        assert!(idle.contains("DESK&lt;07&gt; (windows)"));
        assert!(idle.contains(env!("CARGO_PKG_VERSION")));
        assert!(idle.contains("action=\"/local/pause\""));
        assert!(idle.contains("href=\"/local/config\""));

        let busy = render(&view(Lang::Pt, Some((1, 2)), true));
        assert!(busy.contains("PAUSADO"));
        assert!(busy.contains("NÃO agora"));
        assert!(busy.contains("action=\"/local/resume\""));
        assert!(busy.contains("A pausa termina quando o PC reinicia"));
        assert!(idle.contains("action=\"/local/pause?for=3h\""));
        assert!(idle.contains("pelo resto do dia"));
        let mut timed = view(Lang::Pt, Some((0, 2)), true);
        timed.paused_until = Some(now_unix() + 3600);
        let timed = render(&timed);
        assert!(timed.contains("volta a aceitar sozinho às"), "{timed}");
        assert!(!timed.contains("tarefa nova. A pausa termina quando o PC reinicia"));

        let down = render(&view(Lang::En, None, false));
        assert!(down.contains("DOCKER IS DOWN"));
        let mut refused = view(Lang::En, Some((0, 1)), false);
        refused.poll_error = Some("poll returned 403: the poll URL has not approved this machine");
        refused.warning = Some("config.json is not usable");
        refused.editable = false;
        let refused = render(&refused);
        assert!(refused.contains("has not approved this machine"));
        assert!(refused.contains("config.json is not usable"));
        assert!(!refused.contains("/local/config"));
        assert_eq!(ago(90), "90 s");
        assert_eq!(ago(600), "10 min");
        assert_eq!(ago(7200), "2 h");
    }

    /// One language on the screen at a time, and a link to the other.
    #[test]
    fn the_page_shows_the_computer_and_only_what_was_read() {
        use crate::host::Feed;
        const GIB: u64 = 1 << 30;
        let limits = Limits {
            profile: "shared-pc".into(),
            max_job_cpus: Some(1.0),
            max_job_memory_bytes: Some(2 * GIB),
            min_free_memory_bytes: 2 * GIB,
            min_free_disk_bytes: 0,
        };
        let mut computer = Computer {
            host: Some(Feed {
                idle_secs: Some(90),
                memory_total: Some(12 * GIB),
                memory_available: Some(5 * GIB),
                cpus: Some(8),
                cpu_busy_percent: None,
                disk_available: None,
                ..Feed::default()
            }),
            cpus: Some(4),
            memory_total: Some(4 * GIB),
            memory_available: Some(3 * GIB),
            min_idle_secs: 300,
            user_active: true,
            hold: None,
            lend_hours: None,
            clock: "wed 18:30, UTC-03:00".into(),
            delivery: crate::store::Delivery {
                interrupted: 2,
                interrupted_unnotified: 1,
                pending: 1,
                failed: 3,
            },
        };
        let mut v = view(Lang::Pt, Some((0, 1)), false);
        v.limits = Some(&limits);
        v.waiting_for = Some("the PC is in use: idle for 90s, 300s needed");
        v.computer = Some(&computer);
        let page = render(&v);
        for text in [
            "<h2>Recursos do computador</h2>",
            "Windows (as tarefas rodam dentro do WSL2)",
            // The PC and the VM are two lines with their own numbers.
            "<td>Este PC (Windows)</td><td>8 processadores, 12.0 GiB de memória, 5.0 GiB disponível</td>",
            "<td>WSL2 (o limite dele, e o que está livre)</td><td>4 processadores, 4.0 GiB de memória, 3.0 GiB disponível</td>",
            "<td>Limite de cada tarefa</td><td>CPU: 1, memória: 2.0 GiB</td>",
            "<td>Tarefas rodando / máximo</td><td>0 / 1</td>",
            "<td>PC sem uso há</td><td>90 s (uma tarefa começa depois de 5 min)</td>",
            "<td>Energia</td><td>não informado</td>",
            "<td>Empresta</td><td>a qualquer hora</td>",
            "esperando: o PC está em uso",
            "<td>Tarefas interrompidas</td><td>2 (1 sem aviso entregue)</td>",
            "<td>Entrega de callbacks</td><td>1 aguardando nova tentativa, 3 desistidos</td>",
        ] {
            assert!(page.contains(text), "{text}\n{page}");
        }
        assert!(!page.contains("Computer resources"));

        // No supervisor feed: said so, not guessed. Short of memory is its
        // own answer, and a full or paused worker comes first.
        computer.host = None;
        computer.user_active = false;
        let mut v = view(Lang::En, Some((0, 1)), false);
        v.limits = Some(&limits);
        v.computer = Some(&computer);
        v.waiting_for = Some("low on memory: 512 MiB free, 2.0 GiB needed");
        let page = render(&v);
        assert!(page.contains("<td>This PC (Windows)</td><td>not reported</td>"));
        assert!(page.contains("<td>PC without use for</td><td>not reported ("));
        assert!(page.contains("waiting, not enough resources: low on memory"));
        v.waiting_for = None;
        assert!(render(&v).contains("<td>Can a new job start?</td><td>yes</td>"));
        // On the battery above 60%: a job starts, and the page says so.
        // Outside lend_hours: the state is a wait, the row says why.
        computer.host = Some(Feed {
            on_battery: Some(true),
            battery_percent: Some(72),
            ..Feed::default()
        });
        computer.lend_hours = Some("mon-fri 19-07".into());
        computer.hold = Some(crate::lending::Hold {
            code: crate::lending::OUTSIDE_LEND_HOURS.into(),
            detail: "outside lend_hours mon-fri 19-07 (now wed 14:05)".into(),
        });
        let mut v = view(Lang::En, Some((0, 1)), false);
        v.limits = Some(&limits);
        v.computer = Some(&computer);
        v.waiting_for = Some("outside lend_hours mon-fri 19-07 (now wed 14:05)");
        let page = render(&v);
        for text in [
            "<h1 class=\"state\">WAITING</h1>",
            "<h2>This PC is on its battery</h2>",
            "<td>Power</td><td>on the battery, 72% (below 60% no new job starts)</td>",
            "<td>Lends itself</td><td><code>mon-fri 19-07</code> &middot; now wed 18:30, UTC-03:00</td>",
            "waiting: Outside the hours this machine lends itself (lend_hours)",
        ] {
            assert!(page.contains(text), "{text}\n{page}");
        }
        let memory = crate::memory::View {
            need: Some(crate::memory::Need {
                place: "pc",
                to_free_bytes: 1536 << 20,
            }),
            waiting_since: Some(1_700_000_000),
            waited_today_secs: 9_600,
            waited_week_secs: 32_400,
            short_hours: Some(crate::memory::ShortHours {
                when: "mon-fri 09-18".into(),
                suggested_lend_hours: None,
            }),
        };
        let programs = [crate::memory::Program {
            name: "chrome<1>".into(),
            processes: 38,
            bytes: 3 << 30,
        }];
        let mut short = view(Lang::Pt, Some((0, 1)), false);
        short.computer = Some(&computer);
        short.limits = Some(&limits);
        short.memory = Some(&memory);
        short.programs = &programs;
        let page = render(&short);
        for text in [
            "<h2>Falta memória</h2>",
            "Para a próxima tarefa começar, libere cerca de 1.5 GiB de memória",
            "Usando mais memória: chrome&lt;1&gt; 3.0 GiB (38 processos)",
            "<td>Esperou por memória</td><td>hoje 2 h 40 min &middot; 7 dias 9 h</td>",
            "<td>Memória curta, 7 dias</td><td><code>mon-fri 09-18</code></td>",
        ] {
            assert!(page.contains(text), "{text}\n{page}");
        }
        computer.hold = None;
        computer.host = None;
        computer.lend_hours = None;
        let mut v = view(Lang::En, Some((0, 1)), false);
        v.limits = Some(&limits);
        v.computer = Some(&computer);
        v.slots = Some((1, 1));
        assert!(render(&v).contains("no: busy"));
        v.draining = true;
        assert!(render(&v).contains("no: paused"));
        // A Linux machine has one line for itself and no idle row.
        let mut v = view(Lang::En, Some((0, 1)), false);
        v.os = "linux";
        v.limits = Some(&limits);
        v.computer = Some(&computer);
        let page = render(&v);
        assert!(page.contains("<td>This machine</td><td>4 processors"));
        assert!(!page.contains("WSL2") && !page.contains("without use"));
    }

    #[test]
    fn each_page_is_in_one_language_only() {
        let english = [
            "RUNNING",
            "Turn off?",
            "Jobs now",
            "Last job",
            "Machine",
            "Pause: take no new jobs",
            "Settings",
            "This page opens only",
        ];
        let portuguese = [
            "RODANDO",
            "Pode desligar?",
            "Tarefas agora",
            "Última tarefa",
            "Máquina",
            "Pausar: não pegar",
            "Configuração",
            "Esta página só abre",
        ];
        let en = render(&view(Lang::En, Some((0, 2)), false));
        let pt = render(&view(Lang::Pt, Some((0, 2)), false));
        for text in english {
            assert!(en.contains(text), "missing in EN: {text}");
            assert!(!pt.contains(text), "English on the PT page: {text}");
        }
        for text in portuguese {
            assert!(pt.contains(text), "missing in PT: {text}");
            assert!(!en.contains(text), "Portuguese on the EN page: {text}");
        }
        assert!(en.contains("<html lang=\"en\">") && en.contains("set=pt&amp;to=/\">Português<"));
        assert!(pt.contains("<html lang=\"pt-BR\">") && pt.contains("set=en&amp;to=/\">English<"));

        // The cookie the button sets wins over the browser's language.
        let headers = |pairs: &[(&'static str, &'static str)]| {
            let mut map = HeaderMap::new();
            for (name, value) in pairs {
                map.insert(*name, value.parse().unwrap());
            }
            map
        };
        assert_eq!(lang_of(&headers(&[]), "en"), Lang::En);
        // The config's `language` is the default the cookie overrides, and
        // it comes before the browser's own language.
        assert_eq!(lang_of(&headers(&[]), "pt"), Lang::Pt);
        assert_eq!(
            lang_of(&headers(&[("accept-language", "en-US")]), "pt"),
            Lang::Pt
        );
        assert_eq!(lang_of(&headers(&[("cookie", "lang=en")]), "pt"), Lang::En);
        assert_eq!(
            lang_of(&headers(&[("accept-language", "pt-BR,pt;q=0.9")]), "en"),
            Lang::Pt
        );
        assert_eq!(
            lang_of(&headers(&[("accept-language", "en-US,pt;q=0.5")]), "en"),
            Lang::En
        );
        assert_eq!(
            lang_of(
                &headers(&[("accept-language", "pt-BR"), ("cookie", "a=b; lang=en")]),
                "en"
            ),
            Lang::En
        );
        assert_eq!(lang_of(&headers(&[("cookie", "lang=pt")]), "en"), Lang::Pt);
    }

    #[test]
    fn a_form_changes_only_the_keys_of_its_own_group() {
        let dir = std::env::temp_dir().join(format!("sar-page-form-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.json");
        let mut map = config::default_map();
        map.insert("poll_token".into(), serde_json::json!("p".repeat(32)));
        config::save_json(&path, &map).unwrap();
        let form = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };

        // The open form: the everyday keys. A dangerous key in it is ignored.
        save_fields(
            &path,
            OnPage::Open,
            &form(&[
                ("poll_url", "https://jobs.example/poll?user=ana"),
                ("max_slots", "2"),
                ("poll_interval_secs", ""),
                ("allowlist", "evil/image"),
                ("token", "t".repeat(32).as_str()),
            ]),
        )
        .unwrap();
        let saved = config::read_json(&path).unwrap();
        assert_eq!(saved["poll_url"], "https://jobs.example/poll?user=ana");
        assert_eq!(saved["max_slots"], 2);
        assert_eq!(saved["allowlist"], map["allowlist"]);
        assert_eq!(saved["token"], map["token"]);

        // An invalid value is refused whole and the file stays.
        let error = save_fields(&path, OnPage::Open, &form(&[("max_slots", "0")])).unwrap_err();
        assert!(error.contains("max_slots"), "{error}");
        assert_eq!(config::read_json(&path).unwrap(), saved);

        // The locked form: a blank secret keeps it, an unticked box is false.
        save_fields(
            &path,
            OnPage::Locked,
            &form(&[
                ("allowlist", "alpine\nghcr.io/o/img"),
                ("token", ""),
                ("poll_token", ""),
                ("admin_user", "boss"),
                ("admin_password", "s3cret"),
                ("default_timeout_secs", ""),
                ("max_timeout_secs", ""),
            ]),
        )
        .unwrap();
        let locked = config::read_json(&path).unwrap();
        assert_eq!(
            locked["allowlist"],
            serde_json::json!(["alpine", "ghcr.io/o/img"])
        );
        assert_eq!(locked["token"], map["token"]);
        assert_eq!(locked["poll_token"], map["poll_token"]);
        assert_eq!(locked["poll_any_image"], false);
        assert_eq!(locked["admin_user"], "boss");
        assert_eq!(locked["poll_url"], saved["poll_url"]);

        // The editor shows the file with every secret masked.
        let page = editor(&path, OnPage::Locked, Lang::En, Some("bad <value>"));
        assert!(page.contains("bad &lt;value&gt;"));
        assert!(page.contains("name=\"admin_password\""));
        for secret in ["s3cret", &"p".repeat(32), map["token"].as_str().unwrap()] {
            assert!(!page.contains(secret), "{secret} is on the page");
        }
        let _ = std::fs::remove_dir_all(&dir);
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
            paused_until: None,
            job_ids: vec!["abc".into(), "def".into()],
            memory: Default::default(),
            last_job_at: Some(1_700_000_000),
            last_job_error: None,
            docker_error: None,
            last: None,
            poll_error: None,
            last_poll: Some((1_700_000_000, "nothing")),
            waiting_for: Some("low on memory: 512 MiB free, 2.0 GiB needed".into()),
            limits: Limits {
                profile: "shared-pc".into(),
                max_job_cpus: Some(1.0),
                max_job_memory_bytes: Some(2 << 30),
                min_free_memory_bytes: 2 << 30,
                min_free_disk_bytes: 0,
            },
            config_warning: None,
            computer: Computer::default(),
            safety: crate::safety::Report::default(),
            managed_mode: false,
            managed: Vec::new(),
        };
        let bytes = document(&live, 1_700_000_100, false, None);
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            value["waiting_for"],
            "low on memory: 512 MiB free, 2.0 GiB needed"
        );
        assert_eq!(
            value["limits"],
            serde_json::json!({
                "profile": "shared-pc",
                "max_job_cpus": 1.0,
                "max_job_memory_bytes": 2147483648u64,
                "min_free_memory_bytes": 2147483648u64,
                "min_free_disk_bytes": 0,
            })
        );
        assert_eq!(
            live.limits.line(),
            "profile shared-pc; per job: cpu 1, memory 2.0 GiB; a new job needs free memory 2.0 GiB, free disk not checked"
        );
        assert_eq!(value["state"], "active");
        assert_eq!(value["slots_total"], 2);
        assert_eq!(value["slots_used"], 1);
        assert_eq!(value["job_ids"], serde_json::json!(["abc", "def"]));
        assert_eq!(value["last_job_at"], "2023-11-14T22:13:20Z");
        assert!(value["last_error"].is_null());
        // Is it asking for work? When the last poll went out and how it ended.
        assert_eq!(value["last_poll_at"], "2023-11-14T22:13:20Z");
        assert_eq!(value["last_poll_status"], "nothing");
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
