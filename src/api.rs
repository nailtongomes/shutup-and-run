use crate::bytesize::ByteQty;
use crate::launcher::{metrics_text, LaunchError, LaunchOutcome, LaunchRequest, Launcher};
use crate::store::JobRecord;
use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::{from_fn_with_state, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

const MAX_BODY: usize = 64 * 1024;

pub fn router(launcher: Arc<Launcher>) -> Router {
    let api = api_router(launcher.clone());
    if launcher.settings().status_page {
        // Its own router: the status page has no token and its own gate.
        api.merge(crate::status::router(launcher))
    } else {
        api
    }
}

fn api_router(launcher: Arc<Launcher>) -> Router {
    let mut router = Router::new();
    // A server's own: elsewhere the cleanup runs by itself once a day, and
    // the daemon's other images and volumes are its owner's.
    if launcher.settings().server {
        router = router.route("/v1/maintenance/cleanup", post(cleanup));
    }
    router
        .route("/health", get(health))
        .route("/v1/images/pull", post(prepare_image).get(image_state))
        .route("/metrics", get(metrics))
        .route("/v1/jobs", post(create_job).get(list_jobs))
        .route("/v1/jobs/{id}", get(job_status).delete(cancel_job))
        .route("/v1/drain", post(set_drain))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY))
        .layer(from_fn_with_state(launcher.clone(), require_auth))
        .with_state(launcher)
}

async fn require_auth(
    State(launcher): State<Arc<Launcher>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let ip = client_ip(&request, launcher.settings().trust_forwarded);
    match launcher.ban_until(&ip) {
        Ok(Some(_)) => return StatusCode::FORBIDDEN.into_response(),
        Ok(None) => {}
        Err(_) => {
            return error_body(StatusCode::BAD_GATEWAY, "job history is unavailable", None);
        }
    }
    if !authorized(request.headers(), &launcher) {
        match launcher.note_auth_failure(&ip) {
            Ok(crate::store::Strike::Banned { until }) => {
                tracing::warn!(%ip, until, "banned after repeated authentication failures");
            }
            Ok(crate::store::Strike::Failed { count }) => {
                tracing::warn!(%ip, count, "authentication failed");
            }
            Err(err) => tracing::warn!(error = %err, "could not record an authentication failure"),
        }
        return unauthorized();
    }
    if let Err(err) = launcher.clear_auth_failures(&ip) {
        tracing::warn!(error = %err, "could not clear authentication failures");
    }
    next.run(request).await
}

fn client_ip(request: &Request<Body>, trust_forwarded: bool) -> String {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| normalize_ip(info.0.ip()))
        .unwrap_or_else(|| "0.0.0.0".into());
    if !trust_forwarded {
        return peer;
    }
    let Some(raw) = request
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
    else {
        return peer;
    };
    let first = raw.split(',').next().unwrap_or("").trim();
    first.parse::<IpAddr>().map(normalize_ip).unwrap_or(peer)
}

fn normalize_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(|v4| v4.to_string())
            .unwrap_or_else(|| v6.to_string()),
        other => other.to_string(),
    }
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
                "mode": if launcher.managed_mode().unwrap_or(false) { "managed" } else { "jobs" },
                "machine": launcher.machine_health(),
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
    let more = launcher.metrics_more().await.unwrap_or_default();
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        format!("{}{more}", metrics_text(slots, counts)),
    )
        .into_response()
}

async fn create_job(State(launcher): State<Arc<Launcher>>, body: Bytes) -> Response {
    let request = match launch_request_from_slice(&body) {
        Ok(request) => request,
        Err(_) => {
            return error_body(
                StatusCode::BAD_REQUEST,
                "invalid JSON",
                Some(json!({"reason": "bad_request"})),
            )
        }
    };
    match launcher.launch(request).await {
        Ok(LaunchOutcome::Created(job)) => {
            (StatusCode::ACCEPTED, Json(job_json(&job, false))).into_response()
        }
        Ok(LaunchOutcome::Existing(job)) => {
            (StatusCode::OK, Json(job_json(&job, false))).into_response()
        }
        Err(err) => refusal(err),
    }
}

/// A refused push: its status, its message, and `reason`, the stable word
/// of each refusal (`no_slots`, `image_not_present`, ...), the label of
/// `shutup_and_run_refusals_total`.
fn refusal(err: LaunchError) -> Response {
    let reason = crate::launcher::refusal_reason(&err);
    let body = |status: StatusCode, error: &str, extra: Option<Value>| {
        let mut extra = extra.unwrap_or_else(|| json!({}));
        extra["reason"] = json!(reason);
        error_body(status, error, Some(extra))
    };
    match err {
        LaunchError::BadRequest(msg) => body(StatusCode::BAD_REQUEST, &msg, None),
        LaunchError::Forbidden => body(StatusCode::FORBIDDEN, "image is not allowlisted", None),
        LaunchError::NoSlots { total, used } => body(
            StatusCode::TOO_MANY_REQUESTS,
            "no free slots",
            Some(json!({
                "slots_total": total,
                "slots_used": used,
                "slots_free": total.saturating_sub(used),
            })),
        ),
        LaunchError::Draining => body(
            StatusCode::SERVICE_UNAVAILABLE,
            "draining",
            Some(json!({"draining": true})),
        ),
        LaunchError::LowResources(shortage) => body(
            StatusCode::SERVICE_UNAVAILABLE,
            "insufficient resources",
            Some(json!({
                "resource": shortage.resource,
                "free_bytes": shortage.free_bytes,
                "needed_bytes": shortage.needed_bytes,
            })),
        ),
        LaunchError::Committed(committed) => body(
            StatusCode::SERVICE_UNAVAILABLE,
            "memory committed",
            Some(json!({
                "code": "JOBS_MEMORY_COMMITTED",
                "committed_bytes": committed.committed_bytes,
                "quota_bytes": committed.quota_bytes,
                "needed_bytes": committed.needed_bytes,
            })),
        ),
        // Too many of this caller's own: 429 like a full machine, with the
        // key, so the caller puts this one back in its queue.
        LaunchError::KeyBusy(busy) => body(
            StatusCode::TOO_MANY_REQUESTS,
            "concurrency key busy",
            Some(json!({
                "code": "CONCURRENCY_KEY_BUSY",
                "concurrency_key": busy.concurrency_key,
                "running": busy.running,
                "limit": busy.limit,
            })),
        ),
        LaunchError::UserActive(active) => body(
            StatusCode::SERVICE_UNAVAILABLE,
            "user active",
            Some(json!({
                "idle_secs": active.idle_secs,
                "needed_secs": active.needed_secs,
            })),
        ),
        LaunchError::NotLending(hold) => body(
            StatusCode::SERVICE_UNAVAILABLE,
            "not lending now",
            Some(json!({"code": hold.code, "detail": hold.detail})),
        ),
        // Both are "not now, on this machine": 503, like the other waits. The
        // caller cannot lift either; the second needs a person at the machine.
        LaunchError::SafetyBlocked(reasons) => body(
            StatusCode::SERVICE_UNAVAILABLE,
            "blocked for safety",
            Some(json!({"code": "SAFETY_BLOCKED", "reasons": reasons})),
        ),
        LaunchError::AuthorizationRequired(reasons) => body(
            StatusCode::SERVICE_UNAVAILABLE,
            "authorization required",
            Some(json!({
                "code": crate::safety::AUTHORIZATION_REQUIRED,
                "notice_version": crate::safety::NOTICE_VERSION,
                "reasons": reasons,
            })),
        ),
        // Not "later": the machine was set to run its own containers, and
        // only a person at it changes that.
        LaunchError::ManagedMode => body(
            StatusCode::CONFLICT,
            "managed mode",
            Some(json!({"code": "MANAGED_MODE"})),
        ),
        err @ (LaunchError::JobsMode | LaunchError::AlreadyRunning) => {
            body(StatusCode::BAD_GATEWAY, &err.to_string(), None)
        }
        LaunchError::Unavailable(msg) => body(StatusCode::BAD_GATEWAY, &msg, None),
        // The machine is set to run only images prepared on it, and this
        // one is not: prepare it, then send the job again.
        LaunchError::ImageNotPresent(image) => body(
            StatusCode::CONFLICT,
            "image not present",
            Some(json!({"code": "IMAGE_NOT_PRESENT", "image": image})),
        ),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageBody {
    image: String,
}

#[derive(Debug, Deserialize)]
struct ImageQuery {
    image: Option<String>,
}

/// `POST /v1/images/pull` `{"image": "..."}`: starts pulling an allowlisted
/// image for the jobs to come and answers at once, `202`. Asked again
/// while it pulls, it answers the same pull.
async fn prepare_image(State(launcher): State<Arc<Launcher>>, body: Bytes) -> Response {
    let Ok(ImageBody { image }) = serde_json::from_slice::<ImageBody>(&body) else {
        return error_body(StatusCode::BAD_REQUEST, "invalid JSON", None);
    };
    match launcher.prepare_image(&image) {
        Ok(prepare) => (StatusCode::ACCEPTED, Json(json!(prepare))).into_response(),
        Err(LaunchError::Forbidden) => {
            error_body(StatusCode::FORBIDDEN, "image is not allowlisted", None)
        }
        Err(err) => error_body(StatusCode::BAD_REQUEST, &err.to_string(), None),
    }
}

/// `GET /v1/images/pull?image=...`: `pulling`, `ready`, `failed` (with
/// `error`) or `absent`.
async fn image_state(
    State(launcher): State<Arc<Launcher>>,
    Query(query): Query<ImageQuery>,
) -> Response {
    let Some(image) = query.image else {
        return error_body(StatusCode::BAD_REQUEST, "image is required", None);
    };
    match launcher.image_state(&image).await {
        Ok(prepare) => (StatusCode::OK, Json(json!(prepare))).into_response(),
        Err(err) => error_body(StatusCode::BAD_REQUEST, &err.to_string(), None),
    }
}

/// `POST /v1/maintenance/cleanup` (server only): the cleanup, now, on an
/// idle machine; `409` `JOBS_RUNNING` while anything runs, with nothing
/// done. No job is admitted while it lasts.
async fn cleanup(State(launcher): State<Arc<Launcher>>) -> Response {
    match launcher.clean_now().await {
        Ok(Ok(cleaned)) => (StatusCode::OK, Json(json!(cleaned))).into_response(),
        Ok(Err(running)) => error_body(
            StatusCode::CONFLICT,
            "jobs running",
            Some(json!({"code": "JOBS_RUNNING", "running": running})),
        ),
        Err(err) => error_body(StatusCode::BAD_GATEWAY, &err.to_string(), None),
    }
}

async fn job_status(
    State(launcher): State<Arc<Launcher>>,
    Path(id): Path<String>,
    Query(query): Query<LogsQuery>,
) -> Response {
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

/// `DELETE /v1/jobs/{id}`: SIGTERM, SIGKILL 10 seconds later, `reason`
/// `launcher_kill` and the callback. A job that has not started yet ends
/// before it starts. A job that already ended is answered as it is.
async fn cancel_job(State(launcher): State<Arc<Launcher>>, Path(id): Path<String>) -> Response {
    if !crate::launcher::valid_container_id(&id) {
        return not_found(&id);
    }
    // In managed mode every job is a managed run, stopped on the machine.
    if launcher.managed_mode().unwrap_or(false) {
        return error_body(
            StatusCode::CONFLICT,
            "managed mode",
            Some(json!({"code": "MANAGED_MODE"})),
        );
    }
    if let Err(err) = launcher.stop(&id).await {
        return error_body(StatusCode::BAD_GATEWAY, &err.to_string(), None);
    }
    match launcher.job(&id, false).await {
        Ok(Some(job)) if job.is_finished() => {
            (StatusCode::OK, Json(job_json(&job, false))).into_response()
        }
        // Stopped while it waited or pulled: it ends in a moment.
        Ok(Some(job)) => (StatusCode::ACCEPTED, Json(job_json(&job, false))).into_response(),
        Ok(None) => not_found(&id),
        Err(err) => error_body(StatusCode::BAD_GATEWAY, &err.to_string(), None),
    }
}

fn not_found(id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"id": id, "status": "not_found"})),
    )
        .into_response()
}

