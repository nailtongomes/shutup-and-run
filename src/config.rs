use crate::allowlist::validate_image_name;
use crate::auth::TokenChecker;
use serde::Deserialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

const ENV_KEYS: &[&str] = &[
    "SHUTUP_AND_RUN_CONFIG",
    "SHUTUP_AND_RUN_LISTEN",
    "SHUTUP_AND_RUN_TOKEN",
    "SHUTUP_AND_RUN_MAX_SLOTS",
    "SHUTUP_AND_RUN_ALLOWLIST",
    "SHUTUP_AND_RUN_DEFAULT_TIMEOUT_SECS",
    "SHUTUP_AND_RUN_MAX_TIMEOUT_SECS",
    "SHUTUP_AND_RUN_REAPER_INTERVAL_SECS",
    "SHUTUP_AND_RUN_DOCKER_CONFIG",
    "SHUTUP_AND_RUN_HISTORY_PATH",
    "SHUTUP_AND_RUN_HISTORY_DAYS",
    "SHUTUP_AND_RUN_PURGE_INTERVAL_SECS",
    "SHUTUP_AND_RUN_LOG_LINES",
    "SHUTUP_AND_RUN_PRUNE_INTERVAL_SECS",
    "SHUTUP_AND_RUN_HTTP",
    "SHUTUP_AND_RUN_TLS_CERT",
    "SHUTUP_AND_RUN_TLS_KEY",
    "SHUTUP_AND_RUN_AUTH_FAILURES",
    "SHUTUP_AND_RUN_BAN_HOURS",
    "SHUTUP_AND_RUN_TRUST_X_FORWARDED_FOR",
    "SHUTUP_AND_RUN_POLL_URL",
    "SHUTUP_AND_RUN_POLL_INTERVAL_SECS",
    "SHUTUP_AND_RUN_POLL_TOKEN",
    "SHUTUP_AND_RUN_POLL_ANY_IMAGE",
    "SHUTUP_AND_RUN_WORKER_NAME",
    "SHUTUP_AND_RUN_WORKER_OS",
    "SHUTUP_AND_RUN_RESOURCE_PROFILE",
    "SHUTUP_AND_RUN_MAX_JOB_CPUS",
    "SHUTUP_AND_RUN_MAX_JOB_MEMORY",
    "SHUTUP_AND_RUN_MIN_FREE_MEMORY",
    "SHUTUP_AND_RUN_MIN_FREE_DISK",
];

const DEFAULT_LISTEN: &str = "127.0.0.1:8787";
const DEFAULT_TIMEOUT: u64 = 3600;
const DEFAULT_MAX_TIMEOUT: u64 = 21_600;
const DEFAULT_REAPER: u64 = 2;
const DEFAULT_HISTORY: &str = "/var/lib/shutup-and-run/jobs.sqlite";
const DEFAULT_HISTORY_DAYS: u64 = 30;
const DEFAULT_PURGE: u64 = 3600;
const DEFAULT_LOG_LINES: u64 = 200;
/// The least a machine waits between two polls, and the default. One number
/// for the service, the CLI, the page and the installers: a poll URL serves
/// many machines, and a machine set to a second or two is a flood.
pub const MIN_POLL_INTERVAL_SECS: u64 = 30;
const DEFAULT_POLL_INTERVAL: u64 = MIN_POLL_INTERVAL_SECS;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ConfigError(pub String);

#[derive(Clone, Debug)]
pub struct Settings {
    pub listen: SocketAddr,
    pub max_slots: usize,
    pub allowlist: Vec<String>,
    pub default_timeout_secs: u64,
    pub max_timeout_secs: u64,
    pub reaper_interval: Duration,
    pub docker_config: Option<PathBuf>,
    pub history_path: PathBuf,
    pub history_days: u64,
    pub purge_interval: Duration,
    pub log_lines: usize,
    /// Zero disables pruning.
    pub prune_interval: Duration,
    /// Plain HTTP. Off unless set. Required only for localhost or a private network.
    pub http_plain: bool,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub auth_failures: u32,
    pub ban_secs: u64,
    pub trust_forwarded: bool,
    /// `GET /` for a loopback peer, with no token. Off unless set.
    pub status_page: bool,
    pub token: TokenChecker,
    /// Unset means push only. The process does not call out.
    pub poll_url: Option<String>,
    pub poll_interval: Duration,
    /// The interval the config asked for, when it was under the minimum and
    /// the minimum is what runs. An old config must still start the worker.
    pub poll_interval_raised_from: Option<u64>,
    /// Bearer token for `poll_url`. Environment only, so it cannot be committed in TOML.
    pub poll_token: Option<PollToken>,
    /// A task from `poll_url` skips the allowlist. Push jobs never do. Needs `poll_url`.
    pub poll_any_image: bool,
    /// Sent in the poll body so one URL can serve many machines. Default: the hostname.
    pub worker_name: String,
    /// Sent in the poll body. Default: the OS this binary was built for. The
    /// Windows supervisor sets `windows` for the binary it runs inside WSL.
    pub worker_os: String,
    /// The `shared-pc` profile: the machine is someone's everyday computer,
    /// so the four limits below have defaults. Off, they are unset and zero
    /// unless the config names them.
    pub shared_pc: bool,
    /// The most CPU one job gets, and what a job that asks for none gets.
    /// None: no limit.
    pub max_job_cpus: Option<f64>,
    /// The same for memory, in bytes.
    pub max_job_memory: Option<u64>,
    /// No new job starts with less memory available than this. Zero: off.
    pub min_free_memory: u64,
    /// The same for the disk Docker writes to.
    pub min_free_disk: u64,
    /// Certificate passthrough: every container of this machine gets the
    /// owner's PKCS#12 file and its password. Off unless the owner turned it
    /// on and accepted that in the config.
    pub certificate_enabled: bool,
    /// The file name the certificate was imported from. For display only.
    pub certificate_name: Option<String>,
    /// Where the imported certificate and its password are kept, mode 600.
    pub certificate_dir: PathBuf,
    /// Where each job's own read-only copies live while it runs.
    pub secrets_dir: PathBuf,
    /// The JSON file this config came from. None: config.toml and environment.
    pub config_json: Option<PathBuf>,
    /// The login for the dangerous fields on the local page. Not strong:
    /// it keeps a curious person out, like a BIOS password.
    /// `en` or `pt`: the language of the hints the CLI prints.
    pub language: String,
    pub admin_user: String,
    pub admin_password: String,
    /// Set when the JSON file was unusable and an older copy or the built-in
    /// defaults are running instead. The page, status.json and the doctor show it.
    pub config_warning: Option<String>,
}

/// Outbound poll secret. Debug output is redacted.
#[derive(Clone)]
pub struct PollToken(String);

impl std::fmt::Debug for PollToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PollToken(***)")
    }
}

