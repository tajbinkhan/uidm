//! Chrome native-messaging framing and message types.
//!
//! Framing: a 4-byte little-endian length, then that many bytes of UTF-8 JSON.
//! This is the browser's standard and is transport-agnostic, so your own
//! extension and host are free to use whatever JSON shapes you like. The types
//! below are UDM's own contract, not a copy of any other product's protocol.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

use udm_domain::address::Link;
use udm_domain::bytes::ByteCount;
use udm_domain::grab::{GrabFile, GrabRequest};
use udm_domain::id::{DeviceId, DownloadId, PackageId, QueueId};
use udm_domain::media::MediaInfo;
use udm_domain::package::{Package, PackageName};
use udm_domain::pairing::RemoteState;
use udm_domain::queue::{Queue, WhenDone};
use udm_domain::settings::Settings;

use udm_domain::download::*;

/// The package an `add_many` keeps its downloads in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewPackage {
    pub name: PackageName,
    /// The folder every download goes to; missing leaves each in its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
}

/// Text that must not show in a log line, such as an archive's password:
/// its `Debug` hides it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// A debrid key as a message carries it: never blank, and hidden from
/// `Debug` like `Secret`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct WireKey(pub udm_domain::debrid::DebridKey);

impl TryFrom<String> for WireKey {
    type Error = &'static str;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        udm_domain::debrid::DebridKey::new(&text).map(WireKey).ok_or("a debrid key cannot be blank")
    }
}

impl From<WireKey> for String {
    fn from(key: WireKey) -> Self {
        key.0.expose().to_string()
    }
}

