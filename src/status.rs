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
        .route("/local/config", get(config_page).post(config_save))
        .route("/local/advanced", get(advanced_page).post(advanced_save))
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
/// Without it, the browser's own language decides.
pub(crate) fn lang_of(headers: &HeaderMap) -> Lang {
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
        .filter(|to| matches!(*to, "/local/config" | "/local/advanced"))
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
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let view = View {
        lang: lang_of(&headers),
        worker: &settings.worker_name,
        os: &settings.worker_os,
        slots: live.slots_for_page(),
        draining: live.draining,
        last: live.last.as_ref(),
        now,
        polls: settings.poll_url.is_some(),
        poll_error: live.poll_error.as_deref(),
        warning: live.config_warning.as_deref(),
        editable: settings.config_json.is_some(),
    };
    html(StatusCode::OK, render(&view))
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
    pub last: Option<&'a JobRecord>,
    pub now: i64,
    pub polls: bool,
    pub poll_error: Option<&'a str>,
    pub warning: Option<&'a str>,
    /// The config is config.json, so the page can edit it.
    pub editable: bool,
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
    format!(
        "<!doctype html><html lang=\"{code}\"><head><meta charset=\"utf-8\">{refresh}\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>shutup-and-run</title>\
         <style>body{{font:16px/1.5 system-ui,sans-serif;max-width:44em;margin:2em auto;padding:0 1em}}\
         h1{{font-size:1.1em}}h2{{font-size:1em;margin-top:2em}}\
         .state{{font-size:1.6em;font-weight:700}}.bad{{color:#b00020}}.top{{float:right}}\
         td{{padding:.2em 1em .2em 0;vertical-align:top}}button{{font:inherit;padding:.4em 1em}}\
         label{{display:block;margin:.8em 0 .2em}}input[type=text],input[type=number],input[type=password],textarea\
         {{font:inherit;width:100%;box-sizing:border-box;padding:.3em}}code{{word-break:break-all}}</style>\
         </head><body><p class=\"top\"><a href=\"/local/lang?set={other}&amp;to={here}\">{other_name}</a></p>\
         <h1>shutup-and-run {version}</h1>{body}</body></html>",
        code = lang.t("en", "pt-BR"),
        version = env!("CARGO_PKG_VERSION"),
    )
}

