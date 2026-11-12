//! Optional pull. When `poll_url` is unset this module does nothing.
//! Nothing-to-do and errors are logged. They never stop the process.
//!
//! The poll body names the worker, so one URL can serve many machines. The
//! answer is a task, or a `manifest_url` that is fetched once and must then
//! be a task. The manifest is the same JSON as `POST /v1/jobs`.

use crate::api::launch_request_from_value;
use crate::launcher::{JobSource, LaunchOutcome, LaunchRequest, Launcher};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

const POLL_TIMEOUT: Duration = Duration::from_secs(10);
const BACKOFF_CAP: Duration = Duration::from_secs(300);
const MAX_POLL_BODY: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PollOutcome {
    Idle,
    Started,
    Existing,
    Rejected,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct PollError(String);

/// What a poll answer, or a manifest, turned out to be.
#[derive(Debug)]
enum Answer {
    Nothing,
    Task(Box<LaunchRequest>),
    /// Only a poll answer may carry this. A manifest that does is rejected.
    Manifest(String),
}

pub fn spawn_poller(launcher: Arc<Launcher>) {
    let Some(url) = launcher.settings().poll_url.clone() else {
        return;
    };
    if launcher.settings().poll_token.is_none() {
        tracing::warn!(
            "SHUTUP_AND_RUN_POLL_TOKEN is unset; the poll request has no Authorization header"
        );
    }
    let interval = launcher.settings().poll_interval;
    let host = reqwest::Url::parse(&url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string());
    let client = match client() {
        Ok(client) => client,
        Err(err) => {
            tracing::warn!(error = %err, "poll client; pull mode is off");
            return;
        }
    };
    tracing::info!(%host, interval_secs = interval.as_secs(), "pull mode on");
    tokio::spawn(async move {
        // shortcut: double the wait up to 5 minutes, in this task, no jitter.
        // Upgrade if many machines poll one URL and you need to spread them out.
        let mut wait = interval;
        loop {
            match poll_once(&launcher, &client).await {
                Ok(_) => wait = interval,
                Err(err) => {
                    tracing::warn!(error = %err, "poll");
                    wait = next_wait(wait, interval, true);
                }
            }
            tokio::time::sleep(wait).await;
        }
    });
}

pub(crate) fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(POLL_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|err| err.to_string())
}

/// `failed` doubles `current` up to 300 seconds, or `interval` when that is longer.
/// A success (or a skip) passes `failed = false` and returns `interval`.
pub(crate) fn next_wait(current: Duration, interval: Duration, failed: bool) -> Duration {
    if !failed {
        return interval;
    }
    let cap = BACKOFF_CAP.max(interval);
    current.saturating_mul(2).clamp(interval, cap)
}

pub(crate) async fn poll_once(
    launcher: &Arc<Launcher>,
    client: &reqwest::Client,
) -> Result<PollOutcome, PollError> {
    let Some(url) = launcher.settings().poll_url.clone() else {
        return Ok(PollOutcome::Idle);
    };
    // The slot check only decides whether to ask. launch() checks again.
    // A refused task is not stored. An accepted one is, even when the image
    // then fails to pull, which matches POST /v1/jobs.
    let slots = launcher
        .slots()
        .await
        .map_err(|err| PollError(err.to_string()))?;
    if slots.draining || slots.free == 0 {
        tracing::debug!(free = slots.free, draining = slots.draining, "poll skipped");
        return Ok(PollOutcome::Idle);
    }

    let settings = launcher.settings();
    let mut request = client
        .post(&url)
        .header(reqwest::header::ACCEPT, "application/json")
        .json(&poll_body(
            &settings.worker_name,
            &settings.worker_os,
            slots.free,
        ));
    if let Some(token) = settings.poll_token.as_ref() {
        request = request.bearer_auth(token.as_str());
    }
    let response = request
        .send()
        .await
        .map_err(|err| PollError(err.to_string()))?;
    let status = response.status();
    if status == reqwest::StatusCode::NO_CONTENT {
        tracing::debug!("poll: nothing to do");
        return Ok(PollOutcome::Idle);
    }
    let bytes = read_capped(response).await?;
    if status != reqwest::StatusCode::OK {
        return Err(PollError(format!("poll returned {status}")));
    }
    let answer = match answer_from_body(&bytes, true) {
        Ok(answer) => answer,
        Err(err) => {
            tracing::warn!(error = %err, "poll task rejected");
            return Ok(PollOutcome::Rejected);
        }
    };
    let task = match answer {
        Answer::Nothing => {
            tracing::debug!("poll: nothing to do");
            return Ok(PollOutcome::Idle);
        }
        Answer::Task(task) => *task,
        Answer::Manifest(manifest_url) => {
            match fetch_manifest(launcher, client, &manifest_url).await {
                Ok(Some(task)) => task,
                Ok(None) => {
                    tracing::warn!("manifest rejected: it holds no task");
                    return Ok(PollOutcome::Rejected);
                }
                Err(err) => {
                    tracing::warn!(error = %err, "manifest rejected");
                    return Ok(PollOutcome::Rejected);
                }
            }
        }
    };
    match launcher.launch_from(task, JobSource::Poll).await {
        Ok(LaunchOutcome::Created(job)) => {
            tracing::info!(id = %job.id, image = %job.image, "poll started a job");
            Ok(PollOutcome::Started)
        }
        Ok(LaunchOutcome::Existing(job)) => {
            tracing::info!(id = %job.id, "poll task already recorded");
            Ok(PollOutcome::Existing)
        }
        Err(err) => {
            tracing::warn!(error = %err, "poll task rejected");
            Ok(PollOutcome::Rejected)
        }
    }
}

