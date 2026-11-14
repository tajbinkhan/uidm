//! A download as UDM keeps it: what to fetch and how (`Download`), its
//! place in the list (`DownloadEntry`, `Status`), and the save-as
//! dialog's answer (`Answer`). The daemon and its clients exchange these
//! inside the messages of `udm_infrastructure::protocol`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::address::{Address, Link};
use crate::bytes::ByteCount;
use crate::checksum::Checksum;
use crate::filename::FileName;
use crate::id::{DownloadId, QueueId};
use crate::media::MediaChoice;
use crate::queue::WhenDone;
use crate::settings::{Duplicate, FileExists, Settings, SPEED_LIMIT_RANGE_KBPS};
use serde::{Deserialize, Serialize};

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

/// The save-as dialog's answer for a `confirming` download, or the start of
/// a `waiting` one. `filename` and `folder` replace the proposed ones when
/// given; `filename` is what the user typed, cleaned into a `FileName`
/// when it is used. With `start` false the download waits until started.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    pub id: DownloadId,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default = "yes")]
    pub start: bool,
    /// With `start` false, the queue it waits in; missing uses the queue
    /// Download later is set to, else the main queue.
    #[serde(default)]
    pub queue: Option<QueueId>,
    /// What to do if the file name is taken (rename or overwrite); the
    /// setting decides when missing.
    #[serde(default)]
    pub file_exists: Option<FileExists>,
    /// For a download with `duplicate_of`: what to do about the other
    /// download with the same address. Missing (or `ask`) adds it with a
    /// new name.
    #[serde(default)]
    pub duplicate: Option<Duplicate>,
    /// The category chosen for it, which the list files it under whatever
    /// its file type; missing keeps the one its type gives.
    #[serde(default)]
    pub category: Option<String>,
    /// A note about it; missing keeps the one it has, empty clears it.
    #[serde(default)]
    pub description: Option<String>,
    /// "Remember this path": turn keeping the last chosen folder on (and
    /// keep this one) or off for its category; missing leaves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remember_folder: Option<bool>,
    /// "Remember my selection": make `duplicate` the setting for every
    /// address already in the list.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remember_duplicate: bool,
    /// "Don't ask me again": with `start` false, make `queue` the one
    /// "Download later" uses without asking.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remember_queue: bool,
    /// Save into the folder with administrator rights, because UDM may
    /// not write there itself.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub elevate: bool,
}

impl Answer {
    /// Start download `id` as proposed.
    pub fn start(id: DownloadId) -> Self {
        Answer {
            id,
            filename: None,
            folder: None,
            start: true,
            queue: None,
            file_exists: None,
            duplicate: None,
            category: None,
            description: None,
            remember_folder: None,
            remember_duplicate: false,
            remember_queue: false,
            elevate: false,
        }
    }
}

/// The payload a browser hand-off needs to reissue a request outside the page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Download {
    /// The address as the client sent it; the daemon refuses one that is
    /// not an `Address`.
    pub url: Link,
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
    /// The address sent as the HTTP `Referer` header: the document that
    /// held the link, which may be a frame inside the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referrer: Option<String>,
    /// The web page the download came from (the browser tab's address),
    /// kept apart from the referrer the way IDM keeps both. It is only
    /// shown and sorted by, never sent to the server. Missing means the
    /// referrer is the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// Suggested filename, when the client could derive one. Text as the
    /// client sent it, which may be a path or empty: it is cleaned into a
    /// `FileName` when the download is named (`filename::proposed_name`).
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
    pub queue: Option<QueueId>,
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
    /// What to do when its address is already in the list, answered when it
    /// was added; the setting decides when missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicate: Option<Duplicate>,
    /// A note about the download, shown in the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// A checksum the site published, compared with the file when it
    /// finishes; a mismatch fails the download.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<Checksum>,
    /// The category the list files it under, chosen in the save-as dialog;
    /// missing goes by its file type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// A login for this download alone. It is tried on the download's own
    /// site before any login saved in Options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<DownloadLogin>,
    /// This download's own speed limit in bytes per second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_limit: Option<u64>,
    /// How many connections it opens, instead of the setting's number
    /// (`connections_or`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connections: Option<usize>,
    /// Show the save-as dialog for it (`true`) or not, whatever the
    /// setting says; missing follows the setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<bool>,
    /// The other headers the browser sent with the request, in its order,
    /// so the server sees the same request (IDM sends them all). The daemon
    /// keeps only the ones that are safe to repeat; see
    /// `app::browser_request_headers`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_headers: Vec<(String, String)>,
    /// The addresses the browser was redirected through before `url`,
    /// first to last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redirects: Vec<String>,
    /// The size the browser's response gave (its `Content-Length`), shown
    /// until the daemon asks the server itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<ByteCount>,
    /// The `Content-Type` of the browser's response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// The browser's whole proxy setting, for the "use the browser's
    /// proxy" option. It wins over `proxy`, which only ever named a fixed
    /// proxy and is still sent for older daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_proxy: Option<BrowserProxy>,
    /// Save into a folder that needs administrator rights: the finished
    /// file is moved there after a UAC prompt.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub elevate: bool,
}

/// How the browser reaches the web, as its extension reads it
/// (`chrome.proxy.settings`, or Firefox's `browser.proxy.settings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserProxyMode {
    /// No proxy.
    #[default]
    Direct,
    /// The fixed proxies in `http`, `https` and `socks`.
    Fixed,
    /// A proxy auto-config script, from `pac_url` or given in `pac_script`.
    Pac,
    /// The script WPAD finds on the network.
    AutoDetect,
    /// Whatever the system's proxy setting says.
    System,
}

/// The browser's proxy setting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserProxy {
    #[serde(default)]
    pub mode: BrowserProxyMode,
    /// `scheme://host:port` of the proxy for `http://` addresses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<String>,
    /// `scheme://host:port` of the proxy for `https://` addresses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub https: Option<String>,
    /// `socks4://` or `socks5://host:port` for what has no proxy of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socks: Option<String>,
    /// Where the auto-config script is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pac_url: Option<String>,
    /// The auto-config script itself, when the browser was given its text
    /// instead of an address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pac_script: Option<String>,
    /// Hosts reached directly, as the browser lists them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bypass: Vec<String>,
}

/// The most of each thing a browser request may carry; what is past them
/// is dropped (see `Download::limit_browser_request`). A page decides much
/// of what the browser sends, so none of it is trusted to be small.
pub const MAX_REQUEST_HEADERS: usize = 64;
pub const MAX_HEADER_NAME: usize = 128;
pub const MAX_HEADER_VALUE: usize = 8 * 1024;
pub const MAX_REDIRECTS: usize = 20;
pub const MAX_URL: usize = 8 * 1024;
pub const MAX_PAC_SCRIPT: usize = 512 * 1024;
pub const MAX_BYPASS: usize = 256;

