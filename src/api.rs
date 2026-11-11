use crate::bytesize::ByteQty;
use crate::launcher::{JobState, LaunchError, LaunchRequest, Launcher};
use axum::body::Bytes;
use axum::extract::{Path, State};
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
        .route("/v1/jobs", post(create_job))
        .route("/v1/jobs/{id}", get(job_status))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY))
        .with_state(launcher)
}

async fn health(State(launcher): State<Arc<Launcher>>) -> Response {
    match launcher.slots().await {
        Ok(slots) => (
            StatusCode::OK,
            Json(json!({
                "status": "ok",
                "slots_total": slots.total,
                "slots_used": slots.used,
                "slots_free": slots.free,
            })),
        )
            .into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "docker indisponível"})),
        )
            .into_response(),
    }
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
        Err(_) => return error_body(StatusCode::BAD_REQUEST, "JSON inválido", None),
    };
    let request = LaunchRequest {
        image: parsed.image,
        env: parsed.env,
        timeout_secs: parsed.timeout_secs,
        cpu: parsed.cpu,
        memory_bytes: parsed.memory.map(ByteQty::get),
        shm_bytes: parsed.shm.map(ByteQty::get),
        command: parsed.command,
    };
    match launcher.launch(request).await {
        Ok(id) => (StatusCode::ACCEPTED, Json(json!({"id": id}))).into_response(),
        Err(LaunchError::BadRequest(msg)) => error_body(StatusCode::BAD_REQUEST, &msg, None),
        Err(LaunchError::Forbidden) => {
            error_body(StatusCode::FORBIDDEN, "imagem não permitida", None)
        }
        Err(LaunchError::NoSlots { total, used }) => error_body(
            StatusCode::TOO_MANY_REQUESTS,
            "sem slot livre",
            Some(json!({
                "slots_total": total,
                "slots_used": used,
                "slots_free": total.saturating_sub(used),
            })),
        ),
        Err(LaunchError::Unavailable(msg)) => error_body(StatusCode::BAD_GATEWAY, &msg, None),
    }
}

