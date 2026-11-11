//! Bearer-token check. The raw token is hashed once at startup and compared in
//! constant time, so the request path does not branch on the secret's bytes.

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

#[derive(Clone)]
pub struct TokenChecker {
    hash: [u8; 32],
}

impl std::fmt::Debug for TokenChecker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenChecker(***)")
    }
}

impl TokenChecker {
    pub fn new(token: &str) -> Self {
        let digest = Sha256::digest(token.as_bytes());
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&digest);
        Self { hash }
    }

    pub fn matches(&self, provided: &str) -> bool {
        let digest = Sha256::digest(provided.as_bytes());
        self.hash.ct_eq(digest.as_slice()).into()
    }
}

/// Returns the token after a `Bearer` prefix, or an empty string when the header
/// is missing or not a bearer token. Callers still run [`TokenChecker::matches`]
/// on the result so the missing-header path does the same hash-and-compare work.
pub fn bearer_token(header: &str) -> &str {
    let value = header.trim();
    let Some(rest) = value.get(6..) else {
        return "";
    };
    if !value[..6].eq_ignore_ascii_case("bearer") {
        return "";
    }
    if !rest.starts_with(|c: char| c.is_ascii_whitespace()) {
        return "";
    }
    rest.trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_equal_tokens_and_rejects_others() {
        let checker = TokenChecker::new("token-with-sixteen");
        assert!(checker.matches("token-with-sixteen"));
        assert!(!checker.matches("token-with-sixteex"));
        assert!(!checker.matches("token-with-sixteen-extra"));
        assert!(!checker.matches(""));
        assert!(!checker.matches("short"));
    }

    #[test]
    fn bearer_prefix() {
        assert_eq!(bearer_token("Bearer secret-token"), "secret-token");
        assert_eq!(bearer_token("bearer   secret-token  "), "secret-token");
        assert_eq!(bearer_token("secret-token"), "");
        assert_eq!(bearer_token("Basic abc"), "");
        assert_eq!(bearer_token(""), "");
    }
}
