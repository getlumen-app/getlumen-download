//! Settings → "Refresh Config": decide what a refresh means for the active
//! profile and report when the config on disk was last written.

use std::path::Path;

/// What a refresh can do for a given profile input.
#[derive(Debug, PartialEq, Eq)]
pub enum RefreshSource {
    /// A single pasted `vless://` / `hy2://` link — there is no server config
    /// to download; the profile *is* the config.
    Static,
    /// Subscription endpoints to fetch, in fallback order.
    Urls(Vec<String>),
}

pub fn refresh_source(key: &str) -> Result<RefreshSource, String> {
    let raw = key.trim();
    if raw.is_empty() {
        return Err("No active profile to refresh".to_string());
    }
    // Same normalization as connect: our own subscription URLs collapse to the
    // bare key so the refresh goes through the config gateway, not a raw backend.
    if let Some(extracted) = crate::extract_proteus_key(raw) {
        return Ok(RefreshSource::Urls(crate::config::proteus_config_urls(&extracted)));
    }
    match crate::detect_input_type(raw) {
        "vless" | "hy2" => Ok(RefreshSource::Static),
        "subscription_url" => Ok(RefreshSource::Urls(vec![raw.to_string()])),
        _ => Ok(RefreshSource::Urls(crate::config::proteus_config_urls(raw))),
    }
}

/// Last-modified time of a config file in epoch milliseconds, or `None` when
/// the file does not exist yet (never fetched — not "just now").
pub fn modified_ms(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let ms = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis();
    u64::try_from(ms).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_key_is_an_error_not_a_silent_no_op() {
        assert!(refresh_source("   ").is_err());
    }

    #[test]
    fn single_links_have_nothing_to_download() {
        assert_eq!(
            refresh_source("vless://00000000-0000-4000-8000-000000000001@192.0.2.10:443?security=reality#test-reality").unwrap(),
            RefreshSource::Static
        );
        assert_eq!(
            refresh_source("hy2://secret@192.0.2.11:443#test-hy2").unwrap(),
            RefreshSource::Static
        );
    }

    #[test]
    fn bare_subscription_key_fetches_the_full_config_endpoints() {
        match refresh_source("TestKey0123456789").unwrap() {
            RefreshSource::Urls(urls) => {
                assert!(!urls.is_empty());
                assert!(urls[0].contains("sub=TestKey0123456789"));
                assert!(urls[0].contains("format=json-text"));
            }
            other => panic!("expected Urls, got {other:?}"),
        }
    }

    #[test]
    fn foreign_subscription_url_is_fetched_as_is() {
        assert_eq!(
            refresh_source("https://sub.example.test/path/config.json").unwrap(),
            RefreshSource::Urls(vec!["https://sub.example.test/path/config.json".to_string()])
        );
    }

    #[test]
    fn missing_file_has_no_timestamp() {
        assert_eq!(modified_ms(Path::new("/nonexistent/lumen/config.json")), None);
    }
}
