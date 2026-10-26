//! Chrome native-messaging framing and message types.
//!
//! Framing: a 4-byte little-endian length, then that many bytes of UTF-8 JSON.
//! This is the browser's standard and is transport-agnostic, so your own
//! extension and host are free to use whatever JSON shapes you like. The types
//! below are UIDM's own contract, not a copy of any other product's protocol.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

use crate::queue::{Queue, WhenDone};
use crate::settings::Settings;

/// A request from a client (the browser via the host, or the desktop window)
/// to the daemon.
///
/// `op` selects the variant, e.g. `{"op":"add","url":"https://..."}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Incoming {
    /// Handshake a client may send on connect.
    Hello {
        #[serde(default)]
        version: Option<String>,
        /// What kind of client this is. The desktop window says `window`, so
        /// the daemon knows whether one is open to show a save-as dialog.
        #[serde(default)]
        client: Option<String>,
    },
    /// Liveness check.
    Ping,
    /// A download to hand off to the engine.
    Add(Download),
    /// Cancel a queued or running download.
    Cancel { id: u64 },
    /// Start a failed or canceled download again; it resumes from its partial
    /// file. The old entry is removed and the retry gets a new id.
    Retry { id: u64 },
    /// Forget a finished, failed or canceled download. Files are left alone.
    Remove { id: u64 },
    /// Ask for every download the daemon knows about.
    List,
    /// Stop every queued or running download, keeping its partial file.
    PauseAll,
    /// Start again what `pause_all` stopped, and any interrupted download.
    ResumeAll,
    /// Answer the save-as dialog for a `confirming` download, or start a
    /// `waiting` one. `filename` and `folder` replace the proposed ones when
    /// given. With `start` false the download waits until started.
    Confirm {
        id: u64,
        #[serde(default)]
        filename: Option<String>,
        #[serde(default)]
        folder: Option<String>,
        #[serde(default = "yes")]
        start: bool,
        /// With `start` false, the queue it waits in (the main queue when
        /// missing).
        #[serde(default)]
        queue: Option<u64>,
    },
    /// Create a queue (`id` 0) or change one: name, downloads at once,
    /// schedule, what happens when done, retries.
    SaveQueue { queue: Queue },
    /// Delete a queue; its waiting downloads move to the main queue.
    DeleteQueue { id: u64 },
    StartQueue { id: u64 },
    /// Stop a queue; its running downloads go back to waiting in it, with
    /// their progress kept.
    StopQueue { id: u64 },
    /// What happens when this queue's current run ends, this time only.
    SetQueueOnce {
        id: u64,
        #[serde(default)]
        when_done: Option<WhenDone>,
    },
    /// Put a download in a queue (or move it within one) at `position`, or at
    /// the end. A download that is not running waits there.
    MoveToQueue {
        id: u64,
        queue: u64,
        #[serde(default)]
        position: Option<usize>,
    },
    /// Stop the countdown before sleeping or shutting down.
    CancelCountdown,
    /// Give a failed, canceled, interrupted or waiting download a new link.
    /// With `download`, use it now: the partial file is kept if the new link
    /// is for the same file, otherwise the reply is `refresh_mismatch`, unless
    /// `restart` says to start over anyway. Without `download`, wait for the
    /// next download the browser hands over (`refresh_waiting`).
    RefreshLink {
        id: u64,
        #[serde(default)]
        download: Option<Download>,
        #[serde(default)]
        restart: bool,
    },
    /// Stop waiting for a new link from the browser.
    CancelRefresh,
    /// Ask for the current settings; answered with `settings`.
    GetSettings,
    /// Replace the settings. Invalid values are refused with `error`; valid
    /// ones are saved and sent to every client as `settings`.
    SetSettings { settings: Settings },
}

fn yes() -> bool {
    true
}

