//! Glue shared by the host and the daemon: turning protocol messages into
//! engine requests and engine events into protocol messages.

use std::path::PathBuf;

use crate::engine::{DownloadRequest, Progress};
use crate::protocol::Outgoing;
use udm_domain::download::Download;
use udm_domain::settings::Settings;

/// Largest frame accepted on any channel. Chrome caps host messages at 1 MiB;
/// a little headroom lets an oversized frame fail cleanly.
pub const MAX_MESSAGE_BYTES: u32 = 4 * 1024 * 1024;

/// Build an engine request from an `add` message: take the folder, connection
/// count and file-name policy from the settings, and fold the browser context
/// (user agent, cookies, referrer) into headers.
pub fn to_request(d: Download, settings: &Settings) -> DownloadRequest {
    let post_body = post_body(&d);
    let connections = d.connections_or(settings.connections);
    let mut headers: Vec<(String, String)> = d.headers.into_iter().collect();
    if let Some(ua) = d.user_agent {
        headers.push(("User-Agent".to_string(), ua));
    }
    // A download from the browser carries the browser's User-Agent; the
    // others get the one set in Options, if any.
    let manual = settings.manual_user_agent.trim();
    if !manual.is_empty() && !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("user-agent")) {
        headers.push(("User-Agent".to_string(), manual.to_string()));
    }
    if let Some(cookies) = d.cookies {
        headers.push(("Cookie".to_string(), cookies));
    }
    if let Some(referrer) = d.referrer {
        headers.push(("Referer".to_string(), referrer));
    }
    let extra = browser_request_headers(&d.request_headers, &headers);
    headers.extend(extra);
    let folders = settings.folders_for(d.url.as_str(), &crate::settings::default_downloads_dir());
    let host = url::Url::parse(d.url.as_str()).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase));
    let host_limit = host.and_then(|h| settings.server_limit(&h));
    DownloadRequest {
        url: d.url.into_string(),
        folders,
        filename: d.filename,
        headers,
        connections,
        reuse_path: None,
        file_exists: d.file_exists.unwrap_or(settings.file_exists),
        post_body,
        net: Default::default(),
        media: d.media,
        ffmpeg: crate::settings::ffmpeg(settings),
        speed_limit: d.speed_limit,
        server_date: settings.server_file_date,
        server_date_created: settings.server_file_date_created,
        ignore_date: settings.resume_ignores_date,
        temp_dir: settings.temp_dir.as_deref().map(PathBuf::from).filter(|d| d.is_absolute()),
        reuse_partial: None,
        host_limit,
        elevate: d.elevate,
        resolve: None,
        torrent_files: d.torrent_files,
        torrent_uploaded: udm_domain::bytes::ByteCount::ZERO,
    }
}

/// Headers the browser sent that a download never repeats: the ones about
/// the connection (hop-by-hop), the ones the engine sets itself or that
/// would change what the server answers with (a range, a conditional
/// request, a compressed body the engine could not resume), and the ones
/// that travel in their own fields (cookies, referrer, user agent).
const UNREPEATED_HEADERS: &[&str] = &[
    "accept-encoding",
    "authorization",
    "connection",
    "content-length",
    "content-type",
    "cookie",
    "expect",
    "host",
    "if-match",
    "if-modified-since",
    "if-none-match",
    "if-range",
    "if-unmodified-since",
    "keep-alive",
    "proxy-authorization",
    "proxy-connection",
    "range",
    "referer",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "user-agent",
];

