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
];

const DEFAULT_LISTEN: &str = "127.0.0.1:8787";
const DEFAULT_TIMEOUT: u64 = 3600;
const DEFAULT_MAX_TIMEOUT: u64 = 21_600;
const DEFAULT_REAPER: u64 = 2;
const DEFAULT_HISTORY: &str = "/var/lib/shutup-and-run/jobs.sqlite";
const DEFAULT_HISTORY_DAYS: u64 = 30;
const DEFAULT_PURGE: u64 = 3600;
const DEFAULT_LOG_LINES: u64 = 200;
const DEFAULT_POLL_INTERVAL: u64 = 15;

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
    pub token: TokenChecker,
    /// Unset means push only. The process does not call out.
    pub poll_url: Option<String>,
    pub poll_interval: Duration,
    /// Bearer token for `poll_url`. Environment only, so it cannot be committed in TOML.
    pub poll_token: Option<PollToken>,
    /// A task from `poll_url` skips the allowlist. Push jobs never do. Needs `poll_url`.
    pub poll_any_image: bool,
    /// Sent in the poll body so one URL can serve many machines. Default: the hostname.
    pub worker_name: String,
    /// Sent in the poll body. Default: the OS this binary was built for. The
    /// Windows supervisor sets `windows` for the binary it runs inside WSL.
    pub worker_os: String,
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
    poll_url: Option<String>,
    poll_interval_secs: Option<u64>,
    poll_any_image: Option<bool>,
    worker_name: Option<String>,
    worker_os: Option<String>,
}

impl Settings {
    pub fn load() -> Result<Self, ConfigError> {
        let env = process_env();
        let toml_text = read_config_file(&env)?;
        Self::from_toml_and_env(&toml_text, &env)
    }

    /// Only the SQLite path, for `shutup-and-run bans`. That command runs from
    /// an SSH shell, where the token in the systemd EnvironmentFile is not set,
    /// so it must not need the rest of the settings.
    pub fn history_path() -> Result<PathBuf, ConfigError> {
        let env = process_env();
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
            None => std::env::consts::OS.to_string(),
        };
        let poll_interval_secs = match take_u64(
            env,
            "SHUTUP_AND_RUN_POLL_INTERVAL_SECS",
            file.poll_interval_secs,
        )? {
            Some(value) => {
                if !(1..=3600).contains(&value) {
                    return Err(ConfigError(
                        "poll_interval_secs must be between 1 and 3600".into(),
                    ));
                }
                value
            }
            None => DEFAULT_POLL_INTERVAL,
        };
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

        Ok(Settings {
            listen,
            max_slots,
            allowlist,
            default_timeout_secs,
            max_timeout_secs,
            reaper_interval: Duration::from_secs(reaper),
            docker_config,
            history_path: PathBuf::from(history_path),
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
            token: TokenChecker::new(&token),
            poll_url,
            poll_interval: Duration::from_secs(poll_interval_secs),
            poll_token,
            poll_any_image,
            worker_name,
            worker_os,
        })
    }
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
        assert_eq!(settings.poll_interval, Duration::from_secs(15));

        let settings = Settings::from_toml_and_env(
            &base_toml(""),
            &env(&[("SHUTUP_AND_RUN_POLL_TOKEN", "short")]),
        )
        .unwrap();
        assert!(settings.poll_url.is_none());
        assert!(settings.poll_token.is_none());

        let settings = Settings::from_toml_and_env(
            &base_toml("poll_url = \"https://jobs.example/poll\"\npoll_interval_secs = 5"),
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
        assert_eq!(settings.poll_interval, Duration::from_secs(5));
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
    fn worker_name_and_os_default_and_validate() {
        let settings = Settings::from_toml_and_env(&base_toml(""), &HashMap::new()).unwrap();
        assert!(!settings.worker_name.is_empty());
        assert!(!settings.worker_name.contains(' '));
        assert_eq!(settings.worker_os, std::env::consts::OS);

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
