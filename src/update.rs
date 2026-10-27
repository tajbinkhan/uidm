//! Checking GitHub for a newer UIDM release. Nothing is downloaded or
//! installed: the window shows a notice with a link to the release page.

use serde::{Deserialize, Serialize};

/// The releases of UIDM's repository, newest first by GitHub's reckoning.
pub const LATEST_RELEASE: &str = "https://api.github.com/repos/tajbinkhan/uidm/releases/latest";

/// How often the window's check runs on its own: once a week.
pub const CHECK_EVERY_SECS: u64 = 7 * 24 * 60 * 60;

/// A release on GitHub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// The version, without a leading `v`.
    pub version: String,
    /// Its page on GitHub.
    pub url: String,
}

/// `1.2.3` (or `v1.2.3`, `1.2.3-beta`) as numbers to compare; missing parts
/// count as 0.
fn numbers(version: &str) -> Vec<u64> {
    version
        .trim()
        .trim_start_matches(['v', 'V'])
        .split(['.', '-', '+'])
        .take(3)
        .map(|p| p.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0))
        .collect()
}

/// Whether `candidate` is a later version than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (mut a, mut b) = (numbers(candidate), numbers(current));
    a.resize(3, 0);
    b.resize(3, 0);
    a > b
}

/// The release in GitHub's answer for the latest release.
pub fn parse_release(json: &[u8]) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_slice(json).map_err(|e| format!("GitHub's answer is not JSON: {e}"))?;
    let tag = v["tag_name"].as_str().ok_or("GitHub's answer names no release.")?;
    let url = v["html_url"].as_str().unwrap_or("https://github.com/tajbinkhan/uidm/releases");
    // Only a link to GitHub is ever shown.
    let url = if url.starts_with("https://github.com/") { url } else { "https://github.com/tajbinkhan/uidm/releases" };
    Ok(Release {
        version: tag.trim().trim_start_matches(['v', 'V']).to_string(),
        url: url.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.5"));
        assert!(is_newer("1.0", "0.99.99"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0-beta", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
    }

    #[test]
    fn releases_are_read_from_githubs_answer() {
        let r = parse_release(br#"{"tag_name":"v0.4.1","html_url":"https://github.com/tajbinkhan/uidm/releases/tag/v0.4.1"}"#).unwrap();
        assert_eq!(r.version, "0.4.1");
        assert!(r.url.ends_with("/v0.4.1"));
        let elsewhere = parse_release(br#"{"tag_name":"v9","html_url":"https://evil.example/x"}"#).unwrap();
        assert_eq!(elsewhere.url, "https://github.com/tajbinkhan/uidm/releases");
        assert!(parse_release(b"{}").is_err());
        assert!(parse_release(b"not json").is_err());
    }
}
