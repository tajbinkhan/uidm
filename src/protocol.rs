//! Chrome native-messaging framing and message types.
//!
//! Framing: a 4-byte little-endian length, then that many bytes of UTF-8 JSON.
//! This is the browser's standard and is transport-agnostic, so your own
//! extension and host are free to use whatever JSON shapes you like. The types
//! below are UIDM's own contract, not a copy of any other product's protocol.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

use crate::grab::{GrabFile, GrabRequest};
use crate::media::{MediaChoice, MediaInfo};
use crate::queue::{Queue, WhenDone};
use crate::settings::{Duplicate, FileExists, Settings};

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
    /// Many downloads at once (a batch, an import, the site grabber's
    /// finds), each added to wait in `queue` (the main queue when missing)
    /// without starting. Answered with `added_many`.
    AddMany {
        items: Vec<Download>,
        #[serde(default)]
        queue: Option<u64>,
    },
    /// Read a stream's playlist or manifest and answer with `media_info`: its
    /// qualities, or why it cannot be downloaded. `request` is echoed back.
    ProbeMedia { request: u64, download: Download },
    /// Crawl a site for files; progress comes as `grab_progress` and the
    /// result as `grab_done`, to this client only. One crawl per client.
    Grab(GrabRequest),
    /// Stop this client's crawl; `grab_done` follows with what it found.
    CancelGrab,
    /// Cancel a queued or running download.
    Cancel { id: u64 },
    /// Start a failed or canceled download again; it resumes from its partial
    /// file. The old entry is removed and the retry gets a new id.
    Retry { id: u64 },
    /// Download a finished, failed or canceled download again from the
    /// start, keeping its id, link, request, name and folder. A file still
    /// there is handled by the file-exists setting.
    Redownload { id: u64 },
    /// Forget a finished, failed or canceled download. Its files stay unless
    /// `delete_files`, which sends the finished or partial file, its resume
    /// sidecar and a stream's segments to the Recycle Bin.
    Remove {
        id: u64,
        #[serde(default)]
        delete_files: bool,
    },
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
        /// What to do if the file name is taken (rename or overwrite); the
        /// setting decides when missing.
        #[serde(default)]
        file_exists: Option<FileExists>,
        /// For a download with `duplicate_of`: what to do about the other
        /// download with the same address. Missing (or `ask`) adds it with a
        /// new name.
        #[serde(default)]
        duplicate: Option<Duplicate>,
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
    /// Links found on a page (or in a selection on it) for the window to
    /// list, so the user can tick the ones to download. The window opens
    /// if it is not open; answered with nothing.
    PickLinks {
        page: String,
        links: Vec<PageLink>,
        #[serde(default)]
        user_agent: Option<String>,
    },
    /// Look for a newer UIDM release now; answered with `update`.
    CheckUpdate,
    /// List the files inside a waiting zip download, reading only the end
    /// of the archive; answered with `zip_preview`.
    PreviewZip { id: u64 },
    /// A page as the browser showed it, answering `render_page`: its HTML
    /// after its scripts ran and its address then, or nothing.
    Rendered {
        request: u64,
        #[serde(default)]
        html: Option<String>,
        #[serde(default)]
        url: Option<String>,
    },
    /// The browser's cookies for a URL, answering `cookies_wanted`.
    Cookies {
        request: u64,
        #[serde(default)]
        cookies: Option<String>,
    },
    /// Change a stopped or finished download's link, referrer, description,
    /// login, or (when finished) its file's name and folder.
    SetProperties { id: u64, changes: Properties },
    /// Set or clear one download's own speed limit, in bytes per second. A
    /// running download changes speed at once.
    SetSpeedLimit {
        id: u64,
        #[serde(default)]
        bytes_per_sec: Option<u64>,
    },
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
    /// The folder to save in, a full path, instead of the one its category
    /// picks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// A stream (HLS or DASH) to download as one video, and which quality.
    /// `url` is its playlist or manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<MediaChoice>,
    /// What to do when its file name is taken, chosen in the save-as dialog;
    /// the setting decides when missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_exists: Option<FileExists>,
    /// A note about the download, shown in the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// A login for this download alone. It is tried on the download's own
    /// site before any login saved in Options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<DownloadLogin>,
    /// This download's own speed limit in bytes per second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_limit: Option<u64>,
}

/// A link on a web page, offered in the window's link picker.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PageLink {
    pub url: String,
    /// The link's text, or an image's alt text.
    #[serde(default)]
    pub text: String,
    /// An image shown on the page rather than a link.
    #[serde(default)]
    pub image: bool,
    /// The browser's cookies for the link's site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookies: Option<String>,
}

/// A download's own login.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DownloadLogin {
    pub username: String,
    /// Missing in a `set_properties` change keeps the password it had.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

/// Leaves the password out of logs.
impl std::fmt::Debug for DownloadLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadLogin")
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .finish()
    }
}

/// Changes to a stopped or finished download's details. A missing field is
/// left as it is; an empty referrer or description clears it, and a login
/// with an empty user name removes the download's login.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Properties {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referrer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Not for a finished download, which keeps no request to log in with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<DownloadLogin>,
    /// A new name for a finished download's file, which is renamed on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// A new folder (a full path) for a finished download's file, which is
    /// moved there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
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

