//! What a client of the daemon asks it to do (`Command`), what it hears back
//! (`Event`), and the ports on each side: `DaemonClient` is the desktop
//! window's connection, and `Requests` is what the daemon's server hands
//! each client's commands to, with a `Reply` for the answers.
//!
//! These are plain values: how they travel (the framing, the JSON shapes,
//! the handshake) is the adapters' business, in `udm-infrastructure`.

mod failure;
pub mod service;
mod table;

pub use failure::Failure;
pub use table::{apply, change_of};

use std::fmt;
use std::sync::Arc;

use udm_domain::address::Link;
use udm_domain::bytes::ByteCount;
use udm_domain::debrid::{DebridCheck, DebridKey, DebridService};
use udm_domain::download::{
    Answer, CommandLine, Download, DownloadEntry, OnDone, PageLink, Properties, SegmentInfo, Validator, ZipEntry,
};
use udm_domain::gallery::Gallery;
use udm_domain::grab::{GrabFile, GrabRequest};
use udm_domain::id::{DeviceId, DownloadId, PackageId, QueueId};
use udm_domain::media::{MediaInfo, Playlist, Target};
use udm_domain::package::{Package, Packaging, Password};
use udm_domain::pairing::RemoteState;
use udm_domain::queue::{Queue, WhenDone};
use udm_domain::quota::QuotaNotice;
use udm_domain::settings::Settings;
use udm_domain::tools::{Tool, ToolState};
use udm_domain::torrent::{FileSelection, SeedLimits, TorrentInfo, TorrentStats};
use udm_domain::update::Release;

/// The version of the automation API: the messages `docs/command-line.md`
/// documents for other programs, and `docs/automation-api.schema.json`
/// describes. It goes up only when one of those changes in a way an older
/// program would misread; new optional fields and new messages keep it.
pub const AUTOMATION_API: u32 = 1;

/// A client connected to the daemon, numbered by the daemon's server in
/// the order they connect. Any number is valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct ClientId(u64);

impl ClientId {
    /// The client numbered `id`.
    pub const fn new(id: u64) -> Self {
        ClientId(id)
    }

