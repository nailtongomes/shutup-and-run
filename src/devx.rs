//! Commands for the first hour with a worker, and for the agent that sets
//! it up: `version`, `config check`, `job validate`, `demo` and
//! `certificate`. None of them asks a question. Each takes `--json` and
//! keeps the exit codes of the other commands: 0 done, 1 failed, 2 wrong
//! command line, 3 invalid config, value or job.
//!
//! Nothing here is a second implementation. `job validate` calls the same
//! check a push goes through, `config check` is the doctor's own checks, and
//! the demos start their container through the launcher, with the machine's
//! allowlist, limits and admission.

use crate::certificate;
use crate::cli::{
    check, checks_json, emit, report, soft, Check, Out, EXIT_FAILED, EXIT_INVALID, EXIT_USAGE,
};
use crate::config::{self, JsonMap, Settings};
use crate::launcher::{
    check_request, JobSource, LaunchError, LaunchOutcome, LaunchRequest, Launcher,
};
use crate::runtime::Runtime;
use base64::Engine;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

pub fn usage() -> &'static str {
    "\
shutup-and-run version [--json]
    the version of this build, the platform, and the API version.

shutup-and-run config check [--json]
    a preflight that changes nothing: the config, Docker, the folders the
    service writes to, the resource limits, the allowlist, the certificate
    when passthrough is on, and the poll URL: its host must answer and the
    path must be a poll endpoint (one request with no token and no free
    slot, which takes no task; a 404 is reported, not called reachable).
    Exit 3 when the config itself is invalid, 1 when
    something around it is wrong, 0 otherwise.

shutup-and-run job validate FILE [--poll] [--json]
    check a task (the JSON of POST /v1/jobs) against this machine's rules
    without running it: fields, image and allowlist, timeout, CPU and memory
    limits. FILE may be - for stdin. --poll checks it as a task from the poll
    URL. Nothing is pulled, no slot is taken, no history is written. A valid
    task can still be refused later: slots, a pause, free memory and disk
    and the image pull are checked only when it is sent. Exit 3 when invalid.

shutup-and-run demo [--image IMAGE] [--json]
    run one small container through the worker's own path and check it:
    Docker answers, the job is admitted, the container exits 0, it carried
    the machine's CPU and memory limits, and nothing is left behind.
    Needs no orchestrator. The image (default hello-world) must be one this
    machine may run. The worker's config and history are not touched.

shutup-and-run demo sign [--configured] [--out FILE] [--image IMAGE] [--json]
    sign a made-up PDF inside a container and verify the signature here.
    By default with a throwaway certificate made for this run. --configured
    uses the certificate imported with `certificate set`. The signature is a
    basic PDF signature (adbe.pkcs7.detached). It is not PAdES and says
    nothing about ICP-Brasil. Needs the openssl program and the image
    python:3.12-slim (or --image, with python3 and openssl in it).

shutup-and-run certificate status | validate [--json]
shutup-and-run certificate set --file PATH --accept-risk [--password-stdin] [--apply] [--json]
shutup-and-run certificate disable [--forget] [--apply] [--json]
    certificate passthrough, off by default. `set` imports one PKCS#12 file
    (.pfx, .p12) and its password and turns passthrough on: from then on
    EVERY container this machine starts can read both, at
      /run/shutup-and-run/certificate/client.pfx
      /run/shutup-and-run/secrets/certificate-password
    (also in SHUTUP_AND_RUN_CERT_PATH and SHUTUP_AND_RUN_CERT_PASSWORD_FILE).
    Read-only keeps a job from changing the file. It does not keep a job
    from copying the private key. --accept-risk is the owner saying so.
    The password comes from SHUTUP_AND_RUN_CERT_PASSWORD, or from stdin with
    --password-stdin. It is never an argument. --apply restarts the service;
    running containers are left alone and keep what they had.
    `disable` turns it off; --forget also deletes the imported file."
}

// ---------------------------------------------------------------------------
// Arguments

#[derive(Default)]
struct Args {
    words: Vec<String>,
    flags: Vec<String>,
    values: HashMap<String, String>,
}

impl Args {
    fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
    }
}

fn parse(args: &[String], switches: &[&str], valued: &[&str]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if switches.contains(&arg.as_str()) {
            out.flags.push(arg.clone());
        } else if valued.contains(&arg.as_str()) {
            let value = it.next().ok_or_else(|| format!("{arg} needs a value"))?;
            out.values.insert(arg.clone(), value.clone());
        } else if arg.starts_with("--") {
            return Err(format!("unknown option {arg}"));
        } else {
            out.words.push(arg.clone());
        }
    }
    Ok(out)
}

fn parsed(args: &[String], switches: &[&str], valued: &[&str]) -> Result<Args, ExitCode> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{}", usage());
        return Err(ExitCode::SUCCESS);
    }
    let mut all = vec!["--json"];
    all.extend_from_slice(switches);
    parse(args, &all, valued).map_err(|msg| {
        emit(
            Out::fail(
                EXIT_USAGE,
                format!("{msg}. See `shutup-and-run demo --help`."),
            ),
            args.iter().any(|a| a == "--json"),
        )
    })
}

/// A result made of checks, shaped like the doctor's.
fn checks_out(checks: &[Check], extra: Value, good: &str, bad: &str) -> Out {
    let (lines, healthy) = report(checks);
    let failed = checks.iter().find(|c| !c.ok && !c.soft);
    let mut json = json!({
        "ok": healthy,
        "code": failed.map(|c| c.code()).unwrap_or_else(|| "OK".to_string()),
        "checks": checks_json(checks),
    });
    if let Some(failed) = failed {
        json["error"] = Value::String(failed.detail.clone());
    }
    if let (Some(obj), Value::Object(more)) = (json.as_object_mut(), extra) {
        obj.extend(more);
    }
    Out {
        code: if healthy { 0 } else { EXIT_FAILED },
        json,
        text: format!("{lines}{}", if healthy { good } else { bad }),
    }
}

/// Checks print on stdout whatever the outcome: they are the answer.
fn emit_checks(mut out: Out, as_json: bool) -> ExitCode {
    crate::hints::attach(&mut out.json, out.code);
    println!(
        "{}",
        if as_json {
            out.json.to_string()
        } else {
            out.text.clone()
        }
    );
    ExitCode::from(out.code)
}

// ---------------------------------------------------------------------------
// version

/// `wsl2` where the service runs inside WSL2, or where this is the Windows
/// supervisor of such a service; `docker` on a plain Linux machine.
fn runtime_name() -> &'static str {
    let in_wsl = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|text| text.to_ascii_lowercase().contains("microsoft"))
        .unwrap_or(false);
    if cfg!(windows) || in_wsl {
        "wsl2"
    } else {
        "docker"
    }
}

pub fn version_json() -> Value {
    json!({
        "ok": true,
        "name": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "architecture": std::env::consts::ARCH,
        "runtime": runtime_name(),
        "api_version": "v1",
    })
}

pub fn version_cli(args: &[String]) -> ExitCode {
    let args = match parsed(args, &[], &[]) {
        Ok(args) => args,
        Err(code) => return code,
    };
    let version = version_json();
    let text = format!("shutup-and-run {}", env!("CARGO_PKG_VERSION"));
    emit(Out::ok(version, text), args.has("--json"))
}

// ---------------------------------------------------------------------------
// config check

/// Can the service write where its history, and so its status file, lives?
/// Asked of the kernel; nothing is created.
pub fn storage_check(settings: &Settings) -> Check {
    let dir = settings
        .history_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // A folder that does not exist yet is made at start, if its parent allows.
    let target = if dir.exists() {
        dir
    } else {
        dir.parent().unwrap_or(Path::new("/"))
    };
    let ok = writable(target);
    check(
        "storage",
        ok,
        if ok {
            format!("{} can be written", dir.display())
        } else {
            format!("{} cannot be written by this user", target.display())
        },
    )
    .coded("STORAGE_NOT_WRITABLE")
}

