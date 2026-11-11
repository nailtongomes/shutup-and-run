//! Image allowlist. An entry without a tag allows any tag or digest of that
//! repository. An entry with a tag allows only that tag (optionally with a
//! digest of the same tag). `repo` does not allow `repo-evil`.

pub fn validate_image_name(image: &str) -> Result<(), String> {
    if image.is_empty() || image.len() > 512 {
        return Err("invalid image name".into());
    }
    if image
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c.is_ascii_uppercase())
    {
        return Err("invalid image name".into());
    }
    if image.contains("..")
        || image.contains("//")
        || image.starts_with('-')
        || image.ends_with(':')
    {
        return Err("invalid image name".into());
    }
    Ok(())
}

pub fn image_allowed(image: &str, allowlist: &[String]) -> bool {
    allowlist
        .iter()
        .any(|entry| match image.strip_prefix(entry.as_str()) {
            Some("") => true,
            Some(rest) => rest.starts_with(':') || rest.starts_with('@'),
            None => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow() -> Vec<String> {
        vec![
            "alpine".into(),
            "ghcr.io/owner/selenium-worker".into(),
            "ghcr.io/owner/pinned:1.4.2".into(),
        ]
    }

    #[test]
    fn matches_repo_tag_and_digest_but_not_neighbors() {
        let allow = allow();
        assert!(image_allowed("alpine", &allow));
        assert!(image_allowed("alpine:3.20", &allow));
        assert!(image_allowed("alpine@sha256:abcd", &allow));
        assert!(image_allowed("ghcr.io/owner/selenium-worker:1.2.3", &allow));
        assert!(image_allowed("ghcr.io/owner/pinned:1.4.2", &allow));
        assert!(image_allowed(
            "ghcr.io/owner/pinned:1.4.2@sha256:fff",
            &allow
        ));
        assert!(!image_allowed("alpine-evil:latest", &allow));
        assert!(!image_allowed(
            "ghcr.io/owner/selenium-worker-evil:1",
            &allow
        ));
        assert!(!image_allowed("ghcr.io/other/selenium-worker:1", &allow));
        assert!(!image_allowed("ghcr.io/owner/pinned:1.4.20", &allow));
        assert!(!image_allowed("ghcr.io/owner/pinned:1.4.3", &allow));
        assert!(!image_allowed("redis:7", &allow));
    }

    #[test]
    fn rejects_odd_names() {
        assert!(validate_image_name("Alpine").is_err());
        assert!(validate_image_name("alpine latest").is_err());
        assert!(validate_image_name("../alpine").is_err());
        assert!(validate_image_name("alpine").is_ok());
    }
}