/// One connection's share of a segmented download, for showing each
/// connection's progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentInfo {
    /// First and last byte of the range.
    pub start: u64,
    pub end: u64,
    /// Bytes of the range fetched so far.
    pub done: u64,
    pub state: SegmentState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentState {
    /// No connection has it yet.
    Waiting,
    Connecting,
    Receiving,
    /// The last attempt failed; it tries again after a pause.
    Retrying,
    Done,
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
    /// For a stream: segments fetched and segments in all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<[u64; 2]>,
    /// Something to know about a finished download, such as a stream saved
    /// as two files because ffmpeg was not found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// While `confirming`: another download in the list has the same
    /// address, and the save-as dialog asks what to do about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<u64>,
    /// The page the download came from, for finding it in the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referrer: Option<String>,
    /// When it last started, in seconds since 1970.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_try: Option<u64>,
    /// The user's note about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The user name of the download's own login, if it has one. The
    /// password stays in the daemon's encrypted request store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_user: Option<String>,
    /// Its own speed limit in bytes per second, kept across stops, retries
    /// and restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_limit: Option<u64>,
    /// Where its unfinished data is, when not at `path`: a temporary folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial: Option<String>,
    /// The browser extension took it from the browser.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_browser: bool,
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
        /// Where the data goes until it is complete, when not at `path` (a
        /// temporary folder).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        partial: Option<String>,
    },
    /// Periodic progress for a running download.
    Progress {
        id: u64,
        url: String,
        done: u64,
        total: Option<u64>,
        /// For a stream: segments fetched and segments in all.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parts: Option<[u64; 2]>,
        /// For a download in byte ranges: each connection's range and state.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        segments: Option<Vec<SegmentInfo>>,
    },
    /// A download finished; `path` is the saved file.
    Done {
        id: u64,
        url: String,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
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
    /// Show the completion dialog of finished download `id` again: a new
    /// download had the same address. Other clients ignore it.
    ShowDone { id: u64 },
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
    /// A download's file name is taken and the setting says to ask: nothing
    /// was written, and it waits as `confirming` with `path`, the taken file,
    /// until a window answers with `confirm`.
    NameTaken {
        id: u64,
        url: String,
        path: String,
        total: Option<u64>,
    },
    /// Reply to `refresh_link`: the new link is for another file, so the
    /// partial file cannot be kept. Send `refresh_link` again with
    /// `restart: true` to start over.
    RefreshMismatch {
        id: u64,
        url: String,
        reason: String,
    },
    /// Reply to `add_many`: how many downloads were added, and how many were
    /// skipped because their address was already in the list.
    AddedMany {
        count: usize,
        #[serde(default)]
        skipped: usize,
    },
    /// Reply to `probe_media`.
    MediaInfo { request: u64, info: MediaInfo },
    /// Links from a page for the window's link picker, as `pick_links` sent
    /// them. Sent to windows only.
    PickLinks {
        page: String,
        links: Vec<PageLink>,
        #[serde(default)]
        user_agent: Option<String>,
    },
    /// Where the download quota stands: bytes used in the window and the
    /// limit. Sent when it is nearly reached, when downloads pause for it
    /// (`paused`, with when they resume, in seconds since 1970), when they
    /// resume, and to a window that connects while it matters.
    Quota {
        used: u64,
        limit: u64,
        paused: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resumes_at: Option<u64>,
    },
    /// Asks the browser extension for its cookies for `url` (for a site
    /// crawl that signs in with them); it answers with `cookies`.
    CookiesWanted { request: u64, url: String },
    /// What the update check found: a newer release, none (`latest` is
    /// this version or older), or why it could not look. Sent to the window
    /// that asked, and to every window when the weekly check finds one.
    Update {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        newer: Option<crate::update::Release>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// The files inside a zip download, answering `preview_zip`, or why
    /// they could not be listed.
    ZipPreview {
        id: u64,
        #[serde(default)]
        entries: Vec<crate::zip::ZipEntry>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Asks the browser extension to open `url` in a background tab, let
    /// its scripts run, and answer with `rendered`.
    RenderPage { request: u64, url: String },
    /// A crawl's progress: pages read, bytes read and files found so far.
    GrabProgress { pages: u64, bytes: u64, found: usize },
    /// A crawl ended: what it found, and why it stopped early, if it did.
    GrabDone {
        files: Vec<GrabFile>,
        pages: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stopped: Option<String>,
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

    /// The save-as dialog's answer to a taken name, and a `confirm` without
    /// one (the browser popup, older windows).
    #[test]
    fn parse_confirm_with_and_without_file_exists() {
        let raw = br#"{"op":"confirm","id":4,"file_exists":"overwrite"}"#;
        match serde_json::from_slice(raw).unwrap() {
            Incoming::Confirm { id, start, file_exists, .. } => {
                assert_eq!(id, 4);
                assert!(start, "start defaults to true");
                assert_eq!(file_exists, Some(FileExists::Overwrite));
            }
            _ => panic!("expected confirm"),
        }
        match serde_json::from_slice(br#"{"op":"confirm","id":4}"#).unwrap() {
            Incoming::Confirm { file_exists, duplicate, .. } => {
                assert_eq!(file_exists, None);
                assert_eq!(duplicate, None);
            }
            _ => panic!("expected confirm"),
        }
        match serde_json::from_slice(br#"{"op":"confirm","id":4,"duplicate":"existing"}"#).unwrap() {
            Incoming::Confirm { duplicate, .. } => assert_eq!(duplicate, Some(Duplicate::Existing)),
            _ => panic!("expected confirm"),
        }
        // A download without a choice keeps its JSON as it was.
        let d = Download {
            url: "https://x/a.zip".into(),
            ..Download::default()
        };
        assert!(!serde_json::to_string(&d).unwrap().contains("file_exists"));
    }
}