impl Download {
    /// Drop what is too much or malformed in the request details the
    /// browser sent: headers whose name is not a header name or whose value
    /// could start another header, past the limits above; redirects that
    /// are not web addresses; an overlong content type or proxy setting.
    pub fn limit_browser_request(&mut self) {
        let token = |n: &str| {
            !n.is_empty()
                && n.len() <= MAX_HEADER_NAME
                && n.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
        };
        let value = |v: &str| v.len() <= MAX_HEADER_VALUE && !v.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0);
        self.request_headers.retain(|(n, v)| token(n) && value(v));
        self.request_headers.truncate(MAX_REQUEST_HEADERS);
        self.redirects.retain(|u| u.len() <= MAX_URL && crate::batch::is_web_url(u));
        self.redirects.truncate(MAX_REDIRECTS);
        if self.content_type.as_deref().is_some_and(|t| !value(t) || t.len() > 256) {
            self.content_type = None;
        }
        if let Some(p) = &mut self.browser_proxy {
            for field in [&mut p.http, &mut p.https, &mut p.socks, &mut p.pac_url] {
                if field.as_deref().is_some_and(|a| a.len() > MAX_URL) {
                    *field = None;
                }
            }
            if p.pac_script.as_deref().is_some_and(|s| s.len() > MAX_PAC_SCRIPT) {
                p.pac_script = None;
            }
            p.bypass.retain(|b| !b.trim().is_empty() && b.len() <= 256);
            p.bypass.truncate(MAX_BYPASS);
        }
    }
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
    /// What the user typed, cleaned into a `FileName` when it is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// A new folder (a full path) for the download's file. A finished file
    /// is moved there; a stopped, unfinished download moves its partial
    /// file and resume sidecar there and saves there from then on.
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
        matches!(self, Status::Done | Status::Failed | Status::Canceled | Status::Interrupted | Status::Waiting)
    }

    /// Waiting for its save-as dialog or in a queue: the engine does not
    /// have it.
    pub fn waits(self) -> bool {
        matches!(self, Status::Confirming | Status::Waiting)
    }

    /// Running: queued in the engine or downloading.
    pub fn is_active(self) -> bool {
        matches!(self, Status::Queued | Status::Downloading)
    }

    /// Stopped before finishing, so it can be started again.
    pub fn is_retryable(self) -> bool {
        matches!(self, Status::Failed | Status::Canceled | Status::Interrupted | Status::Waiting)
    }
}

/// One connection's share of a segmented download, for showing each
/// connection's progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentInfo {
    /// First and last byte of the range.
    pub start: ByteCount,
    pub end: ByteCount,
    /// Bytes of the range fetched so far.
    pub done: ByteCount,
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
    Threat {
        detail: String,
    },
    /// The scanner could not run or did not finish.
    Failed {
        detail: String,
    },
}

/// One download as the daemon tracks it, sent in a `downloads` snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DownloadEntry {
    pub id: DownloadId,
    /// Its address. A row read from a file with text that is no address
    /// keeps it, and fails when started.
    pub url: Link,
    /// Output file, once the engine has chosen it.
    pub path: Option<String>,
    pub total: Option<ByteCount>,
    pub done: ByteCount,
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
    pub duplicate_of: Option<DownloadId>,
    /// The Referer its request sends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referrer: Option<String>,
    /// The web page it came from, for finding it in the list. Entries saved
    /// before it was kept apart from the referrer get the referrer here
    /// when the history is read (see `History::load`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    /// When it last started, in seconds since 1970.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_try: Option<u64>,
    /// The user's note about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The checksum it was added with, compared with the file when it
    /// finishes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<Checksum>,
    /// The category chosen for it in the save-as dialog; missing goes by
    /// its file type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// The user name of the download's own login, if it has one. The
    /// password stays in the daemon's encrypted request store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_user: Option<String>,
    /// Its own speed limit in bytes per second, kept across restarts, and
    /// across stops and retries while `limit_remember` is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_limit: Option<u64>,
    /// Keep `speed_limit` when the download is stopped and resumed (IDM's
    /// "Remember on stop and resume"). On unless the user unticks it, as in
    /// IDM; when off, resuming starts it without its own limit.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub limit_remember: bool,
    /// Where its unfinished data is, when not at `path`: a temporary folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial: Option<String>,
    /// The browser extension took it from the browser.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_browser: bool,
    /// What to do once it finishes, if anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_done: Option<OnDone>,
    /// What the server said about the file when it was last downloaded or
    /// checked, so a synchronizing queue can tell whether it changed since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validator: Option<Validator>,
    /// The country of the server its last start connected to, from the
    /// address table UDM ships; `None` when unknown, private or reached
    /// through a proxy.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::country::read_lenient")]
    pub country: Option<crate::country::CountryCode>,
}

/// What a server says identifies one version of a file: its size, ETag and
/// Last-Modified date, whichever it sends.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validator {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<ByteCount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

impl Validator {
    /// Whether the server's copy described by `now` differs from the one
    /// this describes. Only what both sides know is compared: a different
    /// size always means a change; an ETag on both sides then decides alone
    /// (servers can touch a date without changing the file); without one a
    /// different Last-Modified means a change. When nothing can be compared
    /// the file counts as unchanged, so a server that says nothing about its
    /// files is not downloaded again on every run.
    pub fn changed(&self, now: &Validator) -> bool {
        if let (Some(a), Some(b)) = (self.size, now.size) {
            if a != b {
                return true;
            }
        }
        if let (Some(a), Some(b)) = (&self.etag, &now.etag) {
            return a != b;
        }
        matches!((&self.last_modified, &now.last_modified), (Some(a), Some(b)) if a != b)
    }
}

/// What to do once one download finishes, set from its window or
/// Properties. It runs once, after the virus scan if there is one, and is
/// then cleared. A download that fails does none of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OnDone {
    /// Show the completion dialog for it, whatever Options > When done says.
    #[serde(default)]
    pub dialog: bool,
    /// Then exit UDM, or sleep, hibernate or shut down after the usual
    /// countdown.
    #[serde(default)]
    pub then: WhenDone,
    /// Shutting down closes programs that would hold it up.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force_close: bool,
}

