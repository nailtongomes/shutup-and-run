use crate::bytesize::ByteQty;
use crate::launcher::{metrics_text, LaunchError, LaunchOutcome, LaunchRequest, Launcher};
use crate::store::JobRecord;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

const MAX_BODY: usize = 64 * 1024;

pub fn router(launcher: Arc<Launcher>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/v1/jobs", post(create_job))
        .route("/v1/jobs/{id}", get(job_status))
        .route("/v1/drain", post(set_drain))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY))
        .with_state(launcher)
}

async fn health(State(launcher): State<Arc<Launcher>>) -> Response {
    match launcher.slots().await {
        Ok(slots) => (
            StatusCode::OK,
            Json(json!({
                "status": if slots.draining { "draining" } else { "ok" },
                "draining": slots.draining,
                "slots_total": slots.total,
                "slots_used": slots.used,
                "slots_free": slots.free,
            })),
        )
            .into_response(),
        Err(_) => {
            let draining = launcher.draining().unwrap_or(false);
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": "docker is unavailable",
                    "draining": draining,
                })),
            )
                .into_response()
        }
    }
}

async fn metrics(State(launcher): State<Arc<Launcher>>) -> Response {
    let slots = match launcher.slots().await {
        Ok(slots) => slots,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                "docker is unavailable\n",
            )
                .into_response();
        }
    };
    let counts = match launcher.counts() {
        Ok(counts) => counts,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                "job history is unavailable\n",
            )
                .into_response();
        }
    };
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        metrics_text(slots, counts),
    )
        .into_response()
}

async fn create_job(
    State(launcher): State<Arc<Launcher>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorized(&headers, &launcher) {
        return unauthorized();
    }
    let parsed: RunBody = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return error_body(StatusCode::BAD_REQUEST, "invalid JSON", None),
    };
    let request = LaunchRequest {
        image: parsed.image,
        env: parsed.env,
        timeout_secs: parsed.timeout_secs,
        cpu: parsed.cpu,
        memory_bytes: parsed.memory.map(ByteQty::get),
        shm_bytes: parsed.shm.map(ByteQty::get),
        command: parsed.command,
        job_key: parsed.job_key,
        callback_url: parsed.callback_url,
    };
    match launcher.launch(request).await {
        Ok(LaunchOutcome::Created(job)) => {
            (StatusCode::ACCEPTED, Json(job_json(&job, false))).into_response()
        }
        Ok(LaunchOutcome::Existing(job)) => {
            (StatusCode::OK, Json(job_json(&job, false))).into_response()
        }
        Err(LaunchError::BadRequest(msg)) => error_body(StatusCode::BAD_REQUEST, &msg, None),
        Err(LaunchError::Forbidden) => {
            error_body(StatusCode::FORBIDDEN, "image is not allowlisted", None)
        }
        Err(LaunchError::NoSlots { total, used }) => error_body(
            StatusCode::TOO_MANY_REQUESTS,
            "no free slots",
            Some(json!({
                "slots_total": total,
                "slots_used": used,
                "slots_free": total.saturating_sub(used),
            })),
        ),
        Err(LaunchError::Draining) => error_body(
            StatusCode::SERVICE_UNAVAILABLE,
            "draining",
            Some(json!({"draining": true})),
        ),
        Err(LaunchError::StartFailed { id }) => error_body(
            StatusCode::BAD_GATEWAY,
            "failed to start container",
            Some(json!({"id": id, "reason": "start_failed"})),
        ),
        Err(LaunchError::Unavailable(msg)) => error_body(StatusCode::BAD_GATEWAY, &msg, None),
    }
}

async fn job_status(
    State(launcher): State<Arc<Launcher>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<LogsQuery>,
) -> Response {
    if !authorized(&headers, &launcher) {
        return unauthorized();
    }
    let with_logs = query
        .logs
        .as_deref()
        .is_some_and(|value| value == "true" || value == "1");
    match launcher.job(&id, with_logs).await {
        Ok(Some(job)) => (StatusCode::OK, Json(job_json(&job, with_logs))).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"id": id, "status": "not_found"})),
        )
            .into_response(),
        Err(LaunchError::Unavailable(msg)) => error_body(StatusCode::BAD_GATEWAY, &msg, None),
        Err(other) => error_body(StatusCode::BAD_GATEWAY, &other.to_string(), None),
    }
}