async fn read_capped(mut response: reqwest::Response) -> Result<Vec<u8>, PollError> {
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len().saturating_add(chunk.len()) > MAX_POLL_BODY {
                    return Err(PollError("poll response is too large".into()));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(body),
            Err(err) => return Err(PollError(err.to_string())),
        }
    }
}

/// The poll request body. `free_slots` is what the server may hand out now.
pub(crate) fn poll_body(worker: &str, os: &str, free_slots: usize) -> Value {
    serde_json::json!({
        "worker": worker,
        "os": os,
        "free_slots": free_slots,
    })
}

/// A poll answer: nothing, a task, or `{"manifest_url": "..."}`. A manifest body
/// goes through the same parse with `allow_manifest = false`, so a manifest
/// cannot point at another manifest.
fn answer_from_body(bytes: &[u8], allow_manifest: bool) -> Result<Answer, String> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Answer::Nothing);
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| "poll response is not JSON".to_string())?;
    let task = |value: Value| {
        launch_request_from_value(value)
            .map(|task| Answer::Task(Box::new(task)))
            .map_err(|_| "poll task is invalid".to_string())
    };
    match value {
        Value::Null => Ok(Answer::Nothing),
        Value::Object(map) if map.is_empty() => Ok(Answer::Nothing),
        Value::Object(mut map) if map.contains_key("manifest_url") => {
            if !allow_manifest {
                return Err("a manifest cannot point at another manifest".into());
            }
            if map.len() != 1 {
                return Err("poll manifest response has extra fields".into());
            }
            match map.remove("manifest_url") {
                Some(Value::Null) => Ok(Answer::Nothing),
                Some(Value::String(url)) => {
                    validate_manifest_url(&url)?;
                    Ok(Answer::Manifest(url))
                }
                _ => Err("manifest_url is not a string".into()),
            }
        }
        Value::Object(mut map) if map.contains_key("task") => {
            if map.len() != 1 {
                return Err("poll task response has extra fields".into());
            }
            match map.remove("task") {
                Some(Value::Null) => Ok(Answer::Nothing),
                Some(value) => task(value),
                None => Err("poll task is invalid".into()),
            }
        }
        other => task(other),
    }
}

fn validate_manifest_url(url: &str) -> Result<(), String> {
    if url.len() > 2048 || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("manifest_url is invalid".into());
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| "manifest_url is invalid".to_string())?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("manifest_url must be http or https".into());
    }
    if parsed.host_str().is_none() {
        return Err("manifest_url is invalid".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("manifest_url must not embed credentials".into());
    }
    Ok(())
}

/// The poll token goes with the manifest request only when the manifest is on
/// the same scheme, host and port as `poll_url`. A manifest on object storage
/// or a CDN is fetched without it, so the token does not land in a third
/// party's access log.
pub(crate) fn same_origin(poll_url: &str, manifest_url: &str) -> bool {
    let (Ok(a), Ok(b)) = (
        reqwest::Url::parse(poll_url),
        reqwest::Url::parse(manifest_url),
    ) else {
        return false;
    };
    a.scheme() == b.scheme()
        && a.host_str().map(str::to_ascii_lowercase) == b.host_str().map(str::to_ascii_lowercase)
        && a.port_or_known_default() == b.port_or_known_default()
}