/// Why a change to a download was refused. The messages are shown to the
/// user. The variant names are part of the wire format (the daemon's
/// `error_kind`): renaming one changes what clients read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum DownloadError {
    #[error("no download with id {0}")]
    NoDownload(DownloadId),
    #[error("download {0} is still running; cancel it first")]
    StillRunning(DownloadId),
    #[error("download {0} is not failed, canceled or interrupted")]
    NotRetryable(DownloadId),
    #[error("Only a finished or stopped download can be downloaded again.")]
    NotFinishedOrStopped,
    #[error("This stream's playlist is no longer known. Play it in the browser again to download it.")]
    PlaylistGone,
    #[error("The download started again in the meantime.")]
    StartedMeanwhile,
    #[error("{kbps} KB/s is not a speed limit between {lo} and {hi} KB/s.")]
    BadSpeedLimit { kbps: u64, lo: u64, hi: u64 },
    #[error("Stop the download before changing its properties.")]
    NotStopped,
    #[error("Only a stopped download can get a new link.")]
    LinkNeedsStop,
    #[error("{0} is not an http, https or ftp link")]
    NotDownloadLink(String),
    #[error("{0} is not an http or https address")]
    NotWebAddress(String),
    #[error("A finished download keeps no login. Download it again to use one.")]
    LoginOnFinished,
    #[error("Only a finished download's file can be renamed.")]
    RenameUnfinished,
    #[error("The folder must be a full path: {0}")]
    RelativeFolder(String),
    #[error("The download has no file.")]
    NoFile,
    #[error("The download has no file name.")]
    NoFileName,
    #[error("The download has no folder.")]
    NoFolder,
    #[error("{0} already exists.")]
    Exists(String),
    #[error("The download started while its properties were being saved.")]
    StartedWhileSaving,
    #[error("Only a finished download whose file is still there can be converted.")]
    NotConvertible,
    #[error("Converting needs ffmpeg. Install it, or choose it in Options > Tools.")]
    NoFfmpeg,
    #[error("download {0} is not waiting to be started")]
    NotWaiting(DownloadId),
    #[error("the download has no file name")]
    AnswerNoFileName,
    #[error("the download has no folder")]
    AnswerNoFolder,
    #[error("the folder must be a full path: {0}")]
    AnswerRelativeFolder(String),
    #[error("This download has already finished.")]
    AlreadyFinished,
    #[error("could not reach the new link: {0}")]
    Unreachable(String),
    /// Moving its file failed; the message says how.
    #[error("{0}")]
    Files(String),
    /// It left the list, but its files could not go to the Recycle Bin.
    #[error("Removed {url} from the list, but its files stay: {reason}")]
    FilesStay { url: String, reason: String },
}

/// Why a partial download cannot continue from a new link. The window
/// shows these, translated. The variant names are part of the wire format
/// (the daemon's `error_kind`): renaming one changes what clients read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum Mismatch {
    #[error("The new link does not say how big its file is, so it cannot be matched to the partial download.")]
    UnknownSize,
    /// The new link's size and the partial download's, in bytes.
    #[error("The new link is for a {} file; the partial download is of a {} file.", new.megabytes(), partial.megabytes())]
    Size { new: ByteCount, partial: ByteCount },
    #[error("The file at the new link has changed since the download started (its ETag or date differs).")]
    Changed,
    #[error("The new link's server cannot resume downloads, so it must start over.")]
    NotResumable,
}

/// A download's own speed limit from `bytes_per_sec`: `None` (or 0) is no
/// limit, anything else must lie in the settings' range.
pub fn speed_limit(bytes_per_sec: Option<u64>) -> Result<Option<u64>, DownloadError> {
    let limit = bytes_per_sec.filter(|b| *b > 0);
    let (lo, hi) = SPEED_LIMIT_RANGE_KBPS;
    match limit {
        Some(b) if !(lo * 1024..=hi * 1024).contains(&b) => {
            Err(DownloadError::BadSpeedLimit { kbps: b / 1024, lo, hi })
        }
        _ => Ok(limit),
    }
}

impl Status {
    /// Whether a download in this state may be downloaded again from the
    /// first byte.
    pub fn can_download_again(self) -> bool {
        matches!(self, Status::Done | Status::Failed | Status::Canceled | Status::Interrupted)
    }
}

impl DownloadEntry {
    /// Set what it does once it finishes; `None`, or nothing chosen, clears
    /// it. A finished download has done it already.
    pub fn set_on_done(&mut self, on_done: Option<OnDone>) -> Result<(), DownloadError> {
        if self.status == Status::Done {
            return Err(DownloadError::AlreadyFinished);
        }
        self.on_done = on_done.filter(|o| *o != OnDone::default());
        Ok(())
    }

    /// What it asked to happen once it finished, cleared so downloading it
    /// again does not repeat it. `None` unless it finished and asked.
    pub fn take_on_done(&mut self) -> Option<OnDone> {
        if self.status != Status::Done {
            return None;
        }
        self.on_done.take()
    }

    /// Its file's name, or its address when it has no file.
    pub fn display_name(&self) -> String {
        self.path
            .as_deref()
            .and_then(|p| Path::new(p).file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.url.to_string())
    }

    /// Hold it, probed, for the save-as dialog's answer (or with
    /// `start_now`, to start at once) with the file the dialog proposes.
    /// Returns false when it was canceled while being probed.
    pub fn hold_for_answer(
        &mut self,
        start_now: bool,
        path: &Path,
        total: Option<ByteCount>,
        resumable: bool,
        duplicate_of: Option<DownloadId>,
    ) -> bool {
        if self.status != Status::Queued {
            return false;
        }
        self.status = if start_now { Status::Waiting } else { Status::Confirming };
        self.path = Some(path.display().to_string());
        self.total = total;
        self.resumable = resumable;
        self.duplicate_of = duplicate_of;
        true
    }