    /// Its number.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A request to the daemon.
#[derive(Debug, Clone)]
// Each command lives only while it is sent, so boxing the big variants would add churn for no gain.
#[allow(clippy::large_enum_variant)]
pub enum Command {
    /// A client introduces itself. The desktop window names itself
    /// `window`; the browser extension, through its host, names nothing.
    Hello {
        version: Option<String>,
        client: Option<String>,
    },
    /// Answered with `Pong`.
    Ping,
    /// A command line from `udm-gui`; answered with `CommandLine`.
    CommandLine(CommandLine),
    /// Read a stream's playlist; answered with `MediaInfo`.
    ProbeMedia {
        request: u64,
        download: Download,
    },
    /// Links from a page, for the link picker.
    PickLinks {
        page: String,
        links: Vec<PageLink>,
        user_agent: Option<String>,
    },
    /// The videos of a playlist or channel page, for the window to pick
    /// from.
    PickVideos {
        page: String,
        playlist: Playlist,
        user_agent: Option<String>,
    },
    /// "Download gallery" in the browser's menu: the window opens the
    /// gallery dialog for `page`, which asks for its pictures with
    /// `ReadGallery`. No answer.
    PickGallery {
        page: String,
        user_agent: Option<String>,
    },
    /// List the pictures gallery-dl finds on the http or https `page`;
    /// answered with `Gallery`.
    ReadGallery {
        request: u64,
        page: String,
        user_agent: Option<String>,
    },
    /// Install the official build of yt-dlp or ffmpeg, checked against its
    /// published checksum; answered with `Tools`.
    InstallTool {
        tool: Tool,
    },
    /// Say where yt-dlp and ffmpeg are; answered with `Tools`.
    ListTools,
    /// The browser's answer to `RenderPage`: the page's HTML after its
    /// scripts ran and its address then, or nothing, and the addresses its
    /// scripts opened in new windows.
    Rendered {
        request: u64,
        html: Option<String>,
        url: Option<String>,
        popups: Vec<String>,
    },
    /// The browser's answer to `CookiesWanted`.
    Cookies {
        request: u64,
        cookies: Option<String>,
    },
    /// A download to hand off to the engine. A web address of a `.torrent`
    /// file (by its type or name) is fetched and added as the torrent, when
    /// the settings catch torrent files.
    Add(Download),
    /// A `.torrent` file's contents a client read from disk, to add as a
    /// download of its torrent; `download` says where to save it, its queue
    /// and the rest, as for `Add`. Answered as `Add` is.
    AddTorrentFile {
        metainfo: Vec<u8>,
        download: Download,
    },
    /// Choose which of torrent download `id`'s files to download: a running
    /// torrent changes at once, a finished one given more files starts
    /// again.
    SelectTorrentFiles {
        id: DownloadId,
        files: FileSelection,
    },
    /// Give torrent download `id` its own seeding limits, or `None` to
    /// follow the settings'.
    SetSeedLimits {
        id: DownloadId,
        limits: Option<SeedLimits>,
    },
    /// Stop seeding finished torrent download `id`.
    StopSeeding {
        id: DownloadId,
    },
    /// Send torrent download `id`'s files: answered with `TorrentInfo`, to
    /// this client only (rows carry no file list), or `Error`.
    TorrentFiles {
        id: DownloadId,
    },
    /// Many downloads at once, each added to wait in `queue` (the main
    /// queue when `None`) without starting, and kept as packages as
    /// `packaging` says. Answered with `AddedMany`.
    AddMany {
        items: Vec<Download>,
        queue: Option<QueueId>,
        packaging: Packaging,
    },
    /// Unpack a package's archives, with `password` for encrypted ones. Its
    /// state comes back in `Packages`.
    UnpackPackage {
        id: PackageId,
        password: Option<Password>,
    },
    /// Stop keeping a package; its downloads stay in the list.
    UngroupPackage {
        id: PackageId,
    },
    /// Crawl a site for files; progress comes as `GrabProgress` and the
    /// result as `GrabDone`.
    Grab(GrabRequest),
    /// Stop this client's crawl; `GrabDone` follows with what it found.
    CancelGrab,
    /// Ask each link's server for its type and size; each answer comes as
    /// `LinkInfo`.
    ProbeLinks {
        request: u64,
        links: Vec<Download>,
    },
    /// Stop asking about the links of the last `ProbeLinks`.
    StopProbeLinks,
    /// Cancel a queued or running download.
    Cancel {
        id: DownloadId,
    },
    /// Start a failed or canceled download again from its partial file.
    Retry {
        id: DownloadId,
    },
    /// Download a finished, failed or canceled download again from the
    /// start.
    Redownload {
        id: DownloadId,
    },
    /// Forget a download, and with `delete_files` recycle its files.
    Remove {
        id: DownloadId,
        delete_files: bool,
    },
    /// Ask for every download, the queues and the download waiting for a
    /// new link.
    List,
    /// Stop every queued or running download, keeping its partial file.
    PauseAll,
    /// Start again what `PauseAll` stopped, and any interrupted download.
    ResumeAll,
    /// Answer the save-as dialog of a `confirming` download, or start a
    /// `waiting` one.
    Confirm(Answer),
    /// Create a queue (`id` 0) or change one.
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
    /// Stop a queue; its running downloads wait in it again.
    StopQueue {
        id: QueueId,
    },
    /// What happens when this queue's current run ends, this time only.
    SetQueueOnce {
        id: QueueId,
        when_done: Option<WhenDone>,
    },
    /// Put a download in a queue (or move it within one) at `position`, or
    /// at the end.
    MoveToQueue {
        id: DownloadId,
        queue: QueueId,
        position: Option<usize>,
    },
    /// Take a download out of its queue but keep it in the list.
    RemoveFromQueue {
        id: DownloadId,
    },
    /// Stop the countdown before sleeping or shutting down.
    CancelCountdown,
    /// Give a download a new link: `download` now, or the next one the
    /// browser hands over. `restart` starts over when the link is for
    /// another file.
    RefreshLink {
        id: DownloadId,
        download: Option<Download>,
        restart: bool,
    },
    /// Stop waiting for a new link from the browser.
    CancelRefresh,
    /// Look for a newer UDM release now; answered with `Update`.
    CheckUpdate,
    /// Download and run the newest release's Setup; failures come back as
    /// `UpdateFailed`.
    InstallUpdate,
    /// Convert a finished video with ffmpeg.
    Convert {
        id: DownloadId,
        to: Target,
        delete_original: bool,
    },
    /// List the files inside a waiting zip download; answered with
    /// `ZipPreview`.
    PreviewZip {
        id: DownloadId,
    },
    /// Change a stopped or finished download's link, referrer,
    /// description, login, or its file's name and folder.
    SetProperties {
        id: DownloadId,
        changes: Properties,
    },
    /// Set or clear one download's own speed limit, and whether it stays
    /// through a stop.
    SetSpeedLimit {
        id: DownloadId,
        bytes_per_sec: Option<u64>,
        remember: Option<bool>,
    },
    /// Stop the countdown and sleep, hibernate or shut down now.
    CountdownNow,
    /// Set or clear what one unfinished download does once it finishes.
    SetOnDone {
        id: DownloadId,
        on_done: Option<OnDone>,
    },
    /// Ask for the current settings; answered with `Settings`.
    GetSettings,
    /// Replace the settings; answered with `Settings` or `Error`.
    SetSettings {
        settings: Settings,
    },
    /// Ask how remote control stands; answered with `Remote`.
    RemoteStatus,
    /// Show a new pairing code for a phone; answered with `Remote`, which
    /// carries it, or `Error` while the page is not served.
    StartPairing,
    /// Withdraw the pairing code shown; answered with `Remote`.
    CancelPairing,
    /// Unpair a device at once; answered with `Remote` or `Error`.
    RemoveDevice {
        id: DeviceId,
    },
    /// Ask a debrid service about the account, with `key` or else the
    /// stored key; answered with `Debrid`.
    CheckDebrid {
        service: DebridService,
        key: Option<DebridKey>,
    },
    /// Store the debrid account's API key for `service`, in place of any
    /// other; the settings then say which service a key is stored for.
    /// Answered with `Settings` to every client, or `Error`.
    SetDebridKey {
        service: DebridService,
        key: DebridKey,
    },
    /// Remove the stored debrid key. Answered with `Settings` to every
    /// client, or `Error`.
    RemoveDebridKey,
}

/// Why the connection to the daemon went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lost {
    /// The daemon closed it.
    Closed,
    /// It could not be made, or broke, and why.
    Failed(String),
}

