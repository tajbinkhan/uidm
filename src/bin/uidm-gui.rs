//! UIDM desktop window: lists downloads and controls them.
//!
//! It is one more daemon client. A background thread connects (starting the
//! daemon if needed, with the same mutual handshake as the host), asks for the
//! full list and the settings, then applies every event to a `DownloadTable`.
//! The window draws from a copy of that table each frame and sends `add`,
//! `cancel`, `retry`, `remove` and `set_settings` requests over the same
//! connection.
//!
//! It also puts an icon in the tray. Closing the window hides it there (unless
//! Options says to quit), and `--minimized` starts it hidden, which is how
//! "Start with Windows" runs it. A hidden window gets no frames, so anything
//! that must bring it back (the tray, a save-as or completion dialog, a click
//! on a notification) shows it through Win32 directly.

#![windows_subsystem = "windows"]

#[path = "gui/columns.rs"]
mod columns;
#[path = "gui/connections.rs"]
mod connections;
#[path = "gui/find.rs"]
mod find;
#[path = "gui/links.rs"]
mod links;
#[path = "gui/properties.rs"]
mod properties;
#[path = "gui/queues.rs"]
mod queues;
#[path = "gui/tools.rs"]
mod tools;
#[path = "gui/tray.rs"]
mod tray;
#[path = "gui/win.rs"]
mod win;

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::TcpStream;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use columns::Column;
use eframe::egui;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use uidm::app::MAX_MESSAGE_BYTES;
use uidm::ipc;
use uidm::protocol::{
    read_message, write_frame, Download, DownloadEntry, Incoming, Outgoing, Properties, Scan, SegmentInfo, Status,
};
use uidm::grab::{GrabFile, GrabRequest};
use uidm::queue::{Queue, WhenDone, MAIN_QUEUE};
use uidm::settings::{
    parse_extensions, CaptureKey, Category, Duplicate, SelectionButton, ServerLimit, SERVER_CONNECTIONS_RANGE, FileExists, Notify, ProxyMode, Scanner, Settings, SiteLogin, CAPTURE_MIN_SIZE_MAX_KB,
    CONNECTIONS_RANGE, GENERAL, MAX_CONCURRENT_RANGE, SPEED_LIMIT_RANGE_KBPS, USER_AGENT_PRESETS,
};
use uidm::state::DownloadTable;

/// Ignore speed samples closer together than this; they are too noisy.
const SPEED_MIN_INTERVAL: Duration = Duration::from_millis(400);

/// The window icon as raw 64x64 RGBA, made from extension/icons/128.png so no
/// image decoder is needed at run time.
const ICON_RGBA: &[u8] = include_bytes!("../../assets/icon-64.rgba");
const ICON_SIZE: u32 = 64;

/// How often the clipboard's change counter is checked.
const CLIPBOARD_POLL: Duration = Duration::from_millis(700);

/// Width of the file-name column; long names are truncated with a tooltip.
const NAME_WIDTH: f32 = 200.0;

/// A failure gets its dialog after this long, so a download the browser
/// extension takes back (and removes) right away gets none. The daemon waits
/// the same for its notifications.
const FAILED_GRACE: Duration = Duration::from_secs(1);

fn main() -> eframe::Result {
    let minimized = std::env::args().any(|a| a == "--minimized");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("UIDM")
            .with_visible(!minimized)
            .with_icon(egui::IconData {
                rgba: ICON_RGBA.to_vec(),
                width: ICON_SIZE,
                height: ICON_SIZE,
            })
            .with_inner_size([1120.0, 560.0])
            .with_min_inner_size([860.0, 320.0]),
        ..Default::default()
    };
    eframe::run_native(
        "UIDM",
        options,
        Box::new(move |cc| {
            if let Ok(handle) = cc.window_handle() {
                if let RawWindowHandle::Win32(h) = handle.as_raw() {
                    win::set_window(h.hwnd.get());
                }
            }
            let app = App::new(&cc.egui_ctx);
            // Without a tray icon there would be no way back to a hidden
            // window.
            if !tray::installed() {
                win::show();
            }
            Ok(Box::new(app))
        }),
    )
}

#[derive(Clone)]
enum Link {
    Connecting,
    Connected,
    Lost(String),
}

/// Bytes per second, smoothed across progress events.
struct Speed {
    at: Instant,
    done: u64,
    bytes_per_sec: f64,
}

/// State the connection thread writes and the window reads.
struct Shared {
    link: Link,
    table: DownloadTable,
    speeds: HashMap<u64, Speed>,
    /// The daemon's most recent `error` reply, shown in the status bar.
    last_error: Option<String>,
    /// The daemon's settings, once it has sent them.
    settings: Option<Settings>,
    /// True from sending `set_settings` until the daemon answers with
    /// `settings` or `error`.
    saving: bool,
    /// That answer, for the Options dialog to pick up.
    save_result: Option<Result<(), String>>,
    /// Downloads to show a completion dialog for, and whether each failed.
    completed: VecDeque<(u64, bool)>,
    /// The daemon's queues, the main queue first.
    queues: Vec<Queue>,
    /// A countdown before a finished queue sleeps or shuts down: the action,
    /// when it happens, and the queue's name.
    countdown: Option<(WhenDone, Instant, String)>,
    /// The download waiting for a new link from the browser, if any.
    refresh_waiting: Option<u64>,
    /// The daemon's answer that a new link is for another file: the
    /// download, the link and why.
    refresh_mismatch: Option<(u64, String, String)>,
    /// A passing message for the status bar, such as how many downloads a
    /// batch added.
    notice: Option<String>,
    /// The site crawl's news, for the grabber dialog to pick up.
    grab_events: VecDeque<GrabEvent>,
    /// A copied link to offer to download.
    clipboard: Option<String>,
    /// Each download's connections, from its latest progress.
    segments: HashMap<u64, Vec<SegmentInfo>>,
    /// Links from a page for the link picker, until the window opens it.
    picked_links: Option<links::LinksDialog>,
}

/// News of a site crawl.
enum GrabEvent {
    Progress(u64, u64, usize),
    Done(Vec<GrabFile>, u64, Option<String>),
}

impl Shared {
    fn dialog_mode(&self) -> bool {
        self.settings.as_ref().is_some_and(|s| s.notify == Notify::Dialog)
    }

    /// The tray icon's tooltip: how many downloads run, and how fast.
    fn tooltip(&self) -> String {
        let active = self.table.entries().filter(|e| e.status == Status::Downloading).count();
        let queued = self.table.entries().filter(|e| e.status == Status::Queued).count();
        let speed: f64 = self
            .speeds
            .iter()
            .filter(|(id, _)| self.table.get(**id).is_some_and(|e| e.status == Status::Downloading))
            .map(|(_, s)| s.bytes_per_sec)
            .sum();
        match (active, queued) {
            (0, 0) => "UIDM: no active downloads".to_string(),
            (0, q) => format!("UIDM: {q} queued"),
            (a, _) => format!(
                "UIDM: {a} downloading at {}/s",
                human_bytes(speed as u64)
            ),
        }
    }

    fn apply(&mut self, msg: &Outgoing) {
        self.table.apply(msg);
        match msg {
            Outgoing::Queues { items } => self.queues = items.clone(),
            Outgoing::Countdown { action, seconds, queue } => {
                let deadline = Instant::now() + Duration::from_secs(*seconds as u64);
                self.countdown = Some((*action, deadline, queue.clone()));
            }
            Outgoing::CountdownCanceled => self.countdown = None,
            Outgoing::RefreshWaiting { id } => self.refresh_waiting = *id,
            Outgoing::RefreshMismatch { id, url, reason } => {
                self.refresh_mismatch = Some((*id, url.clone(), reason.clone()));
            }
            Outgoing::Settings { settings } => {
                self.settings = Some(settings.clone());
                if self.saving {
                    self.saving = false;
                    self.save_result = Some(Ok(()));
                }
            }
            Outgoing::Error { message } if self.saving => {
                self.saving = false;
                self.save_result = Some(Err(message.clone()));
            }
            Outgoing::Progress { id, done, segments, .. } => {
                self.sample_speed(*id, *done);
                if let Some(s) = segments {
                    self.segments.insert(*id, s.clone());
                }
            }
            Outgoing::Started { id, .. } => {
                self.segments.remove(id);
            }
            Outgoing::PickLinks { page, links, user_agent } => {
                self.picked_links = Some(links::LinksDialog::new(page.clone(), links.clone(), user_agent.clone()));
            }
            Outgoing::AddedMany { count, skipped } => {
                let mut notice = match count {
                    1 => "Added 1 download, waiting in its queue.".to_string(),
                    n => format!("Added {n} downloads, waiting in their queue."),
                };
                match skipped {
                    0 => {}
                    1 => notice.push_str(" Skipped 1 already in the list."),
                    n => notice.push_str(&format!(" Skipped {n} already in the list.")),
                }
                self.notice = Some(notice);
            }
            Outgoing::GrabProgress { pages, bytes, found } => {
                self.grab_events.push_back(GrabEvent::Progress(*pages, *bytes, *found));
            }
            Outgoing::GrabDone { files, pages, stopped } => {
                self.grab_events.push_back(GrabEvent::Done(files.clone(), *pages, stopped.clone()));
            }
            Outgoing::Done { id, .. }
            | Outgoing::Failed { id, .. }
            | Outgoing::Canceled { id, .. }
            | Outgoing::Removed { id } => {
                self.speeds.remove(id);
                if matches!(msg, Outgoing::Removed { .. }) {
                    self.segments.remove(id);
                }
            }
            Outgoing::Error { message } => self.last_error = Some(message.clone()),
            _ => {}
        }
    }

    fn sample_speed(&mut self, id: u64, done: u64) {
        let now = Instant::now();
        let s = self.speeds.entry(id).or_insert(Speed {
            at: now,
            done,
            bytes_per_sec: 0.0,
        });
        let dt = now.duration_since(s.at);
        if dt < SPEED_MIN_INTERVAL || done < s.done {
            return;
        }
        let instant = (done - s.done) as f64 / dt.as_secs_f64();
        s.bytes_per_sec = if s.bytes_per_sec == 0.0 {
            instant
        } else {
            0.7 * s.bytes_per_sec + 0.3 * instant
        };
        s.at = now;
        s.done = done;
    }
}

/// What a click asked for, applied after drawing so no lock is held then.
enum Action {
    Add(String),
    Cancel(u64),
    /// Cancel a download from its save-as dialog; the second time for the
    /// same captured address, offer an exception.
    CancelSaveAs(u64),
    /// Keep a site (or with `site` false, one address) in the browser from
    /// now on, or only stop the offer, as the exception offer's buttons ask.
    Exception {
        url: String,
        add: Option<bool>,
        stop_offering: bool,
    },
    Retry(u64),
    /// Start a finished or stopped download over from the first byte.
    Redownload(u64),
    /// Remove downloads, asking first if Options says so.
    Remove(Vec<u64>),
    /// Remove downloads now, and maybe their files.
    RemoveNow {
        ids: Vec<u64>,
        delete_files: bool,
        /// Stop asking, and remember `delete_files`.
        dont_ask: bool,
    },
    ClearFinished,
    /// Open a finished download; asks first if its scan found a threat.
    Open(u64),
    /// Open a file without asking (after the threat warning).
    OpenAnyway(String),
    /// Close the completion dialog on top.
    DismissCompleted,
    ShowInFolder(String),
    Reconnect,
    OpenOptions,
    /// Open Options at the Connection tab with a new login for this host.
    AddLogin(String),
    SaveSettings(Settings),
    /// Send new settings without the Options dialog (the speed limit toggle).
    QuickSettings(Settings),
    /// Answer a save-as dialog, or start a waiting download.
    Confirm {
        id: u64,
        filename: Option<String>,
        folder: Option<String>,
        start: bool,
        queue: Option<u64>,
        /// What to do if the file name is taken; the setting decides when
        /// missing.
        file_exists: Option<FileExists>,
        /// For a duplicate address, what to do about the other download.
        duplicate: Option<Duplicate>,
    },
    /// Add a URL to the main queue without starting it.
    AddLater(String),
    StartQueue(u64),
    StopQueue(u64),
    SaveQueue(Queue),
    DeleteQueue(u64),
    /// Put a download in a queue, at a position or at the end.
    MoveToQueue {
        id: u64,
        queue: u64,
        position: Option<usize>,
    },
    /// What a queue does when this run ends, this time only.
    SetQueueOnce(u64, Option<WhenDone>),
    CancelCountdown,
    /// Open the New link dialog for a download.
    OpenRefresh(u64),
    /// Give a download this new link; `restart` starts over even if it is for
    /// another file.
    RefreshWith {
        id: u64,
        url: String,
        restart: bool,
    },
    /// Wait for the next download from the browser to give this one its link.
    RefreshFromBrowser(u64),
    CancelRefresh,
    /// Open the Queues dialog, on a queue if given.
    OpenQueues(Option<u64>),
    /// Add downloads to wait in a queue, and maybe start it.
    AddMany {
        items: Vec<Download>,
        queue: u64,
        start_queue: bool,
    },
    OpenBatch,
    /// Pick a list file to import.
    OpenImport,
    OpenExport,
    OpenGrabber,
    Grab(GrabRequest),
    CancelGrab,
    /// Tick or untick a download, for exporting.
    Select(u64, bool),
    OpenProperties(u64),
    OpenConnections(u64),
    /// Open the speed limit dialog for a download.
    OpenSpeedLimit(u64),
    /// Set or clear a download's own limit, in bytes per second.
    SetSpeedLimit(u64, Option<u64>),
    SetProperties {
        id: u64,
        changes: Properties,
    },
}