#[cfg(unix)]
fn writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: access takes a NUL-terminated path and reads nothing else.
    unsafe { libc::access(path.as_ptr(), libc::W_OK | libc::X_OK) == 0 }
}

#[cfg(not(unix))]
fn writable(path: &Path) -> bool {
    path.exists()
}

pub fn allowlist_check(settings: &Settings) -> Check {
    match (settings.allowlist.len(), settings.poll_any_image) {
        (0, _) => soft(
            "allowlist",
            true,
            "empty: a task from the poll URL may name any image, and every push is refused",
        ),
        (n, true) => soft(
            "allowlist",
            true,
            format!("{n} image(s) for a push; a task from the poll URL may name any image"),
        ),
        (n, false) => check(
            "allowlist",
            true,
            format!("{n} image(s): {}", settings.allowlist.join(", ")),
        ),
    }
}

/// The poll URL, asked once without a token and with no free slot: is the
/// host there, and is the path a poll endpoint? A 404 is not "reachable".
pub async fn poll_check(settings: &Settings) -> Check {
    let Some(url) = &settings.poll_url else {
        return soft("poll", false, "no poll URL: the worker would be idle")
            .coded("POLL_URL_UNSET");
    };
    // The query pairs the machine with a person: never printed.
    let shown = crate::config::mask_query(url);
    let unreachable = |err: String| {
        // The service out there, not the config in here: a warning.
        soft("poll", false, format!("cannot reach {shown}: {err}")).coded("POLL_UNREACHABLE")
    };
    if let Err(err) = crate::cli::reach(url).await {
        return unreachable(err);
    }
    match crate::cli::probe(settings, url).await {
        Ok(status) => crate::cli::poll_answer_check(&shown, status, true),
        Err(err) => unreachable(err),
    }
}

pub async fn config_check() -> Out {
    let (found, settings) = crate::cli::base_checks().await;
    let mut checks = found.checks;
    let config_invalid = checks.iter().any(|c| c.name == "config" && !c.ok);
    if let Some(settings) = &settings {
        checks.push(storage_check(settings));
        checks.push(crate::cli::resources_check(
            settings,
            crate::resources::memory_total(),
            crate::resources::read(&crate::resources::disk_path(&settings.history_path)),
        ));
        checks.push(allowlist_check(settings));
        checks.extend(crate::cli::certificate_check(settings));
        checks.push(poll_check(settings).await);
    }
    let checks: Vec<Check> = checks
        .into_iter()
        .map(|c| Check {
            detail: crate::cli::scrub(&c.detail, &found.secrets),
            hint: crate::cli::scrub(&c.hint(), &found.secrets),
            ..c
        })
        .collect();
    let mut out = checks_out(
        &checks,
        json!({"config_valid": !config_invalid}),
        "the config and this machine are ready",
        "not ready: follow the `hint:` under each FAIL, then this again",
    );
    if config_invalid {
        out.code = EXIT_INVALID;
    }
    out
}

pub async fn config_check_cli(args: &[String]) -> ExitCode {
    let args = match parsed(args, &[], &[]) {
        Ok(args) => args,
        Err(code) => return code,
    };
    emit_checks(config_check().await, args.has("--json"))
}

// ---------------------------------------------------------------------------
// job validate

fn job_fail(code: &str, error: &str, detail: String) -> Out {
    Out {
        code: EXIT_INVALID,
        json: json!({"ok": false, "code": code, "error": error, "detail": detail}),
        text: format!("{code}: {detail}"),
    }
}

/// The static half of what `POST /v1/jobs` checks, with the same answers.
pub fn job_validate(bytes: &[u8], settings: &Settings, source: JobSource) -> Out {
    let request = match crate::api::launch_request_from_slice(bytes) {
        Ok(request) => request,
        // The API says only "invalid JSON". Here the reason is worth having.
        Err(err) => return job_fail("JOB_INVALID_JSON", "invalid JSON", err.to_string()),
    };
    let checked = match check_request(request, settings, source) {
        Ok(checked) => checked,
        Err(LaunchError::Forbidden) => {
            return job_fail(
                "JOB_IMAGE_NOT_ALLOWED",
                "image is not allowlisted",
                format!(
                    "this machine's allowlist is [{}]{}",
                    settings.allowlist.join(", "),
                    if settings.poll_any_image && source == JobSource::Push {
                        "; a task from the poll URL may name any image (--poll)"
                    } else {
                        ""
                    }
                ),
            )
        }
        Err(err) => {
            let msg = err.to_string();
            return job_fail("JOB_INVALID_FIELD", &msg, msg.clone());
        }
    };
    let spec = &checked.spec;
    let effective = json!({
        "image": spec.image,
        "timeout_secs": checked.timeout,
        "cpu": spec.nano_cpus.map(|n| n as f64 / 1e9),
        "memory_bytes": spec.memory_bytes,
        "shm_bytes": spec.shm_bytes,
        "env_count": spec.env.len(),
        "job_key": checked.job_key,
        "callback": checked.callback_url.is_some(),
    });
    let crate::resources::Free { memory, disk } =
        crate::resources::read(&crate::resources::disk_path(&settings.history_path));
    let waiting = crate::resources::shortage(
        crate::resources::Free { memory, disk },
        settings.min_free_memory,
        settings.min_free_disk,
    )
    .map(|short| short.to_string());
    Out::ok(
        json!({
            "code": "JOB_VALID",
            "source": if source == JobSource::Poll { "poll" } else { "push" },
            "effective": effective,
            "runtime": {
                "checked": false,
                "note": "slots, a pause, free memory and disk, and the image pull are checked only when the job is sent; a valid job can still be refused then",
                "waiting_for": waiting,
            },
        }),
        format!(
            "JOB_VALID: this machine would accept the fields of this job\n{effective:#}\n\
             Not checked here: free slots, a pause, free memory and disk, the image pull."
        ),
    )
}

pub fn job_cli(args: &[String]) -> ExitCode {
    let args = match parsed(args, &["--poll"], &[]) {
        Ok(args) => args,
        Err(code) => return code,
    };
    let as_json = args.has("--json");
    let words: Vec<&str> = args.words.iter().map(String::as_str).collect();
    let ["validate", file] = words[..] else {
        return emit(
            Out::fail(
                EXIT_USAGE,
                "usage: shutup-and-run job validate FILE [--poll] [--json]",
            ),
            as_json,
        );
    };
    let bytes = if file == "-" {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(1024 * 1024)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    } else {
        std::fs::read(file)
    };
    let bytes = match bytes {
        Ok(bytes) => bytes,
        Err(err) => return emit(Out::fail(EXIT_FAILED, format!("{file}: {err}")), as_json),
    };
    let settings = match Settings::load() {
        Ok(settings) => settings,
        Err(err) => return emit(Out::fail(EXIT_INVALID, err.0), as_json),
    };
    let source = if args.has("--poll") {
        JobSource::Poll
    } else {
        JobSource::Push
    };
    let out = job_validate(&bytes, &settings, source);
    if as_json || out.code == 0 {
        emit(out, as_json)
    } else {
        // The verdict is the answer, so it goes to stdout like the doctor's.
        println!("{}", out.text);
        ExitCode::from(out.code)
    }
}

// ---------------------------------------------------------------------------
// demo

const DEMO_IMAGE: &str = "hello-world";
pub const SIGN_IMAGE: &str = "python:3.12-slim";
const DEMO_TIMEOUT: u64 = 180;

/// A launcher of its own on the machine's Docker: the machine's allowlist,
/// limits and admission, and a history file that is thrown away afterwards,
/// so the worker's own config and history are not touched.
struct Bench {
    launcher: Arc<Launcher>,
    work: PathBuf,
}