impl PollToken {
    fn new(token: &str) -> Self {
        Self(token.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    listen: Option<String>,
    token: Option<String>,
    max_slots: Option<usize>,
    allowlist: Option<Vec<String>>,
    default_timeout_secs: Option<u64>,
    max_timeout_secs: Option<u64>,
    reaper_interval_secs: Option<u64>,
    docker_config: Option<String>,
    history_path: Option<String>,
    history_days: Option<u64>,
    purge_interval_secs: Option<u64>,
    log_lines: Option<u64>,
    prune_interval_secs: Option<u64>,
    http: Option<bool>,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    auth_failures: Option<u64>,
    ban_hours: Option<u64>,
    trust_x_forwarded_for: Option<bool>,
    status_page: Option<bool>,
    poll_url: Option<String>,
    poll_interval_secs: Option<u64>,
    poll_any_image: Option<bool>,
    worker_name: Option<String>,
    worker_os: Option<String>,
    resource_profile: Option<String>,
    max_job_cpus: Option<String>,
    max_job_memory: Option<String>,
    min_free_memory: Option<String>,
    min_free_disk: Option<String>,
    certificate_enabled: Option<bool>,
    certificate_consent: Option<bool>,
    certificate_name: Option<String>,
    // config.json only. config.toml refuses these three.
    poll_token: Option<String>,
    admin_user: Option<String>,
    language: Option<String>,
    admin_password: Option<String>,
}

impl Settings {
    pub fn load() -> Result<Self, ConfigError> {
        let env = process_env();
        if let Some(path) = json_path(&env) {
            return Ok(Self::load_json(&path));
        }
        let toml_text = read_config_file(&env)?;
        Self::from_toml_and_env(&toml_text, &env)
    }

    /// config.json never stops the service. An unusable file falls back to
    /// the copy written by the last save, then to the built-in defaults, and
    /// the warning says which.
    pub fn load_json(path: &Path) -> Self {
        let problem = match read_json(path).and_then(|map| Self::from_json_map(&map)) {
            Ok(mut settings) => {
                settings.config_json = Some(path.to_path_buf());
                settings.certificate_dir = certificate_dir_for(path);
                return settings;
            }
            Err(err) => err.0,
        };
        let (mut settings, running) =
            match read_json(&backup_path(path)).and_then(|map| Self::from_json_map(&map)) {
                Ok(settings) => (settings, "the copy from the last save"),
                Err(_) => (
                    Self::from_json_map(&default_map())
                        .expect("the built-in defaults are a valid config"),
                    "the built-in defaults (idle)",
                ),
            };
        settings.config_json = Some(path.to_path_buf());
        settings.certificate_dir = certificate_dir_for(path);
        settings.config_warning = Some(format!(
            "{} is not usable ({problem}); running on {running}",
            path.display()
        ));
        settings
    }

    /// The whole of config.json. The same rules as config.toml, with no
    /// environment on top: the file is the one source.
    pub fn from_json_map(map: &JsonMap) -> Result<Self, ConfigError> {
        let file: FileConfig = serde_json::from_value(serde_json::Value::Object(map.clone()))
            .map_err(|err| ConfigError(format!("config.json: {err}")))?;
        let mut env = HashMap::new();
        if let Some(token) = file.poll_token.clone().filter(|t| !t.is_empty()) {
            env.insert("SHUTUP_AND_RUN_POLL_TOKEN".to_string(), token);
        }
        let admin_user = file.admin_user.clone();
        let admin_password = file.admin_password.clone();
        let language = file.language.clone();
        let mut settings = Self::build(file, &env)?;
        match language.as_deref() {
            None => {}
            Some(lang @ ("en" | "pt")) => settings.language = lang.to_string(),
            Some(other) => {
                return Err(ConfigError(format!(
                    "language must be en or pt, not {other:?}"
                )))
            }
        }
        if let Some(user) = admin_user {
            settings.admin_user = validate_admin("admin_user", &user)?;
        }
        if let Some(password) = admin_password {
            settings.admin_password = validate_admin("admin_password", &password)?;
        }
        Ok(settings)
    }

    /// Only the SQLite path, for `shutup-and-run bans`. That command runs from
    /// an SSH shell, where the token in the systemd EnvironmentFile is not set,
    /// so it must not need the rest of the settings.
    pub fn history_path() -> Result<PathBuf, ConfigError> {
        let env = process_env();
        if let Some(path) = json_path(&env) {
            return Ok(Self::load_json(&path).history_path);
        }
        let toml_text = read_config_file(&env)?;
        Self::history_path_from(&toml_text, &env)
    }

    pub fn history_path_from(
        toml_text: &str,
        env: &HashMap<String, String>,
    ) -> Result<PathBuf, ConfigError> {
        let file: FileConfig = if toml_text.trim().is_empty() {
            FileConfig::default()
        } else {
            toml::from_str(toml_text).map_err(|err| ConfigError(format!("config.toml: {err}")))?
        };
        Ok(PathBuf::from(
            take_string(env, "SHUTUP_AND_RUN_HISTORY_PATH", file.history_path)
                .unwrap_or_else(|| DEFAULT_HISTORY.to_string()),
        ))
    }

    pub fn from_toml_and_env(
        toml_text: &str,
        env: &HashMap<String, String>,
    ) -> Result<Self, ConfigError> {
        let file: FileConfig = if toml_text.trim().is_empty() {
            FileConfig::default()
        } else {
            toml::from_str(toml_text).map_err(|err| ConfigError(format!("config.toml: {err}")))?
        };
        if file.poll_token.is_some()
            || file.admin_user.is_some()
            || file.admin_password.is_some()
            || file.language.is_some()
        {
            return Err(ConfigError(
                "config.toml: poll_token, admin_user, admin_password and language are config.json keys. \
                 The poll token of a config.toml machine is SHUTUP_AND_RUN_POLL_TOKEN."
                    .into(),
            ));
        }
        Self::build(file, env)
    }

    fn build(file: FileConfig, env: &HashMap<String, String>) -> Result<Self, ConfigError> {
        let listen = take_string(env, "SHUTUP_AND_RUN_LISTEN", file.listen)
            .unwrap_or_else(|| DEFAULT_LISTEN.to_string());
        let listen: SocketAddr = listen
            .parse()
            .map_err(|_| ConfigError(format!("invalid listen address: {listen}")))?;

        let token = take_string(env, "SHUTUP_AND_RUN_TOKEN", file.token).unwrap_or_default();
        if token.chars().any(|c| c.is_whitespace()) || token.len() < 16 {
            return Err(ConfigError(
                "token is missing or shorter than 16 characters. Set SHUTUP_AND_RUN_TOKEN or token in the file.".into(),
            ));
        }

        let max_slots = take_u64(
            env,
            "SHUTUP_AND_RUN_MAX_SLOTS",
            file.max_slots.map(|n| n as u64),
        )?
        .ok_or_else(|| ConfigError("max_slots is required".into()))?
            as usize;
        if !(1..=256).contains(&max_slots) {
            return Err(ConfigError("max_slots must be between 1 and 256".into()));
        }

        let poll_any_image =
            take_bool(env, "SHUTUP_AND_RUN_POLL_ANY_IMAGE", file.poll_any_image)?.unwrap_or(false);
        let allowlist = if let Some(raw) = env.get("SHUTUP_AND_RUN_ALLOWLIST") {
            split_allowlist(raw, poll_any_image)?
        } else {
            file.allowlist.unwrap_or_default()
        };
        // With poll_any_image the poll URL decides the image, so the list may be
        // empty. Then a push job has nothing to match and is refused.
        if allowlist.is_empty() && !poll_any_image {
            return Err(ConfigError(
                "allowlist is empty. List permitted images, for example ghcr.io/OWNER/selenium-worker.".into(),
            ));
        }
        for image in &allowlist {
            validate_image_name(image).map_err(ConfigError)?;
        }

        let default_timeout_secs = take_u64(
            env,
            "SHUTUP_AND_RUN_DEFAULT_TIMEOUT_SECS",
            file.default_timeout_secs,
        )?
        .unwrap_or(DEFAULT_TIMEOUT);
        let max_timeout_secs = take_u64(
            env,
            "SHUTUP_AND_RUN_MAX_TIMEOUT_SECS",
            file.max_timeout_secs,
        )?
        .unwrap_or(DEFAULT_MAX_TIMEOUT);
        if default_timeout_secs == 0 || max_timeout_secs == 0 {
            return Err(ConfigError("timeouts must be greater than zero".into()));
        }
        if default_timeout_secs > max_timeout_secs {
            return Err(ConfigError(
                "default_timeout_secs cannot be greater than max_timeout_secs".into(),
            ));
        }
        if max_timeout_secs > 7 * 24 * 3600 {
            return Err(ConfigError("max_timeout_secs is above 7 days".into()));
        }

        let reaper = take_u64(
            env,
            "SHUTUP_AND_RUN_REAPER_INTERVAL_SECS",
            file.reaper_interval_secs,
        )?
        .unwrap_or(DEFAULT_REAPER);
        if !(1..=300).contains(&reaper) {
            return Err(ConfigError(
                "reaper_interval_secs must be between 1 and 300".into(),
            ));
        }

        let history_days = take_u64(env, "SHUTUP_AND_RUN_HISTORY_DAYS", file.history_days)?
            .unwrap_or(DEFAULT_HISTORY_DAYS);
        if history_days > 3650 {
            return Err(ConfigError(
                "history_days must be between 0 and 3650".into(),
            ));
        }
        let purge = take_u64(
            env,
            "SHUTUP_AND_RUN_PURGE_INTERVAL_SECS",
            file.purge_interval_secs,
        )?
        .unwrap_or(DEFAULT_PURGE);
        if !(1..=86_400).contains(&purge) {
            return Err(ConfigError(
                "purge_interval_secs must be between 1 and 86400".into(),
            ));
        }
        let log_lines =
            take_u64(env, "SHUTUP_AND_RUN_LOG_LINES", file.log_lines)?.unwrap_or(DEFAULT_LOG_LINES);
        if !(1..=5000).contains(&log_lines) {
            return Err(ConfigError("log_lines must be between 1 and 5000".into()));
        }
        let prune = take_u64(
            env,
            "SHUTUP_AND_RUN_PRUNE_INTERVAL_SECS",
            file.prune_interval_secs,
        )?
        .unwrap_or(0);
        if prune > 86_400 {
            return Err(ConfigError(
                "prune_interval_secs must be between 0 and 86400".into(),
            ));
        }

        let history_path = take_string(env, "SHUTUP_AND_RUN_HISTORY_PATH", file.history_path)
            .unwrap_or_else(|| DEFAULT_HISTORY.to_string());
        let docker_config =
            take_string(env, "SHUTUP_AND_RUN_DOCKER_CONFIG", file.docker_config).map(PathBuf::from);

        let http_plain = take_bool(env, "SHUTUP_AND_RUN_HTTP", file.http)?.unwrap_or(false);
        let auth_failures =
            take_u64(env, "SHUTUP_AND_RUN_AUTH_FAILURES", file.auth_failures)?.unwrap_or(3);
        if !(1..=100).contains(&auth_failures) {
            return Err(ConfigError(
                "auth_failures must be between 1 and 100".into(),
            ));
        }
        let ban_hours = take_u64(env, "SHUTUP_AND_RUN_BAN_HOURS", file.ban_hours)?.unwrap_or(48);
        if !(1..=24 * 365).contains(&ban_hours) {
            return Err(ConfigError("ban_hours must be between 1 and 8760".into()));
        }
        let trust_forwarded = take_bool(
            env,
            "SHUTUP_AND_RUN_TRUST_X_FORWARDED_FOR",
            file.trust_x_forwarded_for,
        )?
        .unwrap_or(false);

        let poll_url = take_string(env, "SHUTUP_AND_RUN_POLL_URL", file.poll_url)
            .filter(|url| !url.is_empty());
        if let Some(url) = &poll_url {
            validate_poll_url(url)?;
        }
        if poll_any_image && poll_url.is_none() {
            return Err(ConfigError(
                "poll_any_image is set but poll_url is not. It only applies to tasks from poll_url.".into(),
            ));
        }
        let worker_name = match take_string(env, "SHUTUP_AND_RUN_WORKER_NAME", file.worker_name) {
            Some(name) => validate_worker_field("worker_name", &name)?,
            None => default_worker_name(),
        };
        let worker_os = match take_string(env, "SHUTUP_AND_RUN_WORKER_OS", file.worker_os) {
            Some(os) => validate_worker_field("worker_os", &os)?,
            // In a unit test the machine is a Linux server on every platform,
            // so a fixture that names no OS does not get the Windows defaults
            // (the shared-pc profile) when the tests run on Windows.
            None if cfg!(test) => "linux".to_string(),
            None => std::env::consts::OS.to_string(),
        };
        let shared_pc = match take_string(
            env,
            "SHUTUP_AND_RUN_RESOURCE_PROFILE",
            file.resource_profile,
        )
        .as_deref()
        {
            Some(PROFILE_SHARED_PC) => true,
            Some(PROFILE_NONE) => false,
            Some(other) => {
                return Err(ConfigError(format!(
                    "resource_profile must be {PROFILE_SHARED_PC} or {PROFILE_NONE}, not {other:?}"
                )))
            }
            // A Windows machine is a client's PC. A Linux one is the owner's
            // server unless its config says otherwise.
            None => worker_os == "windows",
        };
        let shared_memory = || crate::resources::shared_pc_memory(crate::resources::memory_total());
        let max_job_cpus = match take_string(env, "SHUTUP_AND_RUN_MAX_JOB_CPUS", file.max_job_cpus)
        {
            Some(raw) => Some(parse_cpus(&raw)?),
            None => shared_pc.then_some(crate::resources::SHARED_PC_CPUS),
        };
        let max_job_memory =
            match take_string(env, "SHUTUP_AND_RUN_MAX_JOB_MEMORY", file.max_job_memory) {
                Some(raw) => {
                    let bytes = parse_size("max_job_memory", &raw)?;
                    if bytes < MIN_JOB_MEMORY {
                        return Err(ConfigError(
                            "max_job_memory must be at least 6m, the least Docker gives a container"
                                .into(),
                        ));
                    }
                    Some(bytes)
                }
                None => shared_pc.then(shared_memory),
            };
        let min_free_memory =
            match take_string(env, "SHUTUP_AND_RUN_MIN_FREE_MEMORY", file.min_free_memory) {
                Some(raw) => parse_size("min_free_memory", &raw)?,
                None if shared_pc => shared_memory(),
                None => 0,
            };
        let min_free_disk =
            match take_string(env, "SHUTUP_AND_RUN_MIN_FREE_DISK", file.min_free_disk) {
                Some(raw) => parse_size("min_free_disk", &raw)?,
                None if shared_pc => crate::resources::SHARED_PC_MIN_DISK,
                None => 0,
            };
        let certificate_enabled = file.certificate_enabled.unwrap_or(false);
        if certificate_enabled && !file.certificate_consent.unwrap_or(false) {
            return Err(ConfigError(
                "certificate_enabled needs certificate_consent = true: every container this \
                 machine runs would get the certificate and its password. Turn it on with \
                 `shutup-and-run certificate set`, which asks for that consent."
                    .into(),
            ));
        }
        let certificate_name = file
            .certificate_name
            .map(|name| validate_worker_field("certificate_name", &name.replace(' ', "_")))
            .transpose()?;
        let poll_interval_secs = match take_u64(
            env,
            "SHUTUP_AND_RUN_POLL_INTERVAL_SECS",
            file.poll_interval_secs,
        )? {
            Some(value) => {
                if !(1..=3600).contains(&value) {
                    return Err(ConfigError(format!(
                        "poll_interval_secs must be between {MIN_POLL_INTERVAL_SECS} and 3600"
                    )));
                }
                value
            }
            None => DEFAULT_POLL_INTERVAL,
        };
        // Loading never refuses a value that used to be allowed: the worker
        // runs on the minimum and says so. Writing one is refused, in save_json.
        let poll_interval_raised_from =
            (poll_interval_secs < MIN_POLL_INTERVAL_SECS).then_some(poll_interval_secs);
        let poll_interval_secs = poll_interval_secs.max(MIN_POLL_INTERVAL_SECS);
        // The token is not a TOML field. deny_unknown_fields rejects `poll_token` in the file.
        // When poll_url is unset the variable is ignored, so a stray value does not change startup.
        let poll_token = if poll_url.is_some() {
            match env
                .get("SHUTUP_AND_RUN_POLL_TOKEN")
                .map(String::as_str)
                .filter(|value| !value.is_empty())
            {
                Some(raw) => {
                    if raw.chars().any(|c| c.is_whitespace()) || raw.len() < 16 {
                        return Err(ConfigError(
                            "SHUTUP_AND_RUN_POLL_TOKEN must be at least 16 characters and contain no whitespace".into(),
                        ));
                    }
                    Some(PollToken::new(raw))
                }
                None => None,
            }
        } else {
            None
        };

        let history_dir = Path::new(&history_path)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        Ok(Settings {
            listen,
            max_slots,
            allowlist,
            default_timeout_secs,
            max_timeout_secs,
            reaper_interval: Duration::from_secs(reaper),
            docker_config,
            history_path: PathBuf::from(&history_path),
            history_days,
            purge_interval: Duration::from_secs(purge),
            log_lines: log_lines as usize,
            prune_interval: Duration::from_secs(prune),
            http_plain,
            tls_cert: take_string(env, "SHUTUP_AND_RUN_TLS_CERT", file.tls_cert).map(PathBuf::from),
            tls_key: take_string(env, "SHUTUP_AND_RUN_TLS_KEY", file.tls_key).map(PathBuf::from),
            auth_failures: auth_failures as u32,
            ban_secs: ban_hours.saturating_mul(3600),
            trust_forwarded,
            status_page: take_bool(env, "SHUTUP_AND_RUN_STATUS_PAGE", file.status_page)?
                .unwrap_or(false),
            token: TokenChecker::new(&token),
            poll_url,
            poll_interval: Duration::from_secs(poll_interval_secs),
            poll_interval_raised_from,
            poll_token,
            poll_any_image,
            worker_name,
            worker_os,
            shared_pc,
            max_job_cpus,
            max_job_memory,
            min_free_memory,
            min_free_disk,
            certificate_enabled,
            certificate_name,
            certificate_dir: PathBuf::from(CERTIFICATE_DIR),
            secrets_dir: default_secrets_dir(&history_dir),
            config_json: None,
            admin_user: DEFAULT_ADMIN.to_string(),
            admin_password: DEFAULT_ADMIN.to_string(),
            language: "en".to_string(),
            config_warning: None,
        })
    }
}

// ---------------------------------------------------------------------------
// config.json: one local file that the CLI and the local page read and write.

pub type JsonMap = serde_json::Map<String, serde_json::Value>;

pub const JSON_DEFAULT: &str = "/etc/shutup-and-run/config.json";
const TOML_DEFAULT: &str = "/etc/shutup-and-run/config.toml";
/// The login of the local page until `config set admin_password` changes it.
pub const DEFAULT_ADMIN: &str = "admin";
pub const MASK: &str = "***";
/// The imported certificate lives beside the config, never in it.
pub const CERTIFICATE_DIR: &str = "/etc/shutup-and-run/certificate";

/// `/run` is memory on a systemd machine and in WSL, so a job's copy of the
/// password does not reach the disk. Elsewhere it goes beside the history.
fn default_secrets_dir(history_dir: &Path) -> PathBuf {
    if Path::new("/run").is_dir() {
        PathBuf::from("/run/shutup-and-run/jobs")
    } else {
        history_dir.join("job-secrets")
    }
}

pub const PROFILE_SHARED_PC: &str = "shared-pc";
pub const PROFILE_NONE: &str = "none";
/// Docker refuses a container with less.
const MIN_JOB_MEMORY: u64 = 6 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Int,
    Bool,
    List,
}

/// Where a key can be edited besides the CLI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnPage {
    /// On the local page, by whoever is at the machine.
    Open,
    /// On the local page, behind the admin login.
    Locked,
    /// CLI only.
    No,
}