/// Which downloads the list shows.
#[derive(Clone, PartialEq)]
enum Filter {
    All,
    Category(String),
    /// A queue's downloads, in its order.
    Queue(u64),
}

struct App {
    shared: Arc<Mutex<Shared>>,
    /// The write half of the daemon connection, when connected.
    writer: Arc<Mutex<Option<TcpStream>>>,
    ctx: egui::Context,
    url_input: String,
    /// The Options dialog, while it is open.
    options: Option<OptionsDraft>,
    /// The save-as dialog for the oldest `confirming` download.
    confirm: Option<ConfirmDraft>,
    /// A file whose scan found a threat, waiting for "Open anyway": its path,
    /// name and the scanner's words.
    threat_prompt: Option<(String, String, String)>,
    /// The tray tooltip last set, so it is only updated when it changes.
    tooltip: String,
    /// Which downloads the list shows.
    filter: Filter,
    /// The Queues dialog, while it is open.
    queues_dialog: Option<queues::QueuesDialog>,
    /// The New link dialog: the download and the address typed so far.
    refresh: Option<(u64, String)>,
    /// The Batch dialog (also used for imports), while it is open.
    batch: Option<tools::BatchDialog>,
    export: Option<tools::ExportDialog>,
    grabber: Option<tools::GrabDialog>,
    /// A list file being imported, read on the picker's thread.
    import: tools::Loaded,
    /// Downloads ticked in the list.
    selected: HashSet<u64>,
    /// The find bar above the list, while it is open.
    find: Option<find::FindBar>,
    /// The list's columns and sort order, saved when they change.
    layout: columns::Layout,
    /// The Columns dialog is open.
    columns_dialog: bool,
    /// Open Properties dialogs, one per download.
    properties: Vec<properties::PropertiesDialog>,
    /// The confirmation before removing downloads.
    remove_prompt: Option<RemovePrompt>,
    /// The speed limit dialog for one download.
    limit_dialog: Option<LimitDialog>,
    /// Downloads whose Connections dialog is open.
    connection_dialogs: Vec<u64>,
    /// The link picker, while it is open.
    links: Option<links::LinksDialog>,
    /// Captured addresses whose save-as dialog was canceled once.
    canceled_once: HashSet<String>,
    /// The offer to stop capturing an address, and its "Don't show again" box.
    exception_offer: Option<(String, bool)>,
}

/// A download's own speed limit being edited.
struct LimitDialog {
    id: u64,
    enabled: bool,
    value: f64,
    unit: SpeedUnit,
}

/// Downloads about to be removed, and the confirmation's boxes.
struct RemovePrompt {
    ids: Vec<u64>,
    delete_files: bool,
    dont_ask: bool,
}

/// A folder chosen in the picker, delivered by its thread.
type Picked = Arc<Mutex<Option<PathBuf>>>;

/// A "Browse..." button that opens the folder picker on its own thread (it
/// blocks) and puts the choice in `slot`.
fn browse_button(ui: &mut egui::Ui, title: &'static str, start: &str, slot: &Picked) {
    if ui.button("Browse...").clicked() {
        let slot = Arc::clone(slot);
        let ctx = ui.ctx().clone();
        let start = start.trim().to_string();
        thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title(title);
            if !start.is_empty() {
                dialog = dialog.set_directory(&start);
            }
            if let Some(folder) = dialog.pick_folder() {
                *slot.lock().unwrap() = Some(folder);
                ctx.request_repaint();
            }
        });
    }
}

/// Copy a picked folder into `field`, if one arrived.
fn take_picked(slot: &Picked, field: &mut String) {
    if let Some(folder) = slot.lock().unwrap().take() {
        *field = folder.display().to_string();
    }
}

/// The save-as dialog's fields.
struct ConfirmDraft {
    id: u64,
    /// The queue "Download later" puts it in.
    later_queue: u64,
    filename: String,
    folder: String,
    picked: Picked,
    /// Under the Ask setting, what to do when the name is taken.
    when_taken: FileExists,
    /// For a duplicate address, what to do about the other download.
    duplicate: Duplicate,
}

/// Units the speed limit can be entered in.
#[derive(Clone, Copy, PartialEq)]
enum SpeedUnit {
    Kb,
    Mb,
}

impl SpeedUnit {
    fn kb(self) -> f64 {
        match self {
            SpeedUnit::Kb => 1.0,
            SpeedUnit::Mb => 1024.0,
        }
    }

    fn label(self) -> &'static str {
        match self {
            SpeedUnit::Kb => "KB/s",
            SpeedUnit::Mb => "MB/s",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum OptionsTab {
    General,
    Categories,
    Capture,
    Notifications,
    Connection,
}

/// One site login being edited. An empty password keeps the stored one.
struct LoginDraft {
    site: String,
    username: String,
    password: String,
    /// Added in this dialog, so it has no stored password yet.
    new: bool,
}

/// One category being edited.
struct CategoryDraft {
    name: String,
    extensions: String,
    /// Empty means a folder named after the category.
    folder: String,
    /// Sites it is limited to, comma-separated; empty means every site.
    sites: String,
    remember_folder: bool,
    /// Kept as it is; only the save-as dialog changes it.
    last_folder: Option<String>,
    picked: Picked,
}

/// The Options dialog's fields, edited freely and checked on Save.
struct OptionsDraft {
    tab: OptionsTab,
    close_to_tray: bool,
    start_with_windows: bool,
    notify: Notify,
    sound_on_done: bool,
    sound_on_failed: bool,
    scan_after_download: bool,
    scanner: Scanner,
    scanner_command: String,
    proxy_mode: ProxyMode,
    proxy_http: String,
    proxy_https: String,
    proxy_socks5: String,
    proxy_username: String,
    /// A new proxy password, when `proxy_password_changed`.
    proxy_password: String,
    proxy_password_changed: bool,
    use_browser_proxy: bool,
    logins: Vec<LoginDraft>,
    /// Empty means the Downloads folder.
    folder: String,
    connections: usize,
    max_concurrent: usize,
    limit_enabled: bool,
    limit_value: f64,
    limit_unit: SpeedUnit,
    resume_at_startup: bool,
    file_exists: FileExists,
    if_duplicate: Duplicate,
    ask_where_to_save: bool,
    categories: Vec<CategoryDraft>,
    capture_types: String,
    capture_min_size_kb: u64,
    /// One site per line.
    capture_excluded_sites: String,
    clipboard_monitor: bool,
    video_button: bool,
    /// One site per line.
    video_button_excluded_sites: String,
    ffmpeg_path: String,
    /// ffmpeg.exe chosen with Browse.
    ffmpeg_picked: Picked,
    confirm_remove: bool,
    remove_deletes_files: bool,
    server_file_date: bool,
    manual_user_agent: String,
    /// Empty writes partial files in place.
    temp_dir: String,
    temp_picked: Picked,
    capture_skip_key: CaptureKey,
    capture_force_key: CaptureKey,
    /// One address per line.
    capture_excluded_urls: String,
    offer_exceptions: bool,
    selection_button: SelectionButton,
    /// One site per line.
    selection_button_sites: String,
    /// Each server and its most connections.
    server_limits: Vec<(String, usize)>,
    error: Option<String>,
    picked: Picked,
}

/// The sites in a one-per-line text field.
fn sites(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.trim().to_ascii_lowercase())
        .filter(|l| !l.is_empty())
        .collect()
}

impl OptionsDraft {
    fn new(s: &Settings) -> Self {
        let kbps = s.speed_limit_kbps;
        let limit_unit = if kbps >= 1024 && kbps.is_multiple_of(1024) {
            SpeedUnit::Mb
        } else {
            SpeedUnit::Kb
        };
        OptionsDraft {
            tab: OptionsTab::General,
            close_to_tray: s.close_to_tray,
            start_with_windows: s.start_with_windows,
            notify: s.notify,
            sound_on_done: s.sound_on_done,
            sound_on_failed: s.sound_on_failed,
            scan_after_download: s.scan_after_download,
            scanner: s.scanner,
            scanner_command: s.scanner_command.clone(),
            proxy_mode: s.proxy_mode,
            proxy_http: s.proxy_http.clone(),
            proxy_https: s.proxy_https.clone(),
            proxy_socks5: s.proxy_socks5.clone(),
            proxy_username: s.proxy_username.clone(),
            proxy_password: String::new(),
            proxy_password_changed: false,
            use_browser_proxy: s.use_browser_proxy,
            logins: s
                .site_logins
                .iter()
                .map(|l| LoginDraft {
                    site: l.site.clone(),
                    username: l.username.clone(),
                    password: String::new(),
                    new: false,
                })
                .collect(),
            folder: s.download_dir.clone().unwrap_or_default(),
            connections: s.connections,
            max_concurrent: s.max_concurrent,
            limit_enabled: s.speed_limit_enabled,
            limit_value: kbps as f64 / limit_unit.kb(),
            limit_unit,
            resume_at_startup: s.resume_at_startup,
            file_exists: s.file_exists,
            if_duplicate: s.if_duplicate,
            ask_where_to_save: s.ask_where_to_save,
            categories: s
                .categories
                .iter()
                .map(|c| CategoryDraft {
                    name: c.name.clone(),
                    extensions: c.extensions.join(", "),
                    folder: c.folder.clone().unwrap_or_default(),
                    sites: c.sites.join(", "),
                    remember_folder: c.remember_folder,
                    last_folder: c.last_folder.clone(),
                    picked: Picked::default(),
                })
                .collect(),
            capture_types: s.capture_types.join(", "),
            capture_min_size_kb: s.capture_min_size_kb,
            capture_excluded_sites: s.capture_excluded_sites.join("\n"),
            clipboard_monitor: s.clipboard_monitor,
            video_button: s.video_button,
            video_button_excluded_sites: s.video_button_excluded_sites.join("\n"),
            ffmpeg_path: s.ffmpeg_path.clone(),
            ffmpeg_picked: Picked::default(),
            confirm_remove: s.confirm_remove,
            remove_deletes_files: s.remove_deletes_files,
            server_file_date: s.server_file_date,
            manual_user_agent: s.manual_user_agent.clone(),
            temp_dir: s.temp_dir.clone().unwrap_or_default(),
            temp_picked: Picked::default(),
            capture_skip_key: s.capture_skip_key,
            capture_force_key: s.capture_force_key,
            capture_excluded_urls: s.capture_excluded_urls.join("\n"),
            offer_exceptions: s.offer_exceptions,
            selection_button: s.selection_button,
            selection_button_sites: s.selection_button_sites.join("\n"),
            server_limits: s.server_limits.iter().map(|l| (l.site.clone(), l.max)).collect(),
            error: None,
            picked: Picked::default(),
        }
    }

    /// The settings these fields describe, checked the same way the daemon
    /// checks them.
    fn to_settings(&self) -> Result<Settings, String> {
        let optional = |text: &str| {
            let t = text.trim();
            (!t.is_empty()).then(|| t.to_string())
        };
        let settings = Settings {
            download_dir: optional(&self.folder),
            connections: self.connections,
            max_concurrent: self.max_concurrent,
            speed_limit_enabled: self.limit_enabled,
            speed_limit_kbps: (self.limit_value * self.limit_unit.kb()).round() as u64,
            resume_at_startup: self.resume_at_startup,
            close_to_tray: self.close_to_tray,
            start_with_windows: self.start_with_windows,
            notify: self.notify,
            sound_on_done: self.sound_on_done,
            sound_on_failed: self.sound_on_failed,
            scan_after_download: self.scan_after_download,
            scanner: self.scanner,
            scanner_command: self.scanner_command.trim().to_string(),
            proxy_mode: self.proxy_mode,
            proxy_http: self.proxy_http.trim().to_string(),
            proxy_https: self.proxy_https.trim().to_string(),
            proxy_socks5: self.proxy_socks5.trim().to_string(),
            proxy_username: self.proxy_username.trim().to_string(),
            proxy_password: self.proxy_password_changed.then(|| self.proxy_password.clone()),
            use_browser_proxy: self.use_browser_proxy,
            site_logins: self
                .logins
                .iter()
                .map(|l| SiteLogin {
                    site: l.site.trim().to_ascii_lowercase(),
                    username: l.username.trim().to_string(),
                    password: (!l.password.is_empty()).then(|| l.password.clone()),
                })
                .collect(),
            file_exists: self.file_exists,
            if_duplicate: self.if_duplicate,
            ask_where_to_save: self.ask_where_to_save,
            categories: self
                .categories
                .iter()
                .map(|c| Category {
                    name: c.name.trim().to_string(),
                    extensions: parse_extensions(&c.extensions),
                    folder: optional(&c.folder),
                    sites: c
                        .sites
                        .split([',', ' ', ';'])
                        .map(|s| s.trim().to_ascii_lowercase())
                        .filter(|s| !s.is_empty())
                        .collect(),
                    remember_folder: c.remember_folder,
                    last_folder: c.last_folder.clone().filter(|_| c.remember_folder),
                })
                .collect(),
            capture_types: parse_extensions(&self.capture_types),
            capture_min_size_kb: self.capture_min_size_kb,
            capture_excluded_sites: sites(&self.capture_excluded_sites),
            clipboard_monitor: self.clipboard_monitor,
            video_button: self.video_button,
            video_button_excluded_sites: sites(&self.video_button_excluded_sites),
            ffmpeg_path: self.ffmpeg_path.trim().to_string(),
            confirm_remove: self.confirm_remove,
            remove_deletes_files: self.remove_deletes_files,
            server_file_date: self.server_file_date,
            manual_user_agent: self.manual_user_agent.trim().to_string(),
            temp_dir: optional(&self.temp_dir),
            capture_skip_key: self.capture_skip_key,
            capture_force_key: self.capture_force_key,
            capture_excluded_urls: self
                .capture_excluded_urls
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect(),
            offer_exceptions: self.offer_exceptions,
            selection_button: self.selection_button,
            selection_button_sites: sites(&self.selection_button_sites),
            server_limits: self
                .server_limits
                .iter()
                .map(|(site, max)| ServerLimit {
                    site: site.trim().to_ascii_lowercase(),
                    max: *max,
                })
                .collect(),
        };
        settings.validate()?;
        if let Some(l) = self.logins.iter().find(|l| l.new && l.password.is_empty()) {
            return Err(format!("The new login for {} needs a password.", l.site.trim()));
        }
        Ok(settings)
    }
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        let app = App {
            shared: Arc::new(Mutex::new(Shared {
                link: Link::Connecting,
                table: DownloadTable::new(),
                speeds: HashMap::new(),
                last_error: None,
                settings: None,
                saving: false,
                save_result: None,
                completed: VecDeque::new(),
                queues: Vec::new(),
                countdown: None,
                refresh_waiting: None,
                refresh_mismatch: None,
                notice: None,
                grab_events: VecDeque::new(),
                clipboard: None,
                segments: HashMap::new(),
                picked_links: None,
            })),
            writer: Arc::new(Mutex::new(None)),
            ctx: ctx.clone(),
            url_input: String::new(),
            options: None,
            confirm: None,
            threat_prompt: None,
            tooltip: String::new(),
            filter: Filter::All,
            queues_dialog: None,
            refresh: None,
            batch: None,
            export: None,
            grabber: None,
            import: tools::Loaded::default(),
            selected: HashSet::new(),
            find: None,
            layout: columns::Layout::load(),
            columns_dialog: false,
            properties: Vec::new(),
            remove_prompt: None,
            limit_dialog: None,
            connection_dialogs: Vec::new(),
            links: None,
            canceled_once: HashSet::new(),
            exception_offer: None,
        };
        app.connect();
        app.install_tray();
        app.watch_clipboard();
        app
    }