impl Bench {
    fn new(mut settings: Settings, runtime: Arc<dyn Runtime>) -> Result<Self, String> {
        let work = std::env::temp_dir().join(format!(
            "shutup-and-run-demo-{}",
            crate::launcher::random_id()
        ));
        std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
        private_dir(&work);
        settings.history_path = work.join("jobs.sqlite");
        settings.log_lines = 2000;
        let launcher = Launcher::new(settings, runtime).map_err(|e| e.to_string())?;
        Ok(Self { launcher, work })
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.work);
    }
}

#[cfg(unix)]
fn private_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn private_dir(_path: &Path) {}

/// What one demo container did.
struct Ran {
    exit_code: Option<i64>,
    reason: Option<String>,
    logs: String,
}

/// Start one job the way a task from this machine's own source would start,
/// wait for it, and add a line to `checks` for each thing found. None when
/// it did not get as far as an exit code.
async fn run_one(bench: &Bench, request: LaunchRequest, checks: &mut Vec<Check>) -> Option<Ran> {
    let launcher = &bench.launcher;
    let settings = launcher.settings();
    // A Windows worker takes its work from the poll URL, which may name any
    // image. Everywhere else the demo is held to the allowlist, like a push.
    let source = if settings.poll_any_image {
        JobSource::Poll
    } else {
        JobSource::Push
    };
    let image = request.image.clone();
    let expected = match check_request(request.clone(), settings, source) {
        Ok(checked) => checked.spec,
        Err(LaunchError::Forbidden) => {
            checks.push(
                check(
                    "image",
                    false,
                    format!("{image} is not on this machine's allowlist"),
                )
                .coded("DEMO_IMAGE_NOT_ALLOWED")
                .with(
                    settings
                        .allowlist
                        .iter()
                        .map(String::as_str)
                        .chain([image.as_str()])
                        .collect::<Vec<_>>()
                        .join(","),
                ),
            );
            return None;
        }
        Err(err) => {
            checks.push(check("job", false, err.to_string()).coded("DEMO_JOB_INVALID"));
            return None;
        }
    };
    checks.push(check(
        "image",
        true,
        format!("{image} may run on this machine"),
    ));

    let id = match launcher.launch_from(request, source).await {
        Ok(LaunchOutcome::Created(job)) | Ok(LaunchOutcome::Existing(job)) => job.id,
        Err(err) => {
            let code = match &err {
                LaunchError::NoSlots { .. } => "WORKER_BUSY",
                LaunchError::LowResources(short) if short.resource == "disk" => "LOW_DISK",
                LaunchError::LowResources(_) => "LOW_MEMORY",
                _ => "DOCKER_UNAVAILABLE",
            };
            checks.push(check("admission", false, err.to_string()).coded(code));
            return None;
        }
    };
    checks.push(check("admission", true, format!("job {id} accepted")));

    let runtime = launcher.runtime();
    let mut applied = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(DEMO_TIMEOUT + 30);
    let job = loop {
        // While the container still exists. Reading the job below removes it.
        if applied.is_none() {
            applied = runtime.applied(&id).await.ok().flatten();
        }
        match launcher.job(&id, true).await {
            Ok(Some(job)) if job.is_finished() => break Some(job),
            Ok(_) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(250)).await
            }
            _ => break None,
        }
    };
    let Some(job) = job else {
        checks.push(
            check("container", false, "the job did not end in time")
                .coded("DEMO_TIMEOUT")
                .with(id.to_string()),
        );
        return None;
    };
    let ran = Ran {
        exit_code: job.exit_code,
        reason: job.reason.clone(),
        logs: job.logs.clone().unwrap_or_default(),
    };
    if job.reason.as_deref() == Some(crate::REASON_START_FAILED) {
        checks.push(
            check(
                "container",
                false,
                format!("{image} did not start: {}", ran.logs.trim()),
            )
            .coded("DEMO_IMAGE_UNAVAILABLE")
            .with(image.to_string()),
        );
        return Some(ran);
    }
    checks.push(
        check(
            "container",
            ran.exit_code == Some(0),
            format!(
                "{image} ended: {} exit {}",
                ran.reason.as_deref().unwrap_or("?"),
                ran.exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".into())
            ),
        )
        .coded("DEMO_JOB_FAILED"),
    );

    let want = (
        expected.nano_cpus.unwrap_or(0),
        expected.memory_bytes.unwrap_or(0),
    );
    checks.push(match applied {
        Some(got) if (got.nano_cpus, got.memory_bytes) == want => {
            let swap_ok = want.1 == 0 || got.memory_swap == want.1;
            check(
                "limits",
                swap_ok,
                if want == (0, 0) {
                    "this machine sets no CPU or memory limit (resource_profile none), and the container had none".to_string()
                } else {
                    format!(
                        "the container had cpu {} and memory {}, swap {}",
                        want.0 as f64 / 1e9,
                        crate::resources::human(want.1 as u64),
                        if swap_ok { "the same (no extra swap)" } else { "NOT the same" }
                    )
                },
            )
            .coded("LIMITS_NOT_APPLIED")
        }
        Some(got) => check(
            "limits",
            false,
            format!(
                "the container had nano_cpus {} and memory {}, not {} and {}",
                got.nano_cpus, got.memory_bytes, want.0, want.1
            ),
        )
        .coded("LIMITS_NOT_APPLIED"),
        None => soft(
            "limits",
            false,
            "the container was gone before its limits could be read",
        )
        .coded("LIMITS_NOT_READ"),
    });

    let gone = matches!(
        runtime.inspect(&id).await,
        Ok(crate::runtime::Inspected::NotFound)
    );
    checks.push(
        check(
            "cleanup",
            gone,
            if gone {
                "the container is removed; the image stays for the next run".to_string()
            } else {
                format!("container shutup-and-run-{id} is still there")
            },
        )
        .coded("DEMO_CLEANUP_FAILED")
        .with(id.to_string()),
    );
    Some(ran)
}

fn request(image: &str, timeout: u64) -> LaunchRequest {
    LaunchRequest {
        image: image.to_string(),
        env: HashMap::new(),
        timeout_secs: Some(timeout),
        cpu: None,
        memory_bytes: None,
        shm_bytes: None,
        command: None,
        job_key: None,
        callback_url: None,
    }
}

/// Docker, as the first line of a demo. None when it does not answer.
async fn docker_runtime(settings: &Settings, checks: &mut Vec<Check>) -> Option<Arc<dyn Runtime>> {
    let docker = match crate::bollard_rt::connect_docker() {
        Ok(docker) => docker,
        Err(err) => {
            checks.push(docker_down(err));
            return None;
        }
    };
    match tokio::time::timeout(Duration::from_secs(10), docker.ping()).await {
        Ok(Ok(_)) => {
            checks.push(check("docker", true, "answers"));
            Some(Arc::new(crate::bollard_rt::BollardRuntime::new(
                docker,
                settings.docker_config.clone(),
            )))
        }
        Ok(Err(err)) => {
            checks.push(docker_down(err.to_string()));
            None
        }
        Err(_) => {
            checks.push(docker_down("no answer in 10 s".into()));
            None
        }
    }
}

fn docker_down(err: String) -> Check {
    check("docker", false, err).coded("DOCKER_UNAVAILABLE")
}

/// `shutup-and-run demo`: one small container, start to finish.
pub async fn demo(settings: Settings, runtime: Option<Arc<dyn Runtime>>, image: &str) -> Out {
    let mut checks = Vec::new();
    let runtime = match runtime {
        Some(runtime) => Some(runtime),
        None => docker_runtime(&settings, &mut checks).await,
    };
    let mut logs = Value::Null;
    if let Some(runtime) = runtime {
        let timeout = DEMO_TIMEOUT.min(settings.max_timeout_secs);
        match Bench::new(settings, runtime) {
            Ok(bench) => {
                if let Some(ran) = run_one(&bench, request(image, timeout), &mut checks).await {
                    logs = Value::String(ran.logs);
                }
            }
            Err(err) => checks.push(check("workspace", false, err).coded("DEMO_WORKSPACE")),
        }
    }
    checks_out(
        &checks,
        json!({"image": image, "logs": logs}),
        "demo passed: this machine runs a container under the worker's rules",
        "demo failed: follow the `hint:` under each FAIL, then the demo again",
    )
}

