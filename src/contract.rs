//! Contract tests. `docs/INTEGRATION-CONTRACT.md` and `docs/schemas` are what
//! an orchestrator is written against, so every documented example goes
//! through the real router, the real poll parser and the real callback, and
//! what the runner emits is checked against the published schemas. A change
//! that breaks one of these is a change to the public contract.

use crate::api::router;
use crate::clock::FakeClock;
use crate::config::Settings;
use crate::launcher::{callback_body, Launcher};
use crate::mock::MockRuntime;
use crate::poll::{answer_from_body, client, poll_body, poll_once, Answer, PollOutcome};
use crate::runtime::Runtime;
use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const TOKEN: &str = "test-token-value-ok";
const BASE: &str =
    "https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/docs/schemas/";

const SCHEMAS: &[(&str, &str)] = &[
    (
        "job-request.schema.json",
        include_str!("../docs/schemas/job-request.schema.json"),
    ),
    (
        "worker-poll-request.schema.json",
        include_str!("../docs/schemas/worker-poll-request.schema.json"),
    ),
    (
        "worker-poll-response.schema.json",
        include_str!("../docs/schemas/worker-poll-response.schema.json"),
    ),
    (
        "manifest.schema.json",
        include_str!("../docs/schemas/manifest.schema.json"),
    ),
    (
        "job-result.schema.json",
        include_str!("../docs/schemas/job-result.schema.json"),
    ),
    (
        "worker-config.schema.json",
        include_str!("../docs/schemas/worker-config.schema.json"),
    ),
];

macro_rules! example {
    ($name:literal) => {
        (
            $name,
            include_str!(concat!("../docs/schemas/examples/", $name)),
        )
    };
}

const JOB_EXAMPLES: &[(&str, &str)] = &[
    example!("job-request.minimal.json"),
    example!("job-request.full.json"),
];
const POLL_ANSWER_EXAMPLES: &[(&str, &str)] = &[
    example!("poll-response.task.json"),
    example!("poll-response.bare-task.json"),
    example!("poll-response.manifest.json"),
];
const MANIFEST_EXAMPLE: (&str, &str) = example!("manifest.json");
const POLL_REQUEST_EXAMPLE: (&str, &str) = example!("poll-request.json");
const RESULT_EXAMPLE: (&str, &str) = example!("job-result.accepted.json");
const CALLBACK_EXAMPLES: &[(&str, &str)] = &[
    example!("callback.exited.json"),
    example!("callback.timeout.json"),
    example!("callback.start-failed.json"),
];

/// A Draft 2020-12 validator for one published schema, or for a `$defs`
/// entry of it (`job-result.schema.json#/$defs/callback`). The other files
/// are registered under their `$id`, so a `$ref` between them resolves
/// without the network.
fn schema(reference: &str) -> jsonschema::Validator {
    let mut registry = jsonschema::Registry::new();
    for (name, text) in SCHEMAS {
        let value: Value = serde_json::from_str(text).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            value["$schema"], "https://json-schema.org/draft/2020-12/schema",
            "{name}"
        );
        assert_eq!(value["$id"], format!("{BASE}{name}"), "{name}");
        registry = registry
            .add(format!("{BASE}{name}"), value)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    let registry = registry.prepare().expect("schemas");
    jsonschema::options()
        .with_registry(&registry)
        .build(&json!({ "$ref": format!("{BASE}{reference}") }))
        .unwrap_or_else(|e| panic!("{reference}: {e}"))
}

fn assert_valid(validator: &jsonschema::Validator, value: &Value, what: &str) {
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{what}: {errors:?}\n{value}");
}

fn parse(text: &str, what: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("{what}: {e}"))
}

struct Machine {
    app: Router,
    launcher: Arc<Launcher>,
    mock: Arc<MockRuntime>,
    clock: Arc<FakeClock>,
}

/// A runner with the example images on its allowlist. `extra` is more TOML.
fn machine(slots: usize, extra: &str, env: &[(&str, &str)]) -> Machine {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("sar-contract-{n}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let toml_text = format!(
        r#"
        token = "{TOKEN}"
        max_slots = {slots}
        allowlist = ["alpine", "ghcr.io/you/worker", "ghcr.io/nailtongomes/shutup-and-run-smoke"]
        auth_failures = 30
        history_path = '{}'
        {extra}
        "#,
        dir.join("jobs.sqlite").display()
    );
    let env: HashMap<String, String> = env
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    let settings = Settings::from_toml_and_env(&toml_text, &env).unwrap();
    let mock = Arc::new(MockRuntime::new());
    let clock = Arc::new(FakeClock::new(1_700_000_000));
    let runtime: Arc<dyn Runtime> = mock.clone();
    let launcher = Launcher::with_clock(settings, runtime, clock.clone()).unwrap();
    Machine {
        app: router(launcher.clone()),
        launcher,
        mock,
        clock,
    }
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Vec<u8>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let mut request = builder.body(Body::from(body)).unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [203, 0, 113, 10],
            9,
        ))));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()))
    };
    (status, value)
}

