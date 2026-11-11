use crate::allowlist::validate_image_name;
use crate::auth::TokenChecker;
use serde::Deserialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

const ENV_KEYS: &[&str] = &[
    "FAIRLEAD_CONFIG",
    "FAIRLEAD_LISTEN",
    "FAIRLEAD_TOKEN",
    "FAIRLEAD_MAX_SLOTS",
    "FAIRLEAD_ALLOWLIST",
    "FAIRLEAD_DEFAULT_TIMEOUT_SECS",
    "FAIRLEAD_MAX_TIMEOUT_SECS",
    "FAIRLEAD_REAPER_INTERVAL_SECS",
    "FAIRLEAD_DOCKER_CONFIG",
    "FAIRLEAD_HISTORY_PATH",
    "FAIRLEAD_HISTORY_DAYS",
    "FAIRLEAD_PURGE_INTERVAL_SECS",
    "FAIRLEAD_LOG_LINES",
    "FAIRLEAD_PRUNE_INTERVAL_SECS",
];

const DEFAULT_LISTEN: &str = "127.0.0.1:8787";
const DEFAULT_TIMEOUT: u64 = 3600;
const DEFAULT_MAX_TIMEOUT: u64 = 21_600;
const DEFAULT_REAPER: u64 = 2;
const DEFAULT_HISTORY: &str = "/var/lib/fairlead/jobs.sqlite";
const DEFAULT_HISTORY_DAYS: u64 = 30;
const DEFAULT_PURGE: u64 = 3600;
const DEFAULT_LOG_LINES: u64 = 200;

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
    pub token: TokenChecker,
}

#[derive(Debug, Deserialize)]
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
}

impl Settings {
    pub fn load() -> Result<Self, ConfigError> {
        let mut env = HashMap::new();
        for key in ENV_KEYS {
            if let Ok(value) = std::env::var(key) {
                env.insert((*key).to_string(), value);
            }
        }
        let toml_text = read_config_file(&env)?;
        Self::from_toml_and_env(&toml_text, &env)
    }

    pub fn from_toml_and_env(
        toml_text: &str,
        env: &HashMap<String, String>,
    ) -> Result<Self, ConfigError> {
        let file: FileConfig = if toml_text.trim().is_empty() {
            FileConfig {
                listen: None,
                token: None,
                max_slots: None,
                allowlist: None,
                default_timeout_secs: None,
                max_timeout_secs: None,
                reaper_interval_secs: None,
                docker_config: None,
                history_path: None,
                history_days: None,
                purge_interval_secs: None,
                log_lines: None,
                prune_interval_secs: None,
            }
        } else {
            toml::from_str(toml_text).map_err(|err| ConfigError(format!("config.toml: {err}")))?
        };

        let listen = take_string(env, "FAIRLEAD_LISTEN", file.listen)
            .unwrap_or_else(|| DEFAULT_LISTEN.to_string());
        let listen: SocketAddr = listen
            .parse()
            .map_err(|_| ConfigError(format!("invalid listen address: {listen}")))?;

        let token = take_string(env, "FAIRLEAD_TOKEN", file.token).unwrap_or_default();
        if token.chars().any(|c| c.is_whitespace()) || token.len() < 16 {
            return Err(ConfigError(
                "token is missing or shorter than 16 characters. Set FAIRLEAD_TOKEN or token in the file.".into(),
            ));
        }

        let max_slots = take_u64(env, "FAIRLEAD_MAX_SLOTS", file.max_slots.map(|n| n as u64))?
            .ok_or_else(|| ConfigError("max_slots is required".into()))?
            as usize;
        if !(1..=256).contains(&max_slots) {
            return Err(ConfigError("max_slots must be between 1 and 256".into()));
        }

        let allowlist = if let Some(raw) = env.get("FAIRLEAD_ALLOWLIST") {
            split_allowlist(raw)?
        } else {
            file.allowlist.unwrap_or_default()
        };
        if allowlist.is_empty() {
            return Err(ConfigError(
                "allowlist is empty. List permitted images, for example ghcr.io/OWNER/selenium-worker.".into(),
            ));
        }
        for image in &allowlist {
            validate_image_name(image).map_err(ConfigError)?;
        }

        let default_timeout_secs = take_u64(
            env,
            "FAIRLEAD_DEFAULT_TIMEOUT_SECS",
            file.default_timeout_secs,
        )?
        .unwrap_or(DEFAULT_TIMEOUT);
        let max_timeout_secs = take_u64(env, "FAIRLEAD_MAX_TIMEOUT_SECS", file.max_timeout_secs)?
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
            "FAIRLEAD_REAPER_INTERVAL_SECS",
            file.reaper_interval_secs,
        )?
        .unwrap_or(DEFAULT_REAPER);
        if !(1..=300).contains(&reaper) {
            return Err(ConfigError(
                "reaper_interval_secs must be between 1 and 300".into(),
            ));
        }

        let history_days = take_u64(env, "FAIRLEAD_HISTORY_DAYS", file.history_days)?
            .unwrap_or(DEFAULT_HISTORY_DAYS);
        if history_days > 3650 {
            return Err(ConfigError(
                "history_days must be between 0 and 3650".into(),
            ));
        }
        let purge = take_u64(
            env,
            "FAIRLEAD_PURGE_INTERVAL_SECS",
            file.purge_interval_secs,
        )?
        .unwrap_or(DEFAULT_PURGE);
        if !(1..=86_400).contains(&purge) {
            return Err(ConfigError(
                "purge_interval_secs must be between 1 and 86400".into(),
            ));
        }
        let log_lines =
            take_u64(env, "FAIRLEAD_LOG_LINES", file.log_lines)?.unwrap_or(DEFAULT_LOG_LINES);
        if !(1..=5000).contains(&log_lines) {
            return Err(ConfigError("log_lines must be between 1 and 5000".into()));
        }
        let prune = take_u64(
            env,
            "FAIRLEAD_PRUNE_INTERVAL_SECS",
            file.prune_interval_secs,
        )?
        .unwrap_or(0);
        if prune > 86_400 {
            return Err(ConfigError(
                "prune_interval_secs must be between 0 and 86400".into(),
            ));
        }

        let history_path = take_string(env, "FAIRLEAD_HISTORY_PATH", file.history_path)
            .unwrap_or_else(|| DEFAULT_HISTORY.to_string());
        let docker_config =
            take_string(env, "FAIRLEAD_DOCKER_CONFIG", file.docker_config).map(PathBuf::from);

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
            token: TokenChecker::new(&token),
        })
    }
}

fn read_config_file(env: &HashMap<String, String>) -> Result<String, ConfigError> {
    if let Some(path) = env.get("FAIRLEAD_CONFIG") {
        return std::fs::read_to_string(path)
            .map_err(|err| ConfigError(format!("could not read {path}: {err}")));
    }
    let default = Path::new("/etc/fairlead/config.toml");
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

fn split_allowlist(raw: &str) -> Result<Vec<String>, ConfigError> {
    let items: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if items.is_empty() {
        return Err(ConfigError("FAIRLEAD_ALLOWLIST is empty".into()));
    }
    Ok(items)
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
                ("FAIRLEAD_MAX_SLOTS", "4"),
                ("FAIRLEAD_TOKEN", "token-from-the-env"),
                (
                    "FAIRLEAD_ALLOWLIST",
                    "alpine, ghcr.io/owner/selenium-worker",
                ),
                ("FAIRLEAD_HISTORY_DAYS", "0"),
                ("FAIRLEAD_LOG_LINES", "50"),
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
}