/// What a client hears: the state of its connection, and the daemon's
/// replies and announcements.
#[derive(Debug, Clone, PartialEq)]
// Each event lives only while it is handled, so boxing the big variants would add churn for no gain.
#[allow(clippy::large_enum_variant)]
pub enum Event {
    /// The connection is made and the client has introduced itself.
    Connected,
    /// The connection went; nothing more comes until `connect` again.
    Lost(Lost),
    /// The daemon's answer to `Hello`, with the automation API version it
    /// speaks (`AUTOMATION_API`; none from a daemon older than the API).
    Hello {
        name: String,
        version: String,
        api: Option<u32>,
    },
    /// The answer to `Ping`.
    Pong,
    /// An `Add` or `Retry` was taken as download `id`.
    Ack {
        id: DownloadId,
        url: Link,
        status: String,
    },
    /// What a `ProbeMedia` stream offers.
    MediaInfo {
        request: u64,
        info: MediaInfo,
    },
    /// Ask the browser for its cookies for `url`; answered with `Cookies`.
    CookiesWanted {
        request: u64,
        url: String,
    },
    /// Ask the browser to render `url`; answered with `Rendered`.
    RenderPage {
        request: u64,
        url: String,
    },
    /// The answer to a `CommandLine`: whether an open window ran it.
    CommandLine {
        window_open: bool,
    },
    /// A download was queued.
    Added {
        id: DownloadId,
        url: Link,
    },
    /// A download has begun.
    Started {
        id: DownloadId,
        url: Link,
        path: String,
        total: Option<ByteCount>,
        resumable: bool,
        connections: usize,
        partial: Option<String>,
        validator: Option<Validator>,
        /// The country of the server it connected to, when known.
        country: Option<udm_domain::country::CountryCode>,
    },
    /// Progress of a running download.
    Progress {
        id: DownloadId,
        url: Link,
        done: ByteCount,
        total: Option<ByteCount>,
        parts: Option<[u64; 2]>,
        segments: Option<Vec<SegmentInfo>>,
    },
    /// A download finished; `path` is the saved file.
    Done {
        id: DownloadId,
        url: Link,
        path: String,
        note: Option<String>,
    },
    Failed {
        id: DownloadId,
        url: Link,
        error: String,
    },
    Canceled {
        id: DownloadId,
        url: Link,
    },
    /// A download left the list.
    Removed {
        id: DownloadId,
    },
    /// A torrent download's metadata is known: its name and files. Sent to
    /// every client when it becomes known, and to one that asks with
    /// `TorrentFiles`.
    TorrentInfo {
        id: DownloadId,
        url: Link,
        info: TorrentInfo,
    },
    /// How a torrent download is doing: its phase, speeds, peers, trackers
    /// and pieces. Every second or so while it downloads or seeds.
    TorrentStats {
        id: DownloadId,
        url: Link,
        stats: TorrentStats,
    },
    /// A download's whole entry, replacing what the client had.
    Updated {
        item: DownloadEntry,
    },
    /// Every download, oldest first.
    Downloads {
        items: Vec<DownloadEntry>,
    },
    /// The current settings.
    Settings {
        settings: Settings,
    },
    /// Come to the front (from a notification).
    Show,
    /// Show finished download `id`'s completion dialog again.
    ShowDone {
        id: DownloadId,
    },
    /// Every queue, the main queue first.
    Queues {
        items: Vec<Queue>,
    },
    /// Every package, oldest first.
    Packages {
        items: Vec<Package>,
    },
    /// The computer sleeps, hibernates or shuts down in `seconds` unless
    /// canceled. `queue` is empty and `file` names the download when one
    /// download asked for it.
    Countdown {
        action: WhenDone,
        seconds: u32,
        queue: String,
        file: Option<String>,
    },
    CountdownCanceled,
    /// UDM exits: windows close.
    Quit,
    /// Which download waits for a new link from the browser, if any.
    RefreshWaiting {
        id: Option<DownloadId>,
    },
    /// A download's file name is taken: it waits as `confirming` with
    /// `path`, the taken file, for the save-as dialog.
    NameTaken {
        id: DownloadId,
        url: Link,
        path: String,
        total: Option<ByteCount>,
    },
    /// The new link of a `RefreshLink` is for another file: `reason` is a
    /// `Failure::Mismatch` (or, from an older daemon, `Other`).
    RefreshMismatch {
        id: DownloadId,
        url: Link,
        reason: Failure,
    },
    /// Downloads an `AddMany` added, and those skipped as duplicates.
    AddedMany {
        count: usize,
        skipped: usize,
    },
    /// What link `index` of a `ProbeLinks` request's server said.
    LinkInfo {
        request: u64,
        index: usize,
        total: Option<ByteCount>,
        content_type: Option<String>,
        error: Option<Failure>,
    },
    /// Links from a page for the link picker.
    PickLinks {
        page: String,
        links: Vec<PageLink>,
        user_agent: Option<String>,
    },
    /// A playlist's videos for the window to pick from.
    PickVideos {
        page: String,
        playlist: Playlist,
        user_agent: Option<String>,
    },
    /// The browser asked for `page`'s gallery: the window opens its dialog.
    PickGallery {
        page: String,
        user_agent: Option<String>,
    },
    /// What `ReadGallery` found: the pictures, or why there are none.
    Gallery {
        request: u64,
        page: String,
        gallery: Option<Gallery>,
        error: Option<Failure>,
    },
    /// Where yt-dlp and ffmpeg are. After `InstallTool`, `done` is the
    /// tool it was for, and `error` why it was not installed, if it was not.
    Tools {
        tools: Vec<ToolState>,
        done: Option<Tool>,
        error: Option<Failure>,
    },
    /// Where the download quota stands.
    Quota {
        used: ByteCount,
        limit: ByteCount,
        paused: bool,
        resumes_at: Option<u64>,
    },
    /// Whether the speed limit is lifted because nobody is using the
    /// computer.
    LimitLifted {
        lifted: bool,
    },
    /// Whether the browser extension is connected, and when one last was.
    BrowserLink {
        connected: bool,
        last_seen: Option<u64>,
    },
    /// What the update check found: a newer release, none, or why not.
    Update {
        newer: Option<Release>,
        error: Option<Failure>,
    },
    UpdateFailed {
        error: Failure,
    },
    /// The files inside a zip download, or why they could not be listed.
    ZipPreview {
        id: DownloadId,
        entries: Vec<ZipEntry>,
        error: Option<Failure>,
    },
    /// A crawl's progress.
    GrabProgress {
        pages: u64,
        bytes: ByteCount,
        found: usize,
        explored: u64,
        added: Vec<DownloadId>,
    },
    /// A crawl ended.
    GrabDone {
        files: Vec<GrabFile>,
        pages: u64,
        explored: u64,
        stopped: Option<String>,
        login_cookies: Option<String>,
    },
    /// A failure the client should hear about.
    Error {
        error: Failure,
    },
    /// An `Add` the daemon did not take, and why.
    Rejected {
        url: Link,
        error: Failure,
    },
    /// How remote control stands: where the page listens, the paired
    /// devices, and the pairing code shown. Sent only to the client that
    /// asked.
    Remote {
        state: RemoteState,
    },
    /// What a debrid service said of the account, to the client that asked
    /// with `CheckDebrid`.
    Debrid {
        check: DebridCheck,
    },
    /// The debrid service refused the account when its hosts were read, so
    /// downloads go on without it; to the windows, at most once an hour.
    DebridRefused {
        refused: udm_domain::debrid::Refused,
    },
}