    /// Take the save-as dialog's answer: the category and description it
    /// gives, and the file name and folder chosen (each keeping the
    /// proposed one when missing). It then waits to be started. Only a
    /// download waiting for its dialog or in a queue takes an answer.
    pub fn apply_answer(
        &mut self,
        filename: Option<&str>,
        folder: Option<&str>,
        category: Option<&str>,
        description: Option<&str>,
    ) -> Result<(), DownloadError> {
        if !self.status.waits() {
            return Err(DownloadError::NotWaiting(self.id));
        }
        let path = if filename.is_none() && folder.is_none() {
            // Keep whatever it already has: a chosen file, a partial file,
            // or nothing yet.
            None
        } else {
            let proposed = self.path.as_deref().map(Path::new);
            let name = filename
                .and_then(crate::filename::sanitize_filename)
                .map(FileName::into_string)
                .or_else(|| proposed.and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()))
                .ok_or(DownloadError::AnswerNoFileName)?;
            let dir = match folder.map(str::trim) {
                Some(f) if !f.is_empty() => PathBuf::from(f),
                _ => proposed.and_then(Path::parent).map(Path::to_path_buf).ok_or(DownloadError::AnswerNoFolder)?,
            };
            if !dir.is_absolute() {
                return Err(DownloadError::AnswerRelativeFolder(dir.display().to_string()));
            }
            Some(dir.join(&name).display().to_string())
        };
        self.duplicate_of = None;
        if let Some(c) = category {
            self.category = Some(c.to_string());
        }
        if let Some(d) = description {
            self.description = Some(d.to_string()).filter(|d| !d.is_empty());
        }
        if path.is_some() {
            self.path = path;
        }
        self.status = Status::Waiting;
        Ok(())
    }

    /// Whether its properties may change: it is finished or stopped.
    pub fn properties_editable(&self) -> bool {
        self.status == Status::Done || self.status.is_retryable()
    }

    /// The request it was made with, as far as its row tells: its address,
    /// referrer and page.
    pub fn base_request(&self) -> Download {
        Download {
            url: self.url.clone(),
            referrer: self.referrer.clone(),
            page: self.page.clone(),
            ..Download::default()
        }
    }

    /// Work out what changing its properties to `p` does, refusing a change
    /// that is not allowed.
    pub fn plan_properties(&self, p: Properties) -> Result<PropertiesPlan, DownloadError> {
        let done = self.status == Status::Done;
        if !self.properties_editable() {
            return Err(DownloadError::NotStopped);
        }
        let trimmed = |s: Option<String>| s.map(|s| s.trim().to_string());
        let url = trimmed(p.url)
            .filter(|u| self.url != u.as_str())
            .map(|u| Address::parse(&u).map_err(|_| DownloadError::NotDownloadLink(u.clone())))
            .transpose()?;
        let referrer = trimmed(p.referrer);
        if let Some(r) = referrer.as_deref().filter(|r| !r.is_empty() && !crate::batch::is_web_url(r)) {
            return Err(DownloadError::NotWebAddress(r.to_string()));
        }
        let description = trimmed(p.description);
        if p.login.is_some() && done {
            return Err(DownloadError::LoginOnFinished);
        }
        let full = |dir: PathBuf| {
            if dir.is_absolute() {
                Ok(dir)
            } else {
                Err(DownloadError::RelativeFolder(dir.display().to_string()))
            }
        };
        let mut new_folder = None;
        let moved = match (p.filename, p.folder) {
            (None, None) => None,
            (Some(_), _) if !done => return Err(DownloadError::RenameUnfinished),
            (None, Some(folder)) if !done => {
                // An unfinished download moves to another folder: its
                // partial file and resume record go along when they are kept
                // beside it, and the next attempt saves there. A partial file
                // in the temporary folder stays put; it is moved into place
                // when the download finishes, as always.
                let dir = full(PathBuf::from(folder.trim()))?;
                new_folder = Some(dir.clone());
                self.path
                    .as_deref()
                    .map(PathBuf::from)
                    .and_then(|from| Some((from.file_name()?.to_owned(), from)))
                    .map(|(name, from)| (from, dir.join(name)))
                    .filter(|(from, to)| to != from)
            }
            (filename, folder) => {
                let from = self.path.as_deref().map(PathBuf::from).ok_or(DownloadError::NoFile)?;
                let name = filename
                    .as_deref()
                    .and_then(crate::filename::sanitize_filename)
                    .map(FileName::into_string)
                    .or_else(|| from.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .ok_or(DownloadError::NoFileName)?;
                let dir = match folder.as_deref().map(str::trim) {
                    Some(f) if !f.is_empty() => PathBuf::from(f),
                    _ => from.parent().map(Path::to_path_buf).ok_or(DownloadError::NoFolder)?,
                };
                let to = full(dir)?.join(name);
                (to != from).then_some((from, to))
            }
        };
        Ok(PropertiesPlan { url, referrer, description, login: p.login, moved, new_folder })
    }

    /// The request to download it again from the first byte with: the
    /// `stored` one when it was kept, otherwise one made from its address
    /// (which a stream cannot do without its playlist). It keeps its
    /// description, category and speed limit, and goes to the same name and
    /// folder when it wrote a file of its own.
    pub fn redownload_request(&self, stored: Option<Download>) -> Result<Download, DownloadError> {
        if !self.status.can_download_again() {
            return Err(DownloadError::NotFinishedOrStopped);
        }
        if stored.is_none() && self.parts.is_some() {
            return Err(DownloadError::PlaylistGone);
        }
        let mut download = stored.unwrap_or_else(|| self.base_request());
        download.description = self.description.clone();
        download.checksum = self.checksum.clone();
        download.category = self.category.clone();
        download.speed_limit = self.speed_limit;
        if let Some(path) = self.path.as_deref().filter(|_| self.wrote_files()).map(Path::new) {
            download.filename = path.file_name().map(|n| n.to_string_lossy().into_owned());
            download.folder = path.parent().map(|d| d.display().to_string());
        }
        Ok(download)
    }

    /// Clear what its last attempt left, so it starts again from the first
    /// byte under the same id.
    pub fn reset_for_redownload(&mut self) {
        self.status = Status::Queued;
        self.done = ByteCount::ZERO;
        self.total = None;
        self.parts = None;
        self.error = None;
        self.scan = None;
        self.note = None;
        self.resumable = false;
        self.connections = 0;
        self.partial = None;
    }

    /// Whether it ever opened a connection. Only such a download has a file
    /// of its own: one that never got past its save-as dialog only has a
    /// proposed path, which may be somebody else's file.
    pub fn started(&self) -> bool {
        self.connections > 0
    }

    /// Whether it wrote files to disk that removing it may delete.
    pub fn wrote_files(&self) -> bool {
        self.status == Status::Done || self.started()
    }

    /// The request to retry it with: the `stored` one when it was kept,
    /// otherwise one made from its address. It keeps its description and
    /// category, and its speed limit unless the user chose not to remember
    /// it on stop and resume.
    pub fn retry_request(&self, stored: Option<Download>) -> Download {
        let download = stored.unwrap_or_else(|| self.base_request());
        Download {
            description: self.description.clone(),
            checksum: self.checksum.clone(),
            category: self.category.clone(),
            speed_limit: self.speed_limit.filter(|_| self.limit_remember),
            ..download
        }
    }
}

/// What a [`Properties`] change does to a download, worked out by
/// [`DownloadEntry::plan_properties`] before anything changes.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertiesPlan {
    /// A new address (trimmed, and different from the old one).
    pub url: Option<Address>,
    /// A new referrer; empty removes it.
    pub referrer: Option<String>,
    /// A new description; empty removes it.
    pub description: Option<String>,
    /// A new login; an empty user name removes it.
    pub login: Option<DownloadLogin>,
    /// The file to move on disk, from and to.
    pub moved: Option<(PathBuf, PathBuf)>,
    /// The folder an unfinished download saves into from now on.
    pub new_folder: Option<PathBuf>,
}

impl PropertiesPlan {
    /// Apply the plan to the request a retry starts from.
    pub fn apply_to_request(&self, req: &mut Download) {
        if let Some(u) = &self.url {
            req.url = u.clone().into();
        }
        if let Some(r) = &self.referrer {
            req.referrer = Some(r.clone()).filter(|r| !r.is_empty());
        }
        if let Some(d) = &self.description {
            req.description = Some(d.clone()).filter(|d| !d.is_empty());
        }
        if let Some(dir) = &self.new_folder {
            req.folder = Some(dir.display().to_string());
        }
        if let Some(l) = &self.login {
            let username = l.username.trim().to_string();
            req.login = (!username.is_empty()).then(|| DownloadLogin {
                password: l.password.clone().or_else(|| req.login.as_ref().and_then(|o| o.password.clone())),
                username,
            });
        }
    }

