//! Best-effort POST when a job ends. Never holds a slot and never fails the job.

use serde_json::Value;
use std::time::Duration;

pub fn validate_callback_url(url: &str) -> Result<(), String> {
    if url.len() > 2048 || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("callback_url is invalid".into());
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| "callback_url is invalid".to_string())?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("callback_url must be http or https".into());
    }
    if parsed.host_str().is_none() {
        return Err("callback_url is invalid".into());
    }
    Ok(())
}

/// The first tries, right when the job ends. A callback they do not deliver
/// stays `pending` in the history and is tried again once a minute.
pub const FIRST_TRIES: u64 = 3;

/// True when the receiver answered 2xx.
pub async fn deliver(url: &str, body: &Value, tries: u64) -> bool {
    // shortcut: a fresh client per callback, no redirects.
    // Upgrade to a shared client if callback volume ever matters.
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            tracing::warn!(error = %err, "callback client");
            return false;
        }
    };
    for attempt in 1..=tries {
        // The error goes to the log without the URL, which is a credential.
        match client.post(url).json(body).send().await {
            Ok(response) if response.status().is_success() => return true,
            Ok(response) => {
                tracing::warn!(status = %response.status(), attempt, "callback rejected");
            }
            Err(err) => {
                tracing::warn!(error = %err.without_url(), attempt, "callback failed");
            }
        }
        if attempt < tries {
            tokio::time::sleep(Duration::from_millis(200 * attempt)).await;
        }
    }
    false
}