// ---------------------------------------------------------------------------
// demo sign

/// Runs in the container: python3 and the openssl program, nothing else.
/// It makes a one-page PDF with made-up text, signs it with the certificate
/// the worker mounted, and prints the signed file as base64.
const SIGN_SCRIPT: &str = r#"
import base64, os, subprocess, sys
cert = os.environ.get("SHUTUP_AND_RUN_CERT_PATH", "")
pwf = os.environ.get("SHUTUP_AND_RUN_CERT_PASSWORD_FILE", "")
if not (os.path.isfile(cert) and os.path.isfile(pwf)):
    sys.exit("DEMO: the worker did not mount a certificate and a password file")
try:
    open(cert, "ab").close()
    print("DEMO_MOUNT=writable")
except OSError:
    print("DEMO_MOUNT=read-only")
pem = "/tmp/identity.pem"
for extra in ([], ["-legacy"]):
    r = subprocess.run(["openssl", "pkcs12", "-in", cert, "-passin", "file:" + pwf,
                        "-nodes", "-out", pem] + extra, capture_output=True)
    if r.returncode == 0:
        break
else:
    sys.exit("DEMO: openssl could not open the certificate with the password")

objs = []
def obj(body):
    objs.append(body)
    return len(objs)
text = b"BT /F1 16 Tf 72 720 Td (FICTITIOUS DOCUMENT - shutup-and-run signing demo) Tj " \
       b"0 -24 Td /F1 11 Tf (Made-up content. Not a filing. Not sent anywhere.) Tj ET"
obj(b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R] /SigFlags 3 >> >>")
obj(b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
obj(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R "
    b"/Resources << /Font << /F1 7 0 R >> >> /Annots [5 0 R] >>")
obj(b"<< /Length %d >>\nstream\n" % len(text) + text + b"\nendstream")
obj(b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (Signature1) /Rect [0 0 0 0] "
    b"/F 132 /P 3 0 R /V 6 0 R >>")
HOLE = 16384
obj(b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached "
    b"/Reason (shutup-and-run signing demo) /M (D:20240101000000Z) "
    b"/ByteRange [0 0000000000 0000000000 0000000000] /Contents <" + b"0" * HOLE + b"> >>")
obj(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
pdf = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n"
offsets = []
for n, body in enumerate(objs, 1):
    offsets.append(len(pdf))
    pdf += b"%d 0 obj\n" % n + body + b"\nendobj\n"
xref = len(pdf)
pdf += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1)
for off in offsets:
    pdf += b"%010d 00000 n \n" % off
pdf += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, xref)

start = pdf.index(b"/Contents <" + b"0" * 16) + len(b"/Contents ")
end = start + HOLE + 2
rng = b"[0 %010d %010d %010d]" % (start, end, len(pdf) - end)
pdf = pdf.replace(b"[0 0000000000 0000000000 0000000000]", rng)
open("/tmp/data", "wb").write(pdf[:start] + pdf[end:])
r = subprocess.run(["openssl", "cms", "-sign", "-binary", "-md", "sha256", "-in", "/tmp/data",
                    "-signer", pem, "-inkey", pem, "-outform", "DER", "-out", "/tmp/sig"],
                   capture_output=True)
os.remove(pem)
if r.returncode != 0:
    sys.exit("DEMO: openssl could not sign: " + r.stderr.decode(errors="replace")[-200:])
sig = open("/tmp/sig", "rb").read().hex().encode()
if len(sig) > HOLE:
    sys.exit("DEMO: the signature is larger than the space kept for it")
pdf = pdf[:start + 1] + sig + b"0" * (HOLE - len(sig)) + pdf[end - 1:]
b64 = base64.b64encode(pdf).decode()
print("DEMO_PDF_BEGIN")
for i in range(0, len(b64), 1000):
    print(b64[i:i + 1000])
print("DEMO_PDF_END")
"#;

/// The signed bytes the script printed, when it got that far.
pub(crate) fn pdf_from_logs(logs: &str) -> Option<Vec<u8>> {
    let begin = logs.find("DEMO_PDF_BEGIN")? + "DEMO_PDF_BEGIN".len();
    let end = logs[begin..].find("DEMO_PDF_END")? + begin;
    let b64: String = logs[begin..end].split_whitespace().collect();
    base64::engine::general_purpose::STANDARD.decode(b64).ok()
}

/// The two parts of a signed PDF: the bytes the signature covers, and the
/// signature itself (DER). Refuses a signature that leaves part of the file
/// uncovered, which is how a signed PDF is tampered with.
pub(crate) fn split_signed_pdf(pdf: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let find = |needle: &[u8], from: usize| {
        pdf[from..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|at| at + from)
    };
    let at = find(b"/ByteRange [", 0).ok_or("the PDF has no /ByteRange: it is not signed")?;
    let close = find(b"]", at).ok_or("the /ByteRange is not closed")?;
    let numbers: Vec<usize> = String::from_utf8_lossy(&pdf[at + 12..close])
        .split_whitespace()
        .filter_map(|n| n.parse().ok())
        .collect();
    let [a, b, c, d] = numbers[..] else {
        return Err("the /ByteRange does not have four numbers".into());
    };
    if a != 0 || b >= c || c.checked_add(d) != Some(pdf.len()) {
        return Err("the signature does not cover the whole file".into());
    }
    if pdf[b] != b'<' || pdf[c - 1] != b'>' {
        return Err("the /ByteRange does not frame the signature".into());
    }
    let hex = &pdf[b + 1..c - 1];
    let mut der = Vec::with_capacity(hex.len() / 2);
    for pair in hex.chunks(2) {
        let text = std::str::from_utf8(pair).map_err(|_| "the signature is not hex")?;
        der.push(u8::from_str_radix(text, 16).map_err(|_| "the signature is not hex")?);
    }
    // The space kept for the signature is padded with zeros. A DER SEQUENCE
    // says how long it is.
    let total = match der.as_slice() {
        [0x30, len, ..] if *len < 0x80 => 2 + *len as usize,
        [0x30, 0x81, len, ..] => 3 + *len as usize,
        [0x30, 0x82, hi, lo, ..] => 4 + ((*hi as usize) << 8 | *lo as usize),
        _ => return Err("the signature is not a DER structure".into()),
    };
    if total > der.len() {
        return Err("the signature is cut short".into());
    }
    der.truncate(total);
    let mut covered = pdf[..b].to_vec();
    covered.extend_from_slice(&pdf[c..]);
    Ok((covered, der))
}

/// Verify the signature with openssl, outside the container that made it:
/// the digest in the signature is the digest of the covered bytes, and the
/// signature was made by the key of the certificate inside it. The
/// certificate's own chain is not judged (`-noverify`).
pub(crate) fn verify_signed_pdf(pdf: &[u8], work: &Path) -> Result<(), String> {
    let (covered, der) = split_signed_pdf(pdf)?;
    let data = work.join("covered.bin");
    let sig = work.join("signature.der");
    std::fs::write(&data, covered).map_err(|e| e.to_string())?;
    std::fs::write(&sig, der).map_err(|e| e.to_string())?;
    let (ok, _, stderr) = certificate::openssl(
        &[
            "cms",
            "-verify",
            "-binary",
            "-noverify",
            "-inform",
            "DER",
            "-in",
            &sig.to_string_lossy(),
            "-content",
            &data.to_string_lossy(),
            "-out",
            "/dev/null",
        ],
        b"",
    )?;
    if ok {
        Ok(())
    } else {
        Err(format!(
            "openssl does not verify the signature: {}",
            stderr.lines().last().unwrap_or("")
        ))
    }
}