/// `GET /v1/jobs?state=running`: the jobs that have not ended, oldest
/// first, with what each used so far. `running` is the only state.
async fn list_jobs(
    State(launcher): State<Arc<Launcher>>,
    Query(query): Query<ListQuery>,
) -> Response {
    if !matches!(query.state.as_deref(), None | Some("running")) {
        return error_body(StatusCode::BAD_REQUEST, "state must be running", None);
    }
    let slots = match launcher.slots().await {
        Ok(slots) => slots,
        Err(err) => return error_body(StatusCode::BAD_GATEWAY, &err.to_string(), None),
    };
    let rows = match launcher.unfinished().await {
        Ok(rows) => rows,
        Err(err) => return error_body(StatusCode::BAD_GATEWAY, &err.to_string(), None),
    };
    let jobs: Vec<Value> = rows
        .iter()
        .map(|(row, starting)| {
            let job = &row.job;
            json!({
                "id": job.id,
                "image": job.image,
                "job_key": job.job_key,
                "concurrency_key": row.concurrency_key,
                "phase": if *starting { "starting" } else { "running" },
                "started_at": job.started_at,
                "cpu_ms": job.cpu_ms,
                "memory_peak_bytes": job.memory_peak,
                "network_bytes": job.network_bytes,
                "pull_done_bytes": job.pull.map(|(done, _)| done),
                "pull_total_bytes": job.pull.map(|(_, total)| total),
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(json!({
            "state": "running",
            "slots_total": slots.total,
            "slots_used": slots.used,
            "slots_free": slots.free,
            "jobs": jobs,
        })),
    )
        .into_response()
}

async fn set_drain(State(launcher): State<Arc<Launcher>>, body: Bytes) -> Response {
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
    if job.oom_killed {
        body["oom_killed"] = Value::Bool(true);
    }
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
    concurrency_key: Option<String>,
}

pub(crate) fn launch_request_from_slice(bytes: &[u8]) -> Result<LaunchRequest, serde_json::Error> {
    launch_request_from_value(serde_json::from_slice(bytes)?)
}

pub(crate) fn launch_request_from_value(value: Value) -> Result<LaunchRequest, serde_json::Error> {
    let parsed: RunBody = serde_json::from_value(value)?;
    Ok(LaunchRequest {
        image: parsed.image,
        env: parsed.env,
        timeout_secs: parsed.timeout_secs,
        cpu: parsed.cpu,
        memory_bytes: parsed.memory.map(ByteQty::get),
        shm_bytes: parsed.shm.map(ByteQty::get),
        command: parsed.command,
        job_key: parsed.job_key,
        callback_url: parsed.callback_url,
        concurrency_key: parsed.concurrency_key,
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DrainBody {
    draining: bool,
}

#[derive(Debug, Deserialize, Default)]
struct ListQuery {
    state: Option<String>,
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
        settings_extra(slots, "auth_failures = 30")
    }

    fn settings_extra(slots: usize, extra: &str) -> Settings {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("shutup-and-run-api-{n}-{}", std::process::id()));
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
                history_path = '{}'
                resource_profile = "none"
                {extra}
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
        post_from(body, token, [203, 0, 113, 10])
    }

    fn post_from(body: &str, token: Option<&str>, ip: [u8; 4]) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/jobs")
            .header("content-type", "application/json");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        stamp(builder.body(Body::from(body.to_string())).unwrap(), ip)
    }

    fn get_from(uri: &str, token: Option<&str>, ip: [u8; 4]) -> Request<Body> {
        let mut builder = Request::builder().uri(uri);
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        stamp(builder.body(Body::empty()).unwrap(), ip)
    }

    fn stamp(mut request: Request<Body>, ip: [u8; 4]) -> Request<Body> {
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                ip, 9,
            ))));
        request
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

        let (status, _) = json_of(
            h.app
                .clone()
                .oneshot(get_from("/health", None, [203, 0, 113, 11]))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

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

        h.launcher.settled().await;
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
                .oneshot(get_from("/health", Some(TOKEN), [203, 0, 113, 10]))
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
                .oneshot(get_from("/metrics", Some(TOKEN), [203, 0, 113, 10]))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let text = text.as_str().unwrap();
        assert!(text.contains("shutup_and_run_slots_total 2"));
        assert!(text.contains("shutup_and_run_slots_used 1"));
        assert!(text.contains("shutup_and_run_draining 0"));
        assert!(text.contains("shutup_and_run_jobs_started_total 1"));

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
        h.launcher.settled().await;
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
        h.launcher.settled().await;
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
                .oneshot(get_from("/health", Some(TOKEN), [203, 0, 113, 10]))
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
                .oneshot(get_from("/metrics", Some(TOKEN), [203, 0, 113, 10]))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(metrics
            .as_str()
            .unwrap()
            .contains("shutup_and_run_jobs_timeout_total 1"));
        assert!(metrics
            .as_str()
            .unwrap()
            .contains("shutup_and_run_draining 1"));
    }

    #[tokio::test]
    async fn ban_after_three_failures_persists_and_expires() {
        let cfg = settings_extra(1, "auth_failures = 3\nban_hours = 1");
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new());
        let launcher = Launcher::with_clock(cfg, runtime, clock.clone()).unwrap();
        let app = router(launcher.clone());
        let ip = [203, 0, 113, 77];
        for _ in 0..3 {
            let (status, body) = json_of(
                app.clone()
                    .oneshot(post_from(ALPINE, Some("wrong-token-value"), ip))
                    .await
                    .unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_eq!(body["error"], "unauthorized");
        }
        let (status, body) = json_of(
            app.clone()
                .oneshot(post_from(ALPINE, Some(TOKEN), ip))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.is_null(), "a ban is an empty 403");

        let mut other = get_from("/health", Some(TOKEN), [203, 0, 113, 78]);
        other
            .headers_mut()
            .insert("x-forwarded-for", "203.0.113.77".parse().unwrap());
        let (status, _) = json_of(app.clone().oneshot(other).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK);

        let runtime2: Arc<dyn Runtime> = Arc::new(MockRuntime::new());
        let restarted =
            Launcher::with_clock(launcher.settings().clone(), runtime2, clock.clone()).unwrap();
        let app2 = router(restarted);
        let (status, _) = json_of(
            app2.clone()
                .oneshot(post_from(ALPINE, Some(TOKEN), ip))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        clock.set(1_700_000_000 + 3600 + 1);
        let (status, body) = json_of(
            app2.oneshot(get_from("/health", Some(TOKEN), ip))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    fn local(
        method: &str,
        uri: &str,
        ip: [u8; 4],
        host: &str,
        origin: Option<&str>,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", host);
        if let Some(origin) = origin {
            builder = builder.header("origin", origin);
        }
        stamp(builder.body(Body::empty()).unwrap(), ip)
    }

    #[tokio::test]
    async fn the_local_page_lists_job_logs_and_serves_them_only_to_this_machine() {
        const LO: [u8; 4] = [127, 0, 0, 1];
        const HOST: &str = "127.0.0.1:8787";
        let mock = Arc::new(MockRuntime::new());
        let runtime: Arc<dyn Runtime> = mock.clone();
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let cfg = settings_extra(1, "status_page = true");
        let launcher = Launcher::with_clock(cfg, runtime, clock).unwrap();
        let app = router(launcher.clone());
        let id = match launcher
            .launch(crate::launcher::LaunchRequest {
                image: "alpine".into(),
                env: HashMap::new(),
                timeout_secs: None,
                cpu: None,
                memory_bytes: None,
                shm_bytes: None,
                command: None,
                job_key: None,
                callback_url: None,
                concurrency_key: None,
            })
            .await
            .unwrap()
        {
            crate::launcher::LaunchOutcome::Created(job)
            | crate::launcher::LaunchOutcome::Existing(job) => job.id,
        };
        launcher.settled().await;
        mock.set_output(&id, "line one\n").await;

        let get = |uri: String, ip: [u8; 4]| {
            let app = app.clone();
            async move {
                let response = app
                    .oneshot(local("GET", &uri, ip, HOST, None))
                    .await
                    .unwrap();
                let status = response.status();
                let disposition = response
                    .headers()
                    .get("content-disposition")
                    .map(|v| v.to_str().unwrap().to_string());
                let bytes = response.into_body().collect().await.unwrap().to_bytes();
                (
                    status,
                    disposition,
                    String::from_utf8_lossy(&bytes).to_string(),
                )
            }
        };
        // Running: no file yet; what it printed so far, from Docker.
        let (_, _, list) = get("/local/logs".into(), LO).await;
        assert!(
            list.contains(&format!("<a href=\"/local/logs/{id}\">so far</a>")),
            "{list}"
        );
        let (status, _, text) = get(format!("/local/logs/{id}"), LO).await;
        assert_eq!((status, text.as_str()), (StatusCode::OK, "line one\n"));
        mock.set_output(&id, "line one\nresult: 42\n").await;
        mock.stop(&id, 0).await;
        launcher.on_exit(&id, 0).await;

        let (status, _, home) = get("/".into(), LO).await;
        assert_eq!(status, StatusCode::OK);
        assert!(home.contains("/local/logs"));
        let (status, _, list) = get("/local/logs".into(), LO).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            list.contains(&format!("/local/logs/{id}?download=1")),
            "{list}"
        );
        assert!(list.contains("alpine") && list.contains("ok"));
        let (status, disposition, text) = get(format!("/local/logs/{id}"), LO).await;
        assert_eq!(
            (status, disposition.as_deref()),
            (StatusCode::OK, Some("inline"))
        );
        assert_eq!(text, "line one\nresult: 42\n");
        let (_, disposition, _) = get(format!("/local/logs/{id}?download=1"), LO).await;
        assert!(disposition.unwrap().starts_with("attachment;"));
        // Not from the network, and only a job id names a file.
        let (status, _, _) = get(format!("/local/logs/{id}"), [203, 0, 113, 9]).await;
        assert_ne!(status, StatusCode::OK);
        let (status, _, _) = get("/local/logs/..%2F..%2Fjobs.sqlite".into(), LO).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _, _) = get("/local/logs/abc123".into(), LO).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// A stand-in for noVNC: any GET answers with its own path, and an
    /// upgrade on /websockify answers 101, speaks first like a VNC server,
    /// then echoes.
    async fn fake_novnc() -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        if stream.read(&mut byte).await.unwrap_or(0) == 0 {
                            return;
                        }
                        head.push(byte[0]);
                    }
                    let head = String::from_utf8_lossy(&head).to_lowercase();
                    let path = head.split(' ').nth(1).unwrap_or("").to_string();
                    if head.contains("upgrade: websocket") {
                        assert_eq!(path, "/websockify");
                        assert!(head.contains("sec-websocket-key: a2v5"), "{head}");
                        assert!(!head.contains("origin"), "{head}");
                        let _ = stream
                            .write_all(
                                b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                                  Connection: Upgrade\r\nSec-WebSocket-Accept: fake-accept\r\n\
                                  Sec-WebSocket-Protocol: binary\r\n\r\nRFB 003.008\n",
                            )
                            .await;
                        let mut buffer = [0u8; 64];
                        while let Ok(read) = stream.read(&mut buffer).await {
                            if read == 0 || stream.write_all(&buffer[..read]).await.is_err() {
                                return;
                            }
                        }
                    } else {
                        let body = format!("novnc {path}");
                        let _ = stream
                            .write_all(
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                    body.len()
                                )
                                .as_bytes(),
                            )
                            .await;
                    }
                });
            }
        });
        address
    }

    #[tokio::test]
    async fn the_page_shows_a_running_jobs_screen_only_to_this_machine() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        const LO: [u8; 4] = [127, 0, 0, 1];
        let novnc = fake_novnc().await;
        let mock = Arc::new(MockRuntime::new());
        let runtime: Arc<dyn Runtime> = mock.clone();
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let cfg = settings_extra(1, "status_page = true\nscreen_view = true");
        let launcher = Launcher::with_clock(cfg, runtime, clock).unwrap();
        let app = router(launcher.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = listener.local_addr().unwrap().to_string();
        let served = app.clone();
        tokio::spawn(async move {
            axum::serve(
                listener,
                served.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let id = match launcher
            .launch(crate::launcher::LaunchRequest {
                image: "alpine".into(),
                env: HashMap::new(),
                timeout_secs: None,
                cpu: None,
                memory_bytes: None,
                shm_bytes: None,
                command: None,
                job_key: None,
                callback_url: None,
                concurrency_key: None,
            })
            .await
            .unwrap()
        {
            crate::launcher::LaunchOutcome::Created(job)
            | crate::launcher::LaunchOutcome::Existing(job) => job.id,
        };
        launcher.settled().await;
        let get = |uri: String, ip: [u8; 4], origin: Option<&'static str>, upgrade: bool| {
            let app = app.clone();
            let host = host.clone();
            async move {
                let mut request = local("GET", &uri, ip, &host, origin);
                if upgrade {
                    let headers = request.headers_mut();
                    headers.insert("upgrade", "websocket".parse().unwrap());
                    headers.insert("connection", "Upgrade".parse().unwrap());
                }
                let response = app.oneshot(request).await.unwrap();
                let status = response.status();
                let location = response
                    .headers()
                    .get("location")
                    .map(|v| v.to_str().unwrap().to_string());
                let bytes = response.into_body().collect().await.unwrap().to_bytes();
                (
                    status,
                    location,
                    String::from_utf8_lossy(&bytes).to_string(),
                )
            }
        };

        // No screen yet: no link, nothing behind the route.
        let (_, _, home) = get("/".into(), LO, None, false).await;
        assert!(!home.contains("/local/screen/"), "{home}");
        let (status, _, _) = get(format!("/local/screen/{id}"), LO, None, false).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        mock.set_screen(&id, novnc).await;
        let (_, _, home) = get("/".into(), LO, None, false).await;
        assert!(
            home.contains(&format!("<a href=\"/local/screen/{id}\""))
                && home.contains("View screen"),
            "{home}"
        );
        let (_, _, logs) = get("/local/logs".into(), LO, None, false).await;
        assert!(logs.contains(&format!("/local/screen/{id}")), "{logs}");
        let (status, location, _) = get(format!("/local/screen/{id}"), LO, None, false).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(
            location.unwrap(),
            format!("/local/screen/{id}/vnc.html?path=../../../local/screen/{id}/websockify&autoconnect=1&view_only=1&resize=scale")
        );
        let (status, _, body) =
            get(format!("/local/screen/{id}/app/ui.js?v=1"), LO, None, false).await;
        assert_eq!(
            (status, body.as_str()),
            (StatusCode::OK, "novnc /app/ui.js?v=1")
        );

        // Not from the network, not another job, not a made-up id.
        let (status, _, _) = get(
            format!("/local/screen/{id}/vnc.html"),
            [203, 0, 113, 9],
            None,
            false,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _, _) = get("/local/screen/abcdef0123/vnc.html".into(), LO, None, false).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _, _) = get(
            "/local/screen/..%2F..%2Fetc/vnc.html".into(),
            LO,
            None,
            false,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        // A WebSocket only from the page's own origin: any site may open one
        // to 127.0.0.1.
        let ws = format!("/local/screen/{id}/websockify");
        let (status, _, _) = get(ws.clone(), LO, Some("http://evil.example"), true).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _, _) = get(ws.clone(), LO, None, true).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // The real thing, over a socket: the 101, what the server says first,
        // then both ways.
        let mut socket = tokio::net::TcpStream::connect(&host).await.unwrap();
        socket
            .write_all(
                format!(
                    "GET {ws} HTTP/1.1\r\nHost: {host}\r\nOrigin: http://{host}\r\n\
                     Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: a2V5\r\n\
                     Sec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: binary\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut seen = Vec::new();
        let mut buffer = [0u8; 256];
        while !String::from_utf8_lossy(&seen).contains("RFB 003.008\n") {
            let read = socket.read(&mut buffer).await.unwrap();
            assert!(read > 0, "{}", String::from_utf8_lossy(&seen));
            seen.extend_from_slice(&buffer[..read]);
        }
        let answer = String::from_utf8_lossy(&seen).to_lowercase();
        assert!(answer.starts_with("http/1.1 101"), "{answer}");
        assert!(
            answer.contains("sec-websocket-accept: fake-accept"),
            "{answer}"
        );
        assert!(
            answer.contains("sec-websocket-protocol: binary"),
            "{answer}"
        );
        socket.write_all(b"ping").await.unwrap();
        let read = socket.read(&mut buffer).await.unwrap();
        assert_eq!(&buffer[..read], b"ping");

        // The job ends: the route and the link go with it.
        mock.stop(&id, 0).await;
        launcher.on_exit(&id, 0).await;
        let (status, _, _) = get(format!("/local/screen/{id}/vnc.html"), LO, None, false).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (_, _, home) = get("/".into(), LO, None, false).await;
        assert!(!home.contains("/local/screen/"), "{home}");
    }

    #[tokio::test]
    async fn without_screen_view_a_running_jobs_screen_is_not_reachable() {
        const LO: [u8; 4] = [127, 0, 0, 1];
        const HOST: &str = "127.0.0.1:8787";
        let novnc = fake_novnc().await;
        let mock = Arc::new(MockRuntime::new());
        let runtime: Arc<dyn Runtime> = mock.clone();
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let cfg = settings_extra(1, "status_page = true");
        let launcher = Launcher::with_clock(cfg, runtime, clock).unwrap();
        let app = router(launcher.clone());
        let id = match launcher
            .launch(crate::launcher::LaunchRequest {
                image: "alpine".into(),
                env: HashMap::new(),
                timeout_secs: None,
                cpu: None,
                memory_bytes: None,
                shm_bytes: None,
                command: None,
                job_key: None,
                callback_url: None,
                concurrency_key: None,
            })
            .await
            .unwrap()
        {
            crate::launcher::LaunchOutcome::Created(job)
            | crate::launcher::LaunchOutcome::Existing(job) => job.id,
        };
        launcher.settled().await;
        mock.set_screen(&id, novnc).await;
        for uri in [
            format!("/local/screen/{id}"),
            format!("/local/screen/{id}/vnc.html"),
        ] {
            let response = app
                .clone()
                .oneshot(local("GET", &uri, LO, HOST, None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
        }
        let response = app
            .clone()
            .oneshot(local("GET", "/", LO, HOST, None))
            .await
            .unwrap();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(!String::from_utf8_lossy(&bytes).contains("/local/screen/"));
    }

    #[tokio::test]
    async fn status_page_is_off_by_default_and_local_only_when_on() {
        const LO: [u8; 4] = [127, 0, 0, 1];
        const HOST: &str = "127.0.0.1:8787";

        // Off: `/` is like any other path without a token.
        let off = harness(1);
        let response = off
            .app
            .clone()
            .oneshot(local("GET", "/", LO, HOST, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let mock = Arc::new(MockRuntime::new());
        let runtime: Arc<dyn Runtime> = mock.clone();
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let cfg = settings_extra(1, "auth_failures = 3\nstatus_page = true");
        let launcher = Launcher::with_clock(cfg, runtime, clock).unwrap();
        let app = router(launcher.clone());

        let response = app
            .clone()
            .oneshot(local("GET", "/", LO, HOST, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8_lossy(&bytes);
        assert!(html.contains("RUNNING"), "{html}");
        // A config.toml machine has nothing to edit on the page.
        assert!(!html.contains("/local/config"));
        let response = app
            .clone()
            .oneshot(local("GET", "/local/config", LO, HOST, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(html.contains("0 / 1"));
        assert!(!html.contains(TOKEN));

        // A browser asks for the icon by itself. With three failures allowed,
        // that must not be what bans this machine's own address.
        for _ in 0..5 {
            let response = app
                .clone()
                .oneshot(local("GET", "/favicon.ico", LO, HOST, None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
        }
        assert!(launcher.list_bans().unwrap().is_empty());

        // Not from another machine, and not under a public host name.
        for request in [
            local("GET", "/", [203, 0, 113, 10], HOST, None),
            local("GET", "/", LO, "worker.example.com", None),
            local(
                "POST",
                "/local/pause",
                [203, 0, 113, 10],
                HOST,
                Some("http://127.0.0.1:8787"),
            ),
        ] {
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }

        // The button works from the page, and not from another site's form.
        for origin in [None, Some("https://evil.example")] {
            let response = app
                .clone()
                .oneshot(local("POST", "/local/pause", LO, HOST, origin))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        assert!(!launcher.draining().unwrap());
        let response = app
            .clone()
            .oneshot(local(
                "POST",
                "/local/pause",
                LO,
                HOST,
                Some("http://127.0.0.1:8787"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(launcher.draining().unwrap());
        let response = app
            .clone()
            .oneshot(local(
                "POST",
                "/local/resume",
                LO,
                HOST,
                Some("http://127.0.0.1:8787"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(!launcher.draining().unwrap());

        // A pause for a while, and only the spans the page offers.
        let pause_for = |span: &str| {
            local(
                "POST",
                &format!("/local/pause?for={span}"),
                LO,
                HOST,
                Some("http://127.0.0.1:8787"),
            )
        };
        let response = app.clone().oneshot(pause_for("forever")).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!launcher.draining().unwrap());
        let response = app.clone().oneshot(pause_for("3h")).await.unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(launcher.draining().unwrap());
        assert!(launcher.paused_until().is_some());
        launcher.set_draining(false).unwrap();
        assert_eq!(launcher.paused_until(), None);

        // The API beside it still wants the token, also from loopback, and
        // the page never counted as a failure.
        let response = app
            .clone()
            .oneshot(get_from("/health", None, LO))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = app
            .clone()
            .oneshot(get_from("/health", Some(TOKEN), LO))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// The page of a config.json machine: language, the open form, and the
    /// dangerous keys behind the admin login.
    #[tokio::test]
    async fn the_local_page_edits_config_json_and_locks_the_dangerous_keys() {
        const LO: [u8; 4] = [127, 0, 0, 1];
        const HOST: &str = "127.0.0.1:8787";
        const ORIGIN: &str = "http://127.0.0.1:8787";
        let dir = std::env::temp_dir().join(format!("shutup-and-run-page-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.json");
        let mut map = crate::config::default_map();
        map.insert(
            "history_path".into(),
            json!(dir.join("jobs.sqlite").display().to_string()),
        );
        map.insert("admin_password".into(), json!("open-sesame"));
        // The fixture is a Linux server on every platform: on Windows the
        // default profile is shared-pc.
        map.insert("worker_os".into(), json!("linux"));
        crate::config::save_json(&path, &map).unwrap();
        let cfg = Settings::load_json(&path);
        assert_eq!(cfg.admin_user, "admin");
        let runtime: Arc<dyn Runtime> = Arc::new(MockRuntime::new());
        let launcher = Launcher::new(cfg, runtime).unwrap();
        let app = router(launcher);

        let send = |method: &'static str,
                    uri: &'static str,
                    headers: Vec<(&'static str, String)>,
                    body: &'static str| {
            let app = app.clone();
            async move {
                let mut builder = Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("host", HOST)
                    .header("content-type", "application/x-www-form-urlencoded");
                for (name, value) in headers {
                    builder = builder.header(name, value);
                }
                let request = stamp(builder.body(Body::from(body)).unwrap(), LO);
                let response = app.oneshot(request).await.unwrap();
                let status = response.status();
                let headers = response.headers().clone();
                let bytes = response.into_body().collect().await.unwrap().to_bytes();
                (status, headers, String::from_utf8_lossy(&bytes).to_string())
            }
        };
        let basic = |pair: &str| {
            use base64::Engine;
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(pair)
            )
        };

        // Language: English by default, the button sets a cookie that lasts,
        // and with the cookie the same page is in Portuguese only.
        let (status, _, english) = send("GET", "/", vec![], "").await;
        assert_eq!(status, StatusCode::OK);
        assert!(english.contains("Turn off?") && !english.contains("Pode desligar?"));
        assert!(english.contains("href=\"/local/config\""));
        let (status, headers, _) =
            send("GET", "/local/lang?set=pt&to=/local/config", vec![], "").await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(headers["location"], "/local/config");
        let cookie = headers["set-cookie"].to_str().unwrap().to_string();
        assert!(cookie.starts_with("lang=pt;") && cookie.contains("Max-Age=315360000"));
        let (_, headers, _) = send(
            "GET",
            "/local/lang?set=pt&to=https://evil.example/",
            vec![],
            "",
        )
        .await;
        assert_eq!(headers["location"], "/");
        let (_, _, portuguese) = send("GET", "/", vec![("cookie", "lang=pt".into())], "").await;
        assert!(portuguese.contains("Pode desligar?") && !portuguese.contains("Turn off?"));

        // The open form saves an everyday key with no login.
        let (status, _, page) = send("GET", "/local/config", vec![], "").await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("name=\"poll_url\"") && !page.contains("name=\"allowlist\""));
        assert!(!page.contains("open-sesame"));
        let (status, _, _) = send(
            "POST",
            "/local/config",
            vec![("origin", ORIGIN.into())],
            "poll_url=https%3A%2F%2Fjobs.example%2Fpoll%3Fuser%3Dana&max_slots=2",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let saved = crate::config::read_json(&path).unwrap();
        assert_eq!(saved["poll_url"], "https://jobs.example/poll?user=ana");
        assert_eq!(saved["max_slots"], 2);
        // The page shows the URL with the values of its query masked, in the
        // form and in the table, and saving the form as shown keeps the URL.
        let (_, _, page) = send("GET", "/local/config", vec![], "").await;
        assert!(page.contains("value=\"https://jobs.example/poll?user=***\""));
        assert!(!page.contains("user=ana"));
        let (status, _, _) = send(
            "POST",
            "/local/config",
            vec![("origin", ORIGIN.into())],
            "poll_url=https%3A%2F%2Fjobs.example%2Fpoll%3Fuser%3D***&max_slots=2",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let saved = crate::config::read_json(&path).unwrap();
        assert_eq!(saved["poll_url"], "https://jobs.example/poll?user=ana");
        // An invalid value comes back as the form with the reason.
        let (status, _, page) = send(
            "POST",
            "/local/config",
            vec![("origin", ORIGIN.into())],
            "max_slots=0",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(page.contains("max_slots must be between 1 and 256"));
        // A poll interval under the minimum is refused the same way.
        let (status, _, page) = send(
            "POST",
            "/local/config",
            vec![("origin", ORIGIN.into())],
            "max_slots=2&poll_interval_secs=5",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(page.contains("poll_interval_secs must be at least 30"));
        assert!(crate::config::read_json(&path)
            .unwrap()
            .get("poll_interval_secs")
            .is_none());
        let (_, _, page) = send("GET", "/local/config", vec![], "").await;
        assert!(page.contains("name=\"poll_interval_secs\" placeholder=\"30\" min=\"30\""));
        // Not from another site's form, nor another port of this machine.
        for origin in [
            "https://evil.example",
            "https://127.0.0.1:9999",
            "https://127.0.0.1",
        ] {
            let (status, _, _) = send(
                "POST",
                "/local/config",
                vec![("origin", origin.into())],
                "max_slots=9",
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{origin}");
        }
        // The same page served over TLS (a server, through an SSH tunnel).
        let (status, _, _) = send(
            "POST",
            "/local/config",
            vec![("origin", "https://127.0.0.1:8787".into())],
            "max_slots=3",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(crate::config::read_json(&path).unwrap()["max_slots"], 3);

        // A dangerous key through the open form is ignored.
        let (status, _, _) = send(
            "POST",
            "/local/config",
            vec![("origin", ORIGIN.into())],
            "max_slots=2&allowlist=evil%2Fimage&poll_any_image=on",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let saved = crate::config::read_json(&path).unwrap();
        assert_eq!(saved["allowlist"], json!(["hello-world"]));
        assert!(saved.get("poll_any_image").is_none());

        // The advanced page asks for the login: none, a wrong one, the default
        // password that was changed. Reading and saving alike.
        for auth in [None, Some(basic("admin:nope")), Some(basic("admin:admin"))] {
            for (method, body) in [("GET", ""), ("POST", "allowlist=evil%2Fimage")] {
                let mut headers = vec![("origin", ORIGIN.to_string())];
                if let Some(auth) = &auth {
                    headers.push(("authorization", auth.clone()));
                }
                let (status, headers, _) = send(method, "/local/advanced", headers, body).await;
                assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {auth:?}");
                assert!(headers["www-authenticate"]
                    .to_str()
                    .unwrap()
                    .starts_with("Basic"));
            }
        }
        assert_eq!(
            crate::config::read_json(&path).unwrap()["allowlist"],
            json!(["hello-world"])
        );
        let login = vec![
            ("origin", ORIGIN.to_string()),
            ("authorization", basic("admin:open-sesame")),
        ];
        let (status, _, page) = send("GET", "/local/advanced", login.clone(), "").await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("name=\"allowlist\"") && page.contains("name=\"admin_password\""));
        // The resource limits are there to see and change, with what runs
        // now in grey when the file does not set them.
        for key in [
            "resource_profile",
            "max_job_cpus",
            "max_job_memory",
            "min_free_memory",
            "min_free_disk",
        ] {
            assert!(
                page.contains(&format!("name=\"{key}\" placeholder=\"")),
                "{key}"
            );
        }
        assert!(page.contains("name=\"resource_profile\" placeholder=\"none\""));
        let (status, _, _) = send(
            "POST",
            "/local/advanced",
            login.clone(),
            "allowlist=alpine%0D%0Aghcr.io%2Fo%2Fimg&token=&poll_token=&admin_user=admin&admin_password=",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let saved = crate::config::read_json(&path).unwrap();
        assert_eq!(saved["allowlist"], json!(["alpine", "ghcr.io/o/img"]));
        assert_eq!(saved["admin_password"], "open-sesame");
        assert_eq!(saved["token"], map["token"]);

        // The certificate section: behind the same login, the same origin
        // rule, and a confirmation. Off until all three are there.
        let (status, _, _) = send("GET", "/local/certificate", vec![], "").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _, _) = send(
            "POST",
            "/local/certificate",
            vec![("origin", ORIGIN.into())],
            "action=forget",
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _, _) = send(
            "POST",
            "/local/certificate",
            vec![
                ("origin", "https://evil.example".into()),
                ("authorization", basic("admin:open-sesame")),
            ],
            "action=forget",
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _, page) = send("GET", "/local/certificate", login.clone(), "").await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("Off. No certificate."));
        assert!(page.contains("every container executed by this worker can access"));
        assert!(page.contains("name=\"confirm\"") && page.contains("value=\"sign\""));
        // No confirmation, then a confirmation with something that is no certificate.
        let (status, _, page) = send(
            "POST",
            "/local/certificate",
            login.clone(),
            "action=save&pfx_base64=AAAA&password=pw-not-shown",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(page.contains("tick the box") && !page.contains("pw-not-shown"));
        let (status, _, page) = send(
            "POST",
            "/local/certificate",
            login.clone(),
            "action=save&confirm=on&pfx_base64=AAAA&password=pw-not-shown",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(!page.contains("pw-not-shown"));
        let saved = crate::config::read_json(&path).unwrap();
        assert!(saved.get("certificate_enabled").is_none());
        assert!(!crate::certificate::imported(&dir.join("certificate")));
        let _ = std::fs::remove_dir_all(&dir);

        // Not through the API, with the machine's token, from the network.
        let request = Request::builder()
            .method("POST")
            .uri("/local/certificate")
            .header("host", "203.0.113.10:8787")
            .header(
                "authorization",
                format!("Bearer {}", map["token"].as_str().unwrap()),
            )
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from("action=forget"))
            .unwrap();
        let response = app
            .clone()
            .oneshot(stamp(request, [203, 0, 113, 5]))
            .await
            .unwrap();
        assert!(
            !response.status().is_success(),
            "a remote caller reached the certificate page: {}",
            response.status()
        );
    }

    /// The two answers a caller gets when the computer's owner comes first,
    /// and the page where that owner, and nobody through the API, says yes.
    #[tokio::test]
    async fn an_unsafe_or_unauthorized_machine_answers_503_and_only_the_page_authorizes() {
        use crate::safety::Readings;
        const LO: [u8; 4] = [127, 0, 0, 1];
        const HOST: &str = "127.0.0.1:8787";
        const ORIGIN: &str = "http://127.0.0.1:8787";
        let mut cfg = settings_extra(2, "status_page = true");
        cfg.shared_pc = true;
        let mock = Arc::new(MockRuntime::new());
        let runtime: Arc<dyn Runtime> = mock.clone();
        let launcher = Launcher::new(cfg, runtime).unwrap();
        let app = router(launcher.clone());
        let mut small = Readings::healthy();
        small.inner.memory_total = Some(5 << 30);
        small.inner.memory_available = Some(3 << 30);
        small.inner.cpus = Some(2);
        launcher.set_readings(small);

        let push = || async {
            json_of(
                app.clone()
                    .oneshot(post(ALPINE, Some(TOKEN)))
                    .await
                    .unwrap(),
            )
            .await
        };
        let (status, body) = push().await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], "authorization required");
        assert_eq!(body["code"], "AUTHORIZATION_REQUIRED");
        assert_eq!(body["notice_version"], crate::safety::NOTICE_VERSION);
        assert_eq!(body["reasons"][0]["code"], "SAFETY_SMALL_MACHINE");
        assert_eq!(body["reasons"][0]["level"], "warning");
        assert_eq!(mock.start_count().await, 0);
        // No field of a request, and no route with the token, says yes.
        let (status, _) = json_of(
            app.clone()
                .oneshot(post(r#"{"image":"alpine","authorize":true}"#, Some(TOKEN)))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let page = |method: &'static str,
                    uri: &'static str,
                    ip: [u8; 4],
                    origin: Option<&'static str>,
                    body: &'static str| {
            let app = app.clone();
            async move {
                let mut builder = Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("host", HOST)
                    .header("content-type", "application/x-www-form-urlencoded");
                if let Some(origin) = origin {
                    builder = builder.header("origin", origin);
                }
                let response = app
                    .oneshot(stamp(builder.body(Body::from(body)).unwrap(), ip))
                    .await
                    .unwrap();
                let status = response.status();
                let bytes = response.into_body().collect().await.unwrap().to_bytes();
                (status, String::from_utf8_lossy(&bytes).to_string())
            }
        };
        // The status page says why nothing runs, and links to the question.
        let (status, home) = page("GET", "/", LO, None, "").await;
        assert_eq!(status, StatusCode::OK);
        assert!(home.contains("WAITING FOR YOUR AUTHORIZATION"), "{home}");
        assert!(home.contains("href=\"/local/safety\"") && home.contains("small computer"));
        let (status, asked) = page("GET", "/local/safety", LO, None, "").await;
        assert_eq!(status, StatusCode::OK);
        assert!(asked.contains("These safeguards reduce the risk. They do not remove it."));
        assert!(asked.contains("by law or by contract"));
        assert!(asked.contains("name=\"confirm\" required") && asked.contains("5.0 GiB"));

        // Not from another machine, not from another site, not without the box.
        let yes = "action=authorize&confirm=on";
        let (status, _) = page("POST", "/local/safety", [203, 0, 113, 9], Some(ORIGIN), yes).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = page(
            "POST",
            "/local/safety",
            LO,
            Some("https://evil.example"),
            yes,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = page("POST", "/local/safety", LO, None, yes).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, refused) = page(
            "POST",
            "/local/safety",
            LO,
            Some(ORIGIN),
            "action=authorize",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(refused.contains("tick the box"));
        assert_eq!(push().await.0, StatusCode::SERVICE_UNAVAILABLE);

        // The owner, at the machine, ticks the box.
        let (status, done) = page("POST", "/local/safety", LO, Some(ORIGIN), yes).await;
        assert_eq!(status, StatusCode::OK);
        assert!(done.contains("Authorized.") && done.contains("Withdraw the authorization"));
        assert_eq!(push().await.0, StatusCode::ACCEPTED);
        launcher.settled().await;
        assert_eq!(mock.start_count().await, 1);
        let (_, home) = page("GET", "/", LO, None, "").await;
        assert!(home.contains("RUNNING") && !home.contains("WAITING FOR YOUR"));

        // A critical reading: 503 with its own body, and the yes changes nothing.
        let mut critical = small;
        critical.inner.memory_available = Some(300 << 20);
        launcher.set_readings(critical);
        let (status, body) = push().await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], "blocked for safety");
        assert_eq!(body["code"], "SAFETY_BLOCKED");
        assert_eq!(body["reasons"][0]["code"], "SAFETY_MEMORY_CRITICAL");
        let (_, home) = page("GET", "/", LO, None, "").await;
        assert!(home.contains("NEW JOBS BLOCKED FOR SAFETY"));
        let (_, blocked) = page("GET", "/local/safety", LO, None, "").await;
        assert!(blocked.contains("no authorization lifts it"));

        // Withdrawn on the page, in Portuguese when the page is.
        launcher.set_readings(small);
        let (status, _) = page("POST", "/local/safety", LO, Some(ORIGIN), "action=revoke").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(push().await.0, StatusCode::SERVICE_UNAVAILABLE);
        let request = stamp(
            Request::builder()
                .uri("/local/safety")
                .header("host", HOST)
                .header("cookie", "lang=pt")
                .body(Body::empty())
                .unwrap(),
            LO,
        );
        let bytes = app
            .clone()
            .oneshot(request)
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let portuguese = String::from_utf8_lossy(&bytes);
        assert!(portuguese.contains("não o eliminam") && portuguese.contains("Autorizar"));
        assert!(!portuguese.contains("Authorize"));

        // status.json carries the same, for a program.
        let live = launcher.live_status().await;
        let file: Value =
            serde_json::from_slice(&crate::status::document(&live, 1_700_000_000, false, None))
                .unwrap();
        assert_eq!(file["safety"]["admission"], "authorization_required");
        assert_eq!(file["safety"]["level"], "warning");
        assert_eq!(
            file["safety"]["needs_authorization"][0],
            "SAFETY_SMALL_MACHINE"
        );
        assert!(file["safety"]["authorization"].is_null());
        let kinds: Vec<&str> = file["safety"]["incidents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["kind"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            [
                "recovered",
                "authorization_revoked",
                "blocked",
                "authorization_granted"
            ]
        );
    }

    #[tokio::test]
    async fn the_local_page_manages_containers_and_managed_mode_refuses_a_push() {
        const SECRET: &str = "tok-page-never-shown";
        const LO: [u8; 4] = [127, 0, 0, 1];
        const HOST: &str = "127.0.0.1:8787";
        const ORIGIN: &str = "http://127.0.0.1:8787";
        let mock = Arc::new(MockRuntime::new());
        let runtime: Arc<dyn Runtime> = mock.clone();
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let cfg = settings_extra(2, "auth_failures = 30\nstatus_page = true");
        let launcher = Launcher::with_clock(cfg, runtime, clock).unwrap();
        let app = router(launcher.clone());
        let send = |method: &'static str, uri: &'static str, login: bool, body: String| {
            let app = app.clone();
            async move {
                use base64::Engine;
                let mut builder = Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("host", HOST)
                    .header("origin", ORIGIN)
                    .header("content-type", "application/x-www-form-urlencoded");
                if login {
                    builder = builder.header(
                        "authorization",
                        format!(
                            "Basic {}",
                            base64::engine::general_purpose::STANDARD.encode("admin:admin")
                        ),
                    );
                }
                let response = app
                    .oneshot(stamp(builder.body(Body::from(body)).unwrap(), LO))
                    .await
                    .unwrap();
                let status = response.status();
                let bytes = response.into_body().collect().await.unwrap().to_bytes();
                (status, String::from_utf8_lossy(&bytes).to_string())
            }
        };
        let definition = |image: &str| {
            format!(
                "action=save&name=rpa&image={image}&policy=always&enabled=on&cpu=1&memory=1g\
                 &env=USE_XVFB%3D1&secrets=API_TOKEN%3D{SECRET}%0ACERT_ID"
            )
        };

        let (status, page) = send("GET", "/local/managed", false, String::new()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("Managed mode is OFF"), "{page}");
        let (_, home) = send("GET", "/", false, String::new()).await;
        assert!(home.contains("href=\"/local/managed\""));

        // What a container is, and the mode, are the admin's.
        for body in [
            definition("alpine"),
            "action=mode_on".into(),
            "action=delete&name=rpa".into(),
        ] {
            assert_eq!(
                send("POST", "/local/managed", false, body).await.0,
                StatusCode::UNAUTHORIZED
            );
        }
        assert_eq!(
            send("GET", "/local/managed/edit", false, String::new())
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        // A form on another site cannot press a button here.
        let cross = Request::builder()
            .method("POST")
            .uri("/local/managed")
            .header("host", HOST)
            .header("origin", "https://evil.example")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from("action=start&name=rpa"))
            .unwrap();
        assert_eq!(
            app.clone()
                .oneshot(stamp(cross, LO))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );

        // Invalid: nothing saved, the reason shown, the typed secret not echoed.
        let (status, page) = send("POST", "/local/managed", true, definition("evil%2Fimage")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            page.contains("allowlist") && page.contains("API_TOKEN"),
            "{page}"
        );
        assert!(!page.contains(SECRET));
        assert!(launcher.store().managed_rows().unwrap().is_empty());

        let (status, _) = send("POST", "/local/managed", true, definition("alpine")).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let (status, editor) =
            send("GET", "/local/managed/edit?name=rpa", true, String::new()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(editor.contains("USE_XVFB=1") && editor.contains("API_TOKEN"));
        assert!(!editor.contains(SECRET));

        // Managed mode on: a push is refused with a reason of its own.
        let (status, _) = send("POST", "/local/managed", true, "action=mode_on".into()).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let response = app
            .clone()
            .oneshot(post(r#"{"image":"alpine"}"#, Some("test-token-value-ok")))
            .await
            .unwrap();
        let (status, body) = json_of(response).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            (body["error"].as_str(), body["code"].as_str()),
            (Some("managed mode"), Some("MANAGED_MODE"))
        );
        let health = app
            .clone()
            .oneshot(get_from(
                "/health",
                Some("test-token-value-ok"),
                [203, 0, 113, 10],
            ))
            .await
            .unwrap();
        assert_eq!(json_of(health).await.1["mode"], "managed");

        // CERT_ID has no value yet, so it waits; with it, it runs.
        crate::managed::pass(&launcher).await;
        assert_eq!(mock.start_count().await, 0);
        let edit = format!("{}%3Dabc", definition("alpine"));
        assert_eq!(
            send("POST", "/local/managed", true, edit).await.0,
            StatusCode::SEE_OTHER
        );
        crate::managed::pass(&launcher).await;
        launcher.settled().await;
        assert_eq!(mock.start_count().await, 1);
        let env = &mock.specs().await[0].env;
        assert!(
            env.contains(&format!("API_TOKEN={SECRET}")) && env.contains(&"CERT_ID=abc".into())
        );
        let (_, page) = send("GET", "/local/managed", false, String::new()).await;
        assert!(page.contains("Managed mode is ON") && page.contains("value=\"stop\""));
        assert!(!page.contains(SECRET) && !page.contains("USE_XVFB"));

        // Busy: the mode stays, and the page says what to do.
        let (status, page) = send("POST", "/local/managed", true, "action=mode_off".into()).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            page.contains("the worker is busy") && page.contains("managed stop"),
            "{page}"
        );
        assert!(launcher.managed_mode().unwrap());

        // Stop is for the person at the PC: no login.
        let (status, _) = send(
            "POST",
            "/local/managed",
            false,
            "action=stop&name=rpa".into(),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        crate::managed::pass(&launcher).await;
        assert_eq!(mock.killed().await.len(), 1);
        let (status, _) = send("POST", "/local/managed", true, "action=mode_off".into()).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert!(!launcher.managed_mode().unwrap());
    }

    #[tokio::test]
    async fn a_caller_lists_cancels_and_is_told_when_its_key_is_busy() {
        let h = harness(4);
        const T: Option<&str> = Some("test-token-value-ok");
        let ip = [203, 0, 113, 10];
        let call = |request| h.app.clone().oneshot(request);
        let push = |key: &str| {
            post(
                &format!(
                    r#"{{"image":"alpine","concurrency_key":"{key}","job_key":"case-{key}-{}"}}"#,
                    rand_suffix()
                ),
                T,
            )
        };
        let (status, first) = json_of(call(push("lawyer-0001")).await.unwrap()).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let (status, _) = json_of(call(push("lawyer-0001")).await.unwrap()).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let (status, busy) = json_of(call(push("lawyer-0001")).await.unwrap()).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            busy,
            json!({"error": "concurrency key busy", "code": "CONCURRENCY_KEY_BUSY",
                   "reason": "concurrency_key_busy",
                   "concurrency_key": "lawyer-0001", "running": 2, "limit": 2})
        );
        h.launcher.settled().await;

        // The listing: both, with their key, oldest first.
        let (status, listed) = json_of(
            call(get_from("/v1/jobs?state=running", T, ip))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            (listed["slots_used"].as_u64(), listed["slots_free"].as_u64()),
            (Some(2), Some(2))
        );
        let jobs = listed["jobs"].as_array().unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0]["id"], first["id"]);
        assert_eq!(jobs[0]["concurrency_key"], "lawyer-0001");
        assert_eq!(jobs[0]["phase"], "running");
        let (status, _) = json_of(
            call(get_from("/v1/jobs?state=exited", T, ip))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = json_of(call(get_from("/v1/jobs", None, ip)).await.unwrap()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // Cancel: the job ends as launcher_kill, and a second DELETE answers the same.
        let id = first["id"].as_str().unwrap().to_string();
        let delete = |id: &str| {
            let mut request = get_from(&format!("/v1/jobs/{id}"), T, ip);
            *request.method_mut() = axum::http::Method::DELETE;
            request
        };
        for _ in 0..2 {
            let (status, ended) = json_of(call(delete(&id)).await.unwrap()).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                (
                    ended["status"].as_str(),
                    ended["reason"].as_str(),
                    ended["exit_code"].as_i64()
                ),
                (Some("exited"), Some("launcher_kill"), Some(137))
            );
        }
        assert!(h.mock.killed().await.contains(&id));
        let (status, _) = json_of(
            call(delete("0123456789abcdef0123456789abcdef"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        // The key has room again.
        let (status, _) = json_of(call(push("lawyer-0001")).await.unwrap()).await;
        assert_eq!(status, StatusCode::ACCEPTED);

        let response = call(get_from("/metrics", T, ip)).await.unwrap();
        let text = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        assert!(text.contains("shutup_and_run_slots_used 2"), "{text}");
        assert!(
            text.contains("shutup_and_run_refusals_total{reason=\"concurrency_key_busy\"} 1"),
            "{text}"
        );
        assert!(
            text.contains("shutup_and_run_jobs_ended{reason=\"launcher_kill\"} 1"),
            "{text}"
        );
        assert!(
            text.contains("shutup_and_run_start_seconds_count"),
            "{text}"
        );
    }

    fn rand_suffix() -> u64 {
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }

    // The server's day, on the mock runtime: prepare the image, bursts of
    // pushes, the cleanup. The figures are the test's, small; the shape is
    // the one in docs/SERVER.md.

    const WORKER: &str = "ghcr.io/owner/selenium-worker:latest";

    fn server_harness(slots: usize, extra: &str) -> Harness {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("shutup-and-run-server-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 100m a job, so twenty fit the test machine's committed memory.
        let settings = Settings::from_toml_and_env(
            &format!(
                r#"
                token = "test-token-value-ok"
                max_slots = {slots}
                allowlist = ["alpine", "ghcr.io/owner/selenium-worker"]
                history_path = '{}'
                resource_profile = "server"
                max_job_memory = "100m"
                auth_failures = 30
                {extra}
                "#,
                dir.join("jobs.sqlite").display()
            ),
            &HashMap::new(),
        )
        .unwrap();
        let mock = Arc::new(MockRuntime::new());
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let runtime: Arc<dyn Runtime> = mock.clone();
        let launcher = Launcher::with_clock(settings, runtime, clock.clone()).unwrap();
        Harness {
            app: router(launcher.clone()),
            launcher,
            mock,
            clock,
        }
    }

    async fn send(
        app: &Router,
        method: &str,
        uri: &str,
        body: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {TOKEN}"));
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        let request = stamp(
            builder
                .body(Body::from(body.unwrap_or_default().to_string()))
                .unwrap(),
            [203, 0, 113, 10],
        );
        json_of(app.clone().oneshot(request).await.unwrap()).await
    }

    async fn prepared(app: &Router, image: &str) -> Value {
        let (status, body) = send(
            app,
            "POST",
            "/v1/images/pull",
            Some(&json!({ "image": image }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(body["state"], "pulling");
        for _ in 0..200 {
            let (status, body) =
                send(app, "GET", &format!("/v1/images/pull?image={image}"), None).await;
            assert_eq!(status, StatusCode::OK);
            if body["state"] != "pulling" {
                return body;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("{image} never left pulling");
    }

    /// 200 pushes at once, from 50 lawyers, to a server with 20 slots that
    /// starts 5 at a time.
    async fn burst(app: &Router, round: &str) -> Vec<(StatusCode, Value)> {
        let pushes = (0..200).map(|i| {
            let app = app.clone();
            let body = json!({
                "image": WORKER,
                "job_key": format!("{round}-case-{i:03}"),
                "concurrency_key": format!("lawyer-{:02}", i % 50),
                "env": {"CASE_ID": format!("fake-case-{i:03}")},
            })
            .to_string();
            tokio::spawn(async move { send(&app, "POST", "/v1/jobs", Some(&body)).await })
        });
        let all = futures_util::future::join_all(pushes);
        tokio::time::timeout(std::time::Duration::from_secs(30), all)
            .await
            .expect("a burst never hangs")
            .into_iter()
            .map(|answer| answer.unwrap())
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_burst_fills_the_free_slots_refuses_the_rest_and_a_repeat_starts_nothing_twice() {
        let h = server_harness(20, "");
        // An image that is not here yet: refused at once, nothing pulled.
        let probe = json!({"image": WORKER, "job_key": "probe"}).to_string();
        let (status, body) = send(&h.app, "POST", "/v1/jobs", Some(&probe)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body,
            json!({"error": "image not present", "code": "IMAGE_NOT_PRESENT",
                   "reason": "image_not_present", "image": WORKER})
        );
        assert!(h.mock.pulls().await.is_empty());
        assert!(h.launcher.recent_jobs(10).unwrap().is_empty());

        // Prepared once a day, then every job finds it.
        assert_eq!(prepared(&h.app, WORKER).await["state"], "ready");

        let first = burst(&h.app, "r1").await;
        let accepted: Vec<&Value> = first
            .iter()
            .filter(|(status, _)| *status == StatusCode::ACCEPTED)
            .map(|(_, body)| body)
            .collect();
        assert_eq!(accepted.len(), 20, "exactly the free slots");
        for (status, body) in &first {
            if *status != StatusCode::ACCEPTED {
                assert_eq!(*status, StatusCode::TOO_MANY_REQUESTS, "{body}");
                let reason = body["reason"].as_str().unwrap();
                assert!(
                    ["no_slots", "concurrency_key_busy"].contains(&reason),
                    "{body}"
                );
            }
        }
        // No lawyer over its two, no slot counted twice, every row there.
        let mut per_key: HashMap<String, usize> = HashMap::new();
        let rows = h.launcher.recent_jobs(1000).unwrap();
        assert_eq!(rows.len(), 20);
        for (row, _) in h.launcher.unfinished().await.unwrap() {
            *per_key.entry(row.concurrency_key.unwrap()).or_default() += 1;
        }
        assert_eq!(per_key.values().sum::<usize>(), 20);
        assert!(per_key.values().all(|n| *n <= 2), "{per_key:?}");
        let (_, health) = send(&h.app, "GET", "/health", None).await;
        assert_eq!(
            (health["slots_used"].as_u64(), health["slots_free"].as_u64()),
            (Some(20), Some(0))
        );
        // The ramp: five start, the fifteen others wait their turn, accepted.
        let mock = h.mock.clone();
        for _ in 0..200 {
            if mock.start_count().await == 5 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(h.mock.start_count().await, 5);
        assert_eq!(h.mock.pulls().await, [WORKER], "no pull during the burst");
        let (_, health) = send(&h.app, "GET", "/health", None).await;
        assert_eq!(health["slots_used"], 20);

        // Four minutes later the orchestrator sends the same 200 again: the
        // jobs it already has answer 200 with the same id, the rest are
        // refused again, and nothing new is stored or started.
        let ids: std::collections::BTreeSet<String> = accepted
            .iter()
            .map(|job| job["id"].as_str().unwrap().to_string())
            .collect();
        let again = burst(&h.app, "r1").await;
        assert!(again.iter().all(|(s, _)| *s != StatusCode::ACCEPTED));
        let same: std::collections::BTreeSet<String> = again
            .iter()
            .filter(|(s, _)| *s == StatusCode::OK)
            .map(|(_, job)| job["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(same, ids);
        assert_eq!(h.launcher.recent_jobs(1000).unwrap().len(), 20);
        assert_eq!(h.mock.start_count().await, 5);
        let (_, metrics) = send(&h.app, "GET", "/metrics", None).await;
        let metrics = metrics.as_str().unwrap().to_string();
        let refused: u64 = metrics
            .lines()
            .filter(|line| {
                line.starts_with("shutup_and_run_refusals_total{reason=\"no_slots\"}")
                    || line.starts_with(
                        "shutup_and_run_refusals_total{reason=\"concurrency_key_busy\"}",
                    )
            })
            .map(|line| line.rsplit(' ').next().unwrap().parse::<u64>().unwrap())
            .sum();
        assert_eq!(refused, 180 + 180, "{metrics}");

        // Every one canceled, started or waiting: every slot comes back.
        for id in &ids {
            let (status, _) = send(&h.app, "DELETE", &format!("/v1/jobs/{id}"), None).await;
            assert!(
                [StatusCode::OK, StatusCode::ACCEPTED].contains(&status),
                "{status}"
            );
        }
        tokio::time::timeout(std::time::Duration::from_secs(10), h.launcher.settled())
            .await
            .expect("the waiting ones end");
        let (_, health) = send(&h.app, "GET", "/health", None).await;
        assert_eq!(health["slots_used"], 0);
        assert!(h.launcher.unfinished().await.unwrap().is_empty());
        assert!(h
            .launcher
            .recent_jobs(1000)
            .unwrap()
            .iter()
            .all(|job| job.reason.as_deref() == Some("launcher_kill")));

        // The next round gets the slots again.
        let next = burst(&h.app, "r2").await;
        let accepted = next
            .iter()
            .filter(|(s, _)| *s == StatusCode::ACCEPTED)
            .count();
        assert_eq!(accepted, 20);
        assert_eq!(h.mock.pulls().await.len(), 1);
    }

    /// A tracing writer into memory.
    #[derive(Clone, Default)]
    struct Captured(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Every log line of this test binary from the first call on. Global:
    /// a subscriber set for one thread misses events while other tests
    /// register the same call sites. Only this test reads it.
    fn captured_logs() -> Captured {
        static LOGS: std::sync::OnceLock<Captured> = std::sync::OnceLock::new();
        LOGS.get_or_init(|| {
            let logs = Captured::default();
            let writer = logs.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::TRACE)
                .with_ansi(false)
                .with_writer(move || writer.clone())
                .finish();
            tracing::subscriber::set_global_default(subscriber).expect("one global subscriber");
            logs
        })
        .clone()
    }

    #[tokio::test]
    async fn env_values_reach_the_container_and_no_answer_metric_log_or_callback() {
        let logs = captured_logs();

        // Obviously fictitious values.
        const VALUE: &str = "fake-portal-password-0000";
        let h = server_harness(2, "pull_policy = \"always\"");
        let job = json!({
            "image": "alpine",
            "job_key": "case-env",
            "concurrency_key": "lawyer-00",
            "env": {"PORTAL_PASSWORD": VALUE, "CASE_ID": "fake-case-0001"},
            "callback_url": "http://127.0.0.1:9/hook",
        })
        .to_string();
        let mut seen = Vec::new();
        let (status, accepted) = send(&h.app, "POST", "/v1/jobs", Some(&job)).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        seen.push(accepted.to_string());
        h.launcher.settled().await;
        let id = accepted["id"].as_str().unwrap().to_string();
        let env = &h.mock.specs().await[0].env;
        assert!(env.contains(&format!("PORTAL_PASSWORD={VALUE}")), "{env:?}");

        // Every answer about it, and the refusals that name the variable.
        for uri in [
            format!("/v1/jobs/{id}"),
            "/v1/jobs?state=running".to_string(),
            "/metrics".to_string(),
            "/health".to_string(),
        ] {
            seen.push(send(&h.app, "GET", &uri, None).await.1.to_string());
        }
        seen.push(
            send(&h.app, "POST", "/v1/jobs", Some(&job))
                .await
                .1
                .to_string(),
        );
        for bad in [
            json!({"image": "alpine", "env": {"PORTAL_PASSWORD": VALUE, "bad-name": VALUE}}),
            json!({"image": "alpine", "env": {"PORTAL_PASSWORD": format!("{VALUE}\u{0}")}}),
            json!({"image": "ghcr.io/someone/else", "env": {"PORTAL_PASSWORD": VALUE}}),
        ] {
            let (status, body) = send(&h.app, "POST", "/v1/jobs", Some(&bad.to_string())).await;
            assert!(status.is_client_error(), "{body}");
            seen.push(body.to_string());
        }
        let (status, body) = send(
            &h.app,
            "POST",
            "/v1/jobs",
            Some(r#"{"image":"alpine","env":{"PORTAL_PASSWORD":1}}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        seen.push(body.to_string());
        // Its end, and the callback it sends.
        h.mock.stop(&id, 0).await;
        let ended = h.launcher.job(&id, false).await.unwrap().unwrap();
        seen.push(crate::launcher::callback_body(&ended).to_string());
        seen.push(
            send(&h.app, "GET", &format!("/v1/jobs/{id}"), None)
                .await
                .1
                .to_string(),
        );

        for text in &seen {
            assert!(!text.contains(VALUE), "{text}");
        }
        let logged = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
        assert!(logged.contains(&format!("id={id}")), "the log was captured");
        assert!(!logged.contains(VALUE), "{logged}");
        // Should a log line ever print the request or the container's spec
        // whole, it has the names only.
        let request = crate::launcher::LaunchRequest {
            image: "alpine".into(),
            env: HashMap::from([("PORTAL_PASSWORD".to_string(), VALUE.to_string())]),
            timeout_secs: None,
            cpu: None,
            memory_bytes: None,
            shm_bytes: None,
            command: None,
            job_key: None,
            callback_url: None,
            concurrency_key: None,
        };
        let spec = h.mock.specs().await[0].clone();
        for debug in [format!("{request:?}"), format!("{spec:?}")] {
            assert!(debug.contains("PORTAL_PASSWORD"), "{debug}");
            assert!(!debug.contains(VALUE), "{debug}");
        }
    }

    #[tokio::test]
    async fn a_server_tells_its_disk_refuses_below_the_minimum_and_a_timeout_frees_the_slot() {
        use crate::resources::GIB;
        let h = server_harness(2, "min_free_disk = \"20g\"\npull_policy = \"always\"");
        let (status, health) = send(&h.app, "GET", "/health", None).await;
        assert_eq!(status, StatusCode::OK);
        let machine = &health["machine"];
        assert_eq!(machine["profile"], "server");
        assert_eq!(machine["disk_free_bytes"], 100 * GIB);
        assert_eq!(machine["min_free_disk_bytes"], 20 * GIB);
        assert_eq!(machine["memory_total_bytes"], 16 * GIB);
        assert_eq!(machine["shortage"], Value::Null);
        assert_eq!(machine["last_cleanup"], Value::Null);

        let mut low = crate::safety::Readings::healthy();
        low.inner.disk_free = Some(5 * GIB);
        h.launcher.set_readings(low);
        let job = r#"{"image":"alpine","timeout_secs":30}"#;
        let (status, body) = send(&h.app, "POST", "/v1/jobs", Some(job)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            (body["reason"].as_str(), body["resource"].as_str()),
            (Some("low_disk"), Some("disk"))
        );
        let (_, health) = send(&h.app, "GET", "/health", None).await;
        assert_eq!(health["machine"]["shortage"]["resource"], "disk");
        assert!(h.launcher.recent_jobs(10).unwrap().is_empty());

        // Room again; a job over its time is killed and its slot is free.
        h.launcher.set_readings(crate::safety::Readings::healthy());
        let (status, body) = send(&h.app, "POST", "/v1/jobs", Some(job)).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        h.launcher.settled().await;
        let id = body["id"].as_str().unwrap().to_string();
        assert_eq!(
            send(&h.app, "GET", "/health", None).await.1["slots_used"],
            1
        );
        h.clock.set(1_700_000_000 + 31);
        assert_eq!(
            h.launcher.reap_once().await.unwrap(),
            std::slice::from_ref(&id)
        );
        let (_, ended) = send(&h.app, "GET", &format!("/v1/jobs/{id}"), None).await;
        assert_eq!(ended["reason"], "timeout_kill");
        assert_eq!(h.mock.killed().await, [id]);
        assert_eq!(
            send(&h.app, "GET", "/health", None).await.1["slots_used"],
            0
        );
    }

    #[tokio::test]
    async fn the_cleanup_waits_for_an_idle_server_and_keeps_what_was_prepared() {
        let h = server_harness(2, "pull_policy = \"always\"");
        let (status, job) = send(&h.app, "POST", "/v1/jobs", Some(ALPINE)).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        h.launcher.settled().await;
        let (status, body) = send(&h.app, "POST", "/v1/maintenance/cleanup", None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body,
            json!({"error": "jobs running", "code": "JOBS_RUNNING", "running": 1})
        );
        assert!(h.mock.removed_images().await.is_empty());

        // The job ends; a container someone left stopped; a month later the
        // image of that job is unused, the one prepared today is not.
        let id = job["id"].as_str().unwrap();
        h.mock.stop(id, 0).await;
        h.launcher.job(id, false).await.unwrap();
        h.mock.seed_running("left-behind", 0).await;
        h.mock.stop_unseen("left-behind", 0).await;
        h.clock.set(1_700_000_000 + 31 * 86_400);
        assert_eq!(prepared(&h.app, WORKER).await["state"], "ready");
        let before = h.mock.removed().await.len();

        let (status, cleaned) = send(&h.app, "POST", "/v1/maintenance/cleanup", None).await;
        assert_eq!(status, StatusCode::OK, "{cleaned}");
        assert_eq!(cleaned["containers"], 1);
        assert_eq!(cleaned["images"], 1);
        assert_eq!(h.mock.removed().await[before..], ["left-behind"]);
        assert_eq!(h.mock.removed_images().await, ["alpine"]);
        assert_eq!(h.mock.prunes().await, (1, 1));
        let (_, health) = send(&h.app, "GET", "/health", None).await;
        assert_eq!(
            health["machine"]["last_cleanup"]["at"],
            1_700_000_000 + 31 * 86_400
        );

        // A failed preparation says why; it is not a job.
        h.mock.fail_pull(true).await;
        let failed = prepared(&h.app, "alpine:3.20").await;
        assert_eq!(failed["state"], "failed");
        assert!(
            failed["error"]
                .as_str()
                .unwrap()
                .contains("manifest unknown"),
            "{failed}"
        );
        let (status, _) = send(
            &h.app,
            "POST",
            "/v1/images/pull",
            Some(r#"{"image":"ghcr.io/someone/else"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (_, absent) = send(&h.app, "GET", "/v1/images/pull?image=alpine:9", None).await;
        assert_eq!(absent["state"], "absent");

        // Only a server has the route.
        let pc = harness(1);
        let (status, _) = send(&pc.app, "POST", "/v1/maintenance/cleanup", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