async fn set_drain(
    State(launcher): State<Arc<Launcher>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorized(&headers, &launcher) {
        return unauthorized();
    }
    let parsed: DrainBody = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return error_body(StatusCode::BAD_REQUEST, "invalid JSON", None),
    };
    match launcher.set_draining(parsed.draining) {
        Ok(draining) => (StatusCode::OK, Json(json!({"draining": draining}))).into_response(),
        Err(LaunchError::Unavailable(msg)) => error_body(StatusCode::BAD_GATEWAY, &msg, None),
        Err(other) => error_body(StatusCode::BAD_GATEWAY, &other.to_string(), None),
    }
}

fn job_json(job: &JobRecord, with_logs: bool) -> Value {
    let mut body = json!({
        "id": job.id,
        "status": job.status(),
        "image": job.image,
        "job_key": job.job_key,
        "started_at": job.started_at,
        "ended_at": job.ended_at,
        "exit_code": job.exit_code,
        "reason": job.reason,
        "duration_secs": job.duration_secs(),
    });
    if with_logs {
        body["logs"] = Value::String(job.logs.clone().unwrap_or_default());
    }
    body
}

fn authorized(headers: &HeaderMap, launcher: &Launcher) -> bool {
    let provided = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(crate::auth::bearer_token)
        .unwrap_or("");
    launcher.token_matches(provided)
}

fn unauthorized() -> Response {
    let mut response = (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "unauthorized"})),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