async fn job_status(
    State(launcher): State<Arc<Launcher>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !authorized(&headers, &launcher) {
        return unauthorized();
    }
    match launcher.job(&id).await {
        Ok(JobState::Running) => {
            (StatusCode::OK, Json(json!({"id": id, "status": "running"}))).into_response()
        }
        Ok(JobState::Exited { exit_code }) => (
            StatusCode::OK,
            Json(json!({"id": id, "status": "exited", "exit_code": exit_code})),
        )
            .into_response(),
        Ok(JobState::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(json!({"id": id, "status": "not_found"})),
        )
            .into_response(),
        Err(LaunchError::Unavailable(msg)) => error_body(StatusCode::BAD_GATEWAY, &msg, None),
        Err(other) => error_body(StatusCode::BAD_GATEWAY, &other.to_string(), None),
    }
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
        Json(json!({"error": "não autorizado"})),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;
    use crate::config::Settings;
    use crate::launcher::Launcher;
    use crate::mock::MockRuntime;
    use crate::runtime::Runtime;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::collections::HashMap;
    use tower::ServiceExt;

    struct Harness {
        app: Router,
        launcher: Arc<Launcher>,
        mock: Arc<MockRuntime>,
        clock: Arc<FakeClock>,
    }

    fn harness(slots: usize) -> Harness {
        let mock = Arc::new(MockRuntime::new());
        let clock = Arc::new(FakeClock::new(1_700_000_000));
        let settings = Settings::from_toml_and_env(
            &format!(
                r#"
                token = "test-token-value-ok"
                max_slots = {slots}
                allowlist = ["alpine", "ghcr.io/nailtongomes/rpa-worker-selenium"]
                default_timeout_secs = 30
                max_timeout_secs = 60
                "#
            ),
            &HashMap::new(),
        )
        .unwrap();
        let runtime: Arc<dyn Runtime> = mock.clone();
        let launcher = Launcher::with_clock(settings, runtime, clock.clone());
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
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    const ALPINE: &str = r#"{"image":"alpine","command":["sleep","30"]}"#;

    #[tokio::test]
    async fn auth_allowlist_and_health() {
        let h = harness(2);

        let (status, body) =
            json_of(h.app.clone().oneshot(post(ALPINE, None)).await.unwrap()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "não autorizado");

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
        assert_eq!(status, StatusCode::UNAUTHORIZED, "token ruim não vira 403");

        let (status, body) = json_of(
            h.app
                .clone()
                .oneshot(post(forbidden, Some("test-token-value-ok")))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "imagem não permitida");
        assert!(
            h.mock.pulls().await.is_empty(),
            "imagem proibida não é baixada"
        );

        let neighbor = r#"{"image":"ghcr.io/nailtongomes/rpa-worker-selenium-evil:1"}"#;
        let (status, _) = json_of(
            h.app
                .clone()
                .oneshot(post(neighbor, Some("test-token-value-ok")))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let tagged = r#"{"image":"ghcr.io/nailtongomes/rpa-worker-selenium:1.2.3","env":{"JOB":"a"},"shm":"64m","memory":"512m","cpu":0.5}"#;
        let (status, body) = json_of(
            h.app
                .clone()
                .oneshot(post(tagged, Some("test-token-value-ok")))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let id = body["id"].as_str().unwrap().to_string();

        let (status, _) = json_of(
            h.app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/v1/jobs/{id}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, got) = json_of(
            h.app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/v1/jobs/{id}"))
                        .header("authorization", "Bearer test-token-value-ok")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(got["status"], "running");

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
        assert_eq!(health["slots_total"], 2);
        assert_eq!(health["slots_used"], 1);
        assert_eq!(health["slots_free"], 1);

        let too_long = r#"{"image":"alpine","timeout_secs":61}"#;
        let (status, _) = json_of(
            h.app
                .clone()
                .oneshot(post(too_long, Some("test-token-value-ok")))
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
                let response = app
                    .oneshot(post(ALPINE, Some("test-token-value-ok")))
                    .await
                    .unwrap();
                response.status()
            }));
        }
        let mut accepted = 0;
        let mut rejected = 0;
        for task in tasks {
            match task.await.unwrap() {
                StatusCode::ACCEPTED => accepted += 1,
                StatusCode::TOO_MANY_REQUESTS => rejected += 1,
                other => panic!("status inesperado: {other}"),
            }
        }
        assert_eq!(accepted, 2);
        assert_eq!(rejected, 6);
        assert_eq!(h.mock.running_ids().await.len(), 2);

        let (status, body) = json_of(
            h.app
                .clone()
                .oneshot(post(ALPINE, Some("test-token-value-ok")))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["slots_free"], 0);
        assert_eq!(body["slots_total"], 2);
    }

    #[tokio::test]
    async fn timeout_kill_is_visible_on_the_status_route() {
        let h = harness(1);
        let (status, body) = json_of(
            h.app
                .clone()
                .oneshot(post(
                    r#"{"image":"alpine","command":["sleep","300"],"timeout_secs":10}"#,
                    Some("test-token-value-ok"),
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let id = body["id"].as_str().unwrap().to_string();

        let (status, _) = json_of(
            h.app
                .clone()
                .oneshot(post(ALPINE, Some("test-token-value-ok")))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

        h.clock.set(1_700_000_010);
        let killed = h.launcher.reap_once().await.unwrap();
        assert_eq!(killed, vec![id.clone()]);

        let (status, got) = json_of(
            h.app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/v1/jobs/{id}"))
                        .header("authorization", "Bearer test-token-value-ok")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(got["status"], "exited");
        assert_eq!(got["exit_code"], 137);

        let (status, _) = json_of(
            h.app
                .oneshot(post(ALPINE, Some("test-token-value-ok")))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn health_reports_docker_failure() {
        let h = harness(1);
        h.mock.fail_lists(true).await;
        let (status, _) = json_of(
            h.app
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
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }
}