    /// Watch the clipboard for download links while Options asks for it,
    /// and offer each new one once. Polls the clipboard's change counter,
    /// which costs nothing, and reads the text only when it changes.
    fn watch_clipboard(&self) {
        let shared = Arc::clone(&self.shared);
        let ctx = self.ctx.clone();
        thread::spawn(move || {
            let mut seen = win::clipboard_sequence();
            let mut last_offered: Option<String> = None;
            loop {
                thread::sleep(CLIPBOARD_POLL);
                let now = win::clipboard_sequence();
                if now == seen {
                    continue;
                }
                seen = now;
                let types = match &shared.lock().unwrap().settings {
                    Some(s) if s.clipboard_monitor => s.capture_types.clone(),
                    _ => continue,
                };
                let Some(text) = win::clipboard_text() else { continue };
                let Some(url) = uidm::batch::clipboard_link(&text, &types) else { continue };
                if last_offered.as_deref() == Some(url.as_str()) {
                    continue;
                }
                last_offered = Some(url.clone());
                shared.lock().unwrap().clipboard = Some(url);
                win::show();
                ctx.request_repaint();
            }
        });
    }

    /// Put UIDM in the tray. Its menu acts at once, without waiting for a
    /// frame, since the window may be hidden.
    fn install_tray(&self) {
        let shared = Arc::clone(&self.shared);
        let writer = Arc::clone(&self.writer);
        let ctx = self.ctx.clone();
        let on_command = Arc::new(move |command: tray::Command| {
            match command {
                tray::Command::Open => win::show(),
                tray::Command::AddFromClipboard => {
                    let text = win::clipboard_text().unwrap_or_default();
                    let url = text.trim();
                    if is_web_url(url) {
                        send(
                            &writer,
                            &Incoming::Add(Download {
                                url: url.to_string(),
                                ..Download::default()
                            }),
                        );
                    } else {
                        shared.lock().unwrap().last_error =
                            Some("The clipboard holds no http or https link.".into());
                        win::show();
                    }
                }
                tray::Command::PauseAll => {
                    send(&writer, &Incoming::PauseAll);
                }
                tray::Command::ResumeAll => {
                    send(&writer, &Incoming::ResumeAll);
                }
                tray::Command::ToggleSpeedLimit => {
                    let settings = shared.lock().unwrap().settings.clone();
                    if let Some(s) = settings {
                        let on = !s.speed_limit_enabled;
                        tray::set_speed_limit_checked(on);
                        send(
                            &writer,
                            &Incoming::SetSettings {
                                settings: Settings {
                                    speed_limit_enabled: on,
                                    ..s
                                },
                            },
                        );
                    }
                }
                // The daemon keeps downloading and exits on its own once idle.
                tray::Command::Exit => std::process::exit(0),
            }
            ctx.request_repaint();
        });
        let shared = Arc::clone(&self.shared);
        let tooltip = Arc::new(move || shared.lock().unwrap().tooltip());
        if let Err(e) = tray::install(ICON_RGBA.to_vec(), ICON_SIZE, on_command, tooltip) {
            self.shared.lock().unwrap().last_error = Some(format!("No tray icon: {e}"));
        }
    }