    /// Apply the plan to the download's row. `login_user` is the user name
    /// its request now logs in with.
    pub fn apply_to_row(&self, e: &mut DownloadEntry, login_user: Option<String>) {
        if let Some(u) = &self.url {
            e.url = u.clone().into();
        }
        if let Some(r) = &self.referrer {
            e.referrer = Some(r.clone()).filter(|r| !r.is_empty());
        }
        if let Some(d) = &self.description {
            e.description = Some(d.clone()).filter(|d| !d.is_empty());
        }
        if let Some((_, to)) = &self.moved {
            e.path = Some(to.display().to_string());
        }
        e.login_user = login_user;
    }
}

/// Whether two download addresses name the same file: equal once parsed (so
/// the case of the scheme and host and a default port do not matter) and with
/// the fragment, which never reaches the server, dropped.
pub fn same_address(a: &Link, b: &Link) -> bool {
    let key = |u: &Link| match url::Url::parse(u.as_str().trim()) {
        Ok(mut parsed) => {
            parsed.set_fragment(None);
            parsed.to_string()
        }
        Err(_) => u.as_str().trim().split('#').next().unwrap_or("").to_string(),
    };
    key(a) == key(b)
}

/// What happens to a download the save-as dialog was to ask about, once
/// the server named its file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterProbe {
    /// Its type starts without asking: start it now.
    StartNow,
    /// Ask in the dialog; with `start_early`, start fetching meanwhile.
    Ask { start_early: bool },
}

/// What a duplicate setting does with a new download whose address an
/// older one has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateStep {
    /// Add the new one as well.
    Add,
    /// Add the new one in place of the old one, which leaves the list
    /// first; the new one overwrites its file.
    Replace,
    /// Do not add it; point the user at the old one instead.
    UseExisting,
}

impl Duplicate {
    /// Whether this choice adds the new download when many are added at
    /// once, with the old one in state `old`. Asking is not possible then.
    pub fn adds_in_bulk(self, old: Status) -> bool {
        match self {
            Duplicate::NewName => true,
            Duplicate::Overwrite => old.is_terminal(),
            Duplicate::Ask | Duplicate::Existing => false,
        }
    }
}

impl Download {
    /// How many connections it opens: its own number, brought into
    /// `CONNECTIONS_RANGE`, or `setting` when it has none.
    pub fn connections_or(&self, setting: usize) -> usize {
        let (low, high) = crate::settings::CONNECTIONS_RANGE;
        self.connections.map_or(setting, |n| n.clamp(low, high))
    }

    /// Whether it is a form post, which cannot be probed or sent again
    /// without the browser's say.
    pub fn is_post(&self) -> bool {
        self.method.as_deref().is_some_and(|m| m.eq_ignore_ascii_case("POST"))
    }

    /// What happens once the server named its file `filename`. A type that
    /// starts without asking skips the dialog after all, unless it asks
    /// about a `duplicate`, was asked for explicitly, or its folder is not
    /// `writable`. Only a plain file the server sends in ranges, of a known
    /// size, can start early and be carried on elsewhere once answered.
    pub fn after_probe(
        &self,
        settings: &Settings,
        filename: &FileName,
        resumable: bool,
        size_known: bool,
        writable: bool,
        duplicate: bool,
    ) -> AfterProbe {
        if !duplicate && self.ask != Some(true) && writable && settings.starts_without_asking(filename.as_str()) {
            return AfterProbe::StartNow;
        }
        let plain = !self.is_post() && self.media.is_none();
        AfterProbe::Ask {
            start_early: settings.start_while_asking && writable && !duplicate && plain && resumable && size_known,
        }
    }

    /// The answer given for download `id` when no window answers its
    /// dialog in time: start as proposed (a "download later" waits instead),
    /// with `file_exists` deciding a taken name.
    pub fn fallback_answer(&self, id: DownloadId, file_exists: Option<FileExists>) -> Answer {
        Answer { start: self.start != Some(false), queue: self.queue, file_exists, ..Answer::start(id) }
    }

    /// Keep the save-as dialog's answer for later starts: what to do with a
    /// taken name, the category, the description (`Some` sets it, to
    /// nothing when it was cleared), and administrator rights
    /// for `folder` (where the file goes), which a retry needs because it
    /// resumes a staged file kept elsewhere.
    pub fn apply_answer(
        &mut self,
        file_exists: Option<FileExists>,
        category: Option<&str>,
        description: Option<Option<&str>>,
        elevate_into: Option<&Path>,
    ) {
        if file_exists.is_some() {
            self.file_exists = file_exists;
        }
        if let Some(c) = category {
            self.category = Some(c.to_string());
        }
        if let Some(d) = description {
            self.description = d.map(str::to_string);
        }
        if let Some(dir) = elevate_into {
            self.elevate = true;
            self.folder = Some(dir.display().to_string());
        }
    }

    /// Whether it can be a duplicate of another download. Form posts and
    /// streams cannot: one address can stand for different files there
    /// (another form, another quality).
    pub fn may_be_duplicate(&self) -> bool {
        !self.is_post() && self.media.is_none()
    }

    /// Prepare it, whose address `old` already has, as `choice` says. A
    /// running download's file cannot be replaced under it, so overwriting
    /// one points at it instead.
    pub fn settle_duplicate(&mut self, old: &DownloadEntry, choice: Duplicate) -> DuplicateStep {
        match choice {
            Duplicate::Ask | Duplicate::NewName => {
                self.file_exists = Some(FileExists::Rename);
                DuplicateStep::Add
            }
            Duplicate::Existing => DuplicateStep::UseExisting,
            Duplicate::Overwrite if !old.status.is_terminal() => DuplicateStep::UseExisting,
            Duplicate::Overwrite => {
                if let Some(path) = old.path.as_deref().map(Path::new) {
                    self.filename = path.file_name().map(|n| n.to_string_lossy().into_owned());
                    self.folder = path.parent().map(|d| d.display().to_string());
                }
                self.file_exists = Some(FileExists::Overwrite);
                DuplicateStep::Replace
            }
        }
    }
}

/// IDM's command-line switches, parsed by `udm_infrastructure::cli`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommandLine {
    /// The download `/d` names, with the folder, name and start choices of
    /// `/p`, `/f`, `/a` and `/n`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<Download>,
    /// `/s`: start the main queue.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub start_queue: bool,
    /// `/q`: exit UDM once the download finishes. Like IDM, only a call
    /// that starts UDM does this; one that finds it running does not.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quit_after: bool,
}