pub struct SignOptions<'a> {
    pub configured: bool,
    pub image: &'a str,
    /// Where the signed PDF goes. None: it is only returned.
    pub out: Option<PathBuf>,
    /// Put the signed PDF in the JSON, for a caller on another filesystem.
    pub inline: bool,
}

/// `shutup-and-run demo sign`.
pub async fn demo_sign(
    mut settings: Settings,
    runtime: Option<Arc<dyn Runtime>>,
    options: SignOptions<'_>,
) -> Out {
    let mut checks = Vec::new();
    let mut extra = json!({
        "image": options.image,
        "certificate": if options.configured { "configured" } else { "demo" },
        "signature": {
            "format": "adbe.pkcs7.detached",
            "digest": "sha256",
            "verified_with": "openssl cms -verify -noverify",
            "claims": "a basic PDF signature whose digest and signer key were verified. Not PAdES. The certificate's chain was not judged, and nothing here says a court's or ICP-Brasil's system accepts it.",
        },
        "output": Value::Null,
    });
    let good = "signing demo passed: a container signed a made-up PDF and the signature verifies";
    let bad = "signing demo failed: follow the `hint:` under each FAIL, then the demo again";

    // The workspace of this run: the throwaway certificate, and the files
    // openssl verifies. Removed at the end, whatever happened.
    let scratch = std::env::temp_dir().join(format!(
        "shutup-and-run-sign-{}",
        crate::launcher::random_id()
    ));
    let _ = std::fs::create_dir_all(&scratch);
    private_dir(&scratch);
    struct Remove(PathBuf);
    impl Drop for Remove {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _remove = Remove(scratch.clone());

    if options.configured {
        let found = certificate::validate_imported(&settings.certificate_dir);
        checks.push(
            check(
                "certificate",
                found.ok,
                if found.ok {
                    format!(
                        "the imported certificate, valid until {}",
                        found.not_after.as_deref().unwrap_or("?")
                    )
                } else {
                    found.detail.clone()
                },
            )
            .coded(found.code),
        );
        if !found.ok {
            return checks_out(&checks, extra, good, bad);
        }
    } else {
        let password = crate::launcher::random_id();
        let dir = scratch.join("certificate");
        let made = std::fs::create_dir_all(&dir)
            .map_err(|e| e.to_string())
            .and_then(|_| certificate::demo_pfx(&dir, &password))
            .and_then(|pfx| certificate::import(&dir, &pfx, &password));
        checks.push(
            check(
                "certificate",
                made.is_ok(),
                match &made {
                    Ok(()) => "a throwaway self-signed certificate, made for this run".to_string(),
                    Err(err) => err.clone(),
                },
            )
            .coded("OPENSSL_MISSING"),
        );
        if made.is_err() {
            return checks_out(&checks, extra, good, bad);
        }
        settings.certificate_dir = dir;
    }
    // The same passthrough a real job gets, for this one job.
    settings.certificate_enabled = true;

    let runtime = match runtime {
        Some(runtime) => Some(runtime),
        None => docker_runtime(&settings, &mut checks).await,
    };
    let Some(runtime) = runtime else {
        return checks_out(&checks, extra, good, bad);
    };
    let timeout = DEMO_TIMEOUT.min(settings.max_timeout_secs);
    let bench = match Bench::new(settings, runtime) {
        Ok(bench) => bench,
        Err(err) => {
            checks.push(check("workspace", false, err).coded("DEMO_WORKSPACE"));
            return checks_out(&checks, extra, good, bad);
        }
    };
    let mut job = request(options.image, timeout);
    job.command = Some(vec![
        "python3".into(),
        "-c".into(),
        "import os;exec(os.environ['DEMO_SCRIPT'])".into(),
    ]);
    job.env.insert("DEMO_SCRIPT".into(), SIGN_SCRIPT.into());
    let ran = run_one(&bench, job, &mut checks).await;
    let Some(ran) = ran.filter(|ran| ran.exit_code == Some(0)) else {
        if let Some(note) = checks.iter_mut().find(|c| c.name == "container" && !c.ok) {
            note.code = "DEMO_SIGN_FAILED".into();
        }
        return checks_out(&checks, extra, good, bad);
    };

    let read_only = ran.logs.contains("DEMO_MOUNT=read-only");
    checks.push(
        check(
            "mount",
            read_only,
            if read_only {
                "the container could read the certificate and could not write to it"
            } else {
                "the container could write to the certificate file"
            },
        )
        .coded("CERTIFICATE_MOUNT_WRITABLE"),
    );
    let pdf = pdf_from_logs(&ran.logs);
    let verified = match &pdf {
        Some(pdf) => verify_signed_pdf(pdf, &scratch),
        None => Err("the container printed no signed PDF".to_string()),
    };
    checks.push(
        check(
            "signature",
            verified.is_ok(),
            match &verified {
                Ok(()) => format!(
                    "the signed PDF ({} bytes) verifies: the signature covers the whole file",
                    pdf.as_ref().map(Vec::len).unwrap_or(0)
                ),
                Err(err) => err.clone(),
            },
        )
        .coded("SIGNATURE_INVALID"),
    );
    if let (Some(pdf), Ok(())) = (&pdf, &verified) {
        if options.inline {
            extra["signed_pdf_base64"] =
                Value::String(base64::engine::general_purpose::STANDARD.encode(pdf));
        }
        if let Some(out) = &options.out {
            let wrote = std::fs::write(out, pdf);
            checks.push(
                check(
                    "output",
                    wrote.is_ok(),
                    match &wrote {
                        Ok(()) => format!("wrote {}", out.display()),
                        Err(err) => format!("{}: {err}", out.display()),
                    },
                )
                .coded("OUTPUT_NOT_WRITTEN"),
            );
            if wrote.is_ok() {
                extra["output"] = json!(out);
            }
        }
    }
    checks_out(&checks, extra, good, bad)
}

pub async fn demo_cli(args: &[String]) -> ExitCode {
    let args = match parsed(args, &["--configured", "--inline"], &["--image", "--out"]) {
        Ok(args) => args,
        Err(code) => return code,
    };
    let as_json = args.has("--json");
    let settings = match Settings::load() {
        Ok(settings) => settings,
        Err(err) => {
            return emit(
                Out::fail(
                    EXIT_INVALID,
                    format!(
                        "{}. `shutup-and-run config check` says what is wrong.",
                        err.0
                    ),
                ),
                as_json,
            )
        }
    };
    let words: Vec<&str> = args.words.iter().map(String::as_str).collect();
    let image = args.values.get("--image").map(String::as_str);
    let out = match words[..] {
        [] => demo(settings, None, image.unwrap_or(DEMO_IMAGE)).await,
        ["sign"] => {
            let inline = args.has("--inline");
            let out = args.values.get("--out").map(PathBuf::from).or_else(|| {
                (!inline).then(|| PathBuf::from("shutup-and-run-demo-signed.pdf"))
            });
            demo_sign(
                settings,
                None,
                SignOptions {
                    configured: args.has("--configured"),
                    image: image.unwrap_or(SIGN_IMAGE),
                    out,
                    inline,
                },
            )
            .await
        }
        _ => {
            return emit(
                Out::fail(
                    EXIT_USAGE,
                    "usage: shutup-and-run demo [sign [--configured] [--out FILE]] [--image IMAGE] [--json]",
                ),
                as_json,
            )
        }
    };
    emit_checks(out, as_json)
}

// ---------------------------------------------------------------------------
// certificate

pub const RISK: &str = "When enabled, every container executed by this worker can access the \
configured certificate and its password. Read-only mounting protects the original file from \
modification but does not prevent copying, exporting, or using its private key. Enable this \
feature only if you trust every workload that may run on this machine.";

fn load_map(path: &Path) -> Result<JsonMap, String> {
    if path.exists() {
        config::read_json(path).map_err(|e| e.0)
    } else {
        Ok(config::default_map())
    }
}

/// What the machine has, with nothing secret in it. The original path is
/// not kept; the file's name is, so the owner can tell which one this is.
pub fn certificate_status(config_path: &Path) -> Value {
    let map = load_map(config_path).unwrap_or_default();
    let dir = config::certificate_dir_for(config_path);
    json!({
        "enabled": map.get("certificate_enabled").and_then(Value::as_bool).unwrap_or(false),
        "imported": certificate::imported(&dir),
        "name": map.get("certificate_name"),
        "container_certificate": certificate::CONTAINER_CERT,
        "container_password_file": certificate::CONTAINER_PASSWORD,
        "environment": [certificate::ENV_CERT, certificate::ENV_PASSWORD_FILE],
    })
}

/// Import a certificate and turn passthrough on. The file is checked in a
/// private folder first: a file or password that does not validate changes
/// nothing. `accepted` is the owner's consent, given on this machine.
pub fn certificate_set(
    config_path: &Path,
    name: &str,
    pfx: &[u8],
    password: &str,
    accepted: bool,
) -> Result<certificate::Validation, Out> {
    if !accepted {
        return Err(Out {
            code: EXIT_USAGE,
            json: json!({"ok": false, "code": "CONSENT_REQUIRED", "error": RISK}),
            text: format!("{RISK}\nTo accept that, add --accept-risk."),
        });
    }
    let invalid = |v: &certificate::Validation| Out {
        code: EXIT_INVALID,
        json: json!({"ok": false, "code": v.code, "error": v.detail, "fix": v.fix}),
        text: format!("{}: {}", v.code, v.detail),
    };
    let failed = |msg: String| Out::fail(EXIT_FAILED, msg);
    let dir = config::certificate_dir_for(config_path);
    let trial = dir.with_file_name(format!("certificate-{}", crate::launcher::random_id()));
    let found =
        certificate::import(&trial, pfx, password).map(|_| certificate::validate_imported(&trial));
    certificate::forget(&trial);
    let found = found.map_err(|msg| Out {
        code: EXIT_INVALID,
        json: json!({"ok": false, "code": "CERTIFICATE_INVALID", "error": msg}),
        text: msg,
    })?;
    if !found.ok {
        return Err(invalid(&found));
    }
    let mut map = load_map(config_path).map_err(failed)?;
    certificate::import(&dir, pfx, password).map_err(failed)?;
    let shown: String = Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .take(64)
        .collect();
    map.insert("certificate_enabled".into(), json!(true));
    map.insert("certificate_consent".into(), json!(true));
    map.insert(
        "certificate_name".into(),
        json!(if shown.is_empty() {
            "certificate"
        } else {
            &shown
        }),
    );
    config::save_json(config_path, &map).map_err(|e| failed(e.0))?;
    Ok(found)
}

pub fn certificate_disable(config_path: &Path, forget: bool) -> Result<(), String> {
    let mut map = load_map(config_path)?;
    map.insert("certificate_enabled".into(), json!(false));
    map.remove("certificate_consent");
    if forget {
        map.remove("certificate_name");
        certificate::forget(&config::certificate_dir_for(config_path));
    }
    config::save_json(config_path, &map).map_err(|e| e.0)
}

fn read_password(from_stdin: bool) -> Result<String, String> {
    if from_stdin {
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| format!("could not read the password from stdin: {e}"))?;
        return Ok(line.trim_end_matches(['\n', '\r']).to_string());
    }
    std::env::var("SHUTUP_AND_RUN_CERT_PASSWORD").map_err(|_| {
        "the certificate password is not given: set SHUTUP_AND_RUN_CERT_PASSWORD, or pass \
         --password-stdin and write it on stdin. It is never a command-line argument."
            .to_string()
    })
}

