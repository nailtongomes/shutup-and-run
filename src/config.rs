use crate::allowlist::validate_image_name;
use crate::auth::TokenChecker;
use serde::Deserialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

const ENV_KEYS: &[&str] = &[
    "N3_CONFIG",
    "N3_LISTEN",
    "N3_TOKEN",
    "N3_MAX_SLOTS",
    "N3_ALLOWLIST",
    "N3_DEFAULT_TIMEOUT_SECS",
    "N3_MAX_TIMEOUT_SECS",
    "N3_REAPER_INTERVAL_SECS",
    "N3_DOCKER_CONFIG",
];

const DEFAULT_LISTEN: &str = "127.0.0.1:8787";
const DEFAULT_TIMEOUT: u64 = 3600;
const DEFAULT_MAX_TIMEOUT: u64 = 21_600;
const DEFAULT_REAPER: u64 = 2;

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
            }
        } else {
            toml::from_str(toml_text).map_err(|e| ConfigError(format!("config.toml: {e}")))?
        };

        let listen = take_string(env, "N3_LISTEN", file.listen)
            .unwrap_or_else(|| DEFAULT_LISTEN.to_string());
        let listen: SocketAddr = listen
            .parse()
            .map_err(|_| ConfigError(format!("listen inválido: {listen}")))?;

        let token = take_string(env, "N3_TOKEN", file.token).unwrap_or_default();
        if token.chars().any(|c| c.is_whitespace()) || token.len() < 16 {
            return Err(ConfigError(
                "token ausente ou curto demais (mínimo 16 caracteres, sem espaço). Defina N3_TOKEN ou token no arquivo.".into(),
            ));
        }

        let max_slots = take_u64(env, "N3_MAX_SLOTS", file.max_slots.map(|n| n as u64))?
            .ok_or_else(|| ConfigError("max_slots é obrigatório".into()))?
            as usize;
        if !(1..=256).contains(&max_slots) {
            return Err(ConfigError("max_slots deve estar entre 1 e 256".into()));
        }

        let allowlist = if let Some(raw) = env.get("N3_ALLOWLIST") {
            split_allowlist(raw)?
        } else {
            file.allowlist.unwrap_or_default()
        };
        if allowlist.is_empty() {
            return Err(ConfigError(
                "allowlist vazia. Liste as imagens permitidas, por exemplo ghcr.io/nailtongomes/rpa-worker-selenium.".into(),
            ));
        }
        for image in &allowlist {
            validate_image_name(image).map_err(ConfigError)?;
        }

        let default_timeout_secs =
            take_u64(env, "N3_DEFAULT_TIMEOUT_SECS", file.default_timeout_secs)?
                .unwrap_or(DEFAULT_TIMEOUT);
        let max_timeout_secs = take_u64(env, "N3_MAX_TIMEOUT_SECS", file.max_timeout_secs)?
            .unwrap_or(DEFAULT_MAX_TIMEOUT);
        if default_timeout_secs == 0 || max_timeout_secs == 0 {
            return Err(ConfigError("timeout deve ser maior que zero".into()));
        }
        if default_timeout_secs > max_timeout_secs {
            return Err(ConfigError(
                "default_timeout_secs não pode ser maior que max_timeout_secs".into(),
            ));
        }
        if max_timeout_secs > 7 * 24 * 3600 {
            return Err(ConfigError("max_timeout_secs acima de 7 dias".into()));
        }

        let reaper = take_u64(env, "N3_REAPER_INTERVAL_SECS", file.reaper_interval_secs)?
            .unwrap_or(DEFAULT_REAPER);
        if !(1..=300).contains(&reaper) {
            return Err(ConfigError(
                "reaper_interval_secs deve estar entre 1 e 300".into(),
            ));
        }

        let docker_config =
            take_string(env, "N3_DOCKER_CONFIG", file.docker_config).map(PathBuf::from);

        Ok(Settings {
            listen,
            max_slots,
            allowlist,
            default_timeout_secs,
            max_timeout_secs,
            reaper_interval: Duration::from_secs(reaper),
            docker_config,
            token: TokenChecker::new(&token),
        })
    }
}

fn read_config_file(env: &HashMap<String, String>) -> Result<String, ConfigError> {
    if let Some(path) = env.get("N3_CONFIG") {
        return std::fs::read_to_string(path)
            .map_err(|e| ConfigError(format!("não li {path}: {e}")));
    }
    let default = Path::new("/etc/n3-launcher/config.toml");
    if default.exists() {
        return std::fs::read_to_string(default)
            .map_err(|e| ConfigError(format!("não li {}: {e}", default.display())));
    }
    Ok(String::new())
}

fn take_string(env: &HashMap<String, String>, key: &str, file: Option<String>) -> Option<String> {
    env.get(key).filter(|v| !v.is_empty()).cloned().or(file)
}

fn take_u64(
    env: &HashMap<String, String>,
    key: &str,
    file: Option<u64>,
) -> Result<Option<u64>, ConfigError> {
    if let Some(raw) = env.get(key) {
        if raw.is_empty() {
            return Err(ConfigError(format!("{key} vazio")));
        }
        let value = raw
            .parse::<u64>()
            .map_err(|_| ConfigError(format!("{key} não é um inteiro")))?;
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
        return Err(ConfigError("N3_ALLOWLIST vazia".into()));
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
            token = "token-do-arquivo-ok"
            max_slots = 2
            allowlist = ["alpine"]
            default_timeout_secs = 30
            max_timeout_secs = 60
        "#;
        let settings = Settings::from_toml_and_env(
            toml_text,
            &env(&[
                ("N3_MAX_SLOTS", "4"),
                ("N3_TOKEN", "token-vindo-do-env"),
                (
                    "N3_ALLOWLIST",
                    "alpine, ghcr.io/nailtongomes/rpa-worker-selenium",
                ),
            ]),
        )
        .unwrap();
        assert_eq!(settings.max_slots, 4);
        assert!(settings.token.matches("token-vindo-do-env"));
        assert!(!settings.token.matches("token-do-arquivo-ok"));
        assert_eq!(settings.allowlist.len(), 2);
        assert_eq!(
            settings.listen,
            "127.0.0.1:9".parse::<SocketAddr>().unwrap()
        );

        let bad = r#"
            listen = "127.0.0.1:9"
            token = "token-do-arquivo-ok"
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
            token = "curto"
            max_slots = 1
            allowlist = ["alpine"]
        "#;
        assert!(Settings::from_toml_and_env(short, &HashMap::new()).is_err());
        let inverted = r#"
            token = "token-do-arquivo-ok"
            max_slots = 1
            allowlist = ["alpine"]
            default_timeout_secs = 100
            max_timeout_secs = 10
        "#;
        assert!(Settings::from_toml_and_env(inverted, &HashMap::new()).is_err());
    }
}