    /// Connect on a background thread, then read events until the daemon goes.
    fn connect(&self) {
        let shared = Arc::clone(&self.shared);
        let writer = Arc::clone(&self.writer);
        let ctx = self.ctx.clone();
        shared.lock().unwrap().link = Link::Connecting;

        thread::spawn(move || {
            let set_link = |link: Link| {
                shared.lock().unwrap().link = link;
                ctx.request_repaint();
            };
            let stream = match ipc::connect_or_spawn() {
                Ok(s) => s,
                Err(e) => return set_link(Link::Lost(e.to_string())),
            };
            let mut reader = match stream.try_clone() {
                Ok(r) => r,
                Err(e) => return set_link(Link::Lost(e.to_string())),
            };
            *writer.lock().unwrap() = Some(stream);
            set_link(Link::Connected);
            send(
                &writer,
                &Incoming::Hello {
                    version: Some(env!("CARGO_PKG_VERSION").to_string()),
                    client: Some("window".to_string()),
                },
            );
            send(&writer, &Incoming::List);
            send(&writer, &Incoming::GetSettings);

            loop {
                match read_message(&mut reader, MAX_MESSAGE_BYTES) {
                    Ok(Some(raw)) => {
                        if let Ok(msg) = serde_json::from_slice::<Outgoing>(&raw) {
                            on_message(&shared, &ctx, &msg);
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            *writer.lock().unwrap() = None;
            set_link(Link::Lost("the daemon closed the connection".into()));
        });
    }

    fn perform(&mut self, action: Action) {
        match action {
            Action::Add(url) => {
                send(
                    &self.writer,
                    &Incoming::Add(Download {
                        url,
                        ..Download::default()
                    }),
                );
            }
            Action::CancelSaveAs(id) => {
                send(&self.writer, &Incoming::Cancel { id });
                let (entry, settings) = {
                    let s = self.shared.lock().unwrap();
                    (s.table.get(id).cloned(), s.settings.clone())
                };
                let offer = settings.is_some_and(|s| s.offer_exceptions);
                if let Some(e) = entry.filter(|e| e.from_browser && offer) {
                    if !self.canceled_once.insert(e.url.clone()) {
                        self.canceled_once.remove(&e.url);
                        self.exception_offer = Some((e.url, false));
                    }
                }
            }
            Action::Exception { url, add, stop_offering } => {
                let settings = self.shared.lock().unwrap().settings.clone();
                let Some(mut s) = settings else {
                    return;
                };
                match add {
                    Some(true) => {
                        let host = url::Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase));
                        if let Some(host) = host.filter(|h| !s.capture_excluded_sites.contains(h)) {
                            s.capture_excluded_sites.push(host);
                        }
                    }
                    Some(false) if !s.capture_excluded_urls.contains(&url) => s.capture_excluded_urls.push(url),
                    _ => {}
                }
                if stop_offering {
                    s.offer_exceptions = false;
                }
                self.perform(Action::QuickSettings(s));
            }
            Action::Cancel(id) => {
                send(&self.writer, &Incoming::Cancel { id });
            }
            Action::Retry(id) => {
                send(&self.writer, &Incoming::Retry { id });
            }
            Action::Redownload(id) => {
                send(&self.writer, &Incoming::Redownload { id });
            }
            Action::Remove(ids) => self.ask_remove(ids),
            Action::RemoveNow {
                ids,
                delete_files,
                dont_ask,
            } => {
                for id in &ids {
                    send(&self.writer, &Incoming::Remove { id: *id, delete_files });
                    self.selected.remove(id);
                }
                let settings = self.shared.lock().unwrap().settings.clone();
                if let Some(s) = settings.filter(|_| dont_ask) {
                    self.perform(Action::QuickSettings(Settings {
                        confirm_remove: false,
                        remove_deletes_files: delete_files,
                        ..s
                    }));
                }
            }
            Action::ClearFinished => {
                let done: Vec<u64> = {
                    let s = self.shared.lock().unwrap();
                    s.table
                        .entries()
                        .filter(|e| e.status == Status::Done)
                        .map(|e| e.id)
                        .collect()
                };
                self.ask_remove(done);
            }
            Action::Open(id) => {
                let entry = self.shared.lock().unwrap().table.get(id).cloned();
                if let Some(e) = entry {
                    if let Some(path) = e.path.clone() {
                        match &e.scan {
                            Some(Scan::Threat { detail }) => {
                                self.threat_prompt = Some((path, display_name(&e), detail.clone()));
                            }
                            _ => open_file(&path),
                        }
                    }
                }
            }
            Action::OpenAnyway(path) => {
                self.threat_prompt = None;
                open_file(&path);
            }
            Action::DismissCompleted => {
                self.shared.lock().unwrap().completed.pop_front();
            }
            Action::ShowInFolder(path) => {
                // Explorer parses /select itself, so pass the argument raw with
                // the path quoted. Windows paths cannot contain a double quote.
                let _ = Command::new("explorer")
                    .raw_arg(format!("/select,\"{path}\""))
                    .spawn();
            }
            Action::Reconnect => self.connect(),
            Action::OpenOptions => {
                if let Some(settings) = &self.shared.lock().unwrap().settings {
                    self.options = Some(OptionsDraft::new(settings));
                }
            }
            Action::AddLogin(host) => {
                if let Some(settings) = &self.shared.lock().unwrap().settings {
                    let mut draft = OptionsDraft::new(settings);
                    draft.tab = OptionsTab::Connection;
                    if !draft.logins.iter().any(|l| l.site.eq_ignore_ascii_case(&host)) {
                        draft.logins.push(LoginDraft {
                            site: host,
                            username: String::new(),
                            password: String::new(),
                            new: true,
                        });
                    }
                    self.options = Some(draft);
                }
            }
            Action::SaveSettings(settings) => {
                {
                    let mut s = self.shared.lock().unwrap();
                    s.saving = true;
                    s.save_result = None;
                }
                if !send(&self.writer, &Incoming::SetSettings { settings }) {
                    let mut s = self.shared.lock().unwrap();
                    s.saving = false;
                    s.save_result = Some(Err("not connected to the UIDM daemon".into()));
                }
            }
            Action::QuickSettings(settings) => {
                send(&self.writer, &Incoming::SetSettings { settings });
            }
            Action::Confirm {
                id,
                filename,
                folder,
                start,
                queue,
                file_exists,
                duplicate,
            } => {
                send(
                    &self.writer,
                    &Incoming::Confirm {
                        id,
                        filename,
                        folder,
                        start,
                        queue,
                        file_exists,
                        duplicate,
                    },
                );
            }
            Action::AddLater(url) => {
                send(
                    &self.writer,
                    &Incoming::Add(Download {
                        url,
                        start: Some(false),
                        ..Download::default()
                    }),
                );
            }
            Action::StartQueue(id) => {
                send(&self.writer, &Incoming::StartQueue { id });
            }
            Action::StopQueue(id) => {
                send(&self.writer, &Incoming::StopQueue { id });
            }
            Action::SaveQueue(queue) => {
                send(&self.writer, &Incoming::SaveQueue { queue });
            }
            Action::DeleteQueue(id) => {
                if self.filter == Filter::Queue(id) {
                    self.filter = Filter::All;
                }
                send(&self.writer, &Incoming::DeleteQueue { id });
            }
            Action::MoveToQueue { id, queue, position } => {
                send(&self.writer, &Incoming::MoveToQueue { id, queue, position });
            }
            Action::SetQueueOnce(id, when_done) => {
                send(&self.writer, &Incoming::SetQueueOnce { id, when_done });
            }
            Action::OpenProperties(id) => {
                let entry = self.shared.lock().unwrap().table.get(id).cloned();
                if let Some(e) = entry.filter(|_| !self.properties.iter().any(|d| d.id == id)) {
                    self.properties.push(properties::PropertiesDialog::new(&e));
                }
            }
            Action::OpenConnections(id) => {
                if !self.connection_dialogs.contains(&id) {
                    self.connection_dialogs.push(id);
                }
            }
            Action::OpenSpeedLimit(id) => {
                let entry = self.shared.lock().unwrap().table.get(id).cloned();
                if let Some(e) = entry {
                    let kbps = e.speed_limit.map_or(512, |b| (b / 1024).max(1));
                    let unit = if kbps >= 1024 && kbps.is_multiple_of(1024) { SpeedUnit::Mb } else { SpeedUnit::Kb };
                    self.limit_dialog = Some(LimitDialog {
                        id,
                        enabled: e.speed_limit.is_some(),
                        value: kbps as f64 / unit.kb(),
                        unit,
                    });
                }
            }
            Action::SetSpeedLimit(id, bytes_per_sec) => {
                send(&self.writer, &Incoming::SetSpeedLimit { id, bytes_per_sec });
            }
            Action::SetProperties { id, changes } => {
                send(&self.writer, &Incoming::SetProperties { id, changes });
            }
            Action::OpenRefresh(id) => {
                self.shared.lock().unwrap().refresh_mismatch = None;
                self.refresh = Some((id, String::new()));
            }
            Action::RefreshWith { id, url, restart } => {
                self.shared.lock().unwrap().refresh_mismatch = None;
                send(
                    &self.writer,
                    &Incoming::RefreshLink {
                        id,
                        download: Some(Download {
                            url,
                            ..Download::default()
                        }),
                        restart,
                    },
                );
            }
            Action::RefreshFromBrowser(id) => {
                send(&self.writer, &Incoming::RefreshLink { id, download: None, restart: false });
            }
            Action::CancelRefresh => {
                send(&self.writer, &Incoming::CancelRefresh);
            }
            Action::CancelCountdown => {
                send(&self.writer, &Incoming::CancelCountdown);
            }
            Action::OpenQueues(select) => {
                let list = self.shared.lock().unwrap().queues.clone();
                self.queues_dialog = Some(queues::QueuesDialog::open(&list, select));
            }
            Action::AddMany {
                items,
                queue,
                start_queue,
            } => {
                send(&self.writer, &Incoming::AddMany { items, queue: Some(queue) });
                // Sent after on the same connection, so it starts with them.
                if start_queue {
                    send(&self.writer, &Incoming::StartQueue { id: queue });
                }
            }
            Action::OpenBatch => self.batch = Some(tools::BatchDialog::new()),
            Action::OpenImport => tools::pick_text_file(&self.ctx, "Import a download list", &self.import),
            Action::OpenExport => self.export = Some(tools::ExportDialog::new(!self.selected.is_empty())),
            Action::OpenGrabber => {
                if self.grabber.is_none() {
                    self.grabber = Some(tools::GrabDialog::new());
                }
            }
            Action::Grab(req) => {
                self.shared.lock().unwrap().grab_events.clear();
                send(&self.writer, &Incoming::Grab(req));
            }
            Action::CancelGrab => {
                send(&self.writer, &Incoming::CancelGrab);
            }
            Action::Select(id, on) => {
                if on {
                    self.selected.insert(id);
                } else {
                    self.selected.remove(&id);
                }
            }
        }
    }

    /// The Batch, Export and site grabber dialogs, and the clipboard offer.
    fn tool_windows(&mut self, ctx: &egui::Context, rows: &[DownloadEntry], connected: bool, actions: &mut Vec<Action>) {
        let queue_list = self.shared.lock().unwrap().queues.clone();
        if let Some(read) = self.import.lock().unwrap().take() {
            let opened = read.and_then(|(name, text)| {
                uidm::listfile::import(&text).map(|(items, rejected)| tools::BatchDialog::imported(name, items, rejected))
            });
            match opened {
                Ok(d) => self.batch = Some(d),
                Err(e) => self.shared.lock().unwrap().last_error = Some(e),
            }
        }
        if let Some(d) = self.batch.as_mut() {
            if !tools::batch_dialog(ctx, d, &queue_list, connected, actions) {
                self.batch = None;
            }
        }
        self.selected.retain(|id| rows.iter().any(|e| e.id == *id));
        if let Some(d) = self.export.as_mut() {
            if !tools::export_dialog(ctx, d, rows, &self.selected) {
                self.export = None;
            }
        }
        let events: Vec<GrabEvent> = self.shared.lock().unwrap().grab_events.drain(..).collect();
        if let Some(d) = self.grabber.as_mut() {
            for event in events {
                match event {
                    GrabEvent::Progress(pages, bytes, found) => d.progress(pages, bytes, found),
                    GrabEvent::Done(files, pages, stopped) => d.done(files, pages, stopped),
                }
            }
            if !connected && d.running() {
                d.failed("The connection to the UIDM daemon was lost.".into());
            }
            if !tools::grab_dialog(ctx, d, &queue_list, connected, actions) {
                self.grabber = None;
            }
        }
        let offer = self.shared.lock().unwrap().clipboard.clone();
        if let Some(url) = offer {
            if !tools::clipboard_offer(ctx, &url, connected, actions) {
                self.shared.lock().unwrap().clipboard = None;
            }
        }
    }

    /// Draw the save-as dialog for the oldest download waiting for one, and
    /// bring the window forward when a new one arrives.
    fn confirm_window(
        &mut self,
        ctx: &egui::Context,
        rows: &[DownloadEntry],
        connected: bool,
        actions: &mut Vec<Action>,
    ) {
        let Some(entry) = rows.iter().find(|e| e.status == Status::Confirming) else {
            self.confirm = None;
            return;
        };
        if self.confirm.as_ref().map(|c| c.id) != Some(entry.id) {
            let path = entry.path.as_deref().map(Path::new);
            self.confirm = Some(ConfirmDraft {
                id: entry.id,
                later_queue: MAIN_QUEUE,
                filename: path
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| display_name(entry)),
                folder: path
                    .and_then(|p| p.parent())
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                picked: Picked::default(),
                when_taken: FileExists::Rename,
                duplicate: Duplicate::NewName,
            });
        }
        let Some(draft) = self.confirm.as_mut() else {
            return;
        };
        take_picked(&draft.picked, &mut draft.folder);
        let waiting = rows.iter().filter(|e| e.status == Status::Confirming).count();
        let (queue_list, ask) = {
            let s = self.shared.lock().unwrap();
            let ask = s.settings.as_ref().is_some_and(|s| s.file_exists == FileExists::Ask);
            (s.queues.clone(), ask)
        };
        // Another download with the same address, which the dialog asks
        // about. A running one cannot be replaced.
        let other = entry.duplicate_of.and_then(|o| rows.iter().find(|e| e.id == o));
        let other_running = other.is_some_and(|o| !o.status.is_terminal());
        if other_running && draft.duplicate == Duplicate::Overwrite {
            draft.duplicate = Duplicate::NewName;
        }
        // Replacing the other download writes over its file, so the name and
        // folder are its own.
        let replaced = other
            .filter(|_| draft.duplicate == Duplicate::Overwrite)
            .and_then(|o| o.path.as_deref());
        // Under Ask, a name taken by a file in the folder needs an answer,
        // unless the duplicate question already covers it.
        let name = uidm::engine::clean_filename(draft.filename.trim());
        let taken =
            ask && other.is_none() && !name.is_empty() && Path::new(draft.folder.trim()).join(&name).is_file();

        egui::Window::new("Save download")
            .id(egui::Id::new("confirm"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::Grid::new("confirm_grid").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                    ui.label("Address");
                    ui.add(egui::Label::new(&entry.url).truncate())
                        .on_hover_text(&entry.url);
                    ui.end_row();

                    ui.label("File name");
                    ui.add_enabled(
                        replaced.is_none(),
                        egui::TextEdit::singleline(&mut draft.filename).desired_width(360.0),
                    );
                    ui.end_row();

                    ui.label("Size");
                    ui.label(match entry.total {
                        Some(t) => human_bytes(t),
                        None => "Unknown".to_string(),
                    });
                    ui.end_row();

                    ui.label("Save to");
                    ui.add_enabled_ui(replaced.is_none(), |ui| {
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut draft.folder).desired_width(280.0));
                            browse_button(ui, "Save to", &draft.folder, &draft.picked);
                        });
                    });
                    ui.end_row();
                });
                if let Some(old) = other {
                    ui.add_space(4.0);
                    let state = match old.status {
                        Status::Done => "finished",
                        Status::Failed => "failed",
                        Status::Canceled => "canceled",
                        Status::Interrupted => "interrupted",
                        Status::Waiting => "waiting",
                        Status::Queued | Status::Downloading | Status::Confirming => "running",
                    };
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!("This address is already in the list as {} ({state}).", display_name(old)),
                    );
                    ui.radio_value(
                        &mut draft.duplicate,
                        Duplicate::NewName,
                        "Add it anyway, with a new name if the old file is in the way",
                    );
                    ui.add_enabled_ui(!other_running, |ui| {
                        ui.radio_value(
                            &mut draft.duplicate,
                            Duplicate::Overwrite,
                            "Replace the old download and overwrite its file",
                        )
                        .on_disabled_hover_text("The old download is still running.");
                    });
                    let existing = match old.status {
                        Status::Done => "Don't add it; show the finished download",
                        s if s.is_retryable() => "Don't add it; resume the old download",
                        _ => "Don't add it; the old download is already running",
                    };
                    ui.radio_value(&mut draft.duplicate, Duplicate::Existing, existing);
                    if let Some(path) = replaced {
                        ui.weak(format!("{path} will be overwritten."));
                    }
                }
                if taken {
                    ui.add_space(4.0);
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!("{name} already exists in this folder."),
                    );
                    ui.horizontal(|ui| {
                        for f in [FileExists::Rename, FileExists::Overwrite] {
                            ui.radio_value(&mut draft.when_taken, f, file_exists_label(f));
                        }
                    });
                }
                if waiting > 1 {
                    ui.weak(format!("{} more downloads are waiting after this one.", waiting - 1));
                }
                if queue_list.len() > 1 {
                    ui.horizontal(|ui| {
                        ui.label("Download later in");
                        let name = queue_list
                            .iter()
                            .find(|q| q.id == draft.later_queue)
                            .map_or("Main queue", |q| q.name.as_str());
                        egui::ComboBox::from_id_salt("later_queue")
                            .selected_text(name)
                            .show_ui(ui, |ui| {
                                for q in &queue_list {
                                    ui.selectable_value(&mut draft.later_queue, q.id, &q.name);
                                }
                            });
                    });
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(connected, |ui| {
                        let answer = |start: bool| Action::Confirm {
                            id: draft.id,
                            filename: Some(draft.filename.trim().to_string()),
                            folder: Some(draft.folder.trim().to_string()),
                            start,
                            queue: Some(draft.later_queue),
                            file_exists: taken.then_some(draft.when_taken),
                            duplicate: other.map(|_| draft.duplicate),
                        };
                        if other.is_some() && draft.duplicate == Duplicate::Existing {
                            if ui.button("OK").clicked() {
                                actions.push(answer(true));
                            }
                        } else {
                            if ui.button("Start download").clicked() {
                                actions.push(answer(true));
                            }
                            if ui.button("Download later").clicked() {
                                actions.push(answer(false));
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            actions.push(Action::CancelSaveAs(draft.id));
                        }
                    });
                });
            });
    }

    /// Draw the Options dialog while it is open.
    fn options_window(&mut self, ctx: &egui::Context, connected: bool, actions: &mut Vec<Action>) {
        // Pick up the daemon's answer to a save.
        let result = self.shared.lock().unwrap().save_result.take();
        let saving = self.shared.lock().unwrap().saving;
        let Some(draft) = self.options.as_mut() else {
            return;
        };
        match result {
            Some(Ok(())) => {
                self.options = None;
                return;
            }
            Some(Err(e)) => draft.error = Some(e),
            None => {}
        }
        take_picked(&draft.picked, &mut draft.folder);
        take_picked(&draft.temp_picked, &mut draft.temp_dir);
        take_picked(&draft.ffmpeg_picked, &mut draft.ffmpeg_path);
        for c in &mut draft.categories {
            take_picked(&c.picked, &mut c.folder);
        }

        let mut open = true;
        let mut close = false;
        egui::Window::new("Options")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .min_width(720.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut draft.tab, OptionsTab::General, "General");
                    ui.selectable_value(&mut draft.tab, OptionsTab::Categories, "Categories");
                    ui.selectable_value(&mut draft.tab, OptionsTab::Capture, "Browser capture");
                    ui.selectable_value(&mut draft.tab, OptionsTab::Notifications, "When done");
                    ui.selectable_value(&mut draft.tab, OptionsTab::Connection, "Connection");
                });
                ui.separator();
                match draft.tab {
                    OptionsTab::General => general_tab(ui, draft),
                    OptionsTab::Categories => categories_tab(ui, draft),
                    OptionsTab::Capture => capture_tab(ui, draft),
                    OptionsTab::Notifications => notifications_tab(ui, draft),
                    OptionsTab::Connection => connection_tab(ui, draft),
                }

                if let Some(err) = &draft.error {
                    ui.add_space(4.0);
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let save = ui.add_enabled(connected && !saving, egui::Button::new("Save"));
                    if save.clicked() {
                        match draft.to_settings() {
                            Ok(settings) => {
                                draft.error = None;
                                actions.push(Action::SaveSettings(settings));
                            }
                            Err(e) => draft.error = Some(e),
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if saving {
                        ui.spinner();
                    }
                });
            });
        if !open || close {
            self.options = None;
        }
    }
}