/// The JSON the Windows supervisor sends on stdin, because the file is on
/// the Windows side and the service is not.
#[derive(serde::Deserialize)]
struct Handover {
    name: String,
    pfx_base64: String,
    password: String,
}

pub fn certificate_cli(args: &[String]) -> ExitCode {
    let args = match parsed(
        args,
        &[
            "--accept-risk",
            "--password-stdin",
            "--apply",
            "--forget",
            "--stdin-json",
        ],
        &["--file"],
    ) {
        Ok(args) => args,
        Err(code) => return code,
    };
    let as_json = args.has("--json");
    let path = match config::json_path_for_cli() {
        Ok(path) => path,
        Err(err) => return emit(Out::fail(EXIT_FAILED, err.0), as_json),
    };
    let words: Vec<&str> = args.words.iter().map(String::as_str).collect();
    let mut out = match words[..] {
        ["status"] => {
            let status = certificate_status(&path);
            let text = format!(
                "passthrough: {}\nimported: {}\nin a container: {} and {}",
                if status["enabled"] == true { "ON" } else { "off" },
                match (status["imported"] == true, status["name"].as_str()) {
                    (true, Some(name)) => name.to_string(),
                    (true, None) => "yes".to_string(),
                    _ => "no certificate".to_string(),
                },
                certificate::CONTAINER_CERT,
                certificate::CONTAINER_PASSWORD
            );
            Out::ok(status, text)
        }
        ["validate"] => {
            let found = certificate::validate_imported(&config::certificate_dir_for(&path));
            validation_out(&found)
        }
        ["set"] => {
            let given = if args.has("--stdin-json") {
                let mut text = String::new();
                use std::io::Read;
                std::io::stdin()
                    .take(1024 * 1024)
                    .read_to_string(&mut text)
                    .map_err(|e| e.to_string())
                    .and_then(|_| serde_json::from_str::<Handover>(&text).map_err(|e| e.to_string()))
                    .and_then(|h| {
                        base64::engine::general_purpose::STANDARD
                            .decode(h.pfx_base64.trim())
                            .map(|pfx| (h.name, pfx, h.password))
                            .map_err(|e| e.to_string())
                    })
            } else {
                match args.values.get("--file") {
                    None => Err("--file PATH names the .pfx or .p12 file".to_string()),
                    Some(file) => std::fs::read(file)
                        .map_err(|e| format!("{file}: {e}"))
                        .and_then(|pfx| {
                            read_password(args.has("--password-stdin"))
                                .map(|password| (file.clone(), pfx, password))
                        }),
                }
            };
            match given {
                Err(msg) => Out::fail(EXIT_FAILED, msg),
                Ok((name, pfx, password)) => {
                    match certificate_set(&path, &name, &pfx, &password, args.has("--accept-risk")) {
                        Ok(found) => {
                            let mut out = validation_out(&found);
                            out.json["enabled"] = json!(true);
                            out.text = format!(
                                "{}\npassthrough is ON: every container this machine starts can read the certificate and its password.{}",
                                out.text,
                                if args.has("--apply") { "" } else { "\nIt takes effect when the service restarts: `shutup-and-run config apply`." }
                            );
                            out
                        }
                        Err(out) => out,
                    }
                }
            }
        }
        ["disable"] => match certificate_disable(&path, args.has("--forget")) {
            Ok(()) => Out::ok(
                json!({"enabled": false, "forgotten": args.has("--forget")}),
                if args.has("--forget") {
                    "passthrough is off and the imported certificate is deleted"
                } else {
                    "passthrough is off; the imported certificate is kept (--forget deletes it)"
                },
            ),
            Err(msg) => Out::fail(EXIT_FAILED, msg),
        },
        _ => Out::fail(
            EXIT_USAGE,
            "usage: shutup-and-run certificate status|validate|set --file PATH --accept-risk|disable [--forget]. See `shutup-and-run demo --help`.",
        ),
    };
    if out.code == 0 && args.has("--apply") && matches!(words[..], ["set"] | ["disable"]) {
        match crate::cli::apply(&path) {
            Ok(applied) => {
                out.json["applied"] = json!(true);
                out.text = format!("{}\n{}", out.text, applied.text).trim().to_string();
            }
            Err(msg) => out = Out::fail(EXIT_FAILED, msg),
        }
    }
    emit(out, as_json)
}

