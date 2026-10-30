//! Checking GitHub for a newer UIDM release, and the Setup program attached
//! to it. The window shows a notice with a link to the release page; Update
//! now has the daemon download that Setup, check it against what GitHub
//! says about it, and run it.

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
    /// Its Setup program, when one is attached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<SetupAsset>,
}

/// A release's `UIDM-Setup-<version>.exe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupAsset {
    pub name: String,
    /// Where GitHub serves it, under this repository's release downloads.
    pub url: String,
    pub size: u64,
    /// Its SHA-256 in hex, when GitHub reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// Where release files are served from; a Setup anywhere else is not used.
const DOWNLOADS: &str = "https://github.com/tajbinkhan/uidm/releases/download/";
/// The largest Setup UIDM downloads.
pub const MAX_SETUP_BYTES: u64 = 256 * 1024 * 1024;

/// The Setup asset among a release's `assets`, if there is one UIDM can
/// use: named `UIDM-Setup-*.exe`, served from the repository's releases,
/// and of a sensible size.
fn setup_asset(assets: &serde_json::Value) -> Option<SetupAsset> {
    assets.as_array()?.iter().find_map(|a| {
        let name = a["name"].as_str()?;
        let url = a["browser_download_url"].as_str()?;
        let size = a["size"].as_u64()?;
        let lower = name.to_ascii_lowercase();
        let usable = lower.starts_with("uidm-setup-")
            && lower.ends_with(".exe")
            && !name.contains(['/', '\\', ':'])
            && url.starts_with(DOWNLOADS)
            && (1..=MAX_SETUP_BYTES).contains(&size);
        let sha256 = a["digest"]
            .as_str()
            .and_then(|d| d.strip_prefix("sha256:"))
            .filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()))
            .map(str::to_ascii_lowercase);
        usable.then(|| SetupAsset {
            name: name.to_string(),
            url: url.to_string(),
            size,
            sha256,
        })
    })
}

/// Check a downloaded Setup against what GitHub said about it: its size,
/// its SHA-256 when given, and that it is a Windows program.
pub fn check_setup(asset: &SetupAsset, bytes: &[u8]) -> Result<(), String> {
    use sha2::Digest as _;
    if bytes.len() as u64 != asset.size {
        return Err(format!("The update is {} bytes, not the {} GitHub lists.", bytes.len(), asset.size));
    }
    if !bytes.starts_with(b"MZ") {
        return Err("The update is not a Windows program.".into());
    }
    if let Some(want) = &asset.sha256 {
        let got: String = sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
        if &got != want {
            return Err("The update does not match the checksum GitHub lists.".into());
        }
    }
    Ok(())
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
        setup: setup_asset(&v["assets"]),
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
        assert_eq!(r.setup, None);
    }

    #[test]
    fn the_setup_asset_is_found_and_checked() {
        let body = b"MZ setup";
        let sha = {
            use sha2::Digest as _;
            sha2::Sha256::digest(body).iter().map(|b| format!("{b:02x}")).collect::<String>()
        };
        let json = format!(
            r#"{{"tag_name":"v0.5.0","html_url":"https://github.com/tajbinkhan/uidm/releases/tag/v0.5.0","assets":[
                {{"name":"uidm-0.5.0.zip","browser_download_url":"https://github.com/tajbinkhan/uidm/releases/download/v0.5.0/uidm-0.5.0.zip","size":5}},
                {{"name":"UIDM-Setup-0.5.0.exe","browser_download_url":"https://github.com/tajbinkhan/uidm/releases/download/v0.5.0/UIDM-Setup-0.5.0.exe","size":{},"digest":"sha256:{sha}"}}
            ]}}"#,
            body.len()
        );
        let setup = parse_release(json.as_bytes()).unwrap().setup.unwrap();
        assert_eq!(setup.name, "UIDM-Setup-0.5.0.exe");
        assert_eq!(setup.sha256.as_deref(), Some(sha.as_str()));
        assert!(check_setup(&setup, body).is_ok());
        assert!(check_setup(&setup, b"MZ setuP").unwrap_err().contains("checksum"));
        assert!(check_setup(&setup, b"MZ").unwrap_err().contains("bytes"));
        assert!(check_setup(&SetupAsset { sha256: None, ..setup.clone() }, b"XX setup").unwrap_err().contains("Windows program"));

        let elsewhere = json.replace("https://github.com/tajbinkhan/uidm/releases/download/v0.5.0/UIDM", "https://evil.example/UIDM");
        assert_eq!(parse_release(elsewhere.as_bytes()).unwrap().setup, None, "only from the repository's releases");
    }
}