/// The payload a browser hand-off needs to reissue a request outside the page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Download {
    pub url: String,
    /// `POST` for a download a form started; anything else is a GET.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// The form body of a POST, base64-encoded. Its content type goes in
    /// `headers`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Request headers as name -> value. A map keeps lookup simple; switch to a
    /// `Vec<(String, String)>` if you need to preserve duplicate header names.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookies: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referrer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// Suggested filename, when the client could derive one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// `false` adds the download to a queue without starting it ("Download
    /// later"). Missing means start it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<bool>,
    /// The proxy the browser uses, such as `http://proxy:3128`, for the
    /// "use the browser's proxy" option.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    /// The queue a download added with `start: false` waits in; the main
    /// queue when missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<u64>,
}

/// Where a download is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Queued,
    Downloading,
    Done,
    Failed,
    Canceled,
    /// Was queued or running when the daemon last stopped. It can be retried,
    /// and resumes from its partial file.
    Interrupted,
    /// Probed and waiting for the user to answer the save-as dialog. `path`
    /// holds the proposed file.
    Confirming,
    /// Added to start later ("Download later"). `path` holds the chosen file.
    Waiting,
}

impl Status {
    /// Finished one way or another; nothing is running for it.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Status::Done
                | Status::Failed
                | Status::Canceled
                | Status::Interrupted
                | Status::Waiting
        )
    }

    /// Stopped before finishing, so it can be started again.
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            Status::Failed | Status::Canceled | Status::Interrupted | Status::Waiting
        )
    }
}

/// The virus scan of a finished download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Scan {
    Scanning,
    Clean,
    /// The scanner reported a threat; `detail` is the end of its output.
    Threat { detail: String },
    /// The scanner could not run or did not finish.
    Failed { detail: String },
}

/// One download as the daemon tracks it, sent in a `downloads` snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DownloadEntry {
    pub id: u64,
    pub url: String,
    /// Output file, once the engine has chosen it.
    pub path: Option<String>,
    pub total: Option<u64>,
    pub done: u64,
    pub status: Status,
    pub error: Option<String>,
    pub resumable: bool,
    pub connections: usize,
    /// The virus scan of the finished file, when scanning is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan: Option<Scan>,
}

/// A message from the daemon to a client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Outgoing {
    /// Reply to `hello`, announcing the daemon's identity.
    Hello { name: String, version: String },
    /// Reply to `ping`.
    Pong,
    /// Reply to `add` or `retry`, confirming the download was accepted. `id`
    /// identifies it in later events and requests.
    Ack { id: u64, url: String, status: String },
    /// A download was queued. Sent to every client.
    Added { id: u64, url: String },
    /// A download has begun; totals may be unknown for a non-ranged server.
    Started {
        id: u64,
        url: String,
        /// The file being written.
        path: String,
        total: Option<u64>,
        resumable: bool,
        connections: usize,
    },
    /// Periodic progress for a running download.
    Progress {
        id: u64,
        url: String,
        done: u64,
        total: Option<u64>,
    },
    /// A download finished; `path` is the saved file.
    Done { id: u64, url: String, path: String },
    /// A download failed partway.
    Failed { id: u64, url: String, error: String },
    /// A download was canceled; its partial file and resume sidecar are kept.
    Canceled { id: u64, url: String },
    /// A download was removed from the list (removed or retried).
    Removed { id: u64 },
    /// A download's whole entry, replacing what clients had: used for the
    /// changes no other event covers (`confirming`, `waiting`, and back to
    /// `queued` once confirmed).
    Updated { item: DownloadEntry },
    /// Reply to `list`: every download the daemon knows about, oldest first.
    Downloads { items: Vec<DownloadEntry> },
    /// The current settings: a reply to `get_settings`, and sent to every
    /// client when they change.
    Settings { settings: Settings },
    /// Ask the desktop window to come to the front (from a notification).
    /// Other clients ignore it.
    Show,
    /// Every queue, in order: a reply to `list`, and sent to every client when
    /// one changes.
    Queues { items: Vec<Queue> },
    /// A finished queue will put the computer to sleep, hibernate it or shut
    /// it down in `seconds` unless someone sends `cancel_countdown`.
    Countdown {
        action: WhenDone,
        seconds: u32,
        queue: String,
    },
    CountdownCanceled,
    /// A finished queue asked UIDM to exit: windows close.
    Quit,
    /// Which download waits for a new link from the browser, if any. Sent to
    /// every client when it changes, and after `downloads` in a `list` reply.
    RefreshWaiting { id: Option<u64> },
    /// Reply to `refresh_link`: the new link is for another file, so the
    /// partial file cannot be kept. Send `refresh_link` again with
    /// `restart: true` to start over.
    RefreshMismatch {
        id: u64,
        url: String,
        reason: String,
    },
    /// Any failure the client should hear about.
    Error { message: String },
}