fn general_tab(ui: &mut egui::Ui, draft: &mut OptionsDraft) {
    egui::Grid::new("options").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
        ui.label("Download folder");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut draft.folder)
                    .hint_text("Your Downloads folder")
                    .desired_width(280.0),
            );
            browse_button(ui, "Download folder", &draft.folder, &draft.picked);
            if ui.button("Default").on_hover_text("Use your Downloads folder").clicked() {
                draft.folder.clear();
            }
        });
        ui.end_row();

        ui.label("Temporary folder");
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut draft.temp_dir)
                        .hint_text("None: partial files sit next to finished ones")
                        .desired_width(280.0),
                );
                browse_button(ui, "Temporary folder", &draft.temp_dir, &draft.temp_picked);
                if ui.button("None").clicked() {
                    draft.temp_dir.clear();
                }
            });
            ui.weak("Unfinished downloads are written here and move to their folder once complete. Paused ones keep their place if you change it.");
        });
        ui.end_row();

        ui.label("Connections per file");
        let (lo, hi) = CONNECTIONS_RANGE;
        ui.add(egui::DragValue::new(&mut draft.connections).range(lo..=hi));
        ui.end_row();

        ui.label("Downloads at once");
        let (lo, hi) = MAX_CONCURRENT_RANGE;
        ui.add(egui::DragValue::new(&mut draft.max_concurrent).range(lo..=hi));
        ui.end_row();

        ui.label("Speed limit");
        ui.horizontal(|ui| {
            ui.checkbox(&mut draft.limit_enabled, "Limit to");
            ui.add_enabled_ui(draft.limit_enabled, |ui| {
                let (lo, hi) = SPEED_LIMIT_RANGE_KBPS;
                let unit = draft.limit_unit.kb();
                ui.add(
                    egui::DragValue::new(&mut draft.limit_value)
                        .range(lo as f64 / unit..=hi as f64 / unit)
                        .max_decimals(1),
                );
                let before = draft.limit_unit;
                egui::ComboBox::from_id_salt("unit")
                    .selected_text(draft.limit_unit.label())
                    .width(70.0)
                    .show_ui(ui, |ui| {
                        for u in [SpeedUnit::Kb, SpeedUnit::Mb] {
                            ui.selectable_value(&mut draft.limit_unit, u, u.label());
                        }
                    });
                // Keep the same speed when only the unit changes.
                if before != draft.limit_unit {
                    draft.limit_value = draft.limit_value * before.kb() / draft.limit_unit.kb();
                }
            });
        });
        ui.end_row();

        ui.label("If the file exists");
        egui::ComboBox::from_id_salt("file_exists")
            .selected_text(file_exists_label(draft.file_exists))
            .show_ui(ui, |ui| {
                for f in [FileExists::Rename, FileExists::Overwrite, FileExists::Ask] {
                    ui.selectable_value(&mut draft.file_exists, f, file_exists_label(f));
                }
            });
        ui.end_row();

        ui.label("If the address is in the list");
        egui::ComboBox::from_id_salt("if_duplicate")
            .selected_text(duplicate_label(draft.if_duplicate))
            .show_ui(ui, |ui| {
                for d in [Duplicate::Ask, Duplicate::NewName, Duplicate::Overwrite, Duplicate::Existing] {
                    ui.selectable_value(&mut draft.if_duplicate, d, duplicate_label(d));
                }
            });
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut draft.ask_where_to_save, "Ask where to save each new download");
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut draft.resume_at_startup, "Resume unfinished downloads at startup");
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut draft.server_file_date, "Give finished files the date the server reports")
            .on_hover_text("Uses the server's Last-Modified date. Streams and form downloads keep the time they were written.");
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut draft.start_with_windows, "Start with Windows, in the tray");
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut draft.close_to_tray, "Closing the window keeps UIDM in the tray");
        ui.end_row();

        ui.label("Removing downloads");
        ui.vertical(|ui| {
            ui.checkbox(&mut draft.confirm_remove, "Ask first");
            let text = if draft.confirm_remove {
                "Tick the box to delete files by default"
            } else {
                "Also send their files to the Recycle Bin"
            };
            ui.checkbox(&mut draft.remove_deletes_files, text);
        });
        ui.end_row();

        ui.label("User-Agent").on_hover_text("For downloads not started from the browser");
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut draft.manual_user_agent)
                        .hint_text("UIDM's default")
                        .desired_width(280.0),
                );
                ui.menu_button("Presets", |ui| {
                    for (name, agent) in USER_AGENT_PRESETS {
                        if ui.button(name).clicked() {
                            draft.manual_user_agent = agent.to_string();
                            ui.close_menu();
                        }
                    }
                    if ui.button("Default").clicked() {
                        draft.manual_user_agent.clear();
                        ui.close_menu();
                    }
                });
            });
            ui.weak("Sent with links you add by hand, batches, imports and the site grabber's finds. Downloads from the browser send the browser's own.");
        });
        ui.end_row();

        ui.label("ffmpeg");
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut draft.ffmpeg_path)
                        .hint_text("Empty looks for it on PATH")
                        .desired_width(280.0),
                );
                if ui.button("Browse...").clicked() {
                    let (slot, ctx) = (Arc::clone(&draft.ffmpeg_picked), ui.ctx().clone());
                    thread::spawn(move || {
                        let file = rfd::FileDialog::new()
                            .set_title("Where is ffmpeg.exe?")
                            .add_filter("ffmpeg", &["exe"])
                            .pick_file();
                        if let Some(file) = file {
                            *slot.lock().unwrap() = Some(file);
                            ctx.request_repaint();
                        }
                    });
                }
            });
            let found = Settings {
                ffmpeg_path: draft.ffmpeg_path.trim().to_string(),
                ..Settings::default()
            }
            .ffmpeg();
            match found {
                Some(path) => ui.weak(format!("Using {}", path.display())),
                None => ui.colored_label(ui.visuals().warn_fg_color, "Not found."),
            };
            ui.weak("Joins streamed videos into one .mp4. Without it they are saved as .ts, or as separate video and audio.");
        });
        ui.end_row();
    });
}

fn connection_tab(ui: &mut egui::Ui, draft: &mut OptionsDraft) {
    egui::Grid::new("connection").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label("Proxy");
        ui.vertical(|ui| {
            ui.radio_value(&mut draft.proxy_mode, ProxyMode::None, "No proxy");
            ui.radio_value(&mut draft.proxy_mode, ProxyMode::System, "Use the Windows proxy setting");
            ui.radio_value(&mut draft.proxy_mode, ProxyMode::Manual, "Use these proxies:");
        });
        ui.end_row();
        let manual = draft.proxy_mode == ProxyMode::Manual;
        for (label, field, hint) in [
            ("HTTP proxy", &mut draft.proxy_http, "proxy.example.com:8080"),
            ("HTTPS proxy", &mut draft.proxy_https, "Same as HTTP when empty"),
            ("SOCKS5 proxy", &mut draft.proxy_socks5, "Used for everything when set"),
        ] {
            ui.label(label);
            ui.add_enabled(manual, egui::TextEdit::singleline(field).hint_text(hint).desired_width(260.0));
            ui.end_row();
        }
        ui.label("Proxy login");
        ui.add_enabled_ui(manual, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut draft.proxy_username).hint_text("User name").desired_width(120.0));
                let hint = if draft.proxy_password_changed { "Password" } else { "Password (unchanged)" };
                let pw = ui.add(
                    egui::TextEdit::singleline(&mut draft.proxy_password)
                        .password(true)
                        .hint_text(hint)
                        .desired_width(130.0),
                );
                if pw.changed() {
                    draft.proxy_password_changed = true;
                }
            });
        });
        ui.end_row();
        ui.label("");
        ui.checkbox(&mut draft.use_browser_proxy, "For downloads from the browser, use the browser's proxy");
        ui.end_row();
    });

    ui.add_space(8.0);
    ui.strong("Connections per server");
    ui.weak("Some servers refuse clients that open many connections. These limits count every download to the server together.");
    let mut gone = None;
    egui::Grid::new("server_limits").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
        for (i, (site, max)) in draft.server_limits.iter_mut().enumerate() {
            ui.add_sized([220.0, 20.0], egui::TextEdit::singleline(site).hint_text("*.example.com"));
            ui.horizontal(|ui| {
                let (lo, hi) = SERVER_CONNECTIONS_RANGE;
                ui.add(egui::DragValue::new(max).range(lo..=hi));
                ui.label("connections at most");
            });
            if ui.small_button("Remove").clicked() {
                gone = Some(i);
            }
            ui.end_row();
        }
    });
    if let Some(i) = gone {
        draft.server_limits.remove(i);
    }
    if ui.button("Add server").clicked() {
        draft.server_limits.push((String::new(), 2));
    }

    ui.add_space(8.0);
    ui.strong("Site logins");
    ui.weak("Sent only to a site that asks for a login, and only to that site.");
    let mut remove = None;
    egui::Grid::new("logins").num_columns(4).spacing([8.0, 6.0]).show(ui, |ui| {
        for (i, l) in draft.logins.iter_mut().enumerate() {
            ui.add_sized([180.0, 20.0], egui::TextEdit::singleline(&mut l.site).hint_text("example.com"));
            ui.add_sized([120.0, 20.0], egui::TextEdit::singleline(&mut l.username).hint_text("User name"));
            let hint = if l.new { "Password" } else { "Password (unchanged)" };
            ui.add_sized([150.0, 20.0], egui::TextEdit::singleline(&mut l.password).password(true).hint_text(hint));
            if ui.small_button("Remove").clicked() {
                remove = Some(i);
            }
            ui.end_row();
        }
    });
    if let Some(i) = remove {
        draft.logins.remove(i);
    }
    if ui.button("Add login").clicked() {
        draft.logins.push(LoginDraft {
            site: String::new(),
            username: String::new(),
            password: String::new(),
            new: true,
        });
    }
}

fn notifications_tab(ui: &mut egui::Ui, draft: &mut OptionsDraft) {
    egui::Grid::new("notifications").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
        ui.label("When a download ends");
        ui.vertical(|ui| {
            ui.radio_value(&mut draft.notify, Notify::Toast, "Show a Windows notification");
            ui.radio_value(&mut draft.notify, Notify::Dialog, "Show a dialog in UIDM");
            ui.radio_value(&mut draft.notify, Notify::Off, "Show nothing");
        });
        ui.end_row();

        ui.label("Sounds");
        ui.vertical(|ui| {
            ui.checkbox(&mut draft.sound_on_done, "When a download finishes");
            ui.checkbox(&mut draft.sound_on_failed, "When a download fails");
        });
        ui.end_row();

        ui.label("Virus scan");
        ui.vertical(|ui| {
            ui.checkbox(&mut draft.scan_after_download, "Scan each finished download");
            ui.add_enabled_ui(draft.scan_after_download, |ui| {
                ui.radio_value(&mut draft.scanner, Scanner::Defender, "Microsoft Defender");
                ui.radio_value(&mut draft.scanner, Scanner::Custom, "This scanner:");
                ui.add_enabled(
                    draft.scanner == Scanner::Custom,
                    egui::TextEdit::singleline(&mut draft.scanner_command)
                        .hint_text(r#""C:\Program Files\ClamAV\clamscan.exe" {path}"#)
                        .desired_width(380.0),
                );
                ui.weak("{path} is replaced by the file. Exit code 0 means clean, 1 a threat.");
            });
        });
        ui.end_row();
    });
}

fn categories_tab(ui: &mut egui::Ui, draft: &mut OptionsDraft) {
    ui.weak(format!(
        "Each download goes to its category's folder, chosen by its file type and, if the category lists \
         sites, by the site it comes from. Other files are \"{GENERAL}\" and go to the download folder."
    ));
    ui.add_space(6.0);
    let mut remove = None;
    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
        egui::Grid::new("categories").num_columns(6).spacing([8.0, 8.0]).show(ui, |ui| {
            ui.strong("Name");
            ui.strong("File types");
            ui.strong("Folder");
            ui.strong("Only from sites");
            ui.strong("");
            ui.label("");
            ui.end_row();
            for (i, c) in draft.categories.iter_mut().enumerate() {
                ui.add_sized([110.0, 20.0], egui::TextEdit::singleline(&mut c.name));
                ui.add_sized(
                    [160.0, 20.0],
                    egui::TextEdit::singleline(&mut c.extensions).hint_text("zip, rar"),
                );
                ui.horizontal(|ui| {
                    let hint = format!("Download folder\\{}", c.name.trim());
                    ui.add_sized(
                        [180.0, 20.0],
                        egui::TextEdit::singleline(&mut c.folder).hint_text(hint),
                    );
                    browse_button(ui, "Category folder", &c.folder, &c.picked);
                });
                ui.add_sized(
                    [150.0, 20.0],
                    egui::TextEdit::singleline(&mut c.sites).hint_text("Every site"),
                )
                .on_hover_text("Such as *.example.com, separated by commas");
                ui.checkbox(&mut c.remember_folder, "Last folder")
                    .on_hover_text("Save new files to the folder last chosen for this category in the save-as dialog");
                if ui.small_button("Remove").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
    });
    if let Some(i) = remove {
        draft.categories.remove(i);
    }
    ui.add_space(4.0);
    if ui.button("Add category").clicked() {
        draft.categories.push(CategoryDraft {
            name: String::new(),
            extensions: String::new(),
            folder: String::new(),
            sites: String::new(),
            remember_folder: false,
            last_folder: None,
            picked: Picked::default(),
        });
    }
}

fn capture_tab(ui: &mut egui::Ui, draft: &mut OptionsDraft) {
    ui.weak("These rules decide which browser downloads UIDM takes. The rest stay in the browser.");
    ui.add_space(6.0);
    egui::Grid::new("capture").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
        ui.label("File types");
        ui.add(
            egui::TextEdit::multiline(&mut draft.capture_types)
                .hint_text("Empty takes every type")
                .desired_rows(3)
                .desired_width(360.0),
        );
        ui.end_row();

        ui.label("Minimum size");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut draft.capture_min_size_kb).range(0..=CAPTURE_MIN_SIZE_MAX_KB));
            ui.label("KB (0 takes every size)");
        });
        ui.end_row();

        ui.label("Never from");
        ui.add(
            egui::TextEdit::multiline(&mut draft.capture_excluded_sites)
                .hint_text("One site per line, such as *.example.com")
                .desired_rows(4)
                .desired_width(360.0),
        );
        ui.end_row();

        ui.label("Never these addresses");
        ui.vertical(|ui| {
            ui.add(
                egui::TextEdit::multiline(&mut draft.capture_excluded_urls)
                    .hint_text("One address per line; * matches anything, as in https://example.com/files/*")
                    .desired_rows(3)
                    .desired_width(360.0),
            );
            ui.checkbox(
                &mut draft.offer_exceptions,
                "When I cancel the same download twice, offer to stop taking it",
            );
        });
        ui.end_row();

        ui.label("Special keys");
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label("Hold");
                key_combo(ui, "skip_key", &mut draft.capture_skip_key);
                ui.label("while clicking a link to keep its download in the browser");
            });
            ui.horizontal(|ui| {
                ui.label("Hold");
                key_combo(ui, "force_key", &mut draft.capture_force_key);
                ui.label("while clicking a link to send it to UIDM, whatever its type");
            });
        });
        ui.end_row();

        ui.label("Selected links");
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label("Show a Download with UIDM button by selected text with links");
                egui::ComboBox::from_id_salt("selection_button")
                    .selected_text(selection_label(draft.selection_button))
                    .show_ui(ui, |ui| {
                        for b in [SelectionButton::Off, SelectionButton::Everywhere, SelectionButton::Sites] {
                            ui.selectable_value(&mut draft.selection_button, b, selection_label(b));
                        }
                    });
            });
            if draft.selection_button == SelectionButton::Sites {
                ui.add(
                    egui::TextEdit::multiline(&mut draft.selection_button_sites)
                        .hint_text("One site per line, such as *.example.com")
                        .desired_rows(2)
                        .desired_width(360.0),
                );
            }
        });
        ui.end_row();

        ui.label("Videos");
        ui.checkbox(&mut draft.video_button, "Show a download button over videos playing in the browser");
        ui.end_row();

        ui.label("No video button on");
        ui.add_enabled(
            draft.video_button,
            egui::TextEdit::multiline(&mut draft.video_button_excluded_sites)
                .hint_text("One site per line, such as *.example.com")
                .desired_rows(2)
                .desired_width(360.0),
        );
        ui.end_row();

        ui.label("Clipboard");
        ui.checkbox(
            &mut draft.clipboard_monitor,
            "Offer to download links I copy whose file type is listed above",
        );
        ui.end_row();
    });
}