/// A request from a client (the browser via the host, or the desktop window)
/// to the daemon.
///
/// `op` selects the variant, e.g. `{"op":"add","url":"https://..."}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
// Each message lives only while it is handled, so boxing the big variants would add churn for no gain.
#[allow(clippy::large_enum_variant)]
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
        queue: Option<QueueId>,
        /// Keep them all as one package, with this name and folder.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        package: Option<NewPackage>,
        /// Without `package`, keep each split archive among them as a
        /// package of its own.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        group_archives: bool,
    },
    /// Unpack a package's archives, with `password` for encrypted ones; its
    /// state comes back in `packages`.
    UnpackPackage {
        id: PackageId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<Secret>,
    },
    /// Stop keeping a package; its downloads stay in the list.
    UngroupPackage {
        id: PackageId,
    },
    /// Read a stream's playlist or manifest and answer with `media_info`: its
    /// qualities, or why it cannot be downloaded. `request` is echoed back.
    ProbeMedia {
        request: u64,
        download: Download,
    },
    /// Crawl a site for files; progress comes as `grab_progress` and the
    /// result as `grab_done`, to this client only. One crawl per client.
    Grab(GrabRequest),
    /// Stop this client's crawl; `grab_done` follows with what it found.
    CancelGrab,
    /// Ask each link's server for its type and size, a few links at a time,
    /// for the window's link picker. Each answer comes as `link_info`, to
    /// this client only, as it arrives. A new one stops the client's last;
    /// `stop_probe_links` stops it.
    ProbeLinks {
        request: u64,
        links: Vec<Download>,
    },
    /// Stop this client's `probe_links`: the links not asked about yet are
    /// not.
    StopProbeLinks,
    /// Cancel a queued or running download.
    Cancel {
        id: DownloadId,
    },
    /// Start a failed or canceled download again; it resumes from its partial
    /// file. The old entry is removed and the retry gets a new id.
    Retry {
        id: DownloadId,
    },
    /// Download a finished, failed or canceled download again from the
    /// start, keeping its id, link, request, name and folder. A file still
    /// there is handled by the file-exists setting.
    Redownload {
        id: DownloadId,
    },
    /// Forget a finished, failed or canceled download. Its files stay unless
    /// `delete_files`, which sends the finished or partial file, its resume
    /// sidecar and a stream's segments to the Recycle Bin.
    Remove {
        id: DownloadId,
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
    /// `waiting` one.
    Confirm(Answer),
    /// Create a queue (`id` 0) or change one: name, downloads at once,
    /// schedule, what happens when done, retries.
    SaveQueue {
        queue: Queue,
    },
    /// Delete a queue; its waiting downloads move to the main queue.
    DeleteQueue {
        id: QueueId,
    },
    StartQueue {
        id: QueueId,
    },
    /// Stop a queue; its running downloads go back to waiting in it, with
    /// their progress kept.
    StopQueue {
        id: QueueId,
    },
    /// What happens when this queue's current run ends, this time only.
    SetQueueOnce {
        id: QueueId,
        #[serde(default)]
        when_done: Option<WhenDone>,
    },
    /// Put a download in a queue (or move it within one) at `position`, or at
    /// the end. A download that is not running waits there.
    MoveToQueue {
        id: DownloadId,
        queue: QueueId,
        #[serde(default)]
        position: Option<usize>,
    },
    /// Take a download out of its queue but keep it in the list. A waiting
    /// download stays waiting, outside every queue, so no queue starts it;
    /// it starts by hand or goes back with `move_to_queue`. A running one
    /// keeps running.
    RemoveFromQueue {
        id: DownloadId,
    },
    /// Stop the countdown before sleeping or shutting down.
    CancelCountdown,
    /// Give a failed, canceled, interrupted or waiting download a new link.
    /// With `download`, use it now: the partial file is kept if the new link
    /// is for the same file, otherwise the reply is `refresh_mismatch`, unless
    /// `restart` says to start over anyway. Without `download`, wait for the
    /// next download the browser hands over (`refresh_waiting`).
    RefreshLink {
        id: DownloadId,
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
    /// The videos of a playlist or channel page (as `media_info`'s
    /// `playlist` listed them), for the window to pick from. No answer.
    PickVideos {
        page: String,
        playlist: udm_domain::media::Playlist,
        #[serde(default)]
        user_agent: Option<String>,
    },
    /// "Download gallery" in the browser's menu: the window opens the
    /// gallery dialog for `page`. No answer.
    PickGallery {
        page: String,
        #[serde(default)]
        user_agent: Option<String>,
    },
    /// List the pictures gallery-dl finds on the http or https `page`;
    /// answered with `gallery`. The window's, never the browser's.
    ReadGallery {
        request: u64,
        page: String,
        #[serde(default)]
        user_agent: Option<String>,
    },
    /// Install the official build of `yt_dlp` or `ffmpeg`, checked against
    /// its published checksum; answered with `tools`.
    InstallTool {
        tool: udm_domain::tools::Tool,
    },
    /// Where yt-dlp and ffmpeg are; answered with `tools`.
    ListTools,
    /// Look for a newer UDM release now; answered with `update`.
    CheckUpdate,
    /// Download the newest release's Setup, check it and run it, then
    /// quit so it can replace UDM's files. Failures come back as
    /// `update_failed`.
    InstallUpdate,
    /// Convert a finished video with ffmpeg: to an MP4, or its sound to an
    /// MP3, M4A, Opus or FLAC. The new file shows as a download of its own, with
    /// progress; the original stays unless `delete_original`.
    Convert {
        id: DownloadId,
        to: udm_domain::media::Target,
        #[serde(default)]
        delete_original: bool,
    },
    /// List the files inside a waiting zip download, reading only the end
    /// of the archive; answered with `zip_preview`.
    PreviewZip {
        id: DownloadId,
    },
    /// A page as the browser showed it, answering `render_page`: its HTML
    /// after its scripts ran and its address then, or nothing; and the
    /// addresses its scripts opened in new windows, which the browser
    /// closed.
    Rendered {
        request: u64,
        #[serde(default)]
        html: Option<String>,
        #[serde(default)]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        popups: Vec<String>,
    },
    /// The browser's cookies for a URL, answering `cookies_wanted`.
    Cookies {
        request: u64,
        #[serde(default)]
        cookies: Option<String>,
    },
    /// Change a stopped or finished download's link, referrer, description,
    /// login, or (when finished) its file's name and folder.
    SetProperties {
        id: DownloadId,
        changes: Properties,
    },
    /// Set or clear one download's own speed limit, in bytes per second. A
    /// running download changes speed at once. `remember` sets whether the
    /// limit stays when the download is stopped and resumed; missing leaves
    /// that as it was.
    SetSpeedLimit {
        id: DownloadId,
        #[serde(default)]
        bytes_per_sec: Option<u64>,
        #[serde(default)]
        remember: Option<bool>,
    },
    /// Stop the countdown and sleep, hibernate or shut down now.
    CountdownNow,
    /// Set or clear what one unfinished download does once it finishes.
    SetOnDone {
        id: DownloadId,
        #[serde(default)]
        on_done: Option<OnDone>,
    },
    /// Ask for the current settings; answered with `settings`.
    GetSettings,
    /// Replace the settings. Invalid values are refused with `error`; valid
    /// ones are saved and sent to every client as `settings`.
    SetSettings {
        settings: Settings,
    },
    /// What `udm-gui` was started with on the command line. With a window
    /// open it runs now; otherwise it waits for the window this process is
    /// about to open. Answered with `command_line`.
    CommandLine(CommandLine),
    /// How remote control stands; answered with `remote`.
    RemoteStatus,
    /// Show a new pairing code for a phone; answered with `remote`, which
    /// carries it, or `error` while remote control is not listening.
    StartPairing,
    /// Withdraw the pairing code shown; answered with `remote`.
    CancelPairing,
    /// Unpair a device at once; answered with `remote` (after `error` when
    /// there was no such device).
    RemoveDevice {
        id: DeviceId,
    },
    /// Ask a debrid service about the account, with `key` or else the
    /// stored key; answered with `debrid`. The window's.
    CheckDebrid {
        service: udm_domain::debrid::DebridService,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<Secret>,
    },
    /// Store the debrid account's API key for `service`, in place of any
    /// other; answered with `settings` to every client, or `error`. A blank
    /// key is not a message the daemon reads. The window's.
    SetDebridKey {
        service: udm_domain::debrid::DebridService,
        key: WireKey,
    },
    /// Remove the stored debrid key; answered with `settings` to every
    /// client, or `error`. The window's.
    RemoveDebridKey,
    /// Choose which files of torrent download `id` to download, by their
    /// numbers in the torrent; `error` when it cannot take them.
    SelectTorrentFiles {
        id: DownloadId,
        files: udm_domain::torrent::FileSelection,
    },
    /// Give torrent download `id` its own seeding limits, or follow the
    /// settings' again with none.
    SetSeedLimits {
        id: DownloadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limits: Option<udm_domain::torrent::SeedLimits>,
    },
    /// Stop seeding finished torrent download `id`.
    StopSeeding {
        id: DownloadId,
    },
    /// Send torrent download `id`'s files, as `torrent_info` to this client
    /// only (rows carry no file list); `error` when none are known.
    TorrentFiles {
        id: DownloadId,
    },
}

/// A message from the daemon to a client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
// Each message lives only while it is sent, so boxing the big variants would add churn for no gain.
#[allow(clippy::large_enum_variant)]
pub enum Outgoing {
    /// Reply to `hello`, announcing the daemon's identity and the
    /// automation API version it speaks (missing from daemons older than
    /// the API).
    Hello {
        name: String,
        version: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api: Option<u32>,
    },
    /// Reply to `ping`.
    Pong,
    /// Reply to `add` or `retry`, confirming the download was accepted. `id`
    /// identifies it in later events and requests.
    Ack {
        id: DownloadId,
        url: Link,
        status: String,
    },
    /// A download was queued. Sent to every client.
    Added {
        id: DownloadId,
        url: Link,
    },
    /// A download has begun; totals may be unknown for a non-ranged server.
    Started {
        id: DownloadId,
        url: Link,
        /// The file being written.
        path: String,
        total: Option<ByteCount>,
        resumable: bool,
        connections: usize,
        /// Where the data goes until it is complete, when not at `path` (a
        /// temporary folder).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        partial: Option<String>,
        /// The server's size, ETag and date for the file, when it said.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        validator: Option<Validator>,
        /// The ISO 3166 code of the server's country (`US`), from the
        /// address table UDM ships, when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        country: Option<udm_domain::country::CountryCode>,
    },
    /// Periodic progress for a running download.
    Progress {
        id: DownloadId,
        url: Link,
        done: ByteCount,
        total: Option<ByteCount>,
        /// For a stream: segments fetched and segments in all.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parts: Option<[u64; 2]>,
        /// For a download in byte ranges: each connection's range and state.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        segments: Option<Vec<SegmentInfo>>,
    },
    /// A download finished; `path` is the saved file.
    Done {
        id: DownloadId,
        url: Link,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// A download failed partway.
    Failed {
        id: DownloadId,
        url: Link,
        error: String,
    },
    /// A download was canceled; its partial file and resume sidecar are kept.
    Canceled {
        id: DownloadId,
        url: Link,
    },
    /// A download was removed from the list (removed or retried).
    Removed {
        id: DownloadId,
    },
    /// A torrent download's metadata is known: its name, files and pieces.
    /// Sent to every client.
    TorrentInfo {
        id: DownloadId,
        url: Link,
        info: udm_domain::torrent::TorrentInfo,
    },
    /// How a torrent download is doing: its phase, speeds, peers, trackers
    /// and pieces. About every second while it downloads or seeds.
    TorrentStats {
        id: DownloadId,
        url: Link,
        stats: udm_domain::torrent::TorrentStats,
    },
    /// A download's whole entry, replacing what clients had: used for the
    /// changes no other event covers (`confirming`, `waiting`, and back to
    /// `queued` once confirmed).
    Updated {
        item: DownloadEntry,
    },
    /// Reply to `list`: every download the daemon knows about, oldest first.
    Downloads {
        items: Vec<DownloadEntry>,
    },
    /// The current settings: a reply to `get_settings`, and sent to every
    /// client when they change.
    Settings {
        settings: Settings,
    },
    /// Ask the desktop window to come to the front (from a notification).
    /// Other clients ignore it.
    Show,
    /// Show the completion dialog of finished download `id` again: a new
    /// download had the same address. Other clients ignore it.
    ShowDone {
        id: DownloadId,
    },
    /// Every queue, in order: a reply to `list`, and sent to every client when
    /// one changes.
    Queues {
        items: Vec<Queue>,
    },
    /// Every package, oldest first: a reply to `list`, and sent to every
    /// client when one changes.
    Packages {
        items: Vec<Package>,
    },
    /// A finished queue or download will put the computer to sleep,
    /// hibernate it or shut it down in `seconds` unless someone sends
    /// `cancel_countdown`.
    Countdown {
        action: WhenDone,
        seconds: u32,
        /// The queue's name; empty when one download asked for it.
        queue: String,
        /// The finished download's file name, when one download asked for it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<String>,
    },
    CountdownCanceled,
    /// A finished queue or download asked UDM to exit: windows close.
    Quit,
    /// Which download waits for a new link from the browser, if any. Sent to
    /// every client when it changes, and after `downloads` in a `list` reply.
    RefreshWaiting {
        id: Option<DownloadId>,
    },
    /// A download's file name is taken and the setting says to ask: nothing
    /// was written, and it waits as `confirming` with `path`, the taken file,
    /// until a window answers with `confirm`.
    NameTaken {
        id: DownloadId,
        url: Link,
        path: String,
        total: Option<ByteCount>,
    },
    /// Reply to `refresh_link`: the new link is for another file, so the
    /// partial file cannot be kept. Send `refresh_link` again with
    /// `restart: true` to start over.
    RefreshMismatch {
        id: DownloadId,
        url: Link,
        reason: String,
        /// The reason's kind and values (see `wire`), for clients that
        /// translate it; `reason` is its English text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_kind: Option<serde_json::Value>,
    },
    /// Reply to `add_many`: how many downloads were added, and how many were
    /// skipped because their address was already in the list.
    AddedMany {
        count: usize,
        #[serde(default)]
        skipped: usize,
    },
    /// Reply to `probe_media`.
    MediaInfo {
        request: u64,
        info: MediaInfo,
    },
    /// What the server said about link `index` of a `probe_links` request:
    /// its size and `Content-Type` when it gave them, or why it could not
    /// be asked.
    LinkInfo {
        request: u64,
        index: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total: Option<ByteCount>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// The error's kind and values (see `wire`), for clients that
        /// translate it; `error` is its English text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_kind: Option<serde_json::Value>,
    },
    /// Links from a page for the window's link picker, as `pick_links` sent
    /// them. Sent to windows only.
    PickLinks {
        page: String,
        links: Vec<PageLink>,
        #[serde(default)]
        user_agent: Option<String>,
    },
    /// A playlist's videos for the window to pick from, as `pick_videos`
    /// sent them.
    PickVideos {
        page: String,
        playlist: udm_domain::media::Playlist,
        #[serde(default)]
        user_agent: Option<String>,
    },
    /// The browser asked for `page`'s gallery, as `pick_gallery` sent it.
    PickGallery {
        page: String,
        #[serde(default)]
        user_agent: Option<String>,
    },
    /// What `read_gallery` found: the pictures, or why there are none.
    Gallery {
        request: u64,
        page: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gallery: Option<udm_domain::gallery::Gallery>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// The error's kind and values (see `wire`); `error` is its
        /// English text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_kind: Option<serde_json::Value>,
    },
    /// Where yt-dlp and ffmpeg are. After `install_tool`, `done` names the
    /// tool, and `error` says why it was not installed, if it was not.
    Tools {
        tools: Vec<udm_domain::tools::ToolState>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        done: Option<udm_domain::tools::Tool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// The error's kind and values (see `wire`), for clients that
        /// translate it; `error` is its English text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_kind: Option<serde_json::Value>,
    },
    /// Where the download quota stands: bytes used in the window and the
    /// limit. Sent when it is nearly reached, when downloads pause for it
    /// (`paused`, with when they resume, in seconds since 1970), when they
    /// resume, and to a window that connects while it matters.
    Quota {
        used: ByteCount,
        limit: ByteCount,
        paused: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resumes_at: Option<u64>,
    },
    /// Whether the speed limit is lifted because nobody is using the
    /// computer (`Settings::speed_limit_only_while_active`). Sent when that
    /// changes, and to a window that connects while it is lifted.
    LimitLifted {
        lifted: bool,
    },
    /// Whether the browser extension is connected now and, when it is not,
    /// when one last was (seconds since 1970; `None` when none has since
    /// the daemon started). Sent to windows when a window connects, and
    /// when the first extension connects or the last one goes.
    BrowserLink {
        connected: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_seen: Option<u64>,
    },
    /// Asks the browser extension for its cookies for `url` (for a site
    /// crawl that signs in with them); it answers with `cookies`.
    CookiesWanted {
        request: u64,
        url: String,
    },
    /// What the update check found: a newer release, none (`latest` is
    /// this version or older), or why it could not look. Sent to the window
    /// that asked, and to every window when the weekly check finds one.
    Update {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        newer: Option<udm_domain::update::Release>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// The error's kind and values (see `wire`), for clients that
        /// translate it; `error` is its English text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_kind: Option<serde_json::Value>,
    },
    /// Update now could not get or start the new Setup, and why.
    UpdateFailed {
        error: String,
        /// The error's kind and values (see `wire`), for clients that
        /// translate it; `error` is its English text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_kind: Option<serde_json::Value>,
    },
    /// The files inside a zip download, answering `preview_zip`, or why
    /// they could not be listed.
    ZipPreview {
        id: DownloadId,
        #[serde(default)]
        entries: Vec<udm_domain::download::ZipEntry>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// The error's kind and values (see `wire`), for clients that
        /// translate it; `error` is its English text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_kind: Option<serde_json::Value>,
    },
    /// Asks the browser extension to open `url` in a background tab, let
    /// its scripts run, and answer with `rendered`.
    RenderPage {
        request: u64,
        url: String,
    },
    /// A crawl's progress: pages read, bytes read, files kept and links to
    /// files looked at so far, and the ids of the downloads it has added to
    /// a queue so far (with `download_found`), so the grabber can count the
    /// finished ones while they download.
    GrabProgress {
        pages: u64,
        bytes: ByteCount,
        found: usize,
        #[serde(default)]
        explored: u64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        added: Vec<DownloadId>,
    },
    /// A crawl ended: what it found, and why it stopped early, if it did.
    GrabDone {
        files: Vec<GrabFile>,
        pages: u64,
        #[serde(default)]
        explored: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stopped: Option<String>,
        /// The login page site's cookies the crawl used, so the files the
        /// window adds from that site carry them. Sent only to the window
        /// that started the crawl.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        login_cookies: Option<String>,
    },
    /// Any failure the client should hear about.
    Error {
        message: String,
        /// The error's kind and values (see `wire`), for clients that
        /// translate it; `message` is its English text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_kind: Option<serde_json::Value>,
    },
    /// Reply to `add` when the download was not taken, so the client can
    /// keep it: `message` says why.
    Rejected {
        url: Link,
        message: String,
        /// The error's kind and values (see `wire`), for clients that
        /// translate it; `message` is its English text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_kind: Option<serde_json::Value>,
    },
    /// Reply to `command_line`: whether a window was open to run it. When
    /// none was, the caller opens one and the command runs once it connects.
    CommandLine {
        window_open: bool,
    },
    /// Reply to `remote_status`, `start_pairing`, `cancel_pairing` and
    /// `remove_device`, to the client that asked only: where the
    /// remote-control page listens, the paired devices (never their
    /// secrets), and the pairing code shown.
    Remote {
        state: RemoteState,
    },
    /// Reply to `check_debrid`, to the client that asked only: the account
    /// the service sees, or why not.
    Debrid {
        check: udm_domain::debrid::DebridCheck,
    },
    /// To the windows: the debrid service refused the account when its
    /// hosts were read, so downloads go on without it. At most once an hour.
    DebridRefused {
        refused: udm_domain::debrid::Refused,
    },
}

/// Read one framed message from `reader`.
///
/// Returns `Ok(None)` on a clean EOF at a frame boundary (the browser closed
/// the port), `Ok(Some(bytes))` for a complete frame, and an error for a
/// partial frame or an oversized length.
pub fn read_message<R: Read>(reader: &mut R, max_bytes: u32) -> io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    if !read_exact_or_eof(reader, &mut len_buf)? {
        return Ok(None);
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
    let len: u32 =
        payload.len().try_into().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame exceeds u32 length"))?;
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
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "stream ended mid-frame"));
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
    use udm_domain::settings::{Duplicate, FileExists};

    #[test]
    fn a_browser_download_carries_its_whole_request() {
        let json = r#"{"op":"add","url":"https://cdn.x/a.zip",
            "request_headers":[["Accept","*/*"],["Sec-Fetch-Mode","navigate"]],
            "redirects":["https://x/get/a"],"size":1048576,"content_type":"application/zip",
            "browser_proxy":{"mode":"pac","pac_url":"http://wpad/proxy.pac","bypass":["<local>"]}}"#;
        let Incoming::Add(d) = serde_json::from_str::<Incoming>(json).unwrap() else {
            panic!("not an add");
        };
        assert_eq!(
            d.request_headers,
            [("Accept".to_string(), "*/*".to_string()), ("Sec-Fetch-Mode".into(), "navigate".into())]
        );
        assert_eq!(d.redirects, ["https://x/get/a"]);
        assert_eq!((d.size, d.content_type.as_deref()), (Some(ByteCount::new(1048576)), Some("application/zip")));
        let proxy = d.browser_proxy.clone().unwrap();
        assert_eq!(
            (proxy.mode, proxy.pac_url.as_deref(), proxy.bypass.len()),
            (BrowserProxyMode::Pac, Some("http://wpad/proxy.pac"), 1)
        );
        // Round trips, and an older add without any of it still reads.
        let back: Download = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(back.request_headers, d.request_headers);
        let old: Download = serde_json::from_str(r#"{"url":"https://x/a"}"#).unwrap();
        assert!(old.request_headers.is_empty() && old.redirects.is_empty() && old.browser_proxy.is_none());
        let text = serde_json::to_string(&old).unwrap();
        assert!(!text.contains("request_headers") && !text.contains("browser_proxy"), "{text}");
    }

    #[test]
    fn an_entry_without_a_validator_still_loads() {
        let e: DownloadEntry = serde_json::from_value(serde_json::json!({
            "id": 1, "url": "https://x/a", "path": null, "total": 5, "done": 5,
            "status": "done", "error": null, "resumable": true, "connections": 1,
        }))
        .unwrap();
        assert_eq!(e.validator, None);
        let json = serde_json::to_string(&e).unwrap();
        assert!(!json.contains("validator"));
    }

    #[test]
    fn an_add_carries_its_page_apart_from_the_referrer() {
        let raw = br#"{"op":"add","url":"https://x/a.zip","referrer":"https://x/frame","page":"https://x/article"}"#;
        let Incoming::Add(d) = serde_json::from_slice(raw).unwrap() else {
            panic!("not an add");
        };
        assert_eq!(d.referrer.as_deref(), Some("https://x/frame"));
        assert_eq!(d.page.as_deref(), Some("https://x/article"));
        // Older senders leave it out, and it is left out when unknown.
        let Incoming::Add(d) = serde_json::from_slice(br#"{"op":"add","url":"https://x/a.zip"}"#).unwrap() else {
            panic!("not an add");
        };
        assert_eq!(d.page, None);
        assert!(!serde_json::to_string(&d).unwrap().contains("page"));
        let e: DownloadEntry = serde_json::from_value(serde_json::json!({
            "id": 1, "url": "https://x/a", "path": null, "total": 5, "done": 5,
            "status": "done", "error": null, "resumable": true, "connections": 1,
            "referrer": "https://x/list",
        }))
        .unwrap();
        assert_eq!((e.referrer.as_deref(), e.page), (Some("https://x/list"), None));
    }

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

    /// Ids travel as plain numbers: messages from clients and daemons
    /// that predate the id types read the same and are written back
    /// unchanged.
    /// Addresses and file names travel as plain strings, and a request
    /// whose address UDM does not take still decodes, so the daemon can
    /// refuse it by name (`rejected`) and the browser keep the download.
    #[test]
    fn addresses_and_names_keep_their_wire_shape() {
        for raw in [
            r#"{"op":"add","url":"https://x/a.zip","filename":"a.zip","referrer":"https://p/"}"#,
            r#"{"op":"add","url":"javascript:void(0)","filename":"../a:b?.zip"}"#,
            r#"{"op":"add_many","items":[{"url":"HTTP://x/a"},{"url":"mailto:a@b"},{"url":""}],"queue":1}"#,
            r#"{"op":"refresh_link","id":3,"download":{"url":"file:///x"},"restart":false}"#,
            r#"{"op":"probe_links","request":2,"links":[{"url":"ftp://f/c.iso"},{"url":"data:,x"}]}"#,
        ] {
            let msg: Incoming = serde_json::from_str(raw).unwrap();
            let want: serde_json::Value = serde_json::from_str(raw).unwrap();
            assert_eq!(serde_json::to_value(&msg).unwrap(), want, "{raw}");
        }
        match serde_json::from_str(r#"{"op":"add","url":"javascript:x","filename":"a/b"}"#).unwrap() {
            Incoming::Add(d) => {
                assert_eq!((d.url.as_str(), d.url.is_address()), ("javascript:x", false));
                assert_eq!(d.filename.as_deref(), Some("a/b"));
            }
            other => panic!("expected add, got {other:?}"),
        }
        for raw in [
            r#"{"op":"added","id":1,"url":"https://x/a"}"#,
            r#"{"op":"canceled","id":1,"url":"not an address"}"#,
            r#"{"op":"failed","id":2,"url":"ftp://f/c","error":"gone"}"#,
            r#"{"op":"rejected","url":"javascript:x","message":"m"}"#,
            r#"{"op":"ack","id":4,"url":"https://x/b","status":"queued"}"#,
        ] {
            let msg: Outgoing = serde_json::from_str(raw).unwrap();
            let want: serde_json::Value = serde_json::from_str(raw).unwrap();
            assert_eq!(serde_json::to_value(&msg).unwrap(), want, "{raw}");
        }
    }

    #[test]
    fn ids_keep_their_wire_shape() {
        for raw in [
            r#"{"op":"cancel","id":7}"#,
            r#"{"op":"move_to_queue","id":3,"queue":2,"position":0}"#,
            r#"{"op":"start_queue","id":2}"#,
            r#"{"op":"add_many","items":[],"queue":4}"#,
        ] {
            let msg: Incoming = serde_json::from_str(raw).unwrap();
            let want: serde_json::Value = serde_json::from_str(raw).unwrap();
            assert_eq!(serde_json::to_value(&msg).unwrap(), want, "{raw}");
        }
        for raw in [
            r#"{"op":"removed","id":7}"#,
            r#"{"op":"refresh_waiting","id":4}"#,
            r#"{"op":"show_done","id":9}"#,
            r#"{"op":"grab_progress","pages":1,"bytes":2,"found":3,"explored":4,"added":[5,6]}"#,
        ] {
            let msg: Outgoing = serde_json::from_str(raw).unwrap();
            let want: serde_json::Value = serde_json::from_str(raw).unwrap();
            assert_eq!(serde_json::to_value(&msg).unwrap(), want, "{raw}");
        }
        match serde_json::from_str(r#"{"op":"move_to_queue","id":3,"queue":2}"#).unwrap() {
            Incoming::MoveToQueue { id, queue, position: None } => {
                assert_eq!((id, queue), (DownloadId::new(3), QueueId::new(2)));
            }
            other => panic!("expected move_to_queue, got {other:?}"),
        }
    }

    /// Byte counts (`ByteCount`) are plain numbers on the wire: messages
    /// carrying them read and write back unchanged.
    #[test]
    fn byte_counts_keep_their_wire_shape() {
        for raw in [
            r#"{"op":"started","id":1,"url":"u","path":"p","total":1048576,"resumable":true,"connections":4,"validator":{"size":1048576,"etag":"e"}}"#,
            r#"{"op":"progress","id":1,"url":"u","done":524288,"total":1048576,"segments":[{"start":0,"end":524287,"done":1000,"state":"receiving"}]}"#,
            r#"{"op":"name_taken","id":1,"url":"u","path":"p","total":5}"#,
            r#"{"op":"link_info","request":3,"index":0,"total":10}"#,
            r#"{"op":"quota","used":900,"limit":1000,"paused":false}"#,
            r#"{"op":"zip_preview","id":2,"entries":[{"name":"a.txt","size":120}]}"#,
        ] {
            let msg: Outgoing = serde_json::from_str(raw).unwrap();
            let want: serde_json::Value = serde_json::from_str(raw).unwrap();
            assert_eq!(serde_json::to_value(&msg).unwrap(), want, "{raw}");
        }
        let add: Incoming = serde_json::from_str(r#"{"op":"add","url":"https://x/a.zip","size":1048576}"#).unwrap();
        assert_eq!(serde_json::to_value(&add).unwrap()["size"], 1048576);
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

    /// The link picker's probes, and an answer that knows only the size.
    #[test]
    fn parse_probe_links_and_link_info() {
        let raw = br#"{"op":"probe_links","request":3,"links":[{"url":"https://x/a.zip"}]}"#;
        match serde_json::from_slice(raw).unwrap() {
            Incoming::ProbeLinks { request: 3, links } => assert_eq!(links[0].url, "https://x/a.zip"),
            other => panic!("expected probe_links, got {other:?}"),
        }
        let info: Outgoing = serde_json::from_str(r#"{"op":"link_info","request":3,"index":0,"total":10}"#).unwrap();
        assert!(
            matches!(info, Outgoing::LinkInfo { total: Some(t), content_type: None, error: None, .. } if t.get() == 10)
        );
    }

    /// The save-as dialog's answer to a taken name, and a `confirm` without
    /// one (the browser popup, older windows).
    #[test]
    fn parse_confirm_with_and_without_file_exists() {
        let raw = br#"{"op":"confirm","id":4,"file_exists":"overwrite"}"#;
        match serde_json::from_slice(raw).unwrap() {
            Incoming::Confirm(a) => {
                assert_eq!(a.id, DownloadId::new(4));
                assert!(a.start, "start defaults to true");
                assert_eq!(a.file_exists, Some(FileExists::Overwrite));
            }
            _ => panic!("expected confirm"),
        }
        match serde_json::from_slice(br#"{"op":"confirm","id":4}"#).unwrap() {
            Incoming::Confirm(a) => assert_eq!(a, Answer::start(DownloadId::new(4))),
            _ => panic!("expected confirm"),
        }
        match serde_json::from_slice(br#"{"op":"confirm","id":4,"duplicate":"existing"}"#).unwrap() {
            Incoming::Confirm(a) => assert_eq!(a.duplicate, Some(Duplicate::Existing)),
            _ => panic!("expected confirm"),
        }
        // The answer travels with its op, as the fields used to.
        let sent = serde_json::to_value(Incoming::Confirm(Answer {
            category: Some("Music".into()),
            ..Answer::start(DownloadId::new(7))
        }))
        .unwrap();
        assert_eq!(sent["op"], "confirm");
        assert_eq!(sent["id"], 7);
        assert_eq!(sent["category"], "Music");
        // A download without a choice keeps its JSON as it was.
        let d = Download { url: "https://x/a.zip".into(), ..Download::default() };
        assert!(!serde_json::to_string(&d).unwrap().contains("file_exists"));
    }
}