pub struct Key {
    pub name: &'static str,
    pub kind: Kind,
    pub secret: bool,
    pub page: OnPage,
}

const fn key(name: &'static str, kind: Kind, page: OnPage) -> Key {
    Key {
        name,
        kind,
        secret: false,
        page,
    }
}

const fn secret(name: &'static str) -> Key {
    Key {
        name,
        kind: Kind::Text,
        secret: true,
        page: OnPage::Locked,
    }
}

/// Every key of config.json. `docs/schemas/worker-config.schema.json` lists
/// the same names; a test keeps the two equal.
pub const KEYS: &[Key] = &[
    key("poll_url", Kind::Text, OnPage::Open),
    key("poll_interval_secs", Kind::Int, OnPage::Open),
    key("max_slots", Kind::Int, OnPage::Open),
    key("worker_name", Kind::Text, OnPage::Open),
    key("allowlist", Kind::List, OnPage::Locked),
    key("poll_any_image", Kind::Bool, OnPage::Locked),
    key("default_timeout_secs", Kind::Int, OnPage::Locked),
    key("max_timeout_secs", Kind::Int, OnPage::Locked),
    secret("token"),
    secret("poll_token"),
    key("admin_user", Kind::Text, OnPage::Locked),
    secret("admin_password"),
    key("language", Kind::Text, OnPage::No),
    key("listen", Kind::Text, OnPage::No),
    key("http", Kind::Bool, OnPage::No),
    key("tls_cert", Kind::Text, OnPage::No),
    key("tls_key", Kind::Text, OnPage::No),
    key("status_page", Kind::Bool, OnPage::No),
    key("worker_os", Kind::Text, OnPage::No),
    key("resource_profile", Kind::Text, OnPage::Locked),
    key("max_job_cpus", Kind::Text, OnPage::Locked),
    key("max_job_memory", Kind::Text, OnPage::Locked),
    key("min_free_memory", Kind::Text, OnPage::Locked),
    key("min_free_disk", Kind::Text, OnPage::Locked),
    key("certificate_enabled", Kind::Bool, OnPage::No),
    key("certificate_consent", Kind::Bool, OnPage::No),
    key("certificate_name", Kind::Text, OnPage::No),
    key("history_path", Kind::Text, OnPage::No),
    key("history_days", Kind::Int, OnPage::No),
    key("docker_config", Kind::Text, OnPage::No),
    key("log_lines", Kind::Int, OnPage::No),
    key("reaper_interval_secs", Kind::Int, OnPage::No),
    key("purge_interval_secs", Kind::Int, OnPage::No),
    key("prune_interval_secs", Kind::Int, OnPage::No),
    key("auth_failures", Kind::Int, OnPage::No),
    key("ban_hours", Kind::Int, OnPage::No),
    key("trust_x_forwarded_for", Kind::Bool, OnPage::No),
];