fn validation_out(found: &certificate::Validation) -> Out {
    let json = serde_json::to_value(found).unwrap_or_default();
    let text = format!(
        "{}: {}{}",
        found.code,
        found.detail,
        match (&found.subject, &found.not_after) {
            (Some(subject), Some(until)) => format!("\n{subject}\nvalid until {until}"),
            _ => String::new(),
        }
    );
    Out {
        code: if found.ok { 0 } else { EXIT_INVALID },
        json,
        text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockRuntime;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shutup-and-run-devx-{name}-{}-{}",
            std::process::id(),
            crate::launcher::random_id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn settings(dir: &Path, extra: &str) -> Settings {
        Settings::from_toml_and_env(
            &format!(
                "token = \"test-token-value-ok\"\nmax_slots = 1\nallowlist = [\"hello-world\"]\n\
                 history_path = '{}'\nresource_profile = \"shared-pc\"\nmin_free_memory = \"0\"\n\
                 min_free_disk = \"0\"\nmax_job_memory = \"1g\"\n{extra}",
                dir.join("jobs.sqlite").display()
            ),
            &HashMap::new(),
        )
        .unwrap()
    }

    fn openssl_here() -> bool {
        certificate::openssl(&["version"], b"").is_ok_and(|(ok, _, _)| ok)
    }

    fn codes(out: &Out) -> Vec<String> {
        out.json["checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["code"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn version_is_the_build_and_nothing_is_hardcoded() {
        let v = version_json();
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(v["name"], "shutup-and-run");
        assert_eq!(v["platform"], std::env::consts::OS);
        assert_eq!(v["architecture"], std::env::consts::ARCH);
        assert_eq!(v["api_version"], "v1");
        assert!(matches!(v["runtime"].as_str(), Some("docker" | "wsl2")));
    }

    #[tokio::test]
    async fn the_demo_runs_one_container_under_the_machines_rules_and_cleans_up() {
        let dir = temp("demo");
        let mock = Arc::new(MockRuntime::new());
        mock.exit_on_start(Some(0)).await;
        let runtime: Arc<dyn Runtime> = mock.clone();
        let cfg = settings(&dir, "");
        let before = std::fs::read_dir(&dir).unwrap().count();
        let out = demo(cfg.clone(), Some(runtime.clone()), "hello-world").await;
        assert_eq!(out.code, 0, "{}", out.text);
        assert_eq!(out.json["ok"], true);
        assert_eq!(
            codes(&out),
            vec![
                "IMAGE_OK",
                "ADMISSION_OK",
                "CONTAINER_OK",
                "LIMITS_OK",
                "CLEANUP_OK"
            ]
        );
        // The container carried the machine's limits and is gone.
        let spec = &mock.specs().await[0];
        assert_eq!(
            (spec.nano_cpus, spec.memory_bytes),
            (Some(1_000_000_000), Some(1 << 30))
        );
        assert!(mock.running_ids().await.is_empty());
        // The worker's own history was not written, and its folder is as it was.
        assert!(!cfg.history_path.exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), before);

        // The demo does not get around the allowlist.
        let out = demo(cfg.clone(), Some(runtime.clone()), "docker.io/other/image").await;
        assert_eq!(out.code, EXIT_FAILED);
        assert_eq!(out.json["code"], "DEMO_IMAGE_NOT_ALLOWED");
        assert!(out.json["checks"][0]["fix"]
            .as_str()
            .unwrap()
            .contains("config set allowlist"));
        assert_eq!(mock.specs().await.len(), 1);

        // A container that fails, an image that does not start, a full machine.
        mock.exit_on_start(Some(3)).await;
        let out = demo(cfg.clone(), Some(runtime.clone()), "hello-world").await;
        assert_eq!(
            (out.code, out.json["code"].as_str()),
            (EXIT_FAILED, Some("DEMO_JOB_FAILED"))
        );
        mock.fail_start(true).await;
        let out = demo(cfg.clone(), Some(runtime.clone()), "hello-world").await;
        assert_eq!(out.json["code"], "DEMO_IMAGE_UNAVAILABLE");
        mock.fail_start(false).await;
        mock.seed_running("busy", 9_999_999_999).await;
        let out = demo(cfg, Some(runtime), "hello-world").await;
        assert_eq!(out.json["code"], "WORKER_BUSY");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_preflight_checks_storage_and_the_allowlist_without_writing() {
        let dir = temp("preflight");
        let cfg = settings(&dir, "");
        let before = std::fs::read_dir(&dir).unwrap().count();
        assert!(storage_check(&cfg).ok);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), before);
        let mut nowhere = cfg.clone();
        nowhere.history_path = PathBuf::from("/proc/nope/deeper/jobs.sqlite");
        let bad = storage_check(&nowhere);
        if cfg!(unix) {
            assert!(!bad.ok);
            assert_eq!(bad.code(), "STORAGE_NOT_WRITABLE");
        }
        assert_eq!(allowlist_check(&cfg).code(), "ALLOWLIST_OK");
        assert!(crate::cli::certificate_check(&cfg).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A poll URL whose host answers and whose path is wrong is not "ok".
    /// The probe carries no token and offers no slot.
    #[tokio::test]
    async fn the_preflight_reads_the_http_status_of_the_poll_url() {
        use axum::http::{HeaderMap, StatusCode};
        use std::sync::{Arc, Mutex};
        let seen: Arc<Mutex<Vec<(bool, serde_json::Value)>>> = Arc::default();
        let record = seen.clone();
        let poll = move |headers: HeaderMap, axum::Json(body): axum::Json<serde_json::Value>| {
            let record = record.clone();
            async move {
                let with_token = headers.contains_key("authorization");
                record.lock().unwrap().push((with_token, body));
                StatusCode::UNAUTHORIZED
            }
        };
        let app = axum::Router::new()
            .route("/poll", axum::routing::post(poll))
            .route(
                "/busy",
                axum::routing::post(|| async { StatusCode::TOO_MANY_REQUESTS }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let dir = temp("poll-status");
        let at = |path: &str| {
            let mut cfg = settings(&dir, "");
            cfg.poll_url = Some(format!("{base}{path}"));
            cfg.poll_token = settings(&dir, "").poll_token;
            cfg
        };
        let found = poll_check(&at("/poll?cpf=00000000000")).await;
        assert!(found.ok, "{found:?}");
        assert_eq!(found.code(), "POLL_OK");
        assert!(found.detail.contains("cpf=***") && !found.detail.contains("00000000000"));
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert!(!seen[0].0, "the probe sent a token");
        assert_eq!(seen[0].1["free_slots"], 0);

        let wrong = poll_check(&at("/no-such-path?cpf=00000000000")).await;
        assert!(!wrong.ok);
        assert_eq!(wrong.code(), "POLL_HTTP_ERROR");
        assert!(wrong.detail.contains("HTTP 404") && !wrong.detail.contains("00000000000"));
        assert!(wrong.hint().contains("config set poll_url"));

        assert_eq!(poll_check(&at("/busy")).await.code(), "POLL_RATE_LIMITED");
        let mut none = settings(&dir, "");
        none.poll_url = None;
        assert_eq!(poll_check(&none).await.code(), "POLL_URL_UNSET");

        // The doctor reads the same from what the service said.
        assert_eq!(
            crate::cli::http_status_in("poll returned 404 Not Found"),
            Some(404)
        );
        assert_eq!(crate::cli::http_status_in("error sending request"), None);
        let said = crate::cli::poll_answer_check("https://jobs.example/poll", 404, false);
        assert_eq!(
            (said.code().as_str(), said.soft),
            ("POLL_HTTP_ERROR", false)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_signed_pdf_must_cover_the_whole_file() {
        // 0x30 0x03 + three bytes: the smallest DER structure, zero padded.
        let hole = "3003010203000000";
        let head = b"%PDF /ByteRange [0 0000000040 0000000058 0000000005] /C ".to_vec();
        assert_eq!(head.len(), 40 + 16);
        let mut pdf = b"%PDF-1.7 ......".to_vec();
        let lead = format!("/ByteRange [0 {:010} {:010} {:010}] /Contents ", 0, 0, 0);
        let start = pdf.len() + lead.len();
        let end = start + hole.len() + 2;
        let tail = b" >> %%EOF";
        pdf.extend_from_slice(
            format!(
                "/ByteRange [0 {start:010} {end:010} {:010}] /Contents ",
                tail.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(format!("<{hole}>").as_bytes());
        pdf.extend_from_slice(tail);
        let (covered, der) = split_signed_pdf(&pdf).unwrap();
        assert_eq!(der, vec![0x30, 0x03, 0x01, 0x02, 0x03]);
        assert_eq!(covered.len(), pdf.len() - hole.len() - 2);
        // Bytes added after the signed range are not covered: refused.
        let mut longer = pdf.clone();
        longer.extend_from_slice(b"extra page");
        assert!(split_signed_pdf(&longer)
            .unwrap_err()
            .contains("whole file"));
        assert!(split_signed_pdf(b"%PDF plain")
            .unwrap_err()
            .contains("not signed"));

        let logs = "noise\nDEMO_MOUNT=read-only\nDEMO_PDF_BEGIN\nJVBE\nRi0x\nDEMO_PDF_END\n";
        assert_eq!(pdf_from_logs(logs).unwrap(), b"%PDF-1");
        assert!(pdf_from_logs("DEMO_PDF_BEGIN\nJVBE").is_none());
    }

    #[test]
    fn a_certificate_is_checked_before_it_is_kept_and_consent_is_asked() {
        if !openssl_here() {
            eprintln!("skipped: no openssl program");
            return;
        }
        let dir = temp("cert");
        let config_path = dir.join("config.json");
        // A folder with a space and an accent, as a Windows profile has.
        let source = dir.join("Área de Trabalho ç");
        std::fs::create_dir_all(&source).unwrap();
        let good = certificate::demo_pfx(&source, "s3cret pass").unwrap();
        let file = source.join("meu certificado.pfx");
        std::fs::write(&file, &good).unwrap();
        let name = file.to_string_lossy().to_string();

        // No consent: nothing is imported and nothing is enabled.
        let refused =
            certificate_set(&config_path, &name, &good, "s3cret pass", false).unwrap_err();
        assert_eq!(refused.code, EXIT_USAGE);
        assert_eq!(refused.json["code"], "CONSENT_REQUIRED");
        assert!(!config_path.exists());
        // Wrong password, and a file that is no certificate: exit 3, nothing kept.
        for (pfx, password, code) in [
            (good.as_slice(), "wrong", "CERTIFICATE_PASSWORD_WRONG"),
            (&b"not a pfx"[..], "s3cret pass", "CERTIFICATE_INVALID"),
        ] {
            let bad = certificate_set(&config_path, &name, pfx, password, true).unwrap_err();
            assert_eq!(
                (bad.code, bad.json["code"].as_str()),
                (EXIT_INVALID, Some(code))
            );
            assert!(!bad.text.contains("s3cret") && !bad.json.to_string().contains("s3cret"));
        }
        assert!(!certificate::imported(&config::certificate_dir_for(
            &config_path
        )));
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "a trial folder was left"
        );

        let found = certificate_set(&config_path, &name, &good, "s3cret pass", true).unwrap();
        assert!(found.ok && found.has_private_key);
        assert!(found.subject.unwrap().contains("not a real certificate"));
        let text = std::fs::read_to_string(&config_path).unwrap();
        assert!(text.contains("\"certificate_enabled\": true"));
        assert!(text.contains("\"certificate_name\": \"meucertificado.pfx\""));
        // Neither the password nor the folder it came from is in the config.
        assert!(!text.contains("s3cret") && !text.contains("Trabalho"));
        let status = certificate_status(&config_path);
        assert_eq!(
            (status["enabled"].as_bool(), status["imported"].as_bool()),
            (Some(true), Some(true))
        );
        assert!(!status.to_string().contains("s3cret"));
        let loaded = Settings::load_json(&config_path);
        assert!(loaded.certificate_enabled && loaded.config_warning.is_none());
        assert_eq!(
            crate::cli::certificate_check(&loaded).unwrap().code(),
            "CERTIFICATE_OK"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let kept = certificate::password_path(&loaded.certificate_dir);
            assert_eq!(
                std::fs::metadata(kept).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // The original was read, not changed.
        assert_eq!(std::fs::read(&file).unwrap(), good);

        // Enabled by hand without the consent key: the config is refused.
        let mut by_hand = config::read_json(&config_path).unwrap();
        by_hand.remove("certificate_consent");
        assert!(Settings::from_json_map(&by_hand)
            .unwrap_err()
            .to_string()
            .contains("certificate_consent"));

        certificate_disable(&config_path, false).unwrap();
        assert!(!Settings::load_json(&config_path).certificate_enabled);
        assert!(certificate::imported(&config::certificate_dir_for(
            &config_path
        )));
        certificate_disable(&config_path, true).unwrap();
        assert!(!certificate::imported(&config::certificate_dir_for(
            &config_path
        )));
        assert_eq!(
            certificate::validate_imported(&config::certificate_dir_for(&config_path)).code,
            "CERTIFICATE_MISSING"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_expired_certificate_and_one_without_a_key_are_named() {
        if !openssl_here() {
            eprintln!("skipped: no openssl program");
            return;
        }
        let dir = temp("cert-bad");
        let at = |name: &str| dir.join(name).to_string_lossy().to_string();
        let run = |args: &[&str]| certificate::openssl(args, b"").unwrap().0;
        assert!(run(&["genrsa", "-out", &at("k.pem"), "2048"]));
        assert!(run(&[
            "req",
            "-new",
            "-key",
            &at("k.pem"),
            "-subj",
            "/CN=old",
            "-out",
            &at("r.csr")
        ]));
        // Signed to have ended yesterday. An openssl that refuses a negative
        // number of days leaves this case to the ones that do not.
        if !run(&[
            "x509",
            "-req",
            "-in",
            &at("r.csr"),
            "-signkey",
            &at("k.pem"),
            "-days",
            "-1",
            "-out",
            &at("c.pem"),
        ]) {
            eprintln!("skipped: this openssl does not make an expired certificate");
            return;
        }
        assert!(run(&[
            "pkcs12",
            "-export",
            "-inkey",
            &at("k.pem"),
            "-in",
            &at("c.pem"),
            "-out",
            &at("expired.pfx"),
            "-passout",
            "pass:pw"
        ]));
        assert!(run(&[
            "pkcs12",
            "-export",
            "-nokeys",
            "-in",
            &at("c.pem"),
            "-out",
            &at("nokey.pfx"),
            "-passout",
            "pass:pw"
        ]));
        let expired = certificate::validate(&dir.join("expired.pfx"), "pw");
        assert_eq!((expired.ok, expired.code), (false, "CERTIFICATE_EXPIRED"));
        assert!(expired.has_private_key && expired.not_after.is_some());
        let nokey = certificate::validate(&dir.join("nokey.pfx"), "pw");
        assert_eq!(
            (nokey.ok, nokey.code),
            (false, "CERTIFICATE_NO_PRIVATE_KEY")
        );
        assert_eq!(
            certificate::validate(&dir.join("absent.pfx"), "pw").code,
            "CERTIFICATE_MISSING"
        );
        assert!(certificate::check_password("two\nlines").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