impl CommandLine {
    /// Whether it asks for nothing: `udm-gui` started with no switches,
    /// which only brings an open window forward.
    pub fn is_empty(&self) -> bool {
        self.download.is_none() && !self.start_queue
    }
}

/// A file inside a zip archive, as the save-as dialog's Preview lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZipEntry {
    pub name: String,
    /// Its size once unpacked.
    pub size: ByteCount,
}

const NEEDS_LOGIN: &str = "This site needs a login: ";
const LOGIN_FOR: &str = "The login for ";
const LOGIN_REFUSED: &str = " was refused";
const REALM: &str = " (realm: ";

fn realm_suffix(realm: Option<&str>) -> String {
    realm.map_or(String::new(), |r| format!("{REALM}{r})"))
}

/// The error of a download that `host` (and its `realm`) wants a login
/// for. `login_wanted` reads it back.
pub fn needs_login_error(host: &str, realm: Option<&str>) -> String {
    format!("{NEEDS_LOGIN}{host}{}", realm_suffix(realm))
}

/// The error of a download whose login `host` (and its `realm`) refused.
pub fn login_refused_error(host: &str, realm: Option<&str>) -> String {
    format!("{LOGIN_FOR}{host}{LOGIN_REFUSED}{}", realm_suffix(realm))
}

/// A login a download failed for: the site that asked, the realm it named,
/// and whether a login was sent and refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginWanted {
    pub host: String,
    pub realm: Option<String>,
    pub refused: bool,
}

/// The login a failed download's `error` says it needs, if that is why it
/// failed.
pub fn login_wanted(error: &str) -> Option<LoginWanted> {
    let (rest, refused) = match error.strip_prefix(NEEDS_LOGIN) {
        Some(rest) => (rest.to_string(), false),
        None => {
            let rest = error.strip_prefix(LOGIN_FOR)?;
            let (host, after) = rest.split_once(LOGIN_REFUSED)?;
            (format!("{host}{after}"), true)
        }
    };
    let (host, realm) = match rest.split_once(REALM) {
        Some((host, realm)) => (host.to_string(), realm.strip_suffix(')').map(str::to_string)),
        None => (rest, None),
    };
    (!host.is_empty() && !host.contains(char::is_whitespace)).then_some(LoginWanted { host, realm, refused })
}