fn selection_label(b: SelectionButton) -> &'static str {
    match b {
        SelectionButton::Off => "Nowhere",
        SelectionButton::Everywhere => "On every site",
        SelectionButton::Sites => "On these sites only",
    }
}

fn key_combo(ui: &mut egui::Ui, id: &str, key: &mut CaptureKey) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(key.label())
        .width(80.0)
        .show_ui(ui, |ui| {
            for k in CaptureKey::ALL {
                ui.selectable_value(key, k, k.label());
            }
        });
}

fn duplicate_label(d: Duplicate) -> &'static str {
    match d {
        Duplicate::Ask => "Ask me",
        Duplicate::NewName => "Add it with a new name",
        Duplicate::Overwrite => "Replace the old download",
        Duplicate::Existing => "Use the old download",
    }
}

fn file_exists_label(f: FileExists) -> &'static str {
    match f {
        FileExists::Rename => "Save with a new name",
        FileExists::Overwrite => "Overwrite it",
        FileExists::Ask => "Ask me",
    }
}

/// Send one request. Returns false when not connected or the write failed.
fn send(writer: &Arc<Mutex<Option<TcpStream>>>, msg: &Incoming) -> bool {
    let mut guard = writer.lock().unwrap();
    let Some(stream) = guard.as_mut() else {
        return false;
    };
    match serde_json::to_vec(msg) {
        Ok(frame) => write_frame(stream, &frame).is_ok(),
        Err(_) => false,
    }
}

/// Apply one daemon message, and bring the window forward when it needs the
/// user: a save-as dialog, a completion dialog, or a notification click.
fn on_message(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context, msg: &Outgoing) {
    let show = {
        let mut s = shared.lock().unwrap();
        s.apply(msg);
        match msg {
            Outgoing::Show => true,
            // A duplicate of a finished download shows its dialog again.
            Outgoing::ShowDone { id } => {
                s.completed.push_back((*id, false));
                true
            }
            Outgoing::Countdown { .. } => true,
            Outgoing::Quit => std::process::exit(0),
            Outgoing::Updated { item } => item.status == Status::Confirming,
            Outgoing::NameTaken { .. } => true,
            Outgoing::Done { id, .. } if s.dialog_mode() => {
                s.completed.push_back((*id, false));
                true
            }
            Outgoing::Failed { id, .. } if s.dialog_mode() => {
                let (shared, ctx, id) = (Arc::clone(shared), ctx.clone(), *id);
                thread::spawn(move || {
                    thread::sleep(FAILED_GRACE);
                    let mut s = shared.lock().unwrap();
                    if s.table.get(id).is_some_and(|e| e.status == Status::Failed) {
                        s.completed.push_back((id, true));
                        drop(s);
                        win::show();
                        ctx.request_repaint();
                    }
                });
                false
            }
            _ => false,
        }
    };
    if show {
        win::show();
    }
    ctx.request_repaint();
}

/// Open a file with its program, as Explorer does.
fn open_file(path: &str) {
    let _ = Command::new("explorer").arg(path).spawn();
}

fn is_web_url(url: &str) -> bool {
    (url.starts_with("http://") || url.starts_with("https://")) && !url.contains(char::is_whitespace)
}

impl App {
    /// The completion dialog for the oldest finished or failed download.
    fn completed_window(&mut self, ctx: &egui::Context, connected: bool, actions: &mut Vec<Action>) {
        let next = {
            let mut s = self.shared.lock().unwrap();
            loop {
                match s.completed.front().copied() {
                    // Removed since: skip it.
                    Some((id, _)) if s.table.get(id).is_none() => {
                        s.completed.pop_front();
                    }
                    Some((id, failed)) => break s.table.get(id).cloned().map(|e| (e, failed, s.completed.len())),
                    None => break None,
                }
            }
        };
        let Some((e, failed, waiting)) = next else {
            return;
        };
        let title = if failed { "Download failed" } else { "Download complete" };
        egui::Window::new(title)
            .id(egui::Id::new("completed"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let path = e.path.as_deref().map(Path::new);
                egui::Grid::new("completed_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("File");
                    ui.strong(display_name(&e));
                    ui.end_row();
                    ui.label("Address");
                    ui.add(egui::Label::new(&e.url).truncate()).on_hover_text(&e.url);
                    ui.end_row();
                    if let Some(t) = e.total {
                        ui.label("Size");
                        ui.label(human_bytes(t));
                        ui.end_row();
                    }
                    if let Some(folder) = path.and_then(|p| p.parent()) {
                        ui.label("Saved in");
                        ui.label(folder.display().to_string());
                        ui.end_row();
                    }
                    if failed {
                        ui.label("Error");
                        ui.colored_label(ui.visuals().error_fg_color, e.error.clone().unwrap_or_default());
                        ui.end_row();
                    }
                    if let Some(scan) = &e.scan {
                        ui.label("Virus scan");
                        scan_label(ui, scan);
                        ui.end_row();
                    }
                });
                if waiting > 1 {
                    ui.weak(format!("{} more after this one.", waiting - 1));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if failed {
                        if ui.add_enabled(connected, egui::Button::new("Retry")).clicked() {
                            actions.push(Action::Retry(e.id));
                            actions.push(Action::DismissCompleted);
                        }
                    } else if let Some(p) = &e.path {
                        if ui.button("Open").clicked() {
                            actions.push(Action::Open(e.id));
                            actions.push(Action::DismissCompleted);
                        }
                        if ui.button("Show in folder").clicked() {
                            actions.push(Action::ShowInFolder(p.clone()));
                            actions.push(Action::DismissCompleted);
                        }
                    }
                    if ui.button("Close").clicked() {
                        actions.push(Action::DismissCompleted);
                    }
                });
            });
    }

    /// "Open anyway?" for a file whose scan found a threat.
    fn threat_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some((path, name, detail)) = self.threat_prompt.clone() else {
            return;
        };
        let mut cancel = false;
        egui::Window::new("Threat found")
            .id(egui::Id::new("threat"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!("The virus scan found a threat in {name}."));
                if !detail.is_empty() {
                    ui.weak(&detail);
                }
                ui.label("Opening it could harm your computer. Open it anyway?");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    let open = egui::Button::new(egui::RichText::new("Open anyway").color(ui.visuals().error_fg_color));
                    if ui.add(open).clicked() {
                        actions.push(Action::OpenAnyway(path.clone()));
                    }
                });
            });
        if cancel {
            self.threat_prompt = None;
        }
    }
}