impl From<QuotaNotice> for Event {
    /// The `Quota` event that tells the clients `notice`.
    fn from(n: QuotaNotice) -> Self {
        Event::Quota { used: n.used, limit: n.limit, paused: n.paused, resumes_at: n.resumes_at }
    }
}

/// Why a one-off request to the daemon failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DaemonError {
    #[error("could not reach the daemon: {0}")]
    Connect(String),
    #[error("the daemon closed the connection")]
    Closed,
    #[error(transparent)]
    Refused(Failure),
}

/// The connection to the daemon.
pub trait DaemonClient: Send + Sync {
    /// Connect in the background, first restarting the daemon if `restart`,
    /// and hand every event to `on_event` until the connection is lost
    /// (which is the last event). Once connected the client introduces
    /// itself and asks for the list and the settings.
    fn connect(&self, restart: bool, on_event: Box<dyn FnMut(Event) + Send>);

    /// Send `command`. Returns false when not connected or sending failed.
    fn send(&self, command: Command) -> bool;

    /// Hand a command line to the daemon on a connection of its own,
    /// starting the daemon if needed. `Ok(true)` means an open window ran
    /// it; `Ok(false)` that it waits for the window about to open. An empty
    /// one asks an open window to come forward.
    fn run_command_line(&self, command: CommandLine) -> Result<bool, DaemonError>;
}

