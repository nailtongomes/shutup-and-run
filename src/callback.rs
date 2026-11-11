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

pub fn spawn_callback(url: String, body: Value) {
    tokio::spawn(async move {
        // shortcut: a fresh client per callback, three tries, no redirects.
        // Upgrade to a shared client if callback volume ever matters.
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
        {
            Ok(client) => client,
            Err(err) => {
                tracing::warn!(error = %err, "callback client");
                return;
            }
        };
        for attempt in 1..=3 {
            match client.post(&url).json(&body).send().await {
                Ok(response) if response.status().is_success() => return,
                Ok(response) => {
                    tracing::warn!(status = %response.status(), attempt, "callback rejected");
                }
                Err(err) => {
                    tracing::warn!(error = %err, attempt, "callback failed");
                }
            }
            tokio::time::sleep(Duration::from_millis(200 * attempt)).await;
        }
    });
}