pub(crate) fn render(view: &View) -> String {
    let lang = view.lang;
    let (state, color) = match worker_state(
        view.slots.is_none(),
        view.draining,
        view.slots.map(|(used, _)| used).unwrap_or(0),
        false,
    ) {
        "failed" => (lang.t("DOCKER IS DOWN", "DOCKER PAROU"), "#b00020"),
        "draining" => (lang.t("PAUSED", "PAUSADO"), "#8a6d00"),
        _ => (lang.t("RUNNING", "RODANDO"), "#0a7a2f"),
    };
    let (slots, off) = match view.slots {
        Some((used, total)) => (
            format!("{used} / {total}"),
            if used == 0 {
                lang.t("YES, the PC can be turned off.", "SIM, pode desligar o PC.")
            } else {
                lang.t(
                    "NOT now: a job is running.",
                    "NÃO agora: há tarefa rodando.",
                )
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
        None => lang.t("none yet", "nenhuma ainda").to_string(),
    };
    let queue = match (view.polls, view.poll_error) {
        (false, _) => lang
            .t(
                "no poll URL: nothing to do",
                "sem URL de poll: nada a fazer",
            )
            .to_string(),
        (true, None) => lang.t("answering", "respondendo").to_string(),
        (true, Some(error)) => format!("<span class=\"bad\">{}</span>", escape(error)),
    };
    let (action, label) = if view.draining {
        (
            "/local/resume",
            lang.t("Resume taking jobs", "Voltar a aceitar tarefas"),
        )
    } else {
        (
            "/local/pause",
            lang.t("Pause: take no new jobs", "Pausar: não pegar novas tarefas"),
        )
    };
    let warning = view
        .warning
        .map(|w| format!("<p class=\"bad\">{}</p>", escape(w)))
        .unwrap_or_default();
    let settings = if view.editable {
        format!(
            "<p><a href=\"/local/config\">{}</a></p>",
            lang.t("Settings", "Configuração")
        )
    } else {
        String::new()
    };
    let body = format!(
        "<p class=\"state\" style=\"color:{color}\">{state}</p>{warning}\
         <table>\
         <tr><td>{t_off}</td><td><b>{off}</b></td></tr>\
         <tr><td>{t_jobs}</td><td>{slots}</td></tr>\
         <tr><td>{t_last}</td><td>{last}</td></tr>\
         <tr><td>{t_queue}</td><td>{queue}</td></tr>\
         <tr><td>{t_machine}</td><td>{worker} ({os})</td></tr>\
         </table>\
         <form method=\"post\" action=\"{action}\"><p><button>{label}</button></p></form>\
         {settings}<p><small>{note}</small></p>",
        t_off = lang.t("Turn off?", "Pode desligar?"),
        t_jobs = lang.t("Jobs now", "Tarefas agora"),
        t_last = lang.t("Last job", "Última tarefa"),
        t_queue = lang.t("Job queue", "Fila de tarefas"),
        t_machine = lang.t("Machine", "Máquina"),
        worker = escape(view.worker),
        os = escape(view.os),
        note = lang.t(
            "This page opens only on this computer and reloads every 10 s. A pause ends when the PC restarts.",
            "Esta página só abre neste computador e se atualiza a cada 10 s. A pausa termina quando o PC reinicia.",
        ),
    );
    shell(lang, "/", Some((10, "/")), &body)
}

// ---------------------------------------------------------------------------
// The config editor. config.json is the source; these pages read it and
// write it through the same validation as the CLI.

fn label(name: &str, lang: Lang) -> &'static str {
    match name {
        "poll_url" => lang.t(
            "Poll URL (with its ?user= filter)",
            "URL de poll (com o filtro ?user=)",
        ),
        "poll_interval_secs" => lang.t("Poll interval, seconds", "Intervalo de poll, segundos"),
        "max_slots" => lang.t("Containers at once", "Containers ao mesmo tempo"),
        "worker_name" => lang.t("Machine name", "Nome da máquina"),
        "allowlist" => lang.t(
            "Allowed images, one per line",
            "Imagens permitidas, uma por linha",
        ),
        "poll_any_image" => lang.t(
            "Run any image the poll URL sends",
            "Rodar qualquer imagem que a URL de poll mandar",
        ),
        "default_timeout_secs" => lang.t(
            "Job time limit, seconds",
            "Tempo limite da tarefa, segundos",
        ),
        "max_timeout_secs" => lang.t(
            "Longest time limit a job may ask for, seconds",
            "Maior tempo limite que uma tarefa pode pedir, segundos",
        ),
        "token" => lang.t("API token (blank keeps it)", "Token da API (vazio mantém)"),
        "poll_token" => lang.t(
            "Poll token: who this machine is to the poll URL (blank keeps it)",
            "Token de poll: quem esta máquina é para a URL de poll (vazio mantém)",
        ),
        "admin_user" => lang.t("Admin user", "Usuário admin"),
        "admin_password" => lang.t(
            "Admin password (blank keeps it)",
            "Senha admin (vazio mantém)",
        ),
        _ => "",
    }
}

/// The form for the keys of one group, filled from the file.
fn form(map: &JsonMap, group: OnPage, action: &str, lang: Lang) -> String {
    let mut fields = String::new();
    for key in config::KEYS.iter().filter(|k| k.page == group) {
        let name = key.name;
        let text = label(name, lang);
        let value = map.get(name);
        fields.push_str(&match key.kind {
            Kind::Bool => format!(
                "<label><input type=\"checkbox\" name=\"{name}\"{}> {text}</label>",
                if value.and_then(|v| v.as_bool()).unwrap_or(false) {
                    " checked"
                } else {
                    ""
                }
            ),
            Kind::List => format!(
                "<label for=\"{name}\">{text}</label><textarea id=\"{name}\" name=\"{name}\" rows=\"4\">{}</textarea>",
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
                "<label for=\"{name}\">{text}</label><input type=\"password\" id=\"{name}\" name=\"{name}\" autocomplete=\"new-password\">"
            ),
            kind => format!(
                "<label for=\"{name}\">{text}</label><input type=\"{}\" id=\"{name}\" name=\"{name}\" value=\"{}\">",
                if kind == Kind::Int { "number" } else { "text" },
                escape(&match value {
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                })
            ),
        });
    }
    format!(
        "<form method=\"post\" action=\"{action}\">{fields}<p><button>{}</button></p></form>",
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
                "<p class=\"bad\"><b>{}</b> {}</p>",
                lang.t("Not saved.", "Não foi salvo."),
                escape(e)
            )
        })
        .unwrap_or_default();
    let body = match config::read_json(path) {
        Ok(map) => {
            let rows: String = config::masked(&map)
                .iter()
                .map(|(k, v)| {
                    format!(
                        "<tr><td>{}</td><td><code>{}</code></td></tr>",
                        escape(k),
                        escape(&v.to_string())
                    )
                })
                .collect();
            format!(
                "{}<h2>{}</h2><table>{rows}</table><p><small>{}</small></p>",
                form(&map, group, here, lang),
                lang.t("The whole config file", "O arquivo de configuração inteiro"),
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
            "<h2>{title}</h2>{error}{body}<p><a href=\"/\">{}</a> &middot; {other}</p>",
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
    let lang = lang_of(headers);
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
    let lang = lang_of(headers);
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
                "<p class=\"state\">Saved.</p><p>The service is restarting on the new settings. This page goes back to the status in a few seconds.</p>",
                "<p class=\"state\">Salvo.</p><p>O serviço está reiniciando com a nova configuração. Esta página volta ao status em alguns segundos.</p>",
            ),
        ),
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
    /// Why the last poll failed. None when it was answered or nothing is polled.
    pub poll_error: Option<String>,
    /// Why no new job starts right now, though a slot is free: low memory
    /// or disk. None when a job could start.
    pub waiting_for: Option<String>,
    pub limits: Limits,
    pub config_warning: Option<String>,
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
            "{}|{:?}|{:?}|{}|{}|{:?}|{:?}|{:?}|{}",
            self.state(false),
            self.slots_used,
            self.slots_total,
            self.draining,
            self.job_ids.join(","),
            self.last_job_at,
            self.last_error(false, None),
            self.poll_error,
            // Not the text: its byte counts change every second.
            self.waiting_for.is_some()
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
    poll_error: Option<String>,
    waiting_for: Option<&'a str>,
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
        waiting_for: live.waiting_for.as_deref(),
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
        job_ids: Vec::new(),
        last_job_at: None,
        last_job_error: None,
        docker_error: None,
        last: None,
        poll_error: None,
        waiting_for: None,
        limits: Limits::default(),
        config_warning: None,
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
            last: None,
            now: 1_700_000_000,
            polls: true,
            poll_error: None,
            warning: None,
            editable: true,
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
        assert_eq!(lang_of(&headers(&[])), Lang::En);
        assert_eq!(
            lang_of(&headers(&[("accept-language", "pt-BR,pt;q=0.9")])),
            Lang::Pt
        );
        assert_eq!(
            lang_of(&headers(&[("accept-language", "en-US,pt;q=0.5")])),
            Lang::En
        );
        assert_eq!(
            lang_of(&headers(&[
                ("accept-language", "pt-BR"),
                ("cookie", "a=b; lang=en")
            ])),
            Lang::En
        );
        assert_eq!(lang_of(&headers(&[("cookie", "lang=pt")])), Lang::Pt);
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
            job_ids: vec!["abc".into(), "def".into()],
            last_job_at: Some(1_700_000_000),
            last_job_error: None,
            docker_error: None,
            last: None,
            poll_error: None,
            waiting_for: Some("low on memory: 512 MiB free, 2.0 GiB needed".into()),
            limits: Limits {
                profile: "shared-pc".into(),
                max_job_cpus: Some(1.0),
                max_job_memory_bytes: Some(2 << 30),
                min_free_memory_bytes: 2 << 30,
                min_free_disk_bytes: 0,
            },
            config_warning: None,
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