pub fn find_key(name: &str) -> Result<&'static Key, ConfigError> {
    KEYS.iter().find(|k| k.name == name).ok_or_else(|| {
        let names: Vec<&str> = KEYS.iter().map(|k| k.name).collect();
        ConfigError(format!(
            "{name} is not a config key. The keys are: {}",
            names.join(", ")
        ))
    })
}

/// Text from a command line or a form field, as the JSON value of that key.
pub fn parse_value(key: &Key, raw: &str) -> Result<serde_json::Value, ConfigError> {
    use serde_json::Value;
    let raw = raw.trim();
    match key.kind {
        Kind::Text => Ok(Value::String(raw.to_string())),
        Kind::Int => raw
            .parse::<u64>()
            .map(Value::from)
            .map_err(|_| ConfigError(format!("{} must be a whole number, not {raw:?}", key.name))),
        Kind::Bool => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Ok(Value::Bool(true)),
            "false" | "0" | "no" | "off" => Ok(Value::Bool(false)),
            _ => Err(ConfigError(format!(
                "{} must be true or false, not {raw:?}",
                key.name
            ))),
        },
        Kind::List => {
            if raw.starts_with('[') {
                let list: Vec<String> = serde_json::from_str(raw).map_err(|_| {
                    ConfigError(format!("{} must be a JSON list of strings", key.name))
                })?;
                return Ok(Value::from(list));
            }
            Ok(Value::from(
                raw.split([',', '\n'])
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<String>>(),
            ))
        }
    }
}

