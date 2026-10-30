//! Exporting the download list to a file and importing it again, to move it
//! to another computer or keep a backup.
//!
//! Three formats:
//!
//! - text: one URL per line;
//! - JSON: `{"uidm_list": 1, "items": [...]}`, each item with its URL, file
//!   name, folder, size, status, referrer and description;
//! - IDM export files (`.ef2`), for moving between IDM and UIDM: each
//!   download between a `<` line and a `>` line, its URL first, then
//!   `referer:`, `cookie:`, `pd:` (a form's POST body) and `User-Agent:`
//!   lines where IDM had them.
//!
//! Exports never hold cookies, request headers or passwords: they are built
//! from the list entries, which have none. An imported IDM file can bring
//! its cookies, user agent, form body and a login with its downloads.

use std::path::Path;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::batch;
use crate::protocol::{Download, DownloadEntry, DownloadLogin, Status};

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

/// `entries` as an IDM export file, which IDM's Tasks > Import > From IDM
/// export file reads: each URL and its referrer.
pub fn export_ef2(entries: &[&DownloadEntry]) -> String {
    let mut out = String::new();
    for e in entries {
        out.push_str("<\r\n");
        out.push_str(&e.url);
        out.push_str("\r\n");
        if let Some(r) = &e.referrer {
            out.push_str(&format!("referer: {r}\r\n"));
        }
        out.push_str(">\r\n");
    }
    out
}

/// Whether `text` (with its byte order mark and leading blank lines gone)
/// is an IDM export file.
fn is_ef2(text: &str) -> bool {
    text.lines().next().is_some_and(|l| l.trim() == "<")
}

/// The downloads in an IDM export file. Besides the lines IDM writes, it
/// takes `username:` and `password:`, which some files have. Unknown lines
/// are skipped.
fn parse_ef2(text: &str) -> Vec<Download> {
    let mut items = Vec::new();
    let mut current: Option<Download> = None;
    for line in text.lines().map(str::trim) {
        match line {
            "<" => {
                current = Some(Download {
                    start: Some(false),
                    ..Download::default()
                });
            }
            ">" => items.extend(current.take().filter(|d| !d.url.is_empty())),
            "" => {}
            _ => {
                let Some(d) = current.as_mut() else {
                    continue;
                };
                if d.url.is_empty() {
                    d.url = line.to_string();
                    continue;
                }
                let Some((key, value)) = line.split_once(':') else {
                    continue;
                };
                let value = value.trim();
                if value.is_empty() {
                    continue;
                }
                match key.trim().to_ascii_lowercase().as_str() {
                    "referer" => d.referrer = Some(value.to_string()).filter(|r| batch::is_web_url(r)),
                    "cookie" => d.cookies = Some(value.to_string()),
                    "user-agent" => d.user_agent = Some(value.to_string()),
                    "pd" => {
                        d.method = Some("POST".into());
                        d.body = Some(base64::engine::general_purpose::STANDARD.encode(value));
                        d.headers
                            .insert("Content-Type".into(), "application/x-www-form-urlencoded".into());
                    }
                    "username" => {
                        d.login.get_or_insert_with(DownloadLogin::default).username = value.to_string();
                    }
                    "password" => {
                        d.login.get_or_insert_with(DownloadLogin::default).password = Some(value.to_string());
                    }
                    _ => {}
                }
            }
        }
    }
    // A password alone is no login.
    for d in &mut items {
        if d.login.as_ref().is_some_and(|l| l.username.is_empty()) {
            d.login = None;
        }
    }
    items
}