/// The headers of the browser's request (`request_headers`) a download
/// sends along with `already`, the ones it has: the browser's order, none
/// listed in `UNREPEATED_HEADERS`, none `already` names, no pseudo-header
/// (`:authority`) and each name once.
pub fn browser_request_headers(sent: &[(String, String)], already: &[(String, String)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (name, value) in sent {
        let lower = name.to_ascii_lowercase();
        let taken = |list: &[(String, String)]| list.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
        if name.is_empty()
            || name.starts_with(':')
            || UNREPEATED_HEADERS.contains(&lower.as_str())
            || taken(already)
            || taken(&out)
        {
            continue;
        }
        out.push((name.clone(), value.clone()));
    }
    out
}

/// The decoded form body when `d` is a POST. A POST without a body sends an
/// empty one.
pub fn post_body(d: &Download) -> Option<Vec<u8>> {
    use base64::Engine as _;
    if !d.method.as_deref().is_some_and(|m| m.eq_ignore_ascii_case("POST")) {
        return None;
    }
    let body = d.body.as_deref().unwrap_or("");
    Some(base64::engine::general_purpose::STANDARD.decode(body).unwrap_or_default())
}

/// Map an engine event to the message sent to clients.
pub fn progress_to_outgoing(p: Progress) -> Outgoing {
    match p {
        Progress::Started { id, url, path, total, resumable, connections, partial, validator, country } => {
            Outgoing::Started {
                id,
                url: url.into(),
                path: path.display().to_string(),
                total,
                resumable,
                connections,
                partial: partial.map(|p| p.display().to_string()),
                validator,
                country,
            }
        }
        Progress::Advanced { id, url, done, total, parts, segments } => {
            Outgoing::Progress { id, url: url.into(), done, total, parts, segments }
        }
        Progress::Finished { id, url, path, note } => {
            Outgoing::Done { id, url: url.into(), path: path.display().to_string(), note }
        }
        Progress::Failed { id, url, error } => Outgoing::Failed { id, url: url.into(), error },
        Progress::Canceled { id, url } => Outgoing::Canceled { id, url: url.into() },
        Progress::NameTaken { id, url, path, total } => {
            Outgoing::NameTaken { id, url: url.into(), path: path.display().to_string(), total }
        }
        Progress::TorrentInfo { id, url, info } => Outgoing::TorrentInfo { id, url: url.into(), info },
        Progress::TorrentStats { id, url, stats } => Outgoing::TorrentStats { id, url: url.into(), stats },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_safe_browser_headers_go_along() {
        let h = |list: &[(&str, &str)]| list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
        let sent = h(&[
            ("Accept", "text/html,*/*"),
            ("Accept-Language", "de-DE"),
            ("Accept-Encoding", "gzip, br"),
            ("Cookie", "a=1"),
            ("Range", "bytes=0-"),
            ("If-None-Match", "\"x\""),
            ("Connection", "keep-alive"),
            ("Host", "x"),
            ("User-Agent", "B/1"),
            (":authority", "x"),
            ("Sec-Fetch-Mode", "navigate"),
            ("Content-Type", "text/plain"),
            ("accept-language", "fr"),
            ("X-Requested-With", "t"),
        ]);
        let already = h(&[("X-Requested-With", "mine")]);
        assert_eq!(
            browser_request_headers(&sent, &already),
            h(&[("Accept", "text/html,*/*"), ("Accept-Language", "de-DE"), ("Sec-Fetch-Mode", "navigate")])
        );

        // In the engine request, after the browser context it carries anyway.
        let d = Download {
            url: "https://x/a".into(),
            cookies: Some("a=1".into()),
            request_headers: sent,
            ..Download::default()
        };
        let headers = to_request(d, &Settings::default()).headers;
        assert_eq!(headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case("cookie")).count(), 1);
        assert!(headers.iter().any(|(k, v)| k == "Accept-Language" && v == "de-DE"));
        assert!(!headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("range")));
    }

    #[test]
    fn only_downloads_without_a_browser_agent_get_the_manual_one() {
        let settings = Settings { manual_user_agent: "Manual/1.0".into(), ..Settings::default() };
        let ua = |d: Download| {
            to_request(d, &settings)
                .headers
                .into_iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
                .map(|(_, v)| v)
                .collect::<Vec<_>>()
        };
        let by_hand = Download { url: "https://x/a".into(), ..Download::default() };
        assert_eq!(ua(by_hand.clone()), ["Manual/1.0"]);
        let browser = Download { user_agent: Some("Browser/2.0".into()), ..by_hand.clone() };
        assert_eq!(ua(browser), ["Browser/2.0"]);
        let none = to_request(by_hand, &Settings::default());
        assert!(!none.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("user-agent")));
    }

    #[test]
    fn a_download_of_its_own_connections_opens_that_many() {
        let settings = Settings { connections: 6, ..Settings::default() };
        let d = Download { url: "https://x/a".into(), ..Download::default() };
        assert_eq!(to_request(d.clone(), &settings).connections, 6);
        let own = Download { connections: Some(2), ..d };
        assert_eq!(to_request(own, &settings).connections, 2);
    }
}