async fn push(app: &Router, body: &str) -> (StatusCode, Value) {
    call(
        app,
        "POST",
        "/v1/jobs",
        Some(TOKEN),
        body.as_bytes().to_vec(),
    )
    .await
}

/// An orchestrator: `/poll` gives each queued answer once and then 204,
/// `/manifest` serves one body, `/callback` keeps what it receives.
struct Orchestrator {
    base: String,
    callbacks: Arc<Mutex<Vec<Value>>>,
    poll_bodies: Arc<Mutex<Vec<Value>>>,
}

async fn orchestrator(answers: Vec<String>, manifest: &'static str) -> Orchestrator {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let callbacks = Arc::new(Mutex::new(Vec::new()));
    let poll_bodies = Arc::new(Mutex::new(Vec::new()));
    let queue = Arc::new(Mutex::new(
        answers
            .into_iter()
            .map(|a| a.replace("{BASE}", &base))
            .collect::<std::collections::VecDeque<_>>(),
    ));
    let (seen, bodies) = (callbacks.clone(), poll_bodies.clone());
    let app = Router::new()
        .route(
            "/poll",
            axum::routing::post(move |body: Bytes| {
                let (queue, bodies) = (queue.clone(), bodies.clone());
                async move {
                    bodies
                        .lock()
                        .unwrap()
                        .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
                    match queue.lock().unwrap().pop_front() {
                        Some(answer) => (StatusCode::OK, answer),
                        None => (StatusCode::NO_CONTENT, String::new()),
                    }
                }
            }),
        )
        .route(
            "/manifest",
            axum::routing::get(move || async move { manifest }),
        )
        .route(
            "/callback",
            axum::routing::post(move |body: Bytes| {
                let seen = seen.clone();
                async move {
                    seen.lock()
                        .unwrap()
                        .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
                    StatusCode::NO_CONTENT
                }
            }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Orchestrator {
        base,
        callbacks,
        poll_bodies,
    }
}

async fn callbacks(orchestrator: &Orchestrator, count: usize) -> Vec<Value> {
    for _ in 0..200 {
        if orchestrator.callbacks.lock().unwrap().len() >= count {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let seen = orchestrator.callbacks.lock().unwrap().clone();
    assert_eq!(seen.len(), count, "{seen:?}");
    seen
}

// 1. Valid documented push payloads are accepted.
#[tokio::test]
async fn documented_job_requests_are_accepted_by_a_push() {
    let request = schema("job-request.schema.json");
    let result = schema("job-result.schema.json");
    assert_valid(
        &result,
        &parse(RESULT_EXAMPLE.1, RESULT_EXAMPLE.0),
        RESULT_EXAMPLE.0,
    );
    let m = machine(4, "", &[]);
    for (name, text) in JOB_EXAMPLES {
        let body = parse(text, name);
        assert_valid(&request, &body, name);

        let (status, job) = push(&m.app, text).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{name}: {job}");
        assert_valid(&result, &job, name);
        assert_eq!(job["status"], "running");
        assert_eq!(job["image"], body["image"]);
        assert_eq!(
            job["job_key"],
            body.get("job_key").cloned().unwrap_or(Value::Null)
        );
        assert_eq!(job["started_at"], 1_700_000_000);

        // The job can be read back, with and without its log lines.
        m.launcher.settled().await;
        let id = job["id"].as_str().unwrap();
        for uri in [format!("/v1/jobs/{id}"), format!("/v1/jobs/{id}?logs=true")] {
            let (status, read) = call(&m.app, "GET", &uri, Some(TOKEN), Vec::new()).await;
            assert_eq!(status, StatusCode::OK);
            assert_valid(&result, &read, &uri);
            assert_eq!(read.get("logs").is_some(), uri.ends_with("true"));
        }

        // The same job_key again: 200, the same job, nothing new starts.
        if body.get("job_key").is_some() {
            let starts = m.mock.start_count().await;
            let (status, again) = push(&m.app, text).await;
            assert_eq!(status, StatusCode::OK, "{name}");
            assert_eq!(again["id"], job["id"]);
            assert_valid(&result, &again, name);
            assert_eq!(m.mock.start_count().await, starts);
        }
    }
    assert_eq!(m.mock.start_count().await, JOB_EXAMPLES.len() as u64);

    // null is the same as absent for every optional field but env.
    let nulls = r#"{"image":"alpine","job_key":null,"command":null,"timeout_secs":null,"cpu":null,"memory":null,"shm":null,"callback_url":null}"#;
    assert_valid(&request, &parse(nulls, "nulls"), "nulls");
    assert_eq!(push(&m.app, nulls).await.0, StatusCode::ACCEPTED);
}

// 2. Invalid payloads are rejected, with the documented body.
#[tokio::test]
async fn invalid_job_requests_are_rejected() {
    let request = schema("job-request.schema.json");
    let m = machine(4, "max_timeout_secs = 3600", &[]);
    let long = format!(
        r#"{{"image":"alpine","command":[{}]}}"#,
        vec![r#""a""#; 33].join(",")
    );
    // (body, the error the runner answers with)
    let cases: Vec<(&str, &str)> = vec![
        (r#"{}"#, "invalid JSON"),
        (r#"[]"#, "invalid JSON"),
        (r#""alpine""#, "invalid JSON"),
        (r#"{"image":42}"#, "invalid JSON"),
        // Fields that do not exist, and never will: the request cannot widen
        // what the container may touch.
        (
            r#"{"image":"alpine","volumes":["/:/host"]}"#,
            "invalid JSON",
        ),
        (r#"{"image":"alpine","mounts":[]}"#, "invalid JSON"),
        (r#"{"image":"alpine","ports":["80:80"]}"#, "invalid JSON"),
        (r#"{"image":"alpine","privileged":true}"#, "invalid JSON"),
        (
            r#"{"image":"alpine","cap_add":["SYS_ADMIN"]}"#,
            "invalid JSON",
        ),
        (
            r#"{"image":"alpine","network_mode":"host"}"#,
            "invalid JSON",
        ),
        (r#"{"image":"alpine","schema_version":1}"#, "invalid JSON"),
        (
            r#"{"image":"alpine","manifest_url":"https://example.com/m.json"}"#,
            "invalid JSON",
        ),
        (
            r#"{"image":"alpine","task":{"image":"alpine"}}"#,
            "invalid JSON",
        ),
        (r#"{"image":"alpine","env":null}"#, "invalid JSON"),
        (r#"{"image":"alpine","env":{"A":1}}"#, "invalid JSON"),
        (r#"{"image":"alpine","env":["A=1"]}"#, "invalid JSON"),
        (r#"{"image":"alpine","timeout_secs":"60"}"#, "invalid JSON"),
        (r#"{"image":"alpine","timeout_secs":-1}"#, "invalid JSON"),
        (r#"{"image":"alpine","timeout_secs":1.5}"#, "invalid JSON"),
        (r#"{"image":"alpine","command":"sleep 1"}"#, "invalid JSON"),
        (r#"{"image":"alpine","cpu":"2"}"#, "invalid JSON"),
        (r#"{"image":"alpine","memory":0}"#, "invalid JSON"),
        (r#"{"image":"alpine","memory":1.5}"#, "invalid JSON"),
        (r#"{"image":"alpine","memory":"lots"}"#, "invalid JSON"),
        (r#"{"image":"alpine","shm":"-1m"}"#, "invalid JSON"),
        (r#"{"image":"alpine","job_key":7}"#, "invalid JSON"),
        (r#"{"image":""}"#, "invalid image name"),
        (r#"{"image":"Alpine"}"#, "invalid image name"),
        (r#"{"image":"alpine latest"}"#, "invalid image name"),
        (r#"{"image":"ghcr.io/you/../worker"}"#, "invalid image name"),
        (r#"{"image":"-alpine"}"#, "invalid image name"),
        (r#"{"image":"alpine:"}"#, "invalid image name"),
        (
            r#"{"image":"alpine","timeout_secs":0}"#,
            "timeout_secs must be greater than zero",
        ),
        (
            r#"{"image":"alpine","env":{"1A":"x"}}"#,
            "invalid environment variable: 1A",
        ),
        (
            r#"{"image":"alpine","env":{"A-B":"x"}}"#,
            "invalid environment variable: A-B",
        ),
        (
            r#"{"image":"alpine","command":[""]}"#,
            "invalid command argument",
        ),
        (long.as_str(), "command accepts at most 32 arguments"),
        (
            r#"{"image":"alpine","job_key":"has space"}"#,
            "job_key must be 1-200 characters of letters, digits, '.', '_', ':' or '-'",
        ),
        (
            r#"{"image":"alpine","job_key":""}"#,
            "job_key must be 1-200 characters of letters, digits, '.', '_', ':' or '-'",
        ),
        (
            r#"{"image":"alpine","callback_url":"ftp://example.com/x"}"#,
            "callback_url must be http or https",
        ),
        (
            r#"{"image":"alpine","callback_url":"/webhook/done"}"#,
            "callback_url is invalid",
        ),
        (
            r#"{"image":"alpine","cpu":0}"#,
            "cpu must be a number between 0 and 128",
        ),
        (
            r#"{"image":"alpine","cpu":129}"#,
            "cpu must be a number between 0 and 128",
        ),
    ];
    for (body, error) in &cases {
        let (status, answer) = push(&m.app, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(answer, json!({ "error": error }), "{body}");
        assert!(
            !request.is_valid(&parse(body, body)),
            "the schema accepts {body}"
        );
    }
    let (status, answer) = push(&m.app, "not json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(answer, json!({"error": "invalid JSON"}));

    // Valid by the schema, refused by this machine: limits that are its config.
    let above = r#"{"image":"alpine","timeout_secs":3601}"#;
    assert!(request.is_valid(&parse(above, above)));
    let (status, answer) = push(&m.app, above).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        answer,
        json!({"error": "timeout_secs 3601 is above the maximum 3600"})
    );
    let other = r#"{"image":"ghcr.io/someone/else"}"#;
    assert!(request.is_valid(&parse(other, other)));
    let (status, answer) = push(&m.app, other).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(answer, json!({"error": "image is not allowlisted"}));

    assert_eq!(m.mock.start_count().await, 0);
    assert!(m.mock.pulls().await.is_empty());
}

// The other answers of POST /v1/jobs, with the bodies the document prints.
#[tokio::test]
async fn push_refusals_have_the_documented_status_and_body() {
    const JOB: &str = r#"{"image":"alpine","command":["sleep","30"]}"#;
    let m = machine(1, "", &[]);

    for token in [None, Some("wrong-token-value")] {
        let (status, body) = call(&m.app, "POST", "/v1/jobs", token, JOB.into()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body, json!({"error": "unauthorized"}));
    }

    // Over 64 KiB is refused before it is parsed.
    let big = format!(
        r#"{{"image":"alpine","env":{{"A":"{}"}}}}"#,
        "x".repeat(70_000)
    );
    let (status, _) = push(&m.app, &big).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

    assert_eq!(push(&m.app, JOB).await.0, StatusCode::ACCEPTED);
    let (status, body) = push(&m.app, JOB).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        body,
        json!({"error": "no free slots", "slots_total": 1, "slots_used": 1, "slots_free": 0})
    );
    m.launcher.settled().await;

    let (status, body) = call(
        &m.app,
        "POST",
        "/v1/drain",
        Some(TOKEN),
        br#"{"draining":true}"#.to_vec(),
    )
    .await;
    assert_eq!((status, body), (StatusCode::OK, json!({"draining": true})));
    let (status, body) = push(&m.app, JOB).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, json!({"error": "draining", "draining": true}));
    // Drain is checked before job_key: a draining machine answers 503 even
    // for a key it already holds. Documented as a limit, pinned here.
    let held = r#"{"image":"alpine","job_key":"held"}"#;
    let (status, _) = push(&m.app, held).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    call(
        &m.app,
        "POST",
        "/v1/drain",
        Some(TOKEN),
        br#"{"draining":false}"#.to_vec(),
    )
    .await;
    m.launcher
        .on_exit(&m.launcher.last_job().unwrap().unwrap().id, 0)
        .await;
    assert_eq!(push(&m.app, held).await.0, StatusCode::ACCEPTED);
    m.launcher.settled().await;
    call(
        &m.app,
        "POST",
        "/v1/drain",
        Some(TOKEN),
        br#"{"draining":true}"#.to_vec(),
    )
    .await;
    assert_eq!(push(&m.app, held).await.0, StatusCode::SERVICE_UNAVAILABLE);
    call(
        &m.app,
        "POST",
        "/v1/drain",
        Some(TOKEN),
        br#"{"draining":false}"#.to_vec(),
    )
    .await;
    assert_eq!(push(&m.app, held).await.0, StatusCode::OK);
    m.launcher
        .on_exit(&m.launcher.last_job().unwrap().unwrap().id, 0)
        .await;

    m.mock.fail_lists(true).await;
    let (status, body) = push(&m.app, JOB).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body, json!({"error": "docker is unavailable"}));
    m.mock.fail_lists(false).await;

    let (status, body) = call(
        &m.app,
        "GET",
        "/v1/jobs/00000000000000000000000000000000",
        Some(TOKEN),
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body,
        json!({"id": "00000000000000000000000000000000", "status": "not_found"})
    );
}

// 3 and 4. Poll answers: every documented form, and every empty one.
#[test]
fn poll_answers_parse_as_documented() {
    let answer = schema("worker-poll-response.schema.json");
    #[derive(Debug, PartialEq)]
    enum Kind {
        Nothing,
        Task,
        Manifest,
        Rejected,
    }
    let kind = |body: &str| match answer_from_body(body.as_bytes(), true) {
        Ok(Answer::Nothing) => Kind::Nothing,
        Ok(Answer::Task(_)) => Kind::Task,
        Ok(Answer::Manifest(_)) => Kind::Manifest,
        Err(_) => Kind::Rejected,
    };

    // A 200 with no body, or only whitespace, is nothing to do. So is a 204,
    // which never reaches the parser.
    for body in ["", " ", "\n", " \r\n\t "] {
        assert_eq!(kind(body), Kind::Nothing, "{body:?}");
    }

    let job = JOB_EXAMPLES[1].1;
    let wrapped = format!(r#"{{"task":{job}}}"#);
    let cases: Vec<(&str, Kind)> = vec![
        ("null", Kind::Nothing),
        ("{}", Kind::Nothing),
        (r#"{"task":null}"#, Kind::Nothing),
        (r#"{"manifest_url":null}"#, Kind::Nothing),
        (POLL_ANSWER_EXAMPLES[0].1, Kind::Task),
        (POLL_ANSWER_EXAMPLES[1].1, Kind::Task),
        (POLL_ANSWER_EXAMPLES[2].1, Kind::Manifest),
        (job, Kind::Task),
        (wrapped.as_str(), Kind::Task),
        (
            r#"{"manifest_url":"http://127.0.0.1:8080/m/1.json?sig=a@b"}"#,
            Kind::Manifest,
        ),
        // Everything else is dropped by the worker.
        ("[]", Kind::Rejected),
        ("false", Kind::Rejected),
        ("0", Kind::Rejected),
        (r#""ghcr.io/you/worker""#, Kind::Rejected),
        (r#"{"task":[]}"#, Kind::Rejected),
        (r#"{"task":{}}"#, Kind::Rejected),
        (r#"{"tasks":[{"image":"alpine"}]}"#, Kind::Rejected),
        (
            r#"{"task":{"image":"alpine"},"lease":"abc"}"#,
            Kind::Rejected,
        ),
        (
            r#"{"task":{"image":"alpine","volumes":["/:/host"]}}"#,
            Kind::Rejected,
        ),
        (r#"{"image":"alpine","privileged":true}"#, Kind::Rejected),
        (
            r#"{"manifest_url":"https://example.com/m.json","task":{"image":"alpine"}}"#,
            Kind::Rejected,
        ),
        (
            r#"{"manifest_url":"https://example.com/m.json","sha256":"00"}"#,
            Kind::Rejected,
        ),
        (
            r#"{"manifest_url":"ftp://example.com/m.json"}"#,
            Kind::Rejected,
        ),
        (
            r#"{"manifest_url":"https://user:secret@example.com/m.json"}"#,
            Kind::Rejected,
        ),
        (r#"{"manifest_url":"/manifests/1.json"}"#, Kind::Rejected),
        (r#"{"manifest_url":42}"#, Kind::Rejected),
        (
            r#"{"manifest_url":["https://example.com/m.json"]}"#,
            Kind::Rejected,
        ),
    ];
    for (body, expected) in &cases {
        assert_eq!(kind(body), *expected, "{body}");
        assert_eq!(
            answer.is_valid(&parse(body, body)),
            *expected != Kind::Rejected,
            "schema and runner disagree on {body}"
        );
    }
    assert_eq!(kind("not json"), Kind::Rejected);

    // The task inside an answer is the JobRequest of a push, field for field.
    let Ok(Answer::Task(task)) = answer_from_body(wrapped.as_bytes(), true) else {
        panic!("task");
    };
    let pushed = crate::api::launch_request_from_slice(job.as_bytes()).unwrap();
    assert_eq!(task.env, pushed.env);
    assert_eq!(
        (
            &task.image,
            &task.job_key,
            &task.command,
            &task.callback_url
        ),
        (
            &pushed.image,
            &pushed.job_key,
            &pushed.command,
            &pushed.callback_url
        )
    );
    assert_eq!(
        (
            task.timeout_secs,
            task.cpu,
            task.memory_bytes,
            task.shm_bytes
        ),
        (
            pushed.timeout_secs,
            pushed.cpu,
            pushed.memory_bytes,
            pushed.shm_bytes
        )
    );
    assert_eq!(task.memory_bytes, Some(2 * 1024 * 1024 * 1024));
    assert_eq!(
        task.env["SCRIPT_URL"],
        "https://scripts.example.com/protocolar.py"
    );
}

// The poll request: what the worker sends is what the schema says.
#[test]
fn the_poll_request_matches_its_schema() {
    let request = schema("worker-poll-request.schema.json");
    assert_valid(
        &request,
        &parse(POLL_REQUEST_EXAMPLE.1, POLL_REQUEST_EXAMPLE.0),
        POLL_REQUEST_EXAMPLE.0,
    );
    let linux = machine(3, "", &[]);
    let windows = machine(
        1,
        "worker_name = \"DESK-07\"\nworker_os = \"windows\"\npoll_url = \"https://n8n.example.com/webhook/n3-worker-poll\"",
        &[],
    );
    for (m, free) in [(&linux, 3), (&linux, 1), (&windows, 1)] {
        let body = poll_body(m.launcher.settings(), free);
        assert_valid(&request, &body, "poll body");
        assert_eq!(body["free_slots"], free);
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    }
    let body = poll_body(windows.launcher.settings(), 1);
    assert_eq!(
        (&body["worker"], &body["os"]),
        (&json!("DESK-07"), &json!("windows"))
    );
    // Not in the body, by decision: an address, a token, the config, the history.
    for key in [
        "ip",
        "address",
        "token",
        "poll_token",
        "allowlist",
        "jobs",
        "job_keys",
    ] {
        let mut more = body.clone();
        more[key] = json!("x");
        assert!(!request.is_valid(&more), "{key}");
    }
}

// 5 and 6. A manifest is one JobRequest, and never another manifest.
#[tokio::test]
async fn a_manifest_is_a_job_request_and_cannot_nest() {
    let manifest = schema("manifest.schema.json");
    let (name, text) = MANIFEST_EXAMPLE;
    assert_valid(&manifest, &parse(text, name), name);
    let wrapped = format!(r#"{{"task":{text}}}"#);
    for body in [text, wrapped.as_str(), JOB_EXAMPLES[0].1, JOB_EXAMPLES[1].1] {
        assert!(manifest.is_valid(&parse(body, body)), "{body}");
        assert!(
            matches!(
                answer_from_body(body.as_bytes(), false),
                Ok(Answer::Task(_))
            ),
            "{body}"
        );
    }
    // Nested, and the forms that mean "nothing" in a poll answer: no task.
    for body in [
        r#"{"manifest_url":"https://example.com/manifests/next.json"}"#,
        r#"{"manifest_url":null}"#,
        r#"{"task":{"manifest_url":"https://example.com/manifests/next.json"}}"#,
        r#"{"task":null}"#,
        "{}",
        "null",
        r#"[{"image":"alpine"}]"#,
        r#"{"image":"alpine","volumes":["/:/host"]}"#,
    ] {
        assert!(
            !manifest.is_valid(&parse(body, body)),
            "the schema accepts {body}"
        );
        assert!(
            !matches!(
                answer_from_body(body.as_bytes(), false),
                Ok(Answer::Task(_))
            ),
            "{body}"
        );
    }
    assert_eq!(
        answer_from_body(
            br#"{"manifest_url":"https://example.com/next.json"}"#,
            false
        )
        .unwrap_err(),
        "a manifest cannot point at another manifest"
    );

    // Over HTTP: the documented manifest starts a job; a nested one starts nothing.
    let good = orchestrator(
        vec![r#"{"manifest_url":"{BASE}/manifest"}"#.into()],
        MANIFEST_EXAMPLE.1,
    )
    .await;
    let m = machine(1, &format!("poll_url = \"{}/poll\"", good.base), &[]);
    assert_eq!(
        poll_once(&m.launcher, &client().unwrap()).await.unwrap(),
        PollOutcome::Started
    );
    m.launcher.settled().await;
    assert_eq!(
        m.mock.pulls().await,
        ["ghcr.io/nailtongomes/shutup-and-run-smoke"]
    );

    let nested = orchestrator(
        vec![r#"{"manifest_url":"{BASE}/manifest"}"#.into()],
        r#"{"manifest_url":"https://example.com/manifests/next.json"}"#,
    )
    .await;
    let m = machine(1, &format!("poll_url = \"{}/poll\"", nested.base), &[]);
    assert_eq!(
        poll_once(&m.launcher, &client().unwrap()).await.unwrap(),
        PollOutcome::Rejected
    );
    assert!(m.mock.pulls().await.is_empty());
    assert!(m.launcher.last_job().unwrap().is_none());
}

// 7. Callbacks conform to the schema, whichever way the job ended.
#[tokio::test]
async fn callbacks_match_the_schema_for_every_way_a_job_ends() {
    let callback = schema("job-result.schema.json#/$defs/callback");
    let result = schema("job-result.schema.json");
    for (name, text) in CALLBACK_EXAMPLES {
        assert_valid(&callback, &parse(text, name), name);
    }
    // A job that is still running is a JobResult and not a callback.
    let running = parse(RESULT_EXAMPLE.1, RESULT_EXAMPLE.0);
    assert!(result.is_valid(&running) && !callback.is_valid(&running));

    // The whole pull cycle: poll, task, container, end, callback.
    let task = |key: &str, timeout: u64| {
        format!(
            r#"{{"task":{{"image":"alpine","job_key":"{key}","timeout_secs":{timeout},"callback_url":"{{BASE}}/callback"}}}}"#
        )
    };
    let o = orchestrator(
        vec![task("ends-0", 60), task("hangs", 60), task("no-image", 60)],
        "",
    )
    .await;
    let m = machine(1, &format!("poll_url = \"{}/poll\"", o.base), &[]);
    let http = client().unwrap();

    // Exits by itself.
    assert_eq!(
        poll_once(&m.launcher, &http).await.unwrap(),
        PollOutcome::Started
    );
    m.launcher.settled().await;
    let id = m.launcher.last_job().unwrap().unwrap().id;
    m.clock.set(1_700_000_005);
    m.launcher.on_exit(&id, 0).await;
    // Killed at its timeout.
    assert_eq!(
        poll_once(&m.launcher, &http).await.unwrap(),
        PollOutcome::Started
    );
    m.launcher.settled().await;
    m.clock.set(1_700_000_005 + 60);
    assert_eq!(m.launcher.reap_once().await.unwrap().len(), 1);
    // Never started.
    m.mock.fail_start(true).await;
    assert_eq!(
        poll_once(&m.launcher, &http).await.unwrap(),
        PollOutcome::Started
    );
    m.launcher.settled().await;

    let seen = callbacks(&o, 3).await;
    let by_key = |key: &str| seen.iter().find(|c| c["job_key"] == key).unwrap().clone();
    for body in &seen {
        assert_valid(&callback, body, "callback");
        assert_valid(&result, body, "callback as a JobResult");
        assert_eq!(body["image"], "alpine");
    }
    let ended = by_key("ends-0");
    assert_eq!(ended["id"], id.as_str());
    assert_eq!(
        (&ended["reason"], &ended["exit_code"]),
        (&json!("exited"), &json!(0))
    );
    assert_eq!(ended["duration_secs"], 5);
    let killed = by_key("hangs");
    assert_eq!(
        (&killed["reason"], &killed["exit_code"]),
        (&json!("timeout_kill"), &json!(137))
    );
    let failed = by_key("no-image");
    assert_eq!(
        (&failed["reason"], &failed["exit_code"]),
        (&json!("start_failed"), &Value::Null)
    );

    // What is sent is the stored job, and the stored job reads back the same.
    let (status, read) = call(
        &m.app,
        "GET",
        &format!("/v1/jobs/{id}"),
        Some(TOKEN),
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read, ended);
    assert_eq!(
        callback_body(&m.launcher.job(&id, false).await.unwrap().unwrap()),
        ended
    );

    // Every poll that was sent matches the request schema.
    let request = schema("worker-poll-request.schema.json");
    let polls = o.poll_bodies.lock().unwrap().clone();
    assert_eq!(polls.len(), 3);
    for body in &polls {
        assert_valid(&request, body, "poll body over HTTP");
    }
}

// 8. What an orchestrator written against an earlier release relies on.
#[tokio::test]
async fn the_existing_surface_did_not_move() {
    let m = machine(2, "", &[]);

    // No version field was added to a request, and none is accepted.
    for (_, text) in SCHEMAS {
        assert!(!text.contains("schema_version"));
    }

    // job_key: local, kept after a failed start, and the first request wins.
    m.mock.fail_start(true).await;
    let first = r#"{"image":"alpine","job_key":"attempt-1"}"#;
    let (status, job) = push(&m.app, first).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    m.launcher.settled().await;
    m.mock.fail_start(false).await;
    let (status, again) = push(
        &m.app,
        r#"{"image":"ghcr.io/you/worker","job_key":"attempt-1","timeout_secs":5}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["id"], job["id"]);
    assert_eq!(
        (&again["image"], &again["reason"]),
        (&json!("alpine"), &json!("start_failed"))
    );
    assert_eq!(m.mock.start_count().await, 0);
    // A new attempt is a new key.
    let (status, retry) = push(&m.app, r#"{"image":"alpine","job_key":"attempt-2"}"#).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_ne!(retry["id"], job["id"]);
    m.launcher.settled().await;
    // Another machine has never heard of the key.
    let other = machine(1, "", &[]);
    assert_eq!(push(&other.app, first).await.0, StatusCode::ACCEPTED);
    other.launcher.settled().await;

    // The key is free again only when the row has left the history.
    m.clock.set(1_700_000_000 + 31 * 86_400);
    m.launcher.purge_once().unwrap();
    assert_eq!(push(&m.app, first).await.0, StatusCode::ACCEPTED);
    m.launcher.settled().await;

    // The routes, and the keys of what they answer.
    let (status, health) = call(&m.app, "GET", "/health", Some(TOKEN), Vec::new()).await;
    assert_eq!(status, StatusCode::OK);
    let mut keys: Vec<&str> = health
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "draining",
            "slots_free",
            "slots_total",
            "slots_used",
            "status"
        ]
    );
    let mut keys: Vec<&str> = job
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "duration_secs",
            "ended_at",
            "exit_code",
            "id",
            "image",
            "job_key",
            "reason",
            "started_at",
            "status"
        ]
    );
    for (method, uri) in [
        ("GET", "/v1/jobs"),
        ("DELETE", "/v1/jobs/abc"),
        ("PUT", "/v1/jobs"),
    ] {
        let (status, _) = call(&m.app, method, uri, Some(TOKEN), Vec::new()).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {uri}");
    }
    for uri in ["/v1/register", "/v1/workers", "/v2/jobs"] {
        let (status, _) = call(&m.app, "POST", uri, Some(TOKEN), b"{}".to_vec()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
}

/// config.json: the schema lists exactly the keys the service knows, and
/// what the service writes and accepts validates against it.
#[test]
fn the_config_schema_matches_the_keys_and_the_validation() {
    use crate::config::{self, Settings};
    let validator = schema("worker-config.schema.json");
    let (name, text) = SCHEMAS.last().unwrap();
    let published = parse(text, name);
    let mut in_schema: Vec<&str> = published["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut in_code: Vec<&str> = config::KEYS.iter().map(|k| k.name).collect();
    in_schema.sort_unstable();
    in_code.sort_unstable();
    assert_eq!(in_schema, in_code);

    let (name, text) = example!("worker-config.json");
    let example = parse(text, name);
    assert_valid(&validator, &example, name);
    Settings::from_json_map(example.as_object().unwrap()).unwrap();

    let defaults = Value::Object(config::default_map());
    assert_valid(&validator, &defaults, "the factory defaults");

    // Refused by the schema and by the service alike.
    for (key, bad) in [
        ("max_slots", json!(0)),
        ("max_slots", json!("2")),
        ("poll_interval_secs", json!(0)),
        ("token", json!("short")),
        ("allowlist", json!("alpine")),
        ("no_such_key", json!(true)),
    ] {
        let mut map = config::default_map();
        map.insert(key.to_string(), bad.clone());
        assert!(
            !validator.is_valid(&Value::Object(map.clone())),
            "schema accepts {key}={bad}"
        );
        let refused = Settings::from_json_map(&map).unwrap_err().to_string();
        assert!(!refused.is_empty(), "{key}={bad}");
    }
    // Required keys.
    for key in ["token", "max_slots"] {
        let mut map = config::default_map();
        map.remove(key);
        assert!(!validator.is_valid(&Value::Object(map.clone())), "{key}");
        assert!(Settings::from_json_map(&map).is_err(), "{key}");
    }
}
