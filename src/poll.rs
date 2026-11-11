//! Optional pull. When `poll_url` is unset this module does nothing.
//! Nothing-to-do and errors are logged. They never stop the process.

use crate::api::launch_request_from_value;
use crate::launcher::{LaunchOutcome, Launcher};
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
    // A refused task is not stored, except a start that fails after the image
    // is accepted, which matches POST /v1/jobs.
    let slots = launcher
        .slots()
        .await
        .map_err(|err| PollError(err.to_string()))?;
    if slots.draining || slots.free == 0 {
        tracing::debug!(free = slots.free, draining = slots.draining, "poll skipped");
        return Ok(PollOutcome::Idle);
    }

    let mut request = client
        .post(&url)
        .header(reqwest::header::ACCEPT, "application/json")
        .json(&serde_json::json!({}));
    if let Some(token) = launcher.settings().poll_token.as_ref() {
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
    let task = match task_from_body(&bytes) {
        Ok(task) => task,
        Err(err) => {
            tracing::warn!(error = %err, "poll task rejected");
            return Ok(PollOutcome::Rejected);
        }
    };
    let Some(task) = task else {
        tracing::debug!("poll: nothing to do");
        return Ok(PollOutcome::Idle);
    };
    match launcher.launch(task).await {
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

fn task_from_body(bytes: &[u8]) -> Result<Option<crate::launcher::LaunchRequest>, String> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| "poll response is not JSON".to_string())?;
    match value {
        Value::Null => Ok(None),
        Value::Object(map) if map.is_empty() => Ok(None),
        Value::Object(mut map) if map.contains_key("task") => {
            if map.len() != 1 {
                return Err("poll task response has extra fields".into());
            }
            match map.remove("task") {
                Some(Value::Null) => Ok(None),
                Some(task) => launch_request_from_value(task)
                    .map(Some)
                    .map_err(|_| "poll task is invalid".to_string()),
                None => Err("poll task is invalid".into()),
            }
        }
        other => launch_request_from_value(other)
            .map(Some)
            .map_err(|_| "poll task is invalid".to_string()),
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
        hits: Arc<AtomicU64>,
        auth: Arc<Mutex<Option<String>>>,
    }

    async fn poll_server(status: StatusCode, body: &'static str) -> PollServer {
        let hits = Arc::new(AtomicU64::new(0));
        let auth = Arc::new(Mutex::new(None));
        let hits2 = hits.clone();
        let auth2 = auth.clone();
        let app = Router::new().route(
            "/poll",
            post(move |headers: HeaderMap, _body: Bytes| {
                let hits2 = hits2.clone();
                let auth2 = auth2.clone();
                async move {
                    hits2.fetch_add(1, Ordering::Relaxed);
                    let got = headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string);
                    *auth2.lock().unwrap() = got;
                    (status, body)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let url = format!("http://127.0.0.1:{port}/poll");
        for _ in 0..50 {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        PollServer { url, hits, auth }
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

        assert!(task_from_body(b"null").unwrap().is_none());
        assert!(task_from_body(b"{\"task\":null}").unwrap().is_none());
        assert!(task_from_body(b"   ").unwrap().is_none());
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

        let again = poll_once(&launcher, &http).await.unwrap();
        assert_eq!(again, PollOutcome::Existing);
        assert_eq!(mock.start_count().await, 1);
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