/// The map with every secret that is set replaced by `***`, and the values
/// of the poll URL's query too.
pub fn masked(map: &JsonMap) -> JsonMap {
    let mut out = map.clone();
    for key in KEYS.iter().filter(|k| k.secret) {
        if let Some(value) = out.get_mut(key.name) {
            *value = serde_json::Value::String(MASK.to_string());
        }
    }
    if let Some(url) = out.get("poll_url").and_then(|v| v.as_str()) {
        out.insert("poll_url".into(), mask_query(url).into());
    }
    out
}

/// The URL with every value of its query replaced by `***`. The query of a
/// poll URL pairs the machine with a person (a tax id, an e-mail, a key), so
/// it is not printed: `https://host/poll?cpf=***&email=***`.
pub fn mask_query(url: &str) -> String {
    let Some((base, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let (query, fragment) = match query.split_once('#') {
        Some((query, fragment)) => (query, Some(fragment)),
        None => (query, None),
    };
    let pairs: Vec<String> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((name, _)) => format!("{name}={MASK}"),
            None => MASK.to_string(),
        })
        .collect();
    let mut out = format!("{base}?{}", pairs.join("&"));
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

/// The values of a URL's query, raw and percent-decoded: what `scrub` takes
/// out of any text that quoted the URL.
pub fn query_values(url: &str) -> Vec<String> {
    let mut values = Vec::new();
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    let query = query.split('#').next().unwrap_or("");
    for pair in query.split('&') {
        let value = pair.split_once('=').map(|(_, v)| v).unwrap_or(pair);
        if value.is_empty() {
            continue;
        }
        values.push(value.to_string());
        if let Ok(parsed) = reqwest::Url::parse(&format!("http://x/?v={value}")) {
            if let Some((_, decoded)) = parsed.query_pairs().next() {
                values.push(decoded.into_owned());
            }
        }
    }
    values
}

/// The secrets a report must not carry, longest first so one is not left
/// half masked inside another.
pub fn secret_values(map: &JsonMap) -> Vec<String> {
    let mut values: Vec<String> = KEYS
        .iter()
        .filter(|k| k.secret)
        .filter_map(|k| map.get(k.name).and_then(|v| v.as_str()))
        .map(str::to_string)
        .chain(
            map.get("poll_url")
                .and_then(|v| v.as_str())
                .map(query_values)
                .unwrap_or_default(),
        )
        .filter(|v| v.len() >= 4)
        .collect();
    values.sort_by_key(|v| std::cmp::Reverse(v.len()));
    values
}

/// What a machine with no config runs: one slot, nothing to poll, the local
/// page on, and a token of its own.
pub fn default_map() -> JsonMap {
    let value = serde_json::json!({
        "listen": DEFAULT_LISTEN,
        "http": true,
        "status_page": true,
        "max_slots": 1,
        "allowlist": ["hello-world"],
        "token": crate::launcher::random_id(),
    });
    match value {
        serde_json::Value::Object(map) => map,
        _ => JsonMap::new(),
    }
}

/// A factory reset: the defaults, keeping what says which machine this is
/// and where it listens, when the old file can still be read.
pub fn reset_map(old: Option<&JsonMap>) -> JsonMap {
    let mut map = default_map();
    if let Some(old) = old {
        for name in [
            "token",
            "poll_token",
            "worker_name",
            "worker_os",
            "language",
            "listen",
            "http",
            "tls_cert",
            "tls_key",
            "status_page",
            "history_path",
        ] {
            if let Some(value) = old.get(name) {
                map.insert(name.to_string(), value.clone());
            }
        }
    }
    if Settings::from_json_map(&map).is_err() {
        return default_map();
    }
    map
}

/// The JSON file the service reads, or None on a machine set up with
/// config.toml and environment. `SHUTUP_AND_RUN_CONFIG` names either kind;
/// without it, config.json wins when it exists.
pub fn json_path(env: &HashMap<String, String>) -> Option<PathBuf> {
    match env.get("SHUTUP_AND_RUN_CONFIG") {
        Some(path) => path.ends_with(".json").then(|| PathBuf::from(path)),
        None => Path::new(JSON_DEFAULT)
            .exists()
            .then(|| PathBuf::from(JSON_DEFAULT)),
    }
}

/// The JSON file the `config` commands work on. A machine with no config
/// file at all gets config.json; one with a config.toml is left alone.
pub fn json_path_for_cli() -> Result<PathBuf, ConfigError> {
    let env = process_env();
    if let Some(path) = json_path(&env) {
        return Ok(path);
    }
    if env.contains_key("SHUTUP_AND_RUN_CONFIG") || Path::new(TOML_DEFAULT).exists() {
        return Err(ConfigError(
            "this machine is set up with config.toml and environment variables. \
             The config commands and the page editor work on config.json; see docs/FOR-AGENTS.md."
                .into(),
        ));
    }
    Ok(PathBuf::from(JSON_DEFAULT))
}

/// `certificate/` beside the config file.
pub fn certificate_dir_for(config: &Path) -> PathBuf {
    config
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .join("certificate")
}

pub fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

pub fn read_json(path: &Path) -> Result<JsonMap, ConfigError> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| ConfigError(format!("could not read {}: {err}", path.display())))?;
    parse_json(&text)
}

pub fn parse_json(text: &str) -> Result<JsonMap, ConfigError> {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(map)) => Ok(map),
        Ok(_) => Err(ConfigError("config.json must be a JSON object".into())),
        Err(err) => Err(ConfigError(format!("config.json is not JSON: {err}"))),
    }
}

pub fn to_text(map: &JsonMap) -> String {
    let mut text = serde_json::to_string_pretty(map).unwrap_or_else(|_| "{}".to_string());
    text.push('\n');
    text
}