/// The text of a list file read as `bytes`: UTF-16 when it starts with
/// UTF-16's byte order mark, or when it has no mark but its first
/// character is ASCII written as UTF-16 little-endian (Windows' "Unicode",
/// which IDM files can be saved in); else UTF-8, any invalid bytes
/// replaced.
pub fn decode(bytes: &[u8]) -> String {
    let utf16 = |body: &[u8], big_endian: bool| {
        let units: Vec<u16> = body
            .chunks_exact(2)
            .map(|c| if big_endian { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xFF, 0xFE, body @ ..] => utf16(body, false),
        [0xFE, 0xFF, body @ ..] => utf16(body, true),
        [first, 0, ..] if first.is_ascii() && *first != 0 && bytes.len().is_multiple_of(2) => utf16(bytes, false),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// The downloads in an exported file, in any of the formats, each set to
/// wait rather than start. Items without a web address, and repeats, are
/// left out; so is a folder that is not a full path. Also returns how many
/// items were left out for not being web addresses.
pub fn import(text: &str) -> Result<(Vec<Download>, usize), String> {
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if is_ef2(trimmed) {
        return keep_valid(parse_ef2(trimmed));
    }
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
    keep_valid(
        list.items
            .into_iter()
            .map(|item| Download {
                url: item.url,
                filename: item.filename.filter(|n| !n.trim().is_empty()),
                folder: item.folder.filter(|f| Path::new(f).is_absolute()),
                referrer: item.referrer.filter(|r| batch::is_web_url(r)),
                description: item.description.filter(|d| !d.trim().is_empty()),
                start: Some(false),
                ..Download::default()
            })
            .collect(),
    )
}

/// `candidates` without repeats and without those whose URL is not a web
/// address, and how many of those there were. Too many is an error.
fn keep_valid(candidates: Vec<Download>) -> Result<(Vec<Download>, usize), String> {
    let mut seen = std::collections::HashSet::new();
    let mut items = Vec::new();
    let mut rejected = 0;
    for mut d in candidates {
        d.url = d.url.trim().to_string();
        if !batch::is_download_url(&d.url) || d.url.contains(char::is_whitespace) {
            rejected += 1;
            continue;
        }
        if seen.insert(d.url.clone()) {
            items.push(d);
        }
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
            category: None,
            login_user: None,
            speed_limit: None,
            partial: None,
            from_browser: false,
            on_done: None,
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

    /// An export file as IDM 6.43 writes one, with a login some files carry.
    const EF2: &str = "\u{feff}<\r\nhttps://example.com/files/a.zip\r\nreferer: https://example.com/files/\r\n\
        cookie: session=abc; theme=dark\r\nUser-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64)\r\n>\r\n\
        <\r\nhttps://example.com/form/report.pdf\r\npd: id=7&kind=full\r\nusername: me\r\npassword: secret\r\n>\r\n\
        <\r\njavascript:void(0)\r\n>\r\n\
        <\r\nhttps://example.com/files/a.zip\r\n>\r\n";

    #[test]
    fn idm_files_bring_their_request() {
        let (items, rejected) = import(EF2).unwrap();
        assert_eq!(rejected, 1, "not a web address");
        assert_eq!(items.len(), 2, "the repeat is left out");
        let a = &items[0];
        assert_eq!(a.url, "https://example.com/files/a.zip");
        assert_eq!(a.referrer.as_deref(), Some("https://example.com/files/"));
        assert_eq!(a.cookies.as_deref(), Some("session=abc; theme=dark"));
        assert_eq!(a.user_agent.as_deref(), Some("Mozilla/5.0 (Windows NT 10.0; Win64; x64)"));
        assert_eq!((a.method.as_deref(), a.login.as_ref()), (None, None));
        assert_eq!(a.start, Some(false), "nothing starts on import");

        let form = &items[1];
        assert_eq!(form.method.as_deref(), Some("POST"));
        let body = base64::engine::general_purpose::STANDARD.decode(form.body.as_deref().unwrap()).unwrap();
        assert_eq!(body, b"id=7&kind=full");
        assert_eq!(form.headers.get("Content-Type").map(String::as_str), Some("application/x-www-form-urlencoded"));
        let login = form.login.as_ref().unwrap();
        assert_eq!((login.username.as_str(), login.password.as_deref()), ("me", Some("secret")));
    }

    #[test]
    fn idm_files_saved_as_utf16() {
        let le = |text: &str| text.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>();
        let be = |text: &str| text.encode_utf16().flat_map(u16::to_be_bytes).collect::<Vec<u8>>();
        let plain = EF2.trim_start_matches('\u{feff}');
        // EF2 starts with a byte order mark, which encodes as FF FE / FE FF.
        for bytes in [le(EF2), be(EF2), le(plain)] {
            let text = decode(&bytes);
            assert_eq!(text.trim_start_matches('\u{feff}'), plain);
            assert_eq!(import(&text).unwrap().0.len(), 2);
        }
        assert_eq!(decode(EF2.as_bytes()), EF2, "UTF-8 stays as it is");
        assert_eq!(decode(b"https://x/a.zip\n"), "https://x/a.zip\n");
    }

    #[test]
    fn ef2_round_trip() {
        let mut a = entry(1, "https://x/a.zip", Some(r"C:\dl\a.zip"), Status::Done);
        a.referrer = Some("https://x/downloads".into());
        a.login_user = Some("me".into());
        let b = entry(2, "https://x/b.iso", None, Status::Failed);
        let text = export_ef2(&[&a, &b]);
        assert_eq!(
            text,
            "<\r\nhttps://x/a.zip\r\nreferer: https://x/downloads\r\n>\r\n<\r\nhttps://x/b.iso\r\n>\r\n"
        );
        let (items, _) = import(&text).unwrap();
        let urls: Vec<&str> = items.iter().map(|d| d.url.as_str()).collect();
        assert_eq!(urls, ["https://x/a.zip", "https://x/b.iso"]);
        assert_eq!(items[0].referrer.as_deref(), Some("https://x/downloads"));
        assert!(items.iter().all(|d| d.login.is_none() && d.cookies.is_none()), "no secrets leave");
    }
}
