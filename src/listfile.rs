//! Exporting the download list to a file and importing it again, to move it
//! to another computer or keep a backup.
//!
//! Two formats:
//!
//! - text: one URL per line;
//! - JSON: `{"uidm_list": 1, "items": [...]}`, each item with its URL, file
//!   name, folder, size, status, referrer and description.
//!
//! Neither ever holds cookies, request headers or passwords: they are built
//! from the list entries, which have none.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::batch;
use crate::protocol::{Download, DownloadEntry, Status};

/// The version written in a JSON export.
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListFile {
    pub uidm_list: u32,
    pub items: Vec<ListItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListItem {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// The folder the file was or would be saved in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    /// The page the download came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referrer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl ListItem {
    pub fn from_entry(e: &DownloadEntry) -> Self {
        let path = e.path.as_deref().map(Path::new);
        ListItem {
            url: e.url.clone(),
            filename: path
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned()),
            folder: path
                .and_then(|p| p.parent())
                .map(|p| p.display().to_string())
                .filter(|p| !p.is_empty()),
            size: e.total,
            status: Some(e.status),
            referrer: e.referrer.clone(),
            description: e.description.clone(),
        }
    }
}

/// The URLs of `entries`, one per line.
pub fn export_text(entries: &[&DownloadEntry]) -> String {
    entries.iter().map(|e| format!("{}\r\n", e.url)).collect()
}

/// `entries` as a JSON list.
pub fn export_json(entries: &[&DownloadEntry]) -> String {
    let list = ListFile {
        uidm_list: FORMAT_VERSION,
        items: entries.iter().map(|e| ListItem::from_entry(e)).collect(),
    };
    serde_json::to_string_pretty(&list).unwrap_or_default()
}

/// The downloads in an exported file, in either format, each set to wait
/// rather than start. Items without a web address, and repeats, are left out;
/// so is a folder that is not a full path. Also returns how many items were
/// left out for not being web addresses.
pub fn import(text: &str) -> Result<(Vec<Download>, usize), String> {
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if !trimmed.starts_with('{') {
        let (urls, rejected) = batch::parse_list(text)?;
        let items = urls
            .into_iter()
            .map(|url| Download {
                url,
                start: Some(false),
                ..Download::default()
            })
            .collect();
        return Ok((items, rejected));
    }
    let list: ListFile =
        serde_json::from_str(trimmed).map_err(|e| format!("This is not a UIDM list: {e}"))?;
    if list.uidm_list > FORMAT_VERSION {
        return Err("This list was written by a newer UIDM.".into());
    }
    let mut seen = std::collections::HashSet::new();
    let mut items = Vec::new();
    let mut rejected = 0;
    for item in list.items {
        let url = item.url.trim().to_string();
        if !batch::is_download_url(&url) || url.contains(char::is_whitespace) {
            rejected += 1;
            continue;
        }
        if !seen.insert(url.clone()) {
            continue;
        }
        items.push(Download {
            url,
            filename: item.filename.filter(|n| !n.trim().is_empty()),
            folder: item.folder.filter(|f| Path::new(f).is_absolute()),
            referrer: item.referrer.filter(|r| batch::is_web_url(r)),
            description: item.description.filter(|d| !d.trim().is_empty()),
            start: Some(false),
            ..Download::default()
        });
    }
    if items.len() > batch::MAX_BATCH {
        return Err(format!(
            "The list has {} downloads; at most {} can be added at once.",
            items.len(),
            batch::MAX_BATCH
        ));
    }
    Ok((items, rejected))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u64, url: &str, path: Option<&str>, status: Status) -> DownloadEntry {
        DownloadEntry {
            id,
            url: url.into(),
            path: path.map(str::to_string),
            total: Some(1234),
            done: 0,
            status,
            error: None,
            resumable: true,
            connections: 4,
            scan: None,
            parts: None,
            note: None,
            duplicate_of: None,
            referrer: None,
            last_try: None,
            description: None,
            login_user: None,
            speed_limit: None,
            partial: None,
            from_browser: false,
        }
    }

    #[test]
    fn json_round_trip() {
        let a = entry(1, "https://x/a.zip", Some(r"C:\dl\a.zip"), Status::Done);
        let b = entry(2, "https://x/b.iso?sig=1", None, Status::Failed);
        let json = export_json(&[&a, &b]);
        assert!(json.contains("\"uidm_list\": 1"));
        let (items, rejected) = import(&json).unwrap();
        assert_eq!(rejected, 0);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].url, "https://x/a.zip");
        assert_eq!(items[0].filename.as_deref(), Some("a.zip"));
        assert_eq!(items[0].folder.as_deref(), Some(r"C:\dl"));
        assert_eq!(items[0].start, Some(false), "nothing starts on import");
        assert_eq!(items[1].filename, None);
        assert_eq!(items[1].start, Some(false));
    }

    #[test]
    fn descriptions_and_referrers_survive_a_round_trip() {
        let mut a = entry(1, "https://x/a.zip", None, Status::Done);
        a.description = Some("The March build".into());
        a.referrer = Some("https://x/downloads".into());
        a.login_user = Some("me".into());
        let json = export_json(&[&a]);
        assert!(!json.contains("\"me\""), "no login in exports");
        let (items, _) = import(&json).unwrap();
        assert_eq!(items[0].description.as_deref(), Some("The March build"));
        assert_eq!(items[0].referrer.as_deref(), Some("https://x/downloads"));
        assert!(items[0].login.is_none());
    }

    #[test]
    fn text_round_trip() {
        let a = entry(1, "https://x/a.zip", None, Status::Waiting);
        let b = entry(2, "https://x/b.zip", None, Status::Canceled);
        let text = export_text(&[&a, &b]);
        assert_eq!(text, "https://x/a.zip\r\nhttps://x/b.zip\r\n");
        let (items, _) = import(&text).unwrap();
        let urls: Vec<&str> = items.iter().map(|d| d.url.as_str()).collect();
        assert_eq!(urls, ["https://x/a.zip", "https://x/b.zip"]);
        assert!(items.iter().all(|d| d.start == Some(false)));
    }

    #[test]
    fn exports_never_hold_secrets() {
        // A list entry carries no cookies or headers to leak; the export has
        // exactly the fields of a list item.
        let a = entry(1, "https://x/a.zip", Some(r"C:\dl\a.zip"), Status::Done);
        let value: serde_json::Value = serde_json::from_str(&export_json(&[&a])).unwrap();
        let mut keys: Vec<&str> = value["items"][0].as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["filename", "folder", "size", "status", "url"]);
    }

    #[test]
    fn imports_check_what_they_take() {
        let json = r#"{"uidm_list":1,"items":[
            {"url":"https://x/a.zip","folder":"relative\\dir","cookies":"secret"},
            {"url":"javascript:alert(1)"},
            {"url":"https://x/a.zip"},
            {"url":"https://x/b.zip","filename":"  "}
        ]}"#;
        let (items, rejected) = import(json).unwrap();
        assert_eq!(rejected, 1);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].folder, None, "only full paths are kept");
        assert_eq!(items[0].cookies, None);
        assert_eq!(items[1].filename, None);
        assert!(import("{not json").is_err());
        assert!(import(r#"{"uidm_list":9,"items":[]}"#).is_err());
    }
}