/// Validate, then replace the file and its backup copy. Nothing invalid is
/// ever written, so the file on disk always starts the service.
pub fn save_json(path: &Path, map: &JsonMap) -> Result<(), ConfigError> {
    let map = &with_poll_minimum(map, read_json(path).ok().as_ref())?;
    Settings::from_json_map(map)?;
    let text = to_text(map);
    for target in [path.to_path_buf(), backup_path(path)] {
        write_private(&target, text.as_bytes())
            .map_err(|err| ConfigError(format!("could not write {}: {err}", target.display())))?;
    }
    Ok(())
}

/// The poll interval rule for a write. A value under the minimum is refused.
/// The one exception is the value the file already had, from a release that
/// allowed it: that is raised to the minimum in this write, so that an old
/// install is not locked out of changing its other keys.
pub fn with_poll_minimum(map: &JsonMap, on_disk: Option<&JsonMap>) -> Result<JsonMap, ConfigError> {
    const KEY: &str = "poll_interval_secs";
    let mut map = map.clone();
    let Some(value) = map.get(KEY).and_then(|v| v.as_u64()) else {
        return Ok(map);
    };
    if (1..MIN_POLL_INTERVAL_SECS).contains(&value) {
        if on_disk
            .and_then(|old| old.get(KEY))
            .and_then(|v| v.as_u64())
            != Some(value)
        {
            return Err(ConfigError(format!(
                "poll_interval_secs must be at least {MIN_POLL_INTERVAL_SECS} (and at most 3600), not {value}: \
                 a poll URL serves many machines"
            )));
        }
        map.insert(
            KEY.to_string(),
            serde_json::Value::from(MIN_POLL_INTERVAL_SECS),
        );
    }
    Ok(map)
}

/// Mode 600, written beside the target and renamed over it.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

/// `1`, `0.5`, `2`: CPUs, as `docker run --cpus` counts them.
fn parse_cpus(raw: &str) -> Result<f64, ConfigError> {
    match raw.trim().parse::<f64>() {
        Ok(cpus) if cpus.is_finite() && (0.01..=128.0).contains(&cpus) => Ok(cpus),
        _ => Err(ConfigError(format!(
            "max_job_cpus must be a number from 0.01 to 128, not {raw:?}"
        ))),
    }
}

/// `2g`, `512m`, or `0`.
fn parse_size(key: &str, raw: &str) -> Result<u64, ConfigError> {
    if raw.trim() == "0" {
        return Ok(0);
    }
    crate::bytesize::parse_qty(raw)
        .map(|bytes| bytes as u64)
        .map_err(|err| ConfigError(format!("{key} must be a size like 2g or 512m: {err}")))
}

fn validate_admin(key: &str, value: &str) -> Result<String, ConfigError> {
    if value.is_empty() || value.len() > 64 || value.chars().any(|c| c.is_control() || c == ':') {
        return Err(ConfigError(format!(
            "{key} must be 1 to 64 characters with no colon"
        )));
    }
    Ok(value.to_string())
}

fn process_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    for key in ENV_KEYS {
        if let Ok(value) = std::env::var(key) {
            env.insert((*key).to_string(), value);
        }
    }
    env
}

fn validate_poll_url(url: &str) -> Result<(), ConfigError> {
    if url.len() > 2048 || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(ConfigError("poll_url is invalid".into()));
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| ConfigError("poll_url is invalid".into()))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(ConfigError("poll_url must be http or https".into()));
    }
    if parsed.host_str().is_none() {
        return Err(ConfigError("poll_url is invalid".into()));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ConfigError(
            "poll_url must not embed credentials. Set SHUTUP_AND_RUN_POLL_TOKEN.".into(),
        ));
    }
    Ok(())
}

fn read_config_file(env: &HashMap<String, String>) -> Result<String, ConfigError> {
    if let Some(path) = env.get("SHUTUP_AND_RUN_CONFIG") {
        return std::fs::read_to_string(path)
            .map_err(|err| ConfigError(format!("could not read {path}: {err}")));
    }
    let default = Path::new("/etc/shutup-and-run/config.toml");
    if default.exists() {
        return std::fs::read_to_string(default)
            .map_err(|err| ConfigError(format!("could not read {}: {err}", default.display())));
    }
    Ok(String::new())
}

fn take_string(env: &HashMap<String, String>, key: &str, file: Option<String>) -> Option<String> {
    env.get(key)
        .filter(|value| !value.is_empty())
        .cloned()
        .or(file)
}

fn take_bool(
    env: &HashMap<String, String>,
    key: &str,
    file: Option<bool>,
) -> Result<Option<bool>, ConfigError> {
    if let Some(raw) = env.get(key) {
        return match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" => Ok(Some(true)),
            "0" | "false" | "no" => Ok(Some(false)),
            _ => Err(ConfigError(format!("{key} must be true or false"))),
        };
    }
    Ok(file)
}

fn take_u64(
    env: &HashMap<String, String>,
    key: &str,
    file: Option<u64>,
) -> Result<Option<u64>, ConfigError> {
    if let Some(raw) = env.get(key) {
        if raw.is_empty() {
            return Err(ConfigError(format!("{key} is empty")));
        }
        let value = raw
            .parse::<u64>()
            .map_err(|_| ConfigError(format!("{key} is not an integer")))?;
        return Ok(Some(value));
    }
    Ok(file)
}

fn split_allowlist(raw: &str, may_be_empty: bool) -> Result<Vec<String>, ConfigError> {
    let items: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if items.is_empty() && !may_be_empty {
        return Err(ConfigError("SHUTUP_AND_RUN_ALLOWLIST is empty".into()));
    }
    Ok(items)
}

/// `worker_name` and `worker_os` travel in the poll body. Printable ASCII, no
/// spaces, at most 64 characters, so a server can log and compare them as is.
fn validate_worker_field(key: &str, value: &str) -> Result<String, ConfigError> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 64
        || !value
            .chars()
            .all(|c| c.is_ascii_graphic() && c != '"' && c != '\\')
    {
        return Err(ConfigError(format!(
            "{key} must be 1 to 64 printable ASCII characters with no spaces or quotes"
        )));
    }
    Ok(value.to_string())
}

