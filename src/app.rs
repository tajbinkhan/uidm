//! Glue shared by the host and the daemon: turning protocol messages into
//! engine requests and engine events into protocol messages.

use std::path::PathBuf;

use crate::engine::{DownloadRequest, Progress};
use crate::protocol::{Download, Outgoing};

/// Parallel connections per download unless a request overrides it.
pub const DEFAULT_CONNECTIONS: usize = 8;
/// How many downloads run at once; the rest queue.
pub const MAX_CONCURRENT_DOWNLOADS: usize = 3;
/// Largest frame accepted on any channel. Chrome caps host messages at 1 MiB;
/// a little headroom lets an oversized frame fail cleanly.
pub const MAX_MESSAGE_BYTES: u32 = 4 * 1024 * 1024;

/// Build an engine request from an `add` message: pick the output directory and
/// fold the browser context (user agent, cookies, referrer) into headers.
pub fn to_request(d: Download) -> DownloadRequest {
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
        output_dir: downloads_dir(),
        filename: d.filename,
        headers,
        connections: DEFAULT_CONNECTIONS,
    }
}

/// Map an engine event to the message sent to clients.
pub fn progress_to_outgoing(p: Progress) -> Outgoing {
    match p {
        Progress::Started {
            id,
            url,
            total,
            resumable,
            connections,
        } => Outgoing::Started {
            id,
            url,
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

/// The current user's Downloads folder, or the temp dir if it can't be found.
pub fn downloads_dir() -> PathBuf {
    if let Ok(profile) = std::env::var("USERPROFILE") {
        let dir = PathBuf::from(profile).join("Downloads");
        if dir.is_dir() {
            return dir;
        }
    }
    std::env::temp_dir()
}
