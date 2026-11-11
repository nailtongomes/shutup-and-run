//! SHA-256 fingerprint of a PEM certificate, matching `openssl x509 -fingerprint -sha256`.

use base64::{engine::general_purpose::STANDARD, Engine};
use sha2::{Digest, Sha256};
use std::path::Path;

pub fn fingerprint_file(path: &Path) -> Result<String, String> {
    let pem = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    fingerprint_pem(&pem)
}

pub fn fingerprint_pem(pem: &str) -> Result<String, String> {
    let b64: String = pem
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("-----"))
        .collect();
    let der = STANDARD
        .decode(b64)
        .map_err(|err| format!("certificate is not valid PEM: {err}"))?;
    let digest = Sha256::digest(&der);
    Ok(digest
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":"))
}