/// The machine's hostname, read without a crate. Inside WSL this is the
/// Windows computer name. `unknown` when nothing answers.
fn default_worker_name() -> String {
    let from_env = ["HOSTNAME", "COMPUTERNAME"]
        .iter()
        .find_map(|key| std::env::var(key).ok())
        .filter(|value| !value.trim().is_empty());
    let raw = from_env
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .unwrap_or_default();
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| c.is_ascii_graphic() && *c != '"' && *c != '\\')
        .take(64)
        .collect();
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn history_path_does_not_need_the_token() {
        // What the installer writes: the token is only in the systemd env file.
        let toml_text = r#"
            listen = "0.0.0.0:8787"
            max_slots = 2
            allowlist = ["alpine"]
            history_path = "/var/lib/elsewhere/jobs.sqlite"
        "#;
        assert!(Settings::from_toml_and_env(toml_text, &HashMap::new()).is_err());
        assert_eq!(
            Settings::history_path_from(toml_text, &HashMap::new()).unwrap(),
            PathBuf::from("/var/lib/elsewhere/jobs.sqlite")
        );
        assert_eq!(
            Settings::history_path_from("", &HashMap::new()).unwrap(),
            PathBuf::from(DEFAULT_HISTORY)
        );
        assert_eq!(
            Settings::history_path_from(
                toml_text,
                &env(&[("SHUTUP_AND_RUN_HISTORY_PATH", "/tmp/x.sqlite")])
            )
            .unwrap(),
            PathBuf::from("/tmp/x.sqlite")
        );
    }

    #[test]
    fn env_overrides_file_and_rejects_unknown_fields() {
        let toml_text = r#"
            listen = "127.0.0.1:9"
            token = "token-from-the-file"
            max_slots = 2
            allowlist = ["alpine"]
            default_timeout_secs = 30
            max_timeout_secs = 60
        "#;
        let settings = Settings::from_toml_and_env(
            toml_text,
            &env(&[
                ("SHUTUP_AND_RUN_MAX_SLOTS", "4"),
                ("SHUTUP_AND_RUN_TOKEN", "token-from-the-env"),
                (
                    "SHUTUP_AND_RUN_ALLOWLIST",
                    "alpine, ghcr.io/owner/selenium-worker",
                ),
                ("SHUTUP_AND_RUN_HISTORY_DAYS", "0"),
                ("SHUTUP_AND_RUN_LOG_LINES", "50"),
            ]),
        )
        .unwrap();
        assert_eq!(settings.max_slots, 4);
        assert!(settings.token.matches("token-from-the-env"));
        assert!(!settings.token.matches("token-from-the-file"));
        assert_eq!(settings.allowlist.len(), 2);
        assert_eq!(settings.history_days, 0);
        assert_eq!(settings.log_lines, 50);
        assert_eq!(settings.prune_interval, Duration::ZERO);
        assert_eq!(
            settings.listen,
            "127.0.0.1:9".parse::<SocketAddr>().unwrap()
        );

        let bad = r#"
            listen = "127.0.0.1:9"
            token = "token-from-the-file"
            max_slots = 1
            allowlist = ["alpine"]
            typo = 1
        "#;
        assert!(Settings::from_toml_and_env(bad, &HashMap::new()).is_err());
    }

    #[test]
    fn requires_token_slots_and_allowlist() {
        assert!(Settings::from_toml_and_env("", &HashMap::new()).is_err());
        let short = r#"
            token = "short"
            max_slots = 1
            allowlist = ["alpine"]
        "#;
        assert!(Settings::from_toml_and_env(short, &HashMap::new()).is_err());
    }

    fn base_toml(extra: &str) -> String {
        format!(
            r#"
            token = "token-from-the-file"
            max_slots = 1
            allowlist = ["alpine"]
            {extra}
            "#
        )
    }

    #[test]
    fn poll_is_off_unless_configured_and_token_is_env_only() {
        let settings = Settings::from_toml_and_env(&base_toml(""), &HashMap::new()).unwrap();
        assert!(settings.poll_url.is_none());
        assert!(settings.poll_token.is_none());
        assert_eq!(settings.poll_interval, Duration::from_secs(30));
        assert_eq!(settings.poll_interval_raised_from, None);

        let settings = Settings::from_toml_and_env(
            &base_toml(""),
            &env(&[("SHUTUP_AND_RUN_POLL_TOKEN", "short")]),
        )
        .unwrap();
        assert!(settings.poll_url.is_none());
        assert!(settings.poll_token.is_none());

        let settings = Settings::from_toml_and_env(
            &base_toml("poll_url = \"https://jobs.example/poll\"\npoll_interval_secs = 45"),
            &env(&[
                ("SHUTUP_AND_RUN_POLL_URL", "http://127.0.0.1:9/poll"),
                ("SHUTUP_AND_RUN_POLL_TOKEN", "poll-token-value-ok"),
            ]),
        )
        .unwrap();
        assert_eq!(
            settings.poll_url.as_deref(),
            Some("http://127.0.0.1:9/poll")
        );
        assert_eq!(settings.poll_interval, Duration::from_secs(45));
        assert_eq!(
            settings.poll_token.as_ref().map(PollToken::as_str),
            Some("poll-token-value-ok")
        );
        assert_eq!(format!("{:?}", settings.poll_token), "Some(PollToken(***))");

        let in_file = base_toml("poll_token = \"poll-token-value-ok\"");
        assert!(Settings::from_toml_and_env(&in_file, &HashMap::new()).is_err());
        assert!(Settings::from_toml_and_env(
            &base_toml("poll_url = \"http://127.0.0.1:9/poll\""),
            &env(&[("SHUTUP_AND_RUN_POLL_TOKEN", "short")]),
        )
        .is_err());
        assert!(Settings::from_toml_and_env(
            &base_toml("poll_url = \"http://user:pass@127.0.0.1:9/poll\""),
            &HashMap::new(),
        )
        .is_err());
        assert!(
            Settings::from_toml_and_env(&base_toml("poll_interval_secs = 0"), &HashMap::new(),)
                .is_err()
        );
    }

    #[test]
    fn poll_any_image_needs_poll_url_and_lets_the_allowlist_be_empty() {
        let settings = Settings::from_toml_and_env(&base_toml(""), &HashMap::new()).unwrap();
        assert!(!settings.poll_any_image);

        let err = Settings::from_toml_and_env(&base_toml("poll_any_image = true"), &HashMap::new())
            .unwrap_err();
        assert!(err.to_string().contains("poll_url"), "{err}");

        let no_list = r#"
            token = "token-from-the-file"
            max_slots = 1
            allowlist = []
            poll_url = "http://127.0.0.1:9/poll"
            poll_any_image = true
            "#;
        let settings = Settings::from_toml_and_env(no_list, &HashMap::new()).unwrap();
        assert!(settings.poll_any_image);
        assert!(settings.allowlist.is_empty());

        // Without the flag an empty list is still refused, from the file and from the environment.
        let still_required = r#"
            token = "token-from-the-file"
            max_slots = 1
            allowlist = []
            poll_url = "http://127.0.0.1:9/poll"
            "#;
        assert!(Settings::from_toml_and_env(still_required, &HashMap::new()).is_err());
        assert!(Settings::from_toml_and_env(
            &base_toml(""),
            &env(&[("SHUTUP_AND_RUN_ALLOWLIST", " , ")]),
        )
        .is_err());
        let settings = Settings::from_toml_and_env(
            &base_toml("poll_url = \"http://127.0.0.1:9/poll\""),
            &env(&[
                ("SHUTUP_AND_RUN_ALLOWLIST", " , "),
                ("SHUTUP_AND_RUN_POLL_ANY_IMAGE", "1"),
            ]),
        )
        .unwrap();
        assert!(settings.allowlist.is_empty());
        assert!(settings.poll_any_image);
    }

    #[test]
    fn the_shared_pc_profile_is_the_default_on_windows_and_each_figure_can_be_set() {
        use crate::resources::{memory_total, shared_pc_memory, GIB};
        let load = |extra: &str| Settings::from_toml_and_env(&base_toml(extra), &env(&[]));
        // A Linux machine without the key is as it was before the profile existed.
        let plain = load("worker_os = \"linux\"").unwrap();
        assert!(!plain.shared_pc);
        assert_eq!((plain.max_job_cpus, plain.max_job_memory), (None, None));
        assert_eq!((plain.min_free_memory, plain.min_free_disk), (0, 0));

        // A Windows machine is a shared PC unless its config says otherwise.
        let memory = shared_pc_memory(memory_total());
        for extra in [
            "worker_os = \"windows\"",
            "resource_profile = \"shared-pc\"",
        ] {
            let shared = load(extra).unwrap();
            assert!(shared.shared_pc, "{extra}");
            assert_eq!(shared.max_job_cpus, Some(1.0));
            assert_eq!(shared.max_job_memory, Some(memory));
            assert_eq!(shared.min_free_memory, memory);
            assert_eq!(shared.min_free_disk, 5 * GIB);
        }
        let opted_out = load("worker_os = \"windows\"\nresource_profile = \"none\"").unwrap();
        assert!(!opted_out.shared_pc);
        assert_eq!(opted_out.max_job_memory, None);

        // Each figure on its own, under either profile; 0 turns a check off.
        let set = load(
            "resource_profile = \"shared-pc\"\nmax_job_cpus = \"0.5\"\nmax_job_memory = \"512m\"\nmin_free_memory = \"0\"\nmin_free_disk = \"1g\"",
        )
        .unwrap();
        assert_eq!(set.max_job_cpus, Some(0.5));
        assert_eq!(set.max_job_memory, Some(512 * 1024 * 1024));
        assert_eq!((set.min_free_memory, set.min_free_disk), (0, GIB));
        let only_one = load("worker_os = \"linux\"\nmax_job_memory = \"4g\"").unwrap();
        assert_eq!(only_one.max_job_memory, Some(4 * GIB));
        assert_eq!(only_one.max_job_cpus, None);
        let from_env = Settings::from_toml_and_env(
            &base_toml(""),
            &env(&[("SHUTUP_AND_RUN_RESOURCE_PROFILE", "shared-pc")]),
        )
        .unwrap();
        assert!(from_env.shared_pc);

        for (bad, why) in [
            ("resource_profile = \"gaming\"", "resource_profile must be"),
            ("max_job_cpus = \"0\"", "max_job_cpus must be"),
            ("max_job_cpus = \"200\"", "max_job_cpus must be"),
            ("max_job_cpus = \"two\"", "max_job_cpus must be"),
            ("max_job_memory = \"0\"", "at least 6m"),
            ("max_job_memory = \"1m\"", "at least 6m"),
            ("max_job_memory = \"lots\"", "max_job_memory must be a size"),
            (
                "min_free_memory = \"-1g\"",
                "min_free_memory must be a size",
            ),
            ("min_free_disk = \"five\"", "min_free_disk must be a size"),
        ] {
            let refused = load(bad).unwrap_err().to_string();
            assert!(refused.contains(why), "{bad}: {refused}");
        }
    }

    /// A config.json written before these keys existed still loads, and on
    /// Windows it gets the profile without being rewritten.
    #[test]
    fn a_config_json_from_before_the_profile_still_loads() {
        let mut old = default_map();
        old.insert("worker_os".into(), "windows".into());
        old.insert("max_slots".into(), 2.into());
        let settings = Settings::from_json_map(&old).unwrap();
        assert!(settings.shared_pc);
        assert_eq!(settings.max_slots, 2);
        assert_eq!(settings.max_job_cpus, Some(1.0));
        let mut linux = default_map();
        linux.insert("worker_os".into(), "linux".into());
        assert!(!Settings::from_json_map(&linux).unwrap().shared_pc);
        // A factory reset drops the figures and keeps the machine's identity.
        old.insert("max_job_memory".into(), "8g".into());
        let reset = reset_map(Some(&old));
        assert!(!reset.contains_key("max_job_memory"));
        assert_eq!(reset["worker_os"], "windows");
        assert!(Settings::from_json_map(&reset).unwrap().shared_pc);
    }

    #[test]
    fn the_poll_interval_has_one_minimum_old_configs_still_load_and_new_ones_are_refused() {
        assert_eq!(MIN_POLL_INTERVAL_SECS, 30);
        assert_eq!(DEFAULT_POLL_INTERVAL, MIN_POLL_INTERVAL_SECS);
        // An installed config from a release that allowed 2 s: the worker
        // starts, runs on the minimum, and remembers what was asked.
        for (text, env_pairs) in [
            ("poll_interval_secs = 2", vec![]),
            ("", vec![("SHUTUP_AND_RUN_POLL_INTERVAL_SECS", "2")]),
        ] {
            let old = Settings::from_toml_and_env(&base_toml(text), &env(&env_pairs)).unwrap();
            assert_eq!(old.poll_interval, Duration::from_secs(30));
            assert_eq!(old.poll_interval_raised_from, Some(2));
        }
        let mut map = default_map();
        map.insert("poll_interval_secs".into(), 2.into());
        let old = Settings::from_json_map(&map).unwrap();
        assert_eq!(
            (old.poll_interval.as_secs(), old.poll_interval_raised_from),
            (30, Some(2))
        );
        // At and above the minimum nothing is raised. Zero and too long stay invalid.
        for ok in [30u64, 31, 3600] {
            map.insert("poll_interval_secs".into(), ok.into());
            let s = Settings::from_json_map(&map).unwrap();
            assert_eq!(
                (s.poll_interval.as_secs(), s.poll_interval_raised_from),
                (ok, None)
            );
        }
        for bad in [0u64, 3601] {
            map.insert("poll_interval_secs".into(), bad.into());
            assert!(Settings::from_json_map(&map)
                .unwrap_err()
                .to_string()
                .contains("between 30 and 3600"));
        }

        // Writing: a new value under the minimum is refused and the file stays.
        let dir = std::env::temp_dir().join(format!("sar-poll-min-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.json");
        let mut fresh = default_map();
        save_json(&path, &fresh).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        for low in [1u64, 2, 29] {
            fresh.insert("poll_interval_secs".into(), low.into());
            let refused = save_json(&path, &fresh).unwrap_err().to_string();
            assert!(refused.contains("at least 30") && refused.contains(&low.to_string()));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        }
        fresh.insert("poll_interval_secs".into(), 30.into());
        save_json(&path, &fresh).unwrap();

        // An old file that already holds 2 is not locked: the next write of
        // any key carries the interval up to the minimum, once.
        let mut old = default_map();
        old.insert("poll_interval_secs".into(), 2.into());
        std::fs::write(&path, to_text(&old)).unwrap();
        assert_eq!(
            Settings::load_json(&path).poll_interval_raised_from,
            Some(2)
        );
        assert!(Settings::load_json(&path).config_warning.is_none());
        old.insert("max_slots".into(), 2.into());
        save_json(&path, &old).unwrap();
        let saved = read_json(&path).unwrap();
        assert_eq!(
            (
                saved["poll_interval_secs"].as_u64(),
                saved["max_slots"].as_u64()
            ),
            (Some(30), Some(2))
        );
        // But not to another value under it.
        old.insert("poll_interval_secs".into(), 5.into());
        assert!(save_json(&path, &old).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn worker_name_and_os_default_and_validate() {
        let settings = Settings::from_toml_and_env(&base_toml(""), &HashMap::new()).unwrap();
        assert!(!settings.worker_name.is_empty());
        assert!(!settings.worker_name.contains(' '));
        assert_eq!(settings.worker_os, "linux");

        let settings = Settings::from_toml_and_env(
            &base_toml("worker_name = \"desk-07\"\nworker_os = \"windows\""),
            &env(&[("SHUTUP_AND_RUN_WORKER_NAME", "desk-08")]),
        )
        .unwrap();
        assert_eq!(settings.worker_name, "desk-08");
        assert_eq!(settings.worker_os, "windows");

        for bad in ["", "has space", "a\"quote", &"x".repeat(65)] {
            let toml_text = base_toml(&format!("worker_name = {}", serde_json::json!(bad)));
            assert!(
                Settings::from_toml_and_env(&toml_text, &HashMap::new()).is_err(),
                "{bad:?} was accepted"
            );
        }
        assert_eq!(validate_worker_field("worker_os", " wsl ").unwrap(), "wsl");
    }
}