/// Where the daemon's answers to one client go. Cloned freely, so work on
/// other threads (a crawl, a probe) can answer when it is done.
#[derive(Clone)]
pub struct Reply(Arc<dyn Fn(Event) -> bool + Send + Sync>);

impl Reply {
    /// A reply that hands each event to `send`, which returns false once
    /// the client is gone.
    pub fn new(send: impl Fn(Event) -> bool + Send + Sync + 'static) -> Self {
        Reply(Arc::new(send))
    }

    /// Send `event` to the client. False when the client is gone.
    pub fn send(&self, event: Event) -> bool {
        (self.0)(event)
    }
}

/// The daemon's connected clients: which are windows and which the browser
/// extension, and how an event reaches them.
pub trait ClientList: Send + Sync {
    /// Whether no client is connected.
    fn is_empty(&self) -> bool;

    /// Whether a desktop window is connected.
    fn window_open(&self) -> bool;

    /// Whether client `client` is a desktop window.
    fn is_window(&self, client: ClientId) -> bool;

    /// Note that client `client` is a desktop window.
    fn add_window(&self, client: ClientId);

    /// Note that client `client` is the browser extension; the windows
    /// hear of it when it is the only one.
    fn browser_connected(&self, client: ClientId);

    /// Whether the browser extension is connected, and when one last was.
    fn browser_link(&self) -> Event;

    /// Send `event` to every client.
    fn broadcast(&self, event: Event);

    /// Send `event` to the desktop windows only.
    fn to_windows(&self, event: Event);
}

/// The download list as the clients see it.
pub trait DownloadFeed: Send + Sync {
    /// Apply `event` to the list and tell every client.
    fn announce(&self, event: Event);

    /// Run `then` on the whole list and the queues, while no change can
    /// slip in, so an answer built from them is followed by every later
    /// change.
    fn snapshot<T>(&self, then: impl FnOnce(Vec<DownloadEntry>, Vec<Queue>) -> T) -> T;
}

/// The countdown before a power action, for a window that joins during it.
pub trait Countdowns: Send + Sync {
    /// The countdown still running, as the event a joining window needs.
    fn countdown(&self) -> Option<Event>;
}

/// What the daemon does with its clients' commands: the port its server
/// drives.
pub trait Requests: Send + Sync {
    /// Handle `command` from client `client`; a direct answer goes to
    /// `reply`.
    fn handle(&self, client: ClientId, command: Command, reply: &Reply);

    /// Client `client` disconnected.
    fn gone(&self, client: ClientId);
}