fn scan_label(ui: &mut egui::Ui, scan: &Scan) {
    let v = ui.visuals().clone();
    match scan {
        Scan::Scanning => {
            ui.weak("Scanning...");
        }
        Scan::Clean => {
            ui.label("No threat found");
        }
        Scan::Threat { detail } => {
            ui.colored_label(v.error_fg_color, "Threat found").on_hover_text(detail);
        }
        Scan::Failed { detail } => {
            ui.colored_label(v.warn_fg_color, "Scan failed").on_hover_text(detail);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Copy what this frame needs, then draw without holding the lock.
        let (link, rows, speeds, last_error, settings) = {
            let s = self.shared.lock().unwrap();
            let speeds: HashMap<u64, f64> =
                s.speeds.iter().map(|(id, sp)| (*id, sp.bytes_per_sec)).collect();
            (
                s.link.clone(),
                s.table.entries().cloned().collect::<Vec<DownloadEntry>>(),
                speeds,
                s.last_error.clone(),
                s.settings.clone(),
            )
        };
        let connected = matches!(link, Link::Connected);
        let mut actions = Vec::new();

        // Close keeps UIDM in the tray unless Options says to quit.
        if ctx.input(|i| i.viewport().close_requested())
            && tray::installed()
            && settings.as_ref().is_none_or(|s| s.close_to_tray)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            win::hide();
        }
        let tooltip = self.shared.lock().unwrap().tooltip();
        if tooltip != self.tooltip {
            tray::set_tooltip(&tooltip);
            self.tooltip = tooltip;
        }
        if let Some(s) = &settings {
            tray::set_speed_limit_checked(s.speed_limit_enabled);
        }

        egui::TopBottomPanel::top("add").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading("UIDM");
                ui.add_space(8.0);
                let input = ui.add(
                    egui::TextEdit::singleline(&mut self.url_input)
                        .hint_text("Paste a download URL")
                        .desired_width((ui.available_width() - 300.0).max(120.0)),
                );
                let entered = input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let url = self.url_input.trim().to_string();
                let can_add = connected && (url.starts_with("http://") || url.starts_with("https://"));
                let clicked = ui.add_enabled(can_add, egui::Button::new("Download")).clicked();
                if can_add && (clicked || entered) {
                    actions.push(Action::Add(url.clone()));
                    self.url_input.clear();
                }
                let later = ui
                    .add_enabled(can_add, egui::Button::new("Later"))
                    .on_hover_text("Add it to the main queue without starting it");
                if can_add && later.clicked() {
                    actions.push(Action::AddLater(url));
                    self.url_input.clear();
                }
                ui.add_enabled_ui(connected, |ui| {
                    ui.menu_button("More", |ui| {
                        if ui.button("Add many...").on_hover_text("From a URL pattern or a list").clicked() {
                            actions.push(Action::OpenBatch);
                            ui.close_menu();
                        }
                        if ui.button("Site grabber...").on_hover_text("Find the files a site links to").clicked() {
                            actions.push(Action::OpenGrabber);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Import list...").clicked() {
                            actions.push(Action::OpenImport);
                            ui.close_menu();
                        }
                        if ui.button("Export list...").clicked() {
                            actions.push(Action::OpenExport);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Find in the list...").on_hover_text("Ctrl+F").clicked() {
                            self.find.get_or_insert_with(find::FindBar::new);
                            ui.close_menu();
                        }
                        if ui.button("Columns...").on_hover_text("Show, hide and order the list's columns").clicked() {
                            self.columns_dialog = true;
                            ui.close_menu();
                        }
                    });
                });
                let can_open = connected && settings.is_some();
                if ui.add_enabled(can_open, egui::Button::new("Options")).clicked() {
                    actions.push(Action::OpenOptions);
                }
            });
            ui.add_space(6.0);
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                match &link {
                    Link::Connecting => {
                        ui.spinner();
                        ui.label("Connecting to the UIDM daemon...");
                    }
                    Link::Connected => {
                        ui.label("Connected");
                    }
                    Link::Lost(why) => {
                        ui.colored_label(ui.visuals().error_fg_color, format!("Disconnected: {why}"));
                        if ui.button("Reconnect").clicked() {
                            actions.push(Action::Reconnect);
                        }
                    }
                }
                let refreshing = self.shared.lock().unwrap().refresh_waiting;
                if let Some(e) = refreshing.and_then(|id| rows.iter().find(|e| e.id == id)) {
                    ui.separator();
                    ui.label(format!("Your next browser download gives {} a new link", display_name(e)));
                    if ui.small_button("Stop waiting").clicked() {
                        actions.push(Action::CancelRefresh);
                    }
                }
                let active = rows.iter().filter(|e| !e.status.is_terminal()).count();
                ui.separator();
                ui.label(format!("{active} active, {} total", rows.len()));
                if let Some(err) = &last_error {
                    ui.separator();
                    ui.colored_label(ui.visuals().warn_fg_color, err);
                }
                let notice = self.shared.lock().unwrap().notice.clone();
                if let Some(n) = notice {
                    ui.separator();
                    ui.label(n);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let any_done = rows.iter().any(|e| e.status == Status::Done);
                    if ui
                        .add_enabled(connected && any_done, egui::Button::new("Clear finished"))
                        .clicked()
                    {
                        actions.push(Action::ClearFinished);
                    }
                    let ticked: Vec<u64> = rows
                        .iter()
                        .filter(|e| self.selected.contains(&e.id) && e.status.is_terminal())
                        .map(|e| e.id)
                        .collect();
                    if !ticked.is_empty()
                        && ui
                            .add_enabled(connected, egui::Button::new(format!("Remove ticked ({})", ticked.len())))
                            .on_hover_text("Running downloads are left alone")
                            .clicked()
                    {
                        actions.push(Action::Remove(ticked));
                    }
                    if let Some(s) = &settings {
                        ui.separator();
                        let toggle = if s.speed_limit_enabled { "Turn off" } else { "Limit speed" };
                        let hover = format!("Limit: {}/s", human_bytes(s.speed_limit_kbps * 1024));
                        if ui
                            .add_enabled(connected, egui::Button::new(toggle).small())
                            .on_hover_text(hover)
                            .clicked()
                        {
                            actions.push(Action::QuickSettings(Settings {
                                speed_limit_enabled: !s.speed_limit_enabled,
                                ..s.clone()
                            }));
                        }
                        if s.speed_limit_enabled {
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                format!("Speed limit {}/s", human_bytes(s.speed_limit_kbps * 1024)),
                            );
                        } else {
                            ui.weak("No speed limit");
                        }
                    }
                });
            });
            ui.add_space(4.0);
        });

        // Category of each row, from its file name, and the sidebar to filter by
        // it.
        let category = |e: &DownloadEntry| -> String {
            settings
                .as_ref()
                .map_or(GENERAL, |s| s.category_name(&display_name(e), &e.url))
                .to_string()
        };
        let queue_list = self.shared.lock().unwrap().queues.clone();
        if let Some(s) = &settings {
            let mut names: Vec<String> = s.categories.iter().map(|c| c.name.clone()).collect();
            names.push(GENERAL.to_string());
            let gone = match &self.filter {
                Filter::Category(name) => !names.contains(name),
                Filter::Queue(id) => !queue_list.iter().any(|q| q.id == *id),
                Filter::All => false,
            };
            if gone {
                self.filter = Filter::All;
            }
            egui::SidePanel::left("categories")
                .resizable(false)
                .exact_width(170.0)
                .show(ctx, |ui| {
                    ui.add_space(6.0);
                    let all = format!("All downloads ({})", rows.len());
                    ui.selectable_value(&mut self.filter, Filter::All, all);
                    ui.add_space(4.0);
                    for name in names {
                        let count = rows.iter().filter(|e| category(e) == name).count();
                        let label = format!("{name} ({count})");
                        ui.selectable_value(&mut self.filter, Filter::Category(name), label);
                    }
                    ui.add_space(10.0);
                    ui.weak("Queues");
                    for q in &queue_list {
                        let label = if q.running {
                            format!("{} ({}, started)", q.name, q.order.len())
                        } else {
                            format!("{} ({})", q.name, q.order.len())
                        };
                        ui.selectable_value(&mut self.filter, Filter::Queue(q.id), label);
                    }
                    ui.add_space(4.0);
                    if ui.add_enabled(connected, egui::Button::new("Manage queues...")).clicked() {
                        actions.push(Action::OpenQueues(None));
                    }
                });
        }
        let current_queue = match &self.filter {
            Filter::Queue(id) => queue_list.iter().find(|q| q.id == *id).cloned(),
            _ => None,
        };
        let shown: Vec<&DownloadEntry> = match (&self.filter, &current_queue) {
            (Filter::Queue(_), Some(q)) => q
                .order
                .iter()
                .filter_map(|id| rows.iter().find(|e| e.id == *id))
                .collect(),
            (Filter::Category(name), _) => rows.iter().filter(|e| category(e) == *name).collect(),
            _ => rows.iter().collect(),
        };

        find::shortcuts(ctx, &mut self.find);
        let mut shown = shown;
        if let Some(bar) = &self.find {
            shown.retain(|e| bar.matches(e));
        }
        // A queue's view shows its run order.
        if current_queue.is_none() {
            self.layout.sort(&mut shown, |id| speeds.get(&id).copied());
        }
        let layout_before = self.layout.clone();
        let finding = self.find.as_ref().is_some_and(|b| b.active());
        if let Some(bar) = self.find.as_mut() {
            let ids: Vec<u64> = shown.iter().map(|e| e.id).collect();
            find::step_keys(ctx, bar, &ids);
            let open = egui::TopBottomPanel::top("find")
                .show(ctx, |ui| {
                    ui.add_space(4.0);
                    let open = find::show(ui, bar, &ids);
                    ui.add_space(4.0);
                    open
                })
                .inner;
            if !open {
                self.find = None;
            }
        }
        let (current, scroll) = match self.find.as_mut() {
            Some(bar) => (bar.current, std::mem::take(&mut bar.scroll)),
            None => (None, false),
        };

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(q) = &current_queue {
                queues::header(ui, q, connected, &mut actions);
                ui.separator();
                if shown.is_empty() && !finding {
                    ui.centered_and_justified(|ui| {
                        ui.weak("This queue is empty. Use Later, or a download's Queue menu, to add one.");
                    });
                    return;
                }
            }
            if shown.is_empty() && finding {
                ui.centered_and_justified(|ui| {
                    ui.weak("No downloads here match. Press Escape to see them all.");
                });
                return;
            }
            if shown.is_empty() && !rows.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.weak("No downloads in this category.");
                });
                return;
            }
            if rows.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.weak("No downloads yet. Paste a URL above, or download something in your browser.");
                });
                return;
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let columns = self.layout.columns.clone();
                egui::Grid::new("downloads")
                    .striped(true)
                    .num_columns(columns.len() + 2)
                    .spacing([14.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("");
                        for &column in &columns {
                            let title = egui::RichText::new(format!("{}{}", column.title(), self.layout.arrow(column))).strong();
                            let header = ui.add(egui::Button::new(title).frame(false));
                            let header = if current_queue.is_some() {
                                header.on_hover_text("A queue's downloads stay in its run order")
                            } else {
                                header.on_hover_text("Sort by this column; click again to reverse, a third time for the order added")
                            };
                            if header.clicked() && current_queue.is_none() {
                                self.layout.click(column);
                            }
                            header.context_menu(|ui| {
                                if ui.button("Columns...").clicked() {
                                    self.columns_dialog = true;
                                    ui.close_menu();
                                }
                            });
                        }
                        ui.label("");
                        ui.end_row();
                        let len = shown.len();
                        for (i, entry) in shown.iter().copied().enumerate() {
                            let place = current_queue.as_ref().map(|q| (q.id, i, len));
                            let row = Row {
                                speed: speeds.get(&entry.id).copied(),
                                connected,
                                queues: &queue_list,
                                place,
                                selected: self.selected.contains(&entry.id),
                                columns: &columns,
                                found: current == Some(entry.id),
                                scroll: scroll && current == Some(entry.id),
                            };
                            draw_row(ui, entry, &row, &mut actions);
                            ui.end_row();
                        }
                    });
            });
        });

        self.confirm_window(ctx, &rows, connected, &mut actions);
        self.completed_window(ctx, connected, &mut actions);
        self.threat_window(ctx, &mut actions);
        self.remove_window(ctx, &rows, connected, &mut actions);
        self.limit_window(ctx, &rows, connected, &mut actions);
        self.exception_window(ctx, connected, &mut actions);
        if let Some(d) = self.shared.lock().unwrap().picked_links.take() {
            self.links = Some(d);
        }
        if let Some(d) = self.links.as_mut() {
            if !links::dialog(ctx, d, &queue_list, connected, &mut actions) {
                self.links = None;
            }
        }
        let segments = self.shared.lock().unwrap().segments.clone();
        self.connection_dialogs.retain(|id| {
            let entry = rows.iter().find(|e| e.id == *id);
            connections::dialog(ctx, *id, entry, segments.get(id).map(Vec::as_slice))
        });
        self.refresh_window(ctx, &rows, connected, &mut actions);
        self.tool_windows(ctx, &rows, connected, &mut actions);
        if let Some(d) = self.queues_dialog.as_mut() {
            if !queues::dialog(ctx, d, &queue_list, connected, &mut actions) {
                self.queues_dialog = None;
            }
        }
        let countdown = self.shared.lock().unwrap().countdown.clone();
        if let Some((action, deadline, queue)) = countdown {
            if Instant::now() < deadline {
                queues::countdown(ctx, action, deadline, &queue, connected, &mut actions);
            } else {
                self.shared.lock().unwrap().countdown = None;
            }
        }
        self.options_window(ctx, connected, &mut actions);
        if self.columns_dialog {
            self.columns_dialog = columns::dialog(ctx, &mut self.layout);
        }
        self.properties.retain_mut(|d| {
            let entry = rows.iter().find(|e| e.id == d.id);
            properties::dialog(ctx, d, entry, connected, &mut actions)
        });
        if self.layout != layout_before {
            self.layout.save();
        }

        for action in actions {
            self.perform(action);
        }
    }
}

impl App {
    /// After the save-as dialog of one captured address is canceled twice:
    /// offer to keep its site, or the address, in the browser from now on.
    fn exception_window(&mut self, ctx: &egui::Context, connected: bool, actions: &mut Vec<Action>) {
        let Some((url, dont_show)) = self.exception_offer.as_mut() else {
            return;
        };
        let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
        let mut chosen = None;
        egui::Window::new("Keep it in the browser?")
            .id(egui::Id::new("exception_offer"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("You canceled this download twice. UIDM can leave it to the browser next time.");
                ui.add(egui::Label::new(egui::RichText::new(url.as_str()).weak()).truncate());
                ui.add_space(6.0);
                ui.checkbox(dont_show, "Don't show this again");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(connected, |ui| {
                        if !host.is_empty() && ui.button(format!("Don't capture {host}")).clicked() {
                            chosen = Some(Some(true));
                        }
                        if ui.button("Don't capture this address").clicked() {
                            chosen = Some(Some(false));
                        }
                    });
                    if ui.button("Keep capturing").clicked() {
                        chosen = Some(None);
                    }
                });
            });
        if let Some(add) = chosen {
            if add.is_some() || *dont_show {
                actions.push(Action::Exception {
                    url: url.clone(),
                    add,
                    stop_offering: *dont_show,
                });
            }
            self.exception_offer = None;
        }
    }

    /// The speed limit dialog for one download.
    fn limit_window(&mut self, ctx: &egui::Context, rows: &[DownloadEntry], connected: bool, actions: &mut Vec<Action>) {
        let Some(d) = self.limit_dialog.as_mut() else {
            return;
        };
        let Some(e) = rows.iter().find(|e| e.id == d.id) else {
            self.limit_dialog = None;
            return;
        };
        let mut close = false;
        egui::Window::new("Speed limit")
            .id(egui::Id::new("download_limit"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!("A speed limit for {} alone.", display_name(e)));
                ui.weak("The total limit still applies; the lower one wins. The limit stays when it stops and resumes.");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut d.enabled, "Limit to");
                    ui.add_enabled_ui(d.enabled, |ui| {
                        let (lo, hi) = SPEED_LIMIT_RANGE_KBPS;
                        let unit = d.unit.kb();
                        ui.add(
                            egui::DragValue::new(&mut d.value)
                                .range(lo as f64 / unit..=hi as f64 / unit)
                                .max_decimals(1),
                        );
                        let before = d.unit;
                        egui::ComboBox::from_id_salt("download_limit_unit")
                            .selected_text(d.unit.label())
                            .width(70.0)
                            .show_ui(ui, |ui| {
                                for u in [SpeedUnit::Kb, SpeedUnit::Mb] {
                                    ui.selectable_value(&mut d.unit, u, u.label());
                                }
                            });
                        if before != d.unit {
                            d.value = d.value * before.kb() / d.unit.kb();
                        }
                    });
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(connected, egui::Button::new("Save")).clicked() {
                        let bytes = d.enabled.then(|| (d.value * d.unit.kb() * 1024.0).round() as u64);
                        actions.push(Action::SetSpeedLimit(d.id, bytes));
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
        if close {
            self.limit_dialog = None;
        }
    }

    /// Remove `ids`, asking first unless Options says not to.
    fn ask_remove(&mut self, ids: Vec<u64>) {
        if ids.is_empty() {
            return;
        }
        let settings = self.shared.lock().unwrap().settings.clone().unwrap_or_default();
        if settings.confirm_remove {
            self.remove_prompt = Some(RemovePrompt {
                ids,
                delete_files: settings.remove_deletes_files,
                dont_ask: false,
            });
        } else {
            self.perform(Action::RemoveNow {
                ids,
                delete_files: settings.remove_deletes_files,
                dont_ask: false,
            });
        }
    }

    /// The confirmation before removing downloads, with a box to send their
    /// files to the Recycle Bin too.
    fn remove_window(&mut self, ctx: &egui::Context, rows: &[DownloadEntry], connected: bool, actions: &mut Vec<Action>) {
        let Some(p) = self.remove_prompt.as_mut() else {
            return;
        };
        // One that started again since is left alone.
        p.ids.retain(|id| rows.iter().any(|e| e.id == *id && e.status.is_terminal()));
        let unfinished = rows
            .iter()
            .filter(|e| p.ids.contains(&e.id) && e.status != Status::Done)
            .count();
        let (what, partial) = match p.ids.as_slice() {
            [] => {
                self.remove_prompt = None;
                return;
            }
            [id] => (
                rows.iter().find(|e| e.id == *id).map(display_name).unwrap_or_default(),
                "It is not finished, so its partial file goes too.".to_string(),
            ),
            ids => (
                format!("{} downloads", ids.len()),
                match unfinished {
                    1 => "One of them is not finished; its partial file goes too.".to_string(),
                    n => format!("{n} of them are not finished; their partial files go too."),
                },
            ),
        };
        let mut close = false;
        egui::Window::new("Remove from the list")
            .id(egui::Id::new("remove"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!("Remove {what} from the list?"));
                ui.add_space(4.0);
                ui.checkbox(&mut p.delete_files, "Also send the files to the Recycle Bin");
                if p.delete_files && unfinished > 0 {
                    ui.colored_label(ui.visuals().warn_fg_color, partial);
                }
                ui.checkbox(&mut p.dont_ask, "Don't ask again")
                    .on_hover_text("Options > General can turn the question back on");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(connected, egui::Button::new("Remove")).clicked() {
                        actions.push(Action::RemoveNow {
                            ids: p.ids.clone(),
                            delete_files: p.delete_files,
                            dont_ask: p.dont_ask,
                        });
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
        if close {
            self.remove_prompt = None;
        }
    }

    /// The New link dialog: paste a new address, or take the next download
    /// from the browser; and, when the new link is for another file, whether
    /// to start over. It closes once the download has its new link.
    fn refresh_window(&mut self, ctx: &egui::Context, rows: &[DownloadEntry], connected: bool, actions: &mut Vec<Action>) {
        let Some((id, url)) = self.refresh.as_mut() else {
            return;
        };
        let id = *id;
        let (waiting, mismatch) = {
            let s = self.shared.lock().unwrap();
            (s.refresh_waiting == Some(id), s.refresh_mismatch.clone().filter(|m| m.0 == id))
        };
        // Retrying replaces the entry; a waiting download keeps it with a new
        // URL. Either way the dialog is done.
        let entry = rows.iter().find(|e| e.id == id);
        let Some(entry) = entry.filter(|e| e.status.is_retryable()) else {
            self.refresh = None;
            return;
        };
        let mut close = false;
        egui::Window::new("New link")
            .id(egui::Id::new("refresh"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!("A fresh link for {}", display_name(entry)));
                ui.weak("The part already downloaded is kept if the new link is for the same file.");
                ui.add_space(6.0);
                if let Some((_, new_url, reason)) = &mismatch {
                    ui.colored_label(ui.visuals().warn_fg_color, reason);
                    ui.label("Start over from the beginning with the new link?");
                    ui.horizontal(|ui| {
                        if ui.add_enabled(connected, egui::Button::new("Start over")).clicked() {
                            actions.push(Action::RefreshWith {
                                id,
                                url: new_url.clone(),
                                restart: true,
                            });
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                    return;
                }
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(url).hint_text("Paste the new address").desired_width(360.0));
                    let ok = connected && is_web_url(url.trim());
                    if ui.add_enabled(ok, egui::Button::new("Use this link")).clicked() {
                        actions.push(Action::RefreshWith {
                            id,
                            url: url.trim().to_string(),
                            restart: false,
                        });
                    }
                });
                ui.add_space(4.0);
                if waiting {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Waiting for your next download in the browser...");
                        if ui.button("Stop waiting").clicked() {
                            actions.push(Action::CancelRefresh);
                        }
                    });
                } else if ui
                    .add_enabled(connected, egui::Button::new("Use my next download in the browser"))
                    .on_hover_text("Start the download again on its web page; UIDM takes the new link from it")
                    .clicked()
                {
                    actions.push(Action::RefreshFromBrowser(id));
                }
                ui.add_space(6.0);
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        if close {
            self.refresh = None;
            self.shared.lock().unwrap().refresh_mismatch = None;
        }
    }
}

/// What a row needs besides its download.
struct Row<'a> {
    speed: Option<f64>,
    connected: bool,
    queues: &'a [Queue],
    /// In a queue's view: the queue, this row's place and the row count, for
    /// the move up and down buttons.
    place: Option<(u64, usize, usize)>,
    /// Ticked for exporting.
    selected: bool,
    /// The columns to draw, left to right.
    columns: &'a [Column],
    /// The match the find bar moved to.
    found: bool,
    /// Scroll the list to this row.
    scroll: bool,
}