fn error_body(status: StatusCode, error: &str, extra: Option<Value>) -> Response {
    let mut body = json!({"error": error});
    if let Some(extra) = extra {
        if let (Some(obj), Some(more)) = (body.as_object_mut(), extra.as_object()) {
            for (key, value) in more {
                obj.insert(key.clone(), value.clone());
            }
        }
    }
    (status, Json(body)).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunBody {
    image: String,
    #[serde(default)]
    env: HashMap<String, String>,
    timeout_secs: Option<u64>,
    cpu: Option<f64>,
    memory: Option<ByteQty>,
    shm: Option<ByteQty>,
    command: Option<Vec<String>>,
    job_key: Option<String>,
    callback_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DrainBody {
    draining: bool,
}

#[derive(Debug, Deserialize, Default)]
struct LogsQuery {
    logs: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::config::Settings;
    use crate::mock::MockRuntime;
    use crate::runtime::Runtime;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    struct Harness {
        app: Router,
        launcher: Arc<Launcher>,
        mock: Arc<MockRuntime>,
        clock: Arc<FakeClock>,
    }

    fn settings(slots: usize) -> Settings {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("fairlead-api-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Settings::from_toml_and_env(
            &format!(
                r#"
                token = "test-token-value-ok"
                max_slots = {slots}
                allowlist = ["alpine", "ghcr.io/owner/selenium-worker"]
                default_timeout_secs = 30
                max_timeout_secs = 60
                history_path = "{}"
                "#,
                dir.join("jobs.sqlite").display()
            ),
            &HashMap::new(),
        )
        .unwrap()
    }

    fn harness(slots: usize) -> Harness {
        let mock = Arc::new(MockRuntime::new());
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let runtime: Arc<dyn Runtime> = mock.clone();
        let launcher = Launcher::with_clock(settings(slots), runtime, clock.clone()).unwrap();
        let app = router(launcher.clone());
        Harness {
            app,
            launcher,
            mock,
            clock,
        }
    }

    fn post(body: &str, token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/jobs")
            .header("content-type", "application/json");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder.body(Body::from(body.to_string())).unwrap()
    }

    async fn json_of(response: Response) -> (StatusCode, Value) {
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()))
        };
        (status, value)
    }

    const ALPINE: &str = r#"{"image":"alpine","command":["sleep","30"]}"#;
    const TOKEN: &str = "test-token-value-ok";

    #[tokio::test]
    async fn auth_allowlist_health_and_metrics() {
        let h = harness(2);

        let (status, body) =
            json_of(h.app.clone().oneshot(post(ALPINE, None)).await.unwrap()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "unauthorized");

        let (status, _) = json_of(
            h.app
                .clone()
                .oneshot(post(ALPINE, Some("wrong-token-value")))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let forbidden = r#"{"image":"redis:7"}"#;
        let (status, _) = json_of(
            h.app
                .clone()
                .oneshot(post(forbidden, Some("wrong-token-value")))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, body) = json_of(
            h.app
                .clone()
                .oneshot(post(forbidden, Some(TOKEN)))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "image is not allowlisted");
        assert!(h.mock.pulls().await.is_empty());

        let neighbor = r#"{"image":"ghcr.io/owner/selenium-worker-evil:1"}"#;
        let (status, _) = json_of(
            h.app
                .clone()
                .oneshot(post(neighbor, Some(TOKEN)))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let tagged = r#"{"image":"ghcr.io/owner/selenium-worker:1.2.3","env":{"JOB":"a"},"shm":"64m","memory":"512m","cpu":0.5}"#;
        let (status, body) = json_of(
            h.app
                .clone()
                .oneshot(post(tagged, Some(TOKEN)))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let id = body["id"].as_str().unwrap().to_string();

        let (status, got) = json_of(
            h.app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/v1/jobs/{id}"))
                        .header("authorization", format!("Bearer {TOKEN}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(got["status"], "running");
        assert!(got.get("logs").is_none());

        let (status, with_logs) = json_of(
            h.app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/v1/jobs/{id}?logs=true"))
                        .header("authorization", format!("Bearer {TOKEN}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(with_logs["logs"].as_str().unwrap().contains("log "));

        let (status, health) = json_of(
            h.app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/health")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(health["draining"], false);
        assert_eq!(health["slots_free"], 1);

        let (status, text) = json_of(
            h.app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/metrics")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let text = text.as_str().unwrap();
        assert!(text.contains("fairlead_slots_total 2"));
        assert!(text.contains("fairlead_slots_used 1"));
        assert!(text.contains("fairlead_draining 0"));
        assert!(text.contains("fairlead_jobs_started_total 1"));

        let too_long = r#"{"image":"alpine","timeout_secs":61}"#;
        let (status, _) = json_of(
            h.app
                .clone()
                .oneshot(post(too_long, Some(TOKEN)))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn concurrent_posts_cannot_pass_the_slot_limit() {
        let h = harness(2);
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let app = h.app.clone();
            tasks.push(tokio::spawn(async move {
                let response = app.oneshot(post(ALPINE, Some(TOKEN))).await.unwrap();
                response.status()
            }));
        }
        let mut accepted = 0;
        let mut rejected = 0;
        for task in tasks {
            match task.await.unwrap() {
                StatusCode::ACCEPTED => accepted += 1,
                StatusCode::TOO_MANY_REQUESTS => rejected += 1,
                other => panic!("unexpected status: {other}"),
            }
        }
        assert_eq!(accepted, 2);
        assert_eq!(rejected, 6);
        assert_eq!(h.mock.running_ids().await.len(), 2);
    }

    #[tokio::test]
    async fn idempotent_key_drain_and_callback() {
        let h = harness(2);
        let hits = Arc::new(tokio::sync::Mutex::new(None));
        let state = hits.clone();
        let hook = Router::new().route(
            "/hook",
            axum::routing::post(move |body: Bytes| {
                let state = state.clone();
                async move {
                    let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    *state.lock().await = Some(value);
                    StatusCode::NO_CONTENT
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, hook).await.unwrap();
        });

        let body = format!(
            r#"{{"image":"alpine","command":["sleep","30"],"job_key":"same-key","callback_url":"http://127.0.0.1:{port}/hook"}}"#
        );
        let (status, first) = json_of(
            h.app
                .clone()
                .oneshot(post(&body, Some(TOKEN)))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let id = first["id"].as_str().unwrap().to_string();

        let (status, second) = json_of(
            h.app
                .clone()
                .oneshot(post(&body, Some(TOKEN)))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(second["id"], id);
        assert_eq!(h.mock.start_count().await, 1);

        let (status, _) = json_of(
            h.app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/drain")
                        .header("authorization", format!("Bearer {TOKEN}"))
                        .header("content-type", "application/json")
                        .body(Body::from(r#"{"draining":true}"#))
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, denied) = json_of(
            h.app
                .clone()
                .oneshot(post(ALPINE, Some(TOKEN)))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(denied["error"], "draining");
        let (status, health) = json_of(
            h.app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/health")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(health["status"], "draining");
        assert_eq!(health["draining"], true);

        h.clock.set(1_700_000_000 + 30);
        h.launcher.reap_once().await.unwrap();
        let mut saw = None;
        for _ in 0..50 {
            if let Some(body) = hits.lock().await.clone() {
                saw = Some(body);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let saw = saw.expect("callback");
        assert_eq!(saw["id"], id);
        assert_eq!(saw["reason"], "timeout_kill");
        assert_eq!(saw["exit_code"], 137);
        assert!(saw["duration_secs"].as_i64().unwrap() >= 30);

        let (status, metrics) = json_of(
            h.app
                .oneshot(
                    Request::builder()
                        .uri("/metrics")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(metrics
            .as_str()
            .unwrap()
            .contains("fairlead_jobs_timeout_total 1"));
        assert!(metrics.as_str().unwrap().contains("fairlead_draining 1"));
    }
}
