//! Registry host and credentials taken from the machine's Docker config.
//! Nothing in the HTTP request can supply a registry secret.

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryAuth {
    pub username: Option<String>,
    pub password: Option<String>,
    pub identitytoken: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthSource {
    Anonymous,
    Basic(RegistryAuth),
    /// `docker-credential-<helper> get`, with the server name written to stdin.
    Helper {
        helper: String,
        server: String,
    },
}

/// Registry hostname (and port, if any) of an image reference. Docker Hub names
/// without an explicit registry return `docker.io`.
pub fn registry_host(image: &str) -> &str {
    let Some((first, _)) = image.split_once('/') else {
        return "docker.io";
    };
    if first == "localhost"
        || first.starts_with("localhost:")
        || first.contains('.')
        || first.contains(':')
    {
        first.split('@').next().unwrap_or(first)
    } else {
        "docker.io"
    }
}

/// Pull reference. A name without a tag would make the Docker API pull every
/// tag of the repository, so we pin `:latest` for the pull only.
pub fn pull_reference(image: &str) -> String {
    if image.contains('@') {
        return image.to_string();
    }
    let tail = image
        .rsplit_once('/')
        .map(|(_, tail)| tail)
        .unwrap_or(image);
    if tail.contains(':') {
        image.to_string()
    } else {
        format!("{image}:latest")
    }
}

pub fn default_docker_config_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("DOCKER_CONFIG") {
        return PathBuf::from(dir).join("config.json");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/root"));
    home.join(".docker").join("config.json")
}

#[derive(Debug, Deserialize)]
struct DockerCfg {
    #[serde(default)]
    auths: HashMap<String, AuthEntry>,
    #[serde(rename = "credsStore")]
    creds_store: Option<String>,
    #[serde(default, rename = "credHelpers")]
    cred_helpers: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct AuthEntry {
    auth: Option<String>,
    username: Option<String>,
    password: Option<String>,
    identitytoken: Option<String>,
}

pub fn auth_from_docker_config(config_json: &str, image: &str) -> Result<AuthSource, String> {
    if config_json.trim().is_empty() {
        return Ok(AuthSource::Anonymous);
    }
    let cfg: DockerCfg =
        serde_json::from_str(config_json).map_err(|e| format!("config.json: {e}"))?;
    let host = registry_host(image);
    let keys = candidate_keys(host);

    for key in &keys {
        if let Some(helper) = cfg.cred_helpers.get(key) {
            return helper_source(helper, key);
        }
    }
    for key in &keys {
        if let Some(entry) = cfg.auths.get(key) {
            if let Some(auth) = decode_entry(entry)? {
                return Ok(AuthSource::Basic(auth));
            }
        }
    }
    if let Some(store) = cfg.creds_store.as_deref() {
        let server = if host == "docker.io" {
            "https://index.docker.io/v1/"
        } else {
            host
        };
        return helper_source(store, server);
    }
    Ok(AuthSource::Anonymous)
}

fn candidate_keys(host: &str) -> Vec<String> {
    let mut keys = vec![host.to_string(), format!("https://{host}")];
    if host == "docker.io" || host == "index.docker.io" || host == "registry-1.docker.io" {
        keys.push("https://index.docker.io/v1/".into());
        keys.push("docker.io".into());
        keys.push("index.docker.io".into());
    }
    keys
}

fn helper_source(helper: &str, server: &str) -> Result<AuthSource, String> {
    if !valid_helper_name(helper) {
        return Err("invalid credential helper name".into());
    }
    Ok(AuthSource::Helper {
        helper: helper.to_string(),
        server: server.to_string(),
    })
}

pub fn valid_helper_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn decode_entry(entry: &AuthEntry) -> Result<Option<RegistryAuth>, String> {
    if let Some(token) = entry.identitytoken.clone().filter(|s| !s.is_empty()) {
        return Ok(Some(RegistryAuth {
            username: entry.username.clone(),
            password: None,
            identitytoken: Some(token),
        }));
    }
    if let Some(auth) = entry.auth.as_deref().filter(|s| !s.is_empty()) {
        let bytes = STANDARD
            .decode(auth.trim())
            .map_err(|_| "docker config auth is not base64".to_string())?;
        let text =
            String::from_utf8(bytes).map_err(|_| "docker config auth is not utf-8".to_string())?;
        let (user, pass) = text.split_once(':').unwrap_or((text.as_str(), ""));
        return Ok(Some(RegistryAuth {
            username: Some(user.to_string()),
            password: Some(pass.to_string()),
            identitytoken: None,
        }));
    }
    if entry.username.is_some() || entry.password.is_some() {
        return Ok(Some(RegistryAuth {
            username: entry.username.clone(),
            password: entry.password.clone(),
            identitytoken: None,
        }));
    }
    Ok(None)
}

/// Parse the JSON document printed by `docker-credential-* get`.
pub fn auth_from_helper_output(stdout: &str) -> Result<RegistryAuth, String> {
    let value: Value = serde_json::from_str(stdout)
        .map_err(|_| "credential helper did not return JSON".to_string())?;
    let username = value
        .get("Username")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let secret = value
        .get("Secret")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if username.as_deref() == Some("<token>") {
        return Ok(RegistryAuth {
            username: None,
            password: None,
            identitytoken: secret,
        });
    }
    Ok(RegistryAuth {
        username,
        password: secret,
        identitytoken: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_and_pull_reference() {
        assert_eq!(registry_host("alpine"), "docker.io");
        assert_eq!(registry_host("alpine:3.20"), "docker.io");
        assert_eq!(registry_host("library/alpine"), "docker.io");
        assert_eq!(registry_host("ghcr.io/owner/selenium-worker:1"), "ghcr.io");
        assert_eq!(
            registry_host("ghcr.io/owner/selenium-worker@sha256:abc"),
            "ghcr.io"
        );
        assert_eq!(registry_host("localhost:5000/foo:bar"), "localhost:5000");
        assert_eq!(pull_reference("alpine"), "alpine:latest");
        assert_eq!(
            pull_reference("ghcr.io/acme/job"),
            "ghcr.io/acme/job:latest"
        );
        assert_eq!(
            pull_reference("ghcr.io/acme/job:1.2"),
            "ghcr.io/acme/job:1.2"
        );
        assert_eq!(
            pull_reference("localhost:5000/foo"),
            "localhost:5000/foo:latest"
        );
        assert_eq!(pull_reference("alpine@sha256:abc"), "alpine@sha256:abc");
    }

    #[test]
    fn reads_auth_field_and_helpers() {
        let encoded = STANDARD.encode("alice:sec:ret");
        let json = format!(
            r#"{{"auths":{{"ghcr.io":{{"auth":"{encoded}"}}}},"credHelpers":{{"docker.io":"pass"}}}}"#
        );
        match auth_from_docker_config(&json, "ghcr.io/acme/job:1").unwrap() {
            AuthSource::Basic(auth) => {
                assert_eq!(auth.username.as_deref(), Some("alice"));
                assert_eq!(auth.password.as_deref(), Some("sec:ret"));
            }
            other => panic!("unexpected {other:?}"),
        }
        match auth_from_docker_config(&json, "alpine:3").unwrap() {
            AuthSource::Helper { helper, server } => {
                assert_eq!(helper, "pass");
                assert_eq!(server, "docker.io");
            }
            other => panic!("unexpected {other:?}"),
        }
        let store = r#"{"auths":{},"credsStore":"pass"}"#;
        match auth_from_docker_config(store, "ghcr.io/acme/job:1").unwrap() {
            AuthSource::Helper { helper, server } => {
                assert_eq!(helper, "pass");
                assert_eq!(server, "ghcr.io");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(
            auth_from_docker_config("{}", "alpine").unwrap(),
            AuthSource::Anonymous
        ));
        assert!(auth_from_docker_config(r#"{"credsStore":"evil helper"}"#, "alpine").is_err());
    }

    #[test]
    fn helper_json() {
        let auth = auth_from_helper_output(r#"{"Username":"octo","Secret":"ghp_x"}"#).unwrap();
        assert_eq!(auth.username.as_deref(), Some("octo"));
        assert_eq!(auth.password.as_deref(), Some("ghp_x"));
        let token = auth_from_helper_output(r#"{"Username":"<token>","Secret":"id"}"#).unwrap();
        assert_eq!(token.identitytoken.as_deref(), Some("id"));
    }
}