/// Read one framed message from `reader`.
///
/// Returns `Ok(None)` on a clean EOF at a frame boundary (the browser closed
/// the port), `Ok(Some(bytes))` for a complete frame, and an error for a
/// partial frame or an oversized length.
pub fn read_message<R: Read>(reader: &mut R, max_bytes: u32) -> io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match read_exact_or_eof(reader, &mut len_buf)? {
        false => return Ok(None),
        true => {}
    }

    let len = u32::from_le_bytes(len_buf);
    if len > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("message length {len} exceeds limit {max_bytes}"),
        ));
    }

    let mut body = vec![0u8; len as usize];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

/// Serialize `message` to JSON and write it with the 4-byte length prefix.
pub fn write_message<W: Write>(writer: &mut W, message: &Outgoing) -> io::Result<()> {
    let json = serde_json::to_vec(message)?;
    write_frame(writer, &json)
}

/// Write an already-encoded payload with the 4-byte length prefix. The host
/// uses this to relay frames between browser and daemon without re-parsing.
pub fn write_frame<W: Write>(writer: &mut W, payload: &[u8]) -> io::Result<()> {
    let len: u32 = payload
        .len()
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame exceeds u32 length"))?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(payload)?;
    Ok(())
}

/// Fill `buf` fully. Returns `Ok(false)` if EOF happens before any byte is
/// read (a clean close), `Ok(true)` on success, and an error if EOF happens
/// part-way through (a truncated frame).
fn read_exact_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => {
                if filled == 0 {
                    return Ok(false);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream ended mid-frame",
                ));
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A frame written by `write_message` reads back with the same length.
    #[test]
    fn roundtrip_frame() {
        let mut out = Vec::new();
        write_message(&mut out, &Outgoing::Pong).unwrap();
        // 4-byte prefix must equal the JSON byte count.
        let len = u32::from_le_bytes(out[0..4].try_into().unwrap()) as usize;
        assert_eq!(len, out.len() - 4);

        let mut cur = Cursor::new(out);
        let body = read_message(&mut cur, 1024).unwrap().unwrap();
        assert_eq!(body, br#"{"op":"pong"}"#);
    }

    /// EOF at a frame boundary is a clean close, not an error.
    #[test]
    fn clean_eof() {
        let mut cur = Cursor::new(Vec::new());
        assert!(read_message(&mut cur, 1024).unwrap().is_none());
    }

    /// An oversized length prefix is rejected before allocating.
    #[test]
    fn rejects_oversized() {
        let mut data = 999_999u32.to_le_bytes().to_vec();
        data.push(0);
        let mut cur = Cursor::new(data);
        assert!(read_message(&mut cur, 1024).is_err());
    }

    /// An `add` message parses into the typed download.
    #[test]
    fn parse_add() {
        let raw = br#"{"op":"add","url":"https://example.com/f.zip","method":"GET"}"#;
        let msg: Incoming = serde_json::from_slice(raw).unwrap();
        match msg {
            Incoming::Add(d) => {
                assert_eq!(d.url, "https://example.com/f.zip");
                assert_eq!(d.method.as_deref(), Some("GET"));
            }
            _ => panic!("expected add"),
        }
    }
}