/// The most files a zip preview lists.
pub const MAX_ZIP_ENTRIES: usize = 5000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_download_of_its_own_connections_keeps_them_in_range() {
        let with = |n: Option<usize>| Download { connections: n, ..Download::default() };
        assert_eq!(with(None).connections_or(8), 8);
        assert_eq!(with(Some(3)).connections_or(8), 3);
        // A number from another program is brought into the range.
        assert_eq!(with(Some(0)).connections_or(8), 1);
        assert_eq!(with(Some(99)).connections_or(8), crate::settings::CONNECTIONS_RANGE.1);
        // It travels as a plain number, and is left out when missing.
        let json = serde_json::to_value(with(Some(4))).unwrap();
        assert_eq!(json["connections"], 4);
        assert!(!serde_json::to_string(&with(None)).unwrap().contains("connections"));
    }

    #[test]
    fn a_login_error_reads_back_as_what_it_wants() {
        let asked = needs_login_error("127.0.0.1", Some("files"));
        assert_eq!(asked, "This site needs a login: 127.0.0.1 (realm: files)");
        let wanted = LoginWanted { host: "127.0.0.1".into(), realm: Some("files".into()), refused: false };
        assert_eq!(login_wanted(&asked), Some(wanted));
        let refused = login_refused_error("127.0.0.1", Some("files"));
        assert_eq!(refused, "The login for 127.0.0.1 was refused (realm: files)");
        assert!(login_wanted(&refused).is_some_and(|w| w.refused && w.host == "127.0.0.1"));
        assert_eq!(login_wanted("server returned HTTP 404"), None);
        assert_eq!(login_wanted("This site needs a login: ftp.example.com").map(|w| w.realm), Some(None));
    }

    fn v(size: Option<u64>, etag: Option<&str>, date: Option<&str>) -> Validator {
        Validator {
            size: size.map(ByteCount::new),
            etag: etag.map(str::to_string),
            last_modified: date.map(str::to_string),
        }
    }

    #[test]
    fn what_a_browser_request_may_carry_is_limited() {
        let mut d = Download {
            url: "https://x/a".into(),
            request_headers: vec![
                ("Accept".into(), "*/*".into()),
                ("Bad Name".into(), "x".into()),
                ("X-Split".into(), "a\r\nSet-Cookie: b".into()),
                ("X-Long".into(), "v".repeat(MAX_HEADER_VALUE + 1)),
            ],
            redirects: vec![
                "https://x/1".into(),
                "javascript:alert(1)".into(),
                format!("https://x/{}", "a".repeat(MAX_URL)),
            ],
            content_type: Some("a\nb".into()),
            browser_proxy: Some(BrowserProxy {
                pac_script: Some("x".repeat(MAX_PAC_SCRIPT + 1)),
                bypass: vec!["".into(), "*.corp".into(), "h".repeat(300)],
                ..BrowserProxy::default()
            }),
            ..Download::default()
        };
        d.request_headers.extend((0..100).map(|i| (format!("X-{i}"), "1".to_string())));
        d.limit_browser_request();
        assert_eq!(d.request_headers.len(), MAX_REQUEST_HEADERS);
        assert_eq!(d.request_headers[0].0, "Accept");
        assert_eq!(d.request_headers[1].0, "X-0", "the bad ones are gone");
        assert_eq!(d.redirects, ["https://x/1"]);
        assert_eq!(d.content_type, None);
        let proxy = d.browser_proxy.unwrap();
        assert_eq!((proxy.pac_script, proxy.bypass), (None, vec!["*.corp".to_string()]));
    }

    #[test]
    fn a_server_copy_changes_by_size_etag_or_date() {
        const MON: &str = "Mon, 02 Nov 2026 10:00:00 GMT";
        const TUE: &str = "Tue, 03 Nov 2026 10:00:00 GMT";
        let old = v(Some(1000), Some("\"a\""), Some(MON));
        assert!(!old.changed(&old.clone()), "the same file");
        assert!(old.changed(&v(Some(1001), Some("\"a\""), Some(MON))), "a new size");
        assert!(old.changed(&v(Some(1000), Some("\"b\""), Some(MON))), "a new ETag");
        assert!(!old.changed(&v(Some(1000), Some("\"a\""), Some(TUE))), "the ETag outranks a touched date");
        // Without ETags the date decides.
        let dated = v(Some(1000), None, Some(MON));
        assert!(dated.changed(&v(Some(1000), None, Some(TUE))));
        assert!(!dated.changed(&v(Some(1000), None, Some(MON))));
        assert!(!dated.changed(&v(Some(1000), Some("\"a\""), None)), "nothing new to compare");
        // A size alone (from before validators were kept) still counts.
        assert!(v(Some(10), None, None).changed(&v(Some(20), Some("\"x\""), Some(TUE))));
        assert!(!v(Some(10), None, None).changed(&v(Some(10), Some("\"x\""), Some(TUE))));
        // A server that says nothing is not downloaded again every run.
        assert!(!v(None, None, None).changed(&v(None, None, None)));
        assert!(!v(Some(10), None, None).changed(&v(None, None, None)));
    }

    fn entry(status: Status, connections: usize) -> DownloadEntry {
        let json = serde_json::json!({
            "id": 1, "url": "https://dl.example/a.zip", "status": status, "connections": connections, "done": 0,
            "resumable": false,
            "description": "note", "speed_limit": 1000,
        });
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn only_a_started_or_finished_download_wrote_files() {
        assert!(!entry(Status::Canceled, 0).wrote_files(), "it never left its save-as dialog");
        assert!(entry(Status::Canceled, 2).wrote_files());
        assert!(entry(Status::Done, 0).wrote_files());
    }

    #[test]
    fn a_retry_keeps_the_note_and_the_remembered_limit() {
        let e = DownloadEntry { limit_remember: true, ..entry(Status::Failed, 1) };
        let d = e.retry_request(None);
        assert_eq!(d.url, "https://dl.example/a.zip");
        assert_eq!((d.description.as_deref(), d.speed_limit), (Some("note"), Some(1000)));
        let stored = Download { url: "https://dl.example/b.zip".into(), ..Download::default() };
        let d = DownloadEntry { limit_remember: false, ..e }.retry_request(Some(stored));
        assert_eq!((d.url.as_str(), d.speed_limit), ("https://dl.example/b.zip", None));
    }

    #[test]
    fn a_speed_limit_lies_in_the_settings_range() {
        assert_eq!(speed_limit(None), Ok(None));
        assert_eq!(speed_limit(Some(0)), Ok(None), "0 is no limit");
        assert_eq!(speed_limit(Some(50 * 1024)), Ok(Some(50 * 1024)));
        let err = speed_limit(Some(100)).unwrap_err();
        assert_eq!(err.to_string(), "0 KB/s is not a speed limit between 1 and 10000000 KB/s.");
    }

    #[test]
    fn downloading_again_keeps_the_name_of_a_file_it_wrote() {
        let mut e = entry(Status::Done, 3);
        e.path = Some(if cfg!(windows) { r"C:\dl\a.zip" } else { "/dl/a.zip" }.into());
        let d = e.redownload_request(None).unwrap();
        assert_eq!(d.filename.as_deref(), Some("a.zip"));
        assert_eq!((d.description.as_deref(), d.speed_limit), (Some("note"), Some(1000)));
        let never_started = DownloadEntry { status: Status::Canceled, connections: 0, ..e.clone() };
        assert_eq!(never_started.redownload_request(None).unwrap().filename, None);
        let stream = DownloadEntry { parts: Some([1, 10]), ..e.clone() };
        assert_eq!(stream.redownload_request(None).unwrap_err(), DownloadError::PlaylistGone);
        let running = DownloadEntry { status: Status::Downloading, ..e };
        assert_eq!(running.redownload_request(None).unwrap_err(), DownloadError::NotFinishedOrStopped);
    }

    #[test]
    fn a_reset_starts_from_the_first_byte() {
        let mut e = DownloadEntry {
            done: ByteCount::new(500),
            partial: Some("/tmp/a.part".into()),
            ..entry(Status::Failed, 2)
        };
        e.reset_for_redownload();
        assert_eq!((e.status, e.done, e.connections, e.partial), (Status::Queued, ByteCount::ZERO, 0, None));
    }

    fn full(p: &str) -> String {
        if cfg!(windows) {
            format!("C:{}", p.replace('/', "\\"))
        } else {
            p.to_string()
        }
    }

    #[test]
    fn a_finished_file_is_renamed_in_its_folder() {
        let mut e = entry(Status::Done, 1);
        e.path = Some(full("/dl/a.zip"));
        let p = Properties { filename: Some(" b?.zip ".into()), ..Properties::default() };
        let plan = e.plan_properties(p).unwrap();
        assert_eq!(plan.moved, Some((PathBuf::from(full("/dl/a.zip")), PathBuf::from(full("/dl/b_.zip")))));
        let same = Properties { filename: Some("a.zip".into()), ..Properties::default() };
        assert_eq!(e.plan_properties(same).unwrap().moved, None, "nothing to move");
    }

    #[test]
    fn an_unfinished_download_moves_folder_but_keeps_its_name() {
        let mut e = entry(Status::Canceled, 1);
        e.path = Some(full("/dl/a.zip"));
        let p = Properties { folder: Some(full("/new")), ..Properties::default() };
        let plan = e.plan_properties(p).unwrap();
        assert_eq!(plan.new_folder, Some(PathBuf::from(full("/new"))));
        assert_eq!(plan.moved.map(|(_, to)| to), Some(PathBuf::from(full("/new/a.zip"))));
        let rename = Properties { filename: Some("b.zip".into()), ..Properties::default() };
        assert_eq!(e.plan_properties(rename), Err(DownloadError::RenameUnfinished));
        let relative = Properties { folder: Some("new".into()), ..Properties::default() };
        assert_eq!(e.plan_properties(relative), Err(DownloadError::RelativeFolder("new".into())));
    }

    #[test]
    fn properties_are_checked_before_anything_changes() {
        let e = entry(Status::Failed, 0);
        let bad_link = Properties { url: Some("file:///etc/passwd".into()), ..Properties::default() };
        assert_eq!(e.plan_properties(bad_link), Err(DownloadError::NotDownloadLink("file:///etc/passwd".into())));
        let bad_referrer = Properties { referrer: Some("ftp://x/".into()), ..Properties::default() };
        assert_eq!(e.plan_properties(bad_referrer), Err(DownloadError::NotWebAddress("ftp://x/".into())));
        let login = Properties {
            login: Some(DownloadLogin { username: "me".into(), password: None }),
            ..Properties::default()
        };
        assert_eq!(entry(Status::Done, 1).plan_properties(login.clone()), Err(DownloadError::LoginOnFinished));
        assert_eq!(entry(Status::Downloading, 1).plan_properties(login), Err(DownloadError::NotStopped));
    }

    #[test]
    fn a_new_login_keeps_the_old_password_unless_given() {
        let plan = PropertiesPlan {
            login: Some(DownloadLogin { username: " me ".into(), password: None }),
            ..entry(Status::Failed, 0).plan_properties(Properties::default()).unwrap()
        };
        let mut req = Download {
            login: Some(DownloadLogin { username: "old".into(), password: Some("secret".into()) }),
            ..Download::default()
        };
        plan.apply_to_request(&mut req);
        let login = req.login.unwrap();
        assert_eq!((login.username.as_str(), login.password.as_deref()), ("me", Some("secret")));
    }

    #[test]
    fn a_download_does_its_on_done_once_after_finishing() {
        let then = OnDone { then: WhenDone::ShutDown, ..OnDone::default() };
        let mut e = entry(Status::Downloading, 1);
        e.set_on_done(Some(then)).unwrap();
        assert_eq!(e.take_on_done(), None, "not finished yet");
        e.status = Status::Done;
        assert_eq!(e.set_on_done(None), Err(DownloadError::AlreadyFinished));
        assert_eq!(e.take_on_done(), Some(then));
        assert_eq!(e.take_on_done(), None, "it is cleared");
        let mut e = entry(Status::Waiting, 0);
        e.set_on_done(Some(OnDone::default())).unwrap();
        assert_eq!(e.on_done, None, "choosing nothing clears it");
    }

    #[test]
    fn same_address_ignores_fragment_and_host_case() {
        assert!(same_address(&Link::new("https://Example.com/a.zip"), &Link::new("https://example.com/a.zip#part")));
        assert!(same_address(&Link::new("https://example.com:443/a.zip"), &Link::new(" https://example.com/a.zip ")));
        assert!(!same_address(&Link::new("https://example.com/a.zip"), &Link::new("https://example.com/a.zip?v=2")));
        assert!(!same_address(&Link::new("https://example.com/a.zip"), &Link::new("http://example.com/a.zip")));
        assert!(!same_address(&Link::new("https://example.com/A.zip"), &Link::new("https://example.com/a.zip")));
    }

    #[test]
    fn duplicates_are_settled_as_the_setting_says() {
        let mut old = entry(Status::Done, 1);
        old.path = Some(if cfg!(windows) { r"C:\dl\a.zip" } else { "/dl/a.zip" }.into());
        let mut new = Download { url: old.url.clone(), ..Download::default() };
        assert_eq!(new.settle_duplicate(&old, Duplicate::NewName), DuplicateStep::Add);
        assert_eq!(new.file_exists, Some(FileExists::Rename));
        assert_eq!(new.settle_duplicate(&old, Duplicate::Overwrite), DuplicateStep::Replace);
        assert_eq!((new.filename.as_deref(), new.file_exists), (Some("a.zip"), Some(FileExists::Overwrite)));
        let running = DownloadEntry { status: Status::Downloading, ..old.clone() };
        assert_eq!(new.settle_duplicate(&running, Duplicate::Overwrite), DuplicateStep::UseExisting);
        assert_eq!(new.settle_duplicate(&old, Duplicate::Existing), DuplicateStep::UseExisting);
    }

    #[test]
    fn only_some_choices_add_in_bulk_and_posts_are_never_duplicates() {
        assert!(Duplicate::NewName.adds_in_bulk(Status::Downloading));
        assert!(Duplicate::Overwrite.adds_in_bulk(Status::Done));
        assert!(!Duplicate::Overwrite.adds_in_bulk(Status::Downloading));
        assert!(!Duplicate::Ask.adds_in_bulk(Status::Done) && !Duplicate::Existing.adds_in_bulk(Status::Done));
        let post = Download { method: Some("post".into()), ..Download::default() };
        assert!(!post.may_be_duplicate());
        assert!(Download::default().may_be_duplicate());
    }
    #[test]
    fn an_answer_moves_the_file_and_keeps_what_it_leaves_out() {
        let dl = if cfg!(windows) { r"C:\dl" } else { "/dl" };
        let mut e = entry(Status::Confirming, 0);
        e.path = Some(Path::new(dl).join("a.zip").display().to_string());
        e.duplicate_of = Some(DownloadId::new(7));
        e.apply_answer(Some("b?.zip"), None, Some("Music"), Some("")).unwrap();
        assert_eq!(e.path, Some(Path::new(dl).join("b_.zip").display().to_string()));
        assert_eq!((e.status, e.duplicate_of, e.category.as_deref()), (Status::Waiting, None, Some("Music")));
        assert_eq!(e.description, None, "an empty description clears it");
        let mut e = entry(Status::Confirming, 0);
        e.path = Some("/dl/a.zip".into());
        assert_eq!(
            e.apply_answer(None, Some("rel"), None, None),
            Err(DownloadError::AnswerRelativeFolder("rel".into()))
        );
        assert_eq!(
            entry(Status::Done, 1).apply_answer(None, None, None, None),
            Err(DownloadError::NotWaiting(DownloadId::new(1)))
        );
    }
    #[test]
    fn some_types_start_without_asking_and_some_files_start_early() {
        let mut settings = Settings { start_while_asking: true, ..Settings::default() };
        settings.auto_start_types = vec!["zip".into()];
        let plain = Download::default();
        assert_eq!(
            plain.after_probe(&settings, &FileName::parse("a.zip").unwrap(), true, true, true, false),
            AfterProbe::StartNow
        );
        assert_eq!(
            plain.after_probe(&settings, &FileName::parse("a.zip").unwrap(), true, true, true, true),
            AfterProbe::Ask { start_early: false }
        );
        assert_eq!(
            plain.after_probe(&settings, &FileName::parse("a.zip").unwrap(), true, true, false, false),
            AfterProbe::Ask { start_early: false }
        );
        assert_eq!(
            plain.after_probe(&settings, &FileName::parse("a.iso").unwrap(), true, true, true, false),
            AfterProbe::Ask { start_early: true }
        );
        assert_eq!(
            plain.after_probe(&settings, &FileName::parse("a.iso").unwrap(), false, true, true, false),
            AfterProbe::Ask { start_early: false }
        );
        let post = Download { method: Some("POST".into()), ..Download::default() };
        assert_eq!(
            post.after_probe(&settings, &FileName::parse("a.iso").unwrap(), true, true, true, false),
            AfterProbe::Ask { start_early: false }
        );
    }

    #[test]
    fn a_probed_download_is_held_unless_canceled_meanwhile() {
        let mut e = entry(Status::Queued, 0);
        assert!(e.hold_for_answer(
            false,
            Path::new("/dl/a.zip"),
            Some(ByteCount::new(10)),
            true,
            Some(DownloadId::new(3))
        ));
        assert_eq!(
            (e.status, e.total, e.duplicate_of),
            (Status::Confirming, Some(ByteCount::new(10)), Some(DownloadId::new(3)))
        );
        let mut canceled = entry(Status::Canceled, 0);
        assert!(!canceled.hold_for_answer(true, Path::new("/dl/a.zip"), None, false, None));
        let later = Download { start: Some(false), queue: Some(QueueId::new(4)), ..Download::default() };
        let answer = later.fallback_answer(DownloadId::new(9), Some(FileExists::Rename));
        assert_eq!((answer.id, answer.start, answer.queue), (DownloadId::new(9), false, Some(QueueId::new(4))));
    }
}