/// A "Queue" menu that puts the download in a queue.
fn queue_menu(ui: &mut egui::Ui, e: &DownloadEntry, queues: &[Queue], actions: &mut Vec<Action>) {
    ui.menu_button("Queue", |ui| {
        for q in queues {
            if ui.button(&q.name).clicked() {
                actions.push(Action::MoveToQueue {
                    id: e.id,
                    queue: q.id,
                    position: None,
                });
                ui.close_menu();
            }
        }
    });
}

fn draw_row(ui: &mut egui::Ui, e: &DownloadEntry, row: &Row, actions: &mut Vec<Action>) {
    let connected = row.connected;
    let mut ticked = row.selected;
    if ui.checkbox(&mut ticked, "").on_hover_text("Tick to export it").changed() {
        actions.push(Action::Select(e.id, ticked));
    }
    for column in row.columns {
        draw_cell(ui, *column, e, row);
    }
    ui.horizontal(|ui| {
        if let Some((queue, i, len)) = row.place {
            ui.add_enabled_ui(connected, |ui| {
                if ui.add_enabled(i > 0, egui::Button::new("Up").small()).on_hover_text("Run it earlier").clicked() {
                    actions.push(Action::MoveToQueue {
                        id: e.id,
                        queue,
                        position: Some(i - 1),
                    });
                }
                if ui
                    .add_enabled(i + 1 < len, egui::Button::new("Down").small())
                    .on_hover_text("Run it later")
                    .clicked()
                {
                    actions.push(Action::MoveToQueue {
                        id: e.id,
                        queue,
                        position: Some(i + 1),
                    });
                }
            });
        }
        ui.add_enabled_ui(connected, |ui| match e.status {
            Status::Queued | Status::Downloading | Status::Confirming => {
                if ui.small_button("Cancel").clicked() {
                    actions.push(Action::Cancel(e.id));
                }
            }
            Status::Waiting => {
                if ui.small_button("New link").on_hover_text("Give it a fresh download link").clicked() {
                    actions.push(Action::OpenRefresh(e.id));
                }
                if ui.small_button("Start").clicked() {
                    actions.push(Action::Confirm {
                        id: e.id,
                        filename: None,
                        folder: None,
                        start: true,
                        queue: None,
                        file_exists: None,
                        duplicate: None,
                    });
                }
                queue_menu(ui, e, row.queues, actions);
                if ui.small_button("Remove").clicked() {
                    actions.push(Action::Remove(vec![e.id]));
                }
            }
            Status::Failed | Status::Canceled | Status::Interrupted => {
                if let Some(host) = e.error.as_deref().and_then(|m| m.strip_prefix("This site needs a login: ")) {
                    if ui.small_button("Add login").clicked() {
                        actions.push(Action::AddLogin(host.to_string()));
                    }
                }
                if ui.small_button("New link").on_hover_text("Give it a fresh download link").clicked() {
                    actions.push(Action::OpenRefresh(e.id));
                }
                if ui.small_button("Retry").on_hover_text("Go on from where it stopped").clicked() {
                    actions.push(Action::Retry(e.id));
                }

                queue_menu(ui, e, row.queues, actions);
                if ui.small_button("Remove").clicked() {
                    actions.push(Action::Remove(vec![e.id]));
                }
            }
            Status::Done => {
                if let Some(path) = &e.path {
                    if ui.small_button("Open").clicked() {
                        actions.push(Action::Open(e.id));
                    }
                    if ui.small_button("Show in folder").clicked() {
                        actions.push(Action::ShowInFolder(path.clone()));
                    }
                }

                if ui.small_button("Remove").clicked() {
                    actions.push(Action::Remove(vec![e.id]));
                }
            }
        });
        row_menu(ui, e, connected, actions);
    });
}

/// The "More" menu at the end of a row: Properties, its own speed limit,
/// and downloading it again.
fn row_menu(ui: &mut egui::Ui, e: &DownloadEntry, connected: bool, actions: &mut Vec<Action>) {
    ui.menu_button("More", |ui| {
        if ui.button("Properties...").on_hover_text("Its details, link, description and login").clicked() {
            actions.push(Action::OpenProperties(e.id));
            ui.close_menu();
        }
        if ui.button("Connections...").on_hover_text("Each connection's range and progress").clicked() {
            actions.push(Action::OpenConnections(e.id));
            ui.close_menu();
        }
        let limit = ui
            .add_enabled(connected && e.status != Status::Done, egui::Button::new("Speed limit..."))
            .on_hover_text("A limit for this download alone");
        if limit.clicked() {
            actions.push(Action::OpenSpeedLimit(e.id));
            ui.close_menu();
        }
        let again = matches!(e.status, Status::Done | Status::Failed | Status::Canceled | Status::Interrupted);
        let redo = ui
            .add_enabled(connected && again, egui::Button::new("Download again"))
            .on_hover_text("Start over from the first byte; if the file is still there, the file-exists option decides");
        if redo.clicked() {
            actions.push(Action::Redownload(e.id));
            ui.close_menu();
        }
    });
}

/// One cell of a download's row.
fn draw_cell(ui: &mut egui::Ui, column: Column, e: &DownloadEntry, row: &Row) {
    // A stream counts segments; its size is only known once fetched.
    let parts = e.parts.filter(|[_, all]| *all > 0);
    let running = row.speed.filter(|s| *s > 0.0 && e.status == Status::Downloading);
    match column {
        Column::Name => {
            // A fixed-width, left-aligned cell so long names truncate instead
            // of pushing the action buttons off the window.
            ui.allocate_ui_with_layout(
                egui::vec2(NAME_WIDTH, 18.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_width(NAME_WIDTH);
                    let mut name = egui::RichText::new(display_name(e));
                    if row.found {
                        let selection = ui.visuals().selection;
                        name = name.background_color(selection.bg_fill).color(selection.stroke.color);
                    }
                    let label = ui
                        .add(egui::Label::new(name).truncate())
                        .on_hover_text(e.path.as_deref().unwrap_or(&e.url));
                    if row.scroll {
                        label.scroll_to_me(Some(egui::Align::Center));
                    }
                },
            );
        }
        Column::Size => {
            let size = ui.label(match e.total {
                Some(t) => format!("{} / {}", human_bytes(e.done), human_bytes(t)),
                None => human_bytes(e.done),
            });
            if let Some([done, all]) = parts {
                size.on_hover_text(format!("{done} of {all} segments"));
            }
        }
        Column::Progress => {
            let bar = egui::ProgressBar::new(columns::fraction(e).unwrap_or(0.0)).desired_width(130.0);
            ui.add(if e.total.is_some() || e.status == Status::Done {
                bar.show_percentage()
            } else if let Some([done, all]) = parts {
                bar.text(format!("{done}/{all} segments"))
            } else {
                bar.text(human_bytes(e.done))
            });
        }
        Column::Speed => {
            ui.label(running.map(|bps| format!("{}/s", human_bytes(bps as u64))).unwrap_or_default());
        }
        Column::TimeLeft => {
            ui.label(columns::time_left(e, running).map(human_duration).unwrap_or_default());
        }
        Column::Status => {
            let visuals = ui.visuals().clone();
            let (text, color) = match e.status {
                Status::Queued => ("Queued", visuals.weak_text_color()),
                Status::Downloading => ("Downloading", visuals.text_color()),
                Status::Done => ("Done", visuals.text_color()),
                Status::Failed => ("Failed", visuals.error_fg_color),
                Status::Canceled => ("Canceled", visuals.warn_fg_color),
                Status::Interrupted => ("Interrupted", visuals.warn_fg_color),
                Status::Confirming => ("Confirm", visuals.weak_text_color()),
                Status::Waiting => ("Waiting", visuals.weak_text_color()),
            };
            ui.horizontal(|ui| {
                let status = ui.colored_label(color, text);
                if let Some(err) = &e.error {
                    status.on_hover_text(err);
                }
                if let Some(scan) = &e.scan {
                    scan_label(ui, scan);
                }
                if let Some(note) = &e.note {
                    ui.weak("(note)").on_hover_text(note);
                }
                if let Some(limit) = e.speed_limit {
                    ui.weak("(limited)").on_hover_text(format!("Limited to {}/s", human_bytes(limit)));
                }
            });
        }
        Column::LastTry => {
            ui.label(e.last_try.map(columns::local_date).unwrap_or_default());
        }
        Column::SaveTo => {
            let folder = e.path.as_deref().and_then(|p| Path::new(p).parent()).map(|d| d.display().to_string());
            text_cell(ui, folder.as_deref().unwrap_or(""));
        }
        Column::Referrer => {
            text_cell(ui, e.referrer.as_deref().unwrap_or(""));
        }
        Column::Description => {
            text_cell(ui, e.description.as_deref().unwrap_or(""));
        }
    }
}

/// A long text, cut to the name column's width with the rest in a tooltip.
fn text_cell(ui: &mut egui::Ui, text: &str) {
    ui.allocate_ui_with_layout(
        egui::vec2(NAME_WIDTH, 18.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_width(NAME_WIDTH);
            let label = ui.add(egui::Label::new(text).truncate());
            if !text.is_empty() {
                label.on_hover_text(text);
            }
        },
    );
}

/// The saved file's name, or the last part of the URL before it is known.
fn display_name(e: &DownloadEntry) -> String {
    if let Some(name) = e.path.as_deref().and_then(|p| Path::new(p).file_name()) {
        return name.to_string_lossy().into_owned();
    }
    let no_query = e.url.split(['?', '#']).next().unwrap_or(&e.url);
    match no_query.rsplit('/').next() {
        Some(tail) if !tail.is_empty() => tail.to_string(),
        _ => e.url.clone(),
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn human_duration(secs: f64) -> String {
    let s = secs.round().max(0.0) as u64;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {}m", s / 3600, (s % 3600) / 60),
    }
}
