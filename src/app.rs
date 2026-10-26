//! Glue shared by the host and the daemon: turning protocol messages into
//! engine requests and engine events into protocol messages.

use crate::engine::{DownloadRequest, Progress};
use crate::protocol::{Download, Outgoing};
use crate::settings::Settings;

/// Largest frame accepted on any channel. Chrome caps host messages at 1 MiB;
/// a little headroom lets an oversized frame fail cleanly.
pub const MAX_MESSAGE_BYTES: u32 = 4 * 1024 * 1024;

/// Build an engine request from an `add` message: take the folder, connection
/// count and file-name policy from the settings, and fold the browser context
/// (user agent, cookies, referrer) into headers.
pub fn to_request(d: Download, settings: &Settings) -> DownloadRequest {
    let post_body = post_body(&d);
    let mut headers: Vec<(String, String)> = d.headers.into_iter().collect();
    if let Some(ua) = d.user_agent {
        headers.push(("User-Agent".to_string(), ua));
    }
    if let Some(cookies) = d.cookies {
        headers.push(("Cookie".to_string(), cookies));
    }
    if let Some(referrer) = d.referrer {
        headers.push(("Referer".to_string(), referrer));
    }
    DownloadRequest {
        url: d.url,
        folders: settings.folders(),
        filename: d.filename,
        headers,
        connections: settings.connections,
        reuse_path: None,
        file_exists: settings.file_exists,
        post_body,
        net: Default::default(),
    }
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
        Progress::Started {
            id,
            url,
            path,
            total,
            resumable,
            connections,
        } => Outgoing::Started {
            id,
            url,
            path: path.display().to_string(),
            total,
            resumable,
            connections,
        },
        Progress::Advanced { id, url, done, total } => Outgoing::Progress { id, url, done, total },
        Progress::Finished { id, url, path } => Outgoing::Done {
            id,
            url,
            path: path.display().to_string(),
        },
        Progress::Failed { id, url, error } => Outgoing::Failed { id, url, error },
        Progress::Canceled { id, url } => Outgoing::Canceled { id, url },
    }
}