/// GET the manifest: same client, same 64 KiB cap, no redirects, and the
/// same validation as a task offered inline.
async fn fetch_manifest(
    launcher: &Arc<Launcher>,
    client: &reqwest::Client,
    manifest_url: &str,
) -> Result<Option<LaunchRequest>, PollError> {
    let settings = launcher.settings();
    let mut request = client
        .get(manifest_url)
        .header(reqwest::header::ACCEPT, "application/json");
    let with_token = settings
        .poll_url
        .as_deref()
        .is_some_and(|poll_url| same_origin(poll_url, manifest_url));
    if let (true, Some(token)) = (with_token, settings.poll_token.as_ref()) {
        request = request.bearer_auth(token.as_str());
    }
    let response = request
        .send()
        .await
        .map_err(|err| PollError(err.to_string()))?;
    let status = response.status();
    let bytes = read_capped(response).await?;
    if status != reqwest::StatusCode::OK {
        return Err(PollError(format!("manifest returned {status}")));
    }
    match answer_from_body(&bytes, false).map_err(PollError)? {
        Answer::Nothing => Ok(None),
        Answer::Task(task) => Ok(Some(*task)),
        Answer::Manifest(_) => Err(PollError(
            "a manifest cannot point at another manifest".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;
    use crate::mock::MockRuntime;
    use crate::runtime::Runtime;
    use axum::body::Bytes;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::post;
    use axum::Router;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    fn launcher_for(
        slots: usize,
        extra: &str,
        env_pairs: &[(&str, &str)],
    ) -> (Arc<Launcher>, Arc<MockRuntime>) {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("shutup-and-run-poll-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let toml_text = format!(
            r#"
            token = "test-token-value-ok"
            max_slots = {slots}
            allowlist = ["alpine"]
            default_timeout_secs = 30
            max_timeout_secs = 60
            history_path = "{}"
            {extra}
            "#,
            dir.join("jobs.sqlite").display()
        );
        let env = env_pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect::<HashMap<_, _>>();
        let settings = Settings::from_toml_and_env(&toml_text, &env).unwrap();
        let mock = Arc::new(MockRuntime::new());
        let runtime: Arc<dyn Runtime> = mock.clone();
        let launcher = Launcher::new(settings, runtime).unwrap();
        (launcher, mock)
    }

    struct PollServer {
        url: String,
        base: String,
        hits: Arc<AtomicU64>,
        auth: Arc<Mutex<Option<String>>>,
        body: Arc<Mutex<Option<String>>>,
        /// Hits, auth header and body of GET /manifest.
        manifest_hits: Arc<AtomicU64>,
        manifest_auth: Arc<Mutex<Option<String>>>,
    }

    async fn poll_server(status: StatusCode, body: &'static str) -> PollServer {
        poll_server_with_manifest(status, body, StatusCode::OK, "").await
    }

    /// POST /poll answers `status` and `body`. GET /manifest answers the other pair.
    async fn poll_server_with_manifest(
        status: StatusCode,
        body: &'static str,
        manifest_status: StatusCode,
        manifest: &'static str,
    ) -> PollServer {
        let hits = Arc::new(AtomicU64::new(0));
        let auth = Arc::new(Mutex::new(None));
        let seen = Arc::new(Mutex::new(None));
        let manifest_hits = Arc::new(AtomicU64::new(0));
        let manifest_auth = Arc::new(Mutex::new(None));
        let hits2 = hits.clone();
        let auth2 = auth.clone();
        let seen2 = seen.clone();
        let mhits2 = manifest_hits.clone();
        let mauth2 = manifest_auth.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        // `{BASE}` in the poll body becomes this server's own URL, so an answer
        // can point at its own /manifest.
        let body = body.replace("{BASE}", &base);
        let app = Router::new()
            .route(
                "/poll",
                post(move |headers: HeaderMap, body_bytes: Bytes| {
                    let hits2 = hits2.clone();
                    let auth2 = auth2.clone();
                    let seen2 = seen2.clone();
                    let body = body.clone();
                    async move {
                        hits2.fetch_add(1, Ordering::Relaxed);
                        *auth2.lock().unwrap() = auth_of(&headers);
                        *seen2.lock().unwrap() =
                            Some(String::from_utf8_lossy(&body_bytes).into_owned());
                        (status, body)
                    }
                }),
            )
            .route(
                "/manifest",
                axum::routing::get(move |headers: HeaderMap| {
                    let mhits2 = mhits2.clone();
                    let mauth2 = mauth2.clone();
                    async move {
                        mhits2.fetch_add(1, Ordering::Relaxed);
                        *mauth2.lock().unwrap() = auth_of(&headers);
                        (manifest_status, manifest)
                    }
                }),
            );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let url = format!("{base}/poll");
        for _ in 0..50 {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        PollServer {
            url,
            base,
            hits,
            auth,
            body: seen,
            manifest_hits,
            manifest_auth,
        }
    }

    fn auth_of(headers: &HeaderMap) -> Option<String> {
        headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    #[tokio::test]
    async fn unset_does_not_poll() {
        let (launcher, mock) = launcher_for(1, "", &[]);
        assert!(launcher.settings().poll_url.is_none());
        let outcome = poll_once(&launcher, &client().unwrap()).await.unwrap();
        assert_eq!(outcome, PollOutcome::Idle);
        assert_eq!(mock.start_count().await, 0);
    }

    #[tokio::test]
    async fn empty_response_starts_nothing() {
        let server = poll_server(StatusCode::OK, "{}").await;
        let (launcher, mock) = launcher_for(1, &format!("poll_url = \"{}\"", server.url), &[]);
        let outcome = poll_once(&launcher, &client().unwrap()).await.unwrap();
        assert_eq!(outcome, PollOutcome::Idle);
        assert_eq!(mock.start_count().await, 0);
        assert!(server.hits.load(Ordering::Relaxed) >= 1);

        let quiet = poll_server(StatusCode::NO_CONTENT, "").await;
        let (launcher, mock) = launcher_for(1, &format!("poll_url = \"{}\"", quiet.url), &[]);
        let outcome = poll_once(&launcher, &client().unwrap()).await.unwrap();
        assert_eq!(outcome, PollOutcome::Idle);
        assert_eq!(mock.start_count().await, 0);

        for body in [
            &b"null"[..],
            b"{\"task\":null}",
            b"   ",
            b"{\"manifest_url\":null}",
        ] {
            assert!(
                matches!(answer_from_body(body, true).unwrap(), Answer::Nothing),
                "{:?}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[tokio::test]
    async fn task_started_uses_the_same_path_as_post() {
        let server = poll_server(
            StatusCode::OK,
            r#"{"image":"alpine","command":["sleep","30"],"job_key":"same-key","env":{"JOB_NAME":"example"}}"#,
        )
        .await;
        // Two slots: the running job must leave one free, or the next poll is skipped
        // and never reaches job_key idempotency.
        let (launcher, mock) = launcher_for(
            2,
            &format!("poll_url = \"{}\"", server.url),
            &[("SHUTUP_AND_RUN_POLL_TOKEN", "poll-token-value-ok")],
        );
        let http = client().unwrap();
        let outcome = poll_once(&launcher, &http).await.unwrap();
        assert_eq!(outcome, PollOutcome::Started);
        launcher.settled().await;
        assert_eq!(mock.start_count().await, 1);
        assert_eq!(
            server.auth.lock().unwrap().as_deref(),
            Some("Bearer poll-token-value-ok")
        );
        let body: Value =
            serde_json::from_str(server.body.lock().unwrap().as_deref().unwrap()).unwrap();
        assert_eq!(body["worker"], launcher.settings().worker_name);
        assert_eq!(body["os"], std::env::consts::OS);
        assert_eq!(body["free_slots"], 2);
        assert_eq!(body.as_object().unwrap().len(), 3);

        let again = poll_once(&launcher, &http).await.unwrap();
        assert_eq!(again, PollOutcome::Existing);
        assert_eq!(mock.start_count().await, 1);
        let body: Value =
            serde_json::from_str(server.body.lock().unwrap().as_deref().unwrap()).unwrap();
        assert_eq!(body["free_slots"], 1);
    }

    #[tokio::test]
    async fn worker_name_and_os_from_config_travel_in_the_body() {
        let server = poll_server(StatusCode::NO_CONTENT, "").await;
        let (launcher, _) = launcher_for(
            1,
            &format!(
                "poll_url = \"{}\"\nworker_name = \"desk-07\"\nworker_os = \"windows\"",
                server.url
            ),
            &[],
        );
        poll_once(&launcher, &client().unwrap()).await.unwrap();
        let body: Value =
            serde_json::from_str(server.body.lock().unwrap().as_deref().unwrap()).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"worker": "desk-07", "os": "windows", "free_slots": 1})
        );
    }

    #[tokio::test]
    async fn manifest_url_is_fetched_and_the_token_stays_on_the_poll_origin() {
        // The poll answer points at this same server's /manifest: same origin,
        // so the manifest GET carries the poll token.
        let server = poll_server_with_manifest(
            StatusCode::OK,
            r#"{"manifest_url":"{BASE}/manifest"}"#,
            StatusCode::OK,
            r#"{"task":{"image":"alpine","command":["sleep","30"],"job_key":"m-1"}}"#,
        )
        .await;
        let (launcher, mock) = launcher_for(
            1,
            &format!("poll_url = \"{}\"", server.url),
            &[("SHUTUP_AND_RUN_POLL_TOKEN", "poll-token-value-ok")],
        );
        let outcome = poll_once(&launcher, &client().unwrap()).await.unwrap();
        assert_eq!(outcome, PollOutcome::Started);
        launcher.settled().await;
        assert_eq!(mock.start_count().await, 1);
        assert_eq!(server.manifest_hits.load(Ordering::Relaxed), 1);
        assert_eq!(
            server.manifest_auth.lock().unwrap().as_deref(),
            Some("Bearer poll-token-value-ok")
        );

        // A manifest on another origin (another port here) is fetched without the token.
        let storage = poll_server_with_manifest(
            StatusCode::OK,
            "",
            StatusCode::OK,
            r#"{"image":"alpine","command":["sleep","30"]}"#,
        )
        .await;
        let answer: &'static str = Box::leak(
            format!(r#"{{"manifest_url":"{}/manifest"}}"#, storage.base).into_boxed_str(),
        );
        let poller = poll_server(StatusCode::OK, answer).await;
        let (launcher, mock) = launcher_for(
            1,
            &format!("poll_url = \"{}\"", poller.url),
            &[("SHUTUP_AND_RUN_POLL_TOKEN", "poll-token-value-ok")],
        );
        let outcome = poll_once(&launcher, &client().unwrap()).await.unwrap();
        assert_eq!(outcome, PollOutcome::Started);
        launcher.settled().await;
        assert_eq!(mock.start_count().await, 1);
        assert_eq!(storage.manifest_hits.load(Ordering::Relaxed), 1);
        assert_eq!(storage.manifest_auth.lock().unwrap().as_deref(), None);
        assert_eq!(
            poller.auth.lock().unwrap().as_deref(),
            Some("Bearer poll-token-value-ok")
        );

        assert!(same_origin(
            "https://a.example/poll",
            "https://a.example:443/m/1.json"
        ));
        assert!(same_origin(
            "http://A.example:8080/poll",
            "http://a.example:8080/x"
        ));
        assert!(!same_origin("https://a.example/poll", "http://a.example/x"));
        assert!(!same_origin(
            "https://a.example/poll",
            "https://b.example/x"
        ));
        assert!(!same_origin(
            "https://a.example/poll",
            "https://a.example:8443/x"
        ));
    }

    #[tokio::test]
    async fn manifest_rejections() {
        // The manifest is not a task, points at another manifest, or is not 200.
        for (status, manifest) in [
            (StatusCode::OK, r#"{"manifest_url":"http://127.0.0.1:9/x"}"#),
            (
                StatusCode::OK,
                r#"{"image":"alpine","volumes":["/:/host"]}"#,
            ),
            (StatusCode::NOT_FOUND, ""),
            (StatusCode::OK, "not json"),
        ] {
            let server = poll_server_with_manifest(
                StatusCode::OK,
                r#"{"manifest_url":"{BASE}/manifest"}"#,
                status,
                manifest,
            )
            .await;
            let (launcher, mock) = launcher_for(1, &format!("poll_url = \"{}\"", server.url), &[]);
            let outcome = poll_once(&launcher, &client().unwrap()).await.unwrap();
            assert_eq!(outcome, PollOutcome::Rejected, "{manifest}");
            assert_eq!(mock.start_count().await, 0);
            assert_eq!(server.manifest_hits.load(Ordering::Relaxed), 1);
        }

        // An empty manifest is nothing to do, not an error, but it is still a rejection
        // of this poll round because the server said there was work.
        let server = poll_server_with_manifest(
            StatusCode::OK,
            r#"{"manifest_url":"{BASE}/manifest"}"#,
            StatusCode::OK,
            "{}",
        )
        .await;
        let (launcher, mock) = launcher_for(1, &format!("poll_url = \"{}\"", server.url), &[]);
        assert_eq!(
            poll_once(&launcher, &client().unwrap()).await.unwrap(),
            PollOutcome::Rejected
        );
        assert_eq!(mock.start_count().await, 0);

        // Bad manifest URLs never leave the process.
        for body in [
            r#"{"manifest_url":"ftp://x/y"}"#,
            r#"{"manifest_url":"http://u:p@x/y"}"#,
            r#"{"manifest_url":"/relative"}"#,
            r#"{"manifest_url":42}"#,
            r#"{"manifest_url":"http://x/y","task":{"image":"alpine"}}"#,
        ] {
            assert!(answer_from_body(body.as_bytes(), true).is_err(), "{body}");
        }
        assert!(answer_from_body(br#"{"manifest_url":"http://x/y"}"#, false).is_err());
        assert!(matches!(
            answer_from_body(br#"{"manifest_url":"http://x/y"}"#, true).unwrap(),
            Answer::Manifest(url) if url == "http://x/y"
        ));
    }

    #[tokio::test]
    async fn task_rejected_by_allowlist() {
        let server = poll_server(StatusCode::OK, r#"{"task":{"image":"redis:7"}}"#).await;
        let (launcher, mock) = launcher_for(1, &format!("poll_url = \"{}\"", server.url), &[]);
        let outcome = poll_once(&launcher, &client().unwrap()).await.unwrap();
        assert_eq!(outcome, PollOutcome::Rejected);
        assert_eq!(mock.start_count().await, 0);
        assert_eq!(mock.pulls().await.len(), 0);
    }

    #[tokio::test]
    async fn server_down_is_an_error_and_backoff_grows() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let (launcher, mock) = launcher_for(
            1,
            &format!("poll_url = \"http://127.0.0.1:{port}/poll\""),
            &[],
        );
        let err = poll_once(&launcher, &client().unwrap()).await.unwrap_err();
        assert!(!err.to_string().is_empty());
        assert_eq!(mock.start_count().await, 0);

        let interval = Duration::from_secs(15);
        assert_eq!(next_wait(interval, interval, false), interval);
        assert_eq!(next_wait(interval, interval, true), Duration::from_secs(30));
        assert_eq!(
            next_wait(Duration::from_secs(180), interval, true),
            BACKOFF_CAP
        );
        assert_eq!(
            next_wait(Duration::from_secs(3600), Duration::from_secs(3600), true),
            Duration::from_secs(3600)
        );
    }

    #[tokio::test]
    async fn skips_poll_when_full_or_draining() {
        let server = poll_server(StatusCode::OK, r#"{"image":"alpine"}"#).await;
        let (launcher, mock) = launcher_for(1, &format!("poll_url = \"{}\"", server.url), &[]);
        mock.seed_running("busy", 9_999_999_999).await;
        let outcome = poll_once(&launcher, &client().unwrap()).await.unwrap();
        assert_eq!(outcome, PollOutcome::Idle);
        assert_eq!(server.hits.load(Ordering::Relaxed), 0);
        assert_eq!(mock.start_count().await, 0);

        let server = poll_server(StatusCode::OK, r#"{"image":"alpine"}"#).await;
        let (launcher, mock) = launcher_for(1, &format!("poll_url = \"{}\"", server.url), &[]);
        launcher.set_draining(true).unwrap();
        let outcome = poll_once(&launcher, &client().unwrap()).await.unwrap();
        assert_eq!(outcome, PollOutcome::Idle);
        assert_eq!(server.hits.load(Ordering::Relaxed), 0);
        assert_eq!(mock.start_count().await, 0);
    }
}
