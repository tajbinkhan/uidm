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

#[path = "gui/answers.rs"]
mod answers;
#[path = "gui/columns.rs"]
mod columns;
#[path = "gui/connections.rs"]
mod connections;
#[cfg(windows)]
#[path = "gui/droptarget.rs"]
mod droptarget;
#[cfg(not(windows))]
#[path = "gui/droptarget_other.rs"]
mod droptarget;
#[path = "gui/find.rs"]
mod find;
#[path = "gui/help.rs"]
mod help;
#[path = "gui/i18n.rs"]
mod i18n;
#[path = "gui/links.rs"]
mod links;
#[path = "gui/login.rs"]
mod login;
#[path = "gui/progress.rs"]
mod progress;
#[path = "gui/properties.rs"]
mod properties;
#[path = "gui/queues.rs"]
mod queues;
#[path = "gui/theme.rs"]
mod theme;
#[path = "gui/tools.rs"]
mod tools;
#[path = "gui/tray.rs"]
mod tray;
#[cfg(windows)]
#[path = "gui/win.rs"]
mod win;
#[cfg(not(windows))]
#[path = "gui/win_other.rs"]
mod win;

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use columns::Column;
use i18n::{tr, trf};
use eframe::egui;
#[cfg(windows)]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use uidm::app::MAX_MESSAGE_BYTES;
use uidm::notify::Sound;
use uidm::ipc;
use uidm::protocol::{
    read_message, write_frame, Answer, Download, DownloadEntry, DownloadLogin, Incoming, OnDone, Outgoing, Properties, Scan, SegmentInfo,
    Status,
};
use uidm::grab::{GrabFile, GrabRequest};
use uidm::queue::{Queue, WhenDone, MAIN_QUEUE};
use uidm::settings::{
    parse_extensions, CaptureKey, Category, Duplicate, SelectionButton, SocksVersion, ServerLimit, QUOTA_HOURS_RANGE, SERVER_CONNECTIONS_RANGE, FileExists, Notify, ProxyMode, Scanner, Settings, SiteLogin, CAPTURE_MIN_SIZE_MAX_KB,
    CONNECTIONS_RANGE, GENERAL, MAX_CONCURRENT_RANGE, SPEED_LIMIT_RANGE_KBPS, TEXT_SIZE_RANGE, USER_AGENT_PRESETS,
    BROWSER_MENU_ITEMS, PanelView,
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

/// A failure gets its dialog after this long, so a download the browser
/// extension takes back (and removes) right away gets none. The daemon waits
/// the same for its notifications.
const FAILED_GRACE: Duration = Duration::from_secs(1);

fn main() -> eframe::Result {
    let minimized = std::env::args().any(|a| a == "--minimized");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("UIDM")
            // Elsewhere "hidden" is minimized, which needs a visible window.
            .with_visible(!minimized || !cfg!(windows))
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
            #[cfg(windows)]
            if let Ok(handle) = cc.window_handle() {
                if let RawWindowHandle::Win32(h) = handle.as_raw() {
                    win::set_window(h.hwnd.get());
                }
            }
            #[cfg(not(windows))]
            {
                win::set_context(&cc.egui_ctx);
                // Started hidden in the tray: minimized here (see win_other).
                if minimized {
                    win::hide();
                }
            }
            theme::apply(&cc.egui_ctx);
            // Until the settings arrive, follow Windows.
            i18n::set_language("");
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
    /// A countdown before a finished queue or download sleeps or shuts down:
    /// the action, when it happens, the queue's name, and the download's
    /// file name when one download asked for it.
    countdown: Option<(WhenDone, Instant, String, Option<String>)>,
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
    /// The tray asked for a batch from the clipboard's links.
    clipboard_batch: bool,
    /// Downloads that failed for want of a login, to ask for one.
    login_wanted: VecDeque<u64>,
    /// Each download's connections, from its latest progress.
    segments: HashMap<u64, Vec<SegmentInfo>>,
    /// Downloads that just started, for opening their window.
    started: Vec<u64>,
    /// Links from a page for the link picker, until the window opens it.
    picked_links: Option<links::LinksDialog>,
    /// The quota's warning for the status bar, while it matters.
    quota: Option<String>,
    /// The last update check's answer, for the About box: a newer release,
    /// none, or why it could not look.
    update: Option<Result<Option<uidm::update::Release>, String>>,
    /// A newer release, for the status bar.
    newer_release: Option<uidm::update::Release>,
    /// Update now is under way: `None` while the daemon downloads the
    /// update, or why it failed.
    installing: Option<Option<String>>,
    /// A zip preview the daemon sent: the download and its files, or why
    /// not.
    zip_listed: Option<(u64, Result<Vec<uidm::zip::ZipEntry>, String>)>,
    /// Downloads are paused for the quota.
    quota_paused: bool,
}

/// News of a site crawl.
enum GrabEvent {
    /// Pages read, bytes read, files kept, file links looked at.
    Progress(u64, u64, usize, u64),
    /// The files kept, pages read, file links looked at, and why it stopped.
    Done(Vec<GrabFile>, u64, u64, Option<String>),
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
            (0, 0) => tr("UIDM: no active downloads").to_string(),
            (0, q) => trf!("UIDM: {queued} queued", queued = q),
            (a, _) => trf!("UIDM: {active} downloading at {speed}/s", active = a, speed = human_bytes(speed as u64)),
        }
    }

    fn apply(&mut self, msg: &Outgoing) {
        self.table.apply(msg);
        match msg {
            Outgoing::Queues { items } => self.queues = items.clone(),
            Outgoing::Countdown {
                action,
                seconds,
                queue,
                file,
            } => {
                let deadline = Instant::now() + Duration::from_secs(*seconds as u64);
                self.countdown = Some((*action, deadline, queue.clone(), file.clone()));
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
                self.started.push(*id);
            }
            Outgoing::Quota {
                used,
                limit,
                paused,
                resumes_at,
            } => {
                let amount = trf!("{used} of {limit}", used = human_bytes(*used), limit = human_bytes(*limit));
                let was_paused = std::mem::replace(&mut self.quota_paused, *paused);
                self.quota = if *paused {
                    Some(match *resumes_at {
                        Some(t) => trf!("Download quota reached ({amount}); downloads are paused. They resume around {time}.", amount = amount, time = columns::local_date(t)),
                        None => trf!("Download quota reached ({amount}); downloads are paused.", amount = amount),
                    })
                } else if was_paused {
                    self.notice = Some(tr("The download quota allows downloads again; paused ones resumed.").into());
                    None
                } else if *used >= *limit / 10 * 9 {
                    Some(trf!("Download quota nearly used: {amount}. Downloads pause when it runs out.", amount = amount))
                } else {
                    None
                };
            }
            Outgoing::UpdateFailed { error } => {
                self.installing = Some(Some(error.clone()));
            }
            Outgoing::Update { newer, error } => {
                self.update = Some(match error {
                    Some(e) => Err(e.clone()),
                    None => Ok(newer.clone()),
                });
                if newer.is_some() {
                    self.newer_release = newer.clone();
                }
            }
            Outgoing::ZipPreview { id, entries, error } => {
                let result = match error {
                    Some(e) => Err(e.clone()),
                    None => Ok(entries.clone()),
                };
                self.zip_listed = Some((*id, result));
            }
            Outgoing::PickLinks { page, links, user_agent } => {
                self.picked_links = Some(links::LinksDialog::new(page.clone(), links.clone(), user_agent.clone()));
            }
            Outgoing::AddedMany { count, skipped } => {
                let mut notice = match count {
                    1 => tr("Added 1 download, waiting in its queue.").to_string(),
                    n => trf!("Added {count} downloads, waiting in their queue.", count = n),
                };
                match skipped {
                    0 => {}
                    1 => {
                        notice.push(' ');
                        notice.push_str(tr("Skipped 1 already in the list."));
                    }
                    n => {
                        notice.push(' ');
                        notice.push_str(&trf!("Skipped {count} already in the list.", count = n));
                    }
                }
                self.notice = Some(notice);
            }
            Outgoing::GrabProgress { pages, bytes, found, explored } => {
                self.grab_events.push_back(GrabEvent::Progress(*pages, *bytes, *found, *explored));
            }
            Outgoing::GrabDone { files, pages, explored, stopped } => {
                self.grab_events.push_back(GrabEvent::Done(files.clone(), *pages, *explored, stopped.clone()));
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
            Outgoing::Error { message } | Outgoing::Rejected { message, .. } => self.last_error = Some(message.clone()),
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
    /// Show Windows' Open with picker for a file.
    OpenWith(String),
    /// List the files inside a waiting zip download.
    PreviewZip(u64),
    OpenConvert(u64),
    /// Convert a finished video, and maybe delete the original.
    Convert(u64, uidm::media::convert::Target, bool),
    OpenAbout,
    /// Look for a newer release now.
    CheckUpdate,
    /// Download and run the newer release's Setup.
    InstallUpdate,
    Reconnect,
    OpenOptions,
    /// Open Options at the Connection tab with a new login for this host.
    /// Ask for the login a failed download's site wants.
    OpenLogin(u64),
    /// Add a download as it is (one with a login, from the URL bar).
    AddDownload(Download),
    SaveSettings(Settings),
    /// Send new settings without the Options dialog (the speed limit toggle).
    QuickSettings(Settings),
    /// Answer a save-as dialog, or start a waiting download.
    Confirm(Answer),
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
    /// Sleep, hibernate or shut down without waiting for the countdown.
    CountdownNow,
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
    /// Open the Batch dialog with every link on the clipboard.
    BatchFromClipboard,
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
    /// Open a download's own window with its status, Pause and Cancel.
    OpenProgress(u64),
    /// Turn off one of the download window's tabs.
    HideProgressTab(progress::Tab),
    /// Set or clear what a download does once it finishes.
    SetOnDone(u64, Option<OnDone>),
    /// Stop showing the note that a queue's schedule needs UIDM running.
    HideScheduleNote,
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
    /// Many downloads waiting for answers before they are added.
    answers: Option<answers::Prompt>,
    /// The login prompt on screen.
    login: Option<login::LoginPrompt>,
    /// The URL bar's login fields while shown: user name and password.
    add_login: Option<(String, String)>,
    /// "Don't show this dialog again" in the completion dialog.
    hide_completed: bool,
    /// A file whose scan found a threat, waiting for "Open anyway": its path,
    /// name and the scanner's words.
    threat_prompt: Option<(String, String, String)>,
    /// The tray tooltip last set, so it is only updated when it changes.
    tooltip: String,
    /// Whether the settings last showed the tray icon, so a change shows
    /// or hides it once.
    tray_shown: Option<bool>,
    /// The theme and text size last applied, so they are only set when the
    /// settings change them (Ctrl+Plus and Ctrl+Minus zoom in between).
    appearance: Option<(uidm::settings::Theme, u32)>,
    /// The language setting last applied.
    language: Option<String>,
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
    /// The column layout changed and waits to be saved until no drag is on.
    layout_unsaved: bool,
    /// Open Properties dialogs, one per download.
    properties: Vec<properties::PropertiesDialog>,
    /// The confirmation before removing downloads.
    remove_prompt: Option<RemovePrompt>,
    /// The speed limit dialog for one download.
    limit_dialog: Option<LimitDialog>,
    /// Downloads whose Connections dialog is open.
    connection_dialogs: Vec<u64>,
    /// Open download windows, one per download.
    progress_windows: Vec<progress::ProgressWindow>,
    /// The link picker, while it is open.
    links: Option<links::LinksDialog>,
    /// A zip download's preview, asked for or shown.
    zip_preview: Option<ZipPreview>,
    /// The Convert dialog: the video, what it becomes, and whether the
    /// original goes.
    convert: Option<(u64, uidm::media::convert::Target, bool)>,
    /// This window opened the drop target, so finding it closed means the
    /// user closed it.
    drop_shown: bool,
    /// The About box is open; `true` in the second place while a check
    /// it started waits for its answer.
    about: Option<bool>,
    /// The tip of the day, while it shows.
    tip: Option<help::TipWindow>,
    /// The startup tip was considered, once the settings arrived.
    tip_considered: bool,
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

/// The files inside a zip download, once the daemon has listed them.
struct ZipPreview {
    id: u64,
    result: Option<Result<Vec<uidm::zip::ZipEntry>, String>>,
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
    if ui.button(tr("Browse...")).clicked() {
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

/// A "Browse..." button that opens the file picker on its own thread, for
/// files with extension `ext`, and puts the choice in `slot`.
fn browse_file_button(ui: &mut egui::Ui, title: &'static str, ext: &'static str, slot: &Picked) {
    if ui.button(tr("Browse...")).clicked() {
        let (slot, ctx) = (Arc::clone(slot), ui.ctx().clone());
        thread::spawn(move || {
            if let Some(file) = rfd::FileDialog::new().set_title(title).add_filter(ext, &[ext]).pick_file() {
                *slot.lock().unwrap() = Some(file);
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
    /// The category it is saved and listed under.
    category: String,
    description: String,
    /// The category being added with "+", if its editor is open.
    new_category: Option<NewCategory>,
    /// "Remember this path for <category>".
    remember_folder: bool,
    /// "Remember my selection" for a duplicate address.
    remember_duplicate: bool,
    /// "Don't ask me again" for the Download later queue.
    remember_queue: bool,
}

/// A category added from the save-as dialog.
#[derive(Default)]
struct NewCategory {
    name: String,
    extensions: String,
    /// Empty means a folder named after it inside the download folder.
    folder: String,
    picked: Picked,
    error: Option<String>,
}

impl NewCategory {
    /// The settings with this category added, checked as Options would.
    fn add_to(&self, settings: &Settings) -> Result<Settings, String> {
        let mut new = settings.clone();
        let folder = self.folder.trim();
        new.categories.push(Category {
            extensions: parse_extensions(&self.extensions),
            folder: (!folder.is_empty()).then(|| folder.to_string()),
            ..Category::new(self.name.trim(), &[])
        });
        new.validate()?;
        Ok(new)
    }
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
            SpeedUnit::Kb => tr("KB/s"),
            SpeedUnit::Mb => tr("MB/s"),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum OptionsTab {
    General,
    Categories,
    Capture,
    Video,
    Notifications,
    Connection,
}

/// One proxy's login being edited.
#[derive(Default)]
struct ProxyLoginDraft {
    username: String,
    /// A new password, when `changed`.
    password: String,
    changed: bool,
}

impl ProxyLoginDraft {
    fn new(username: &str) -> Self {
        ProxyLoginDraft {
            username: username.to_string(),
            ..ProxyLoginDraft::default()
        }
    }

    fn new_password(&self) -> Option<String> {
        self.changed.then(|| self.password.clone())
    }
}

/// A browser in Options' list: whether it was registered when the dialog
/// opened, and whether it should be.
struct BrowserDraft {
    browser: uidm::browsers::Browser,
    was: bool,
    on: bool,
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
    tray_icon: bool,
    /// Kept as it is; the View menu changes it.
    categories_panel: bool,
    start_with_windows: bool,
    notify: Notify,
    sound_on_done: bool,
    sound_on_failed: bool,
    sound_on_queue_done: bool,
    /// The WAV files, in `SOUND_EVENTS` order; empty for Windows' sound.
    sound_files: [String; 3],
    sound_picked: [Picked; 3],
    scan_after_download: bool,
    scanner: Scanner,
    scanner_command: String,
    proxy_mode: ProxyMode,
    proxy_http: String,
    proxy_https: String,
    proxy_ftp: String,
    ftp_active: bool,
    proxy_socks5: String,
    proxy_socks_version: SocksVersion,
    proxy_remote_dns: bool,
    proxy_bypass: String,
    proxy_pac_url: String,
    /// The HTTP, HTTPS, FTP and SOCKS proxies' logins, in `ProxyKind::ALL`
    /// order.
    proxy_logins: [ProxyLoginDraft; 4],
    use_browser_proxy: bool,
    browser_proxy_after_errors: bool,
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
    /// Comma-separated.
    auto_start_types: String,
    start_while_asking: bool,
    add_only: bool,
    general_remember_folder: bool,
    /// Kept as it is; only the save-as dialog changes it.
    general_last_folder: Option<String>,
    /// The queue Download later uses without asking; `None` asks.
    later_queue: Option<u64>,
    categories: Vec<CategoryDraft>,
    capture_types: String,
    capture_min_size_kb: u64,
    /// One site per line.
    capture_excluded_sites: String,
    clipboard_monitor: bool,
    clipboard_start: bool,
    video_button: bool,
    /// Each known video type and whether the button offers it.
    video_types: Vec<(String, bool)>,
    /// A type typed in to add to the list.
    video_type_new: String,
    video_min_size_kb: u64,
    video_view: PanelView,
    video_protected_note: bool,
    /// Each context menu item, its label, and whether it shows.
    menu_items: Vec<(&'static str, &'static str, bool)>,
    /// One site per line.
    video_button_excluded_sites: String,
    ffmpeg_path: String,
    /// ffmpeg.exe chosen with Browse.
    ffmpeg_picked: Picked,
    confirm_remove: bool,
    remove_deletes_files: bool,
    server_file_date: bool,
    resume_ignores_date: bool,
    manual_user_agent: String,
    /// Empty writes partial files in place.
    temp_dir: String,
    temp_picked: Picked,
    /// The browsers found or registered, with their registration.
    browsers: Vec<BrowserDraft>,
    /// Browsers found that read Chrome's registration.
    chrome_readers: Vec<&'static str>,
    capture_skip_key: CaptureKey,
    capture_force_key: CaptureKey,
    capture_keys_left_click: bool,
    capture_force_skips_pages: bool,
    /// One address per line.
    capture_excluded_urls: String,
    offer_exceptions: bool,
    selection_button: SelectionButton,
    /// One site per line.
    selection_button_sites: String,
    /// Each server and its most connections.
    server_limits: Vec<(String, usize)>,
    quota_enabled: bool,
    quota_mb: u64,
    quota_hours: u64,
    quota_warn: bool,
    /// The scanner program chosen with Browse.
    scanner_picked: Picked,
    check_updates: bool,
    drop_target: bool,
    progress_on_start: bool,
    progress_speed_tab: bool,
    progress_done_tab: bool,
    /// Kept as it is; only the Queues dialog changes it.
    schedule_note_hidden: bool,
    tips_on_startup: bool,
    /// Kept as it is; only the tip window changes it.
    next_tip: usize,
    theme: uidm::settings::Theme,
    text_size: u32,
    /// Empty follows Windows.
    language: String,
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
            tray_icon: s.tray_icon,
            categories_panel: s.categories_panel,
            start_with_windows: s.start_with_windows,
            notify: s.notify,
            sound_on_done: s.sound_on_done,
            sound_on_failed: s.sound_on_failed,
            sound_on_queue_done: s.sound_on_queue_done,
            sound_files: [s.sound_done_file.clone(), s.sound_failed_file.clone(), s.sound_queue_file.clone()],
            sound_picked: Default::default(),
            scan_after_download: s.scan_after_download,
            scanner: s.scanner,
            scanner_command: s.scanner_command.clone(),
            proxy_mode: s.proxy_mode,
            proxy_http: s.proxy_http.clone(),
            proxy_https: s.proxy_https.clone(),
            proxy_ftp: s.proxy_ftp.clone(),
            ftp_active: s.ftp_active,
            proxy_socks5: s.proxy_socks5.clone(),
            proxy_socks_version: s.proxy_socks_version,
            proxy_remote_dns: s.proxy_remote_dns,
            proxy_bypass: s.proxy_bypass.clone(),
            proxy_pac_url: s.proxy_pac_url.clone(),
            proxy_logins: [
                ProxyLoginDraft::new(&s.proxy_username),
                ProxyLoginDraft::new(&s.proxy_https_username),
                ProxyLoginDraft::new(&s.proxy_ftp_username),
                ProxyLoginDraft::new(&s.proxy_socks_username),
            ],
            use_browser_proxy: s.use_browser_proxy,
            browser_proxy_after_errors: s.browser_proxy_after_errors,
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
            auto_start_types: s.auto_start_types.join(", "),
            start_while_asking: s.start_while_asking,
            add_only: s.add_only,
            general_remember_folder: s.general_remember_folder,
            general_last_folder: s.general_last_folder.clone(),
            later_queue: s.later_queue,
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
            clipboard_start: s.clipboard_start,
            video_button: s.video_button,
            video_types: {
                let mut all: Vec<(String, bool)> = uidm::settings::default_video_types()
                    .into_iter()
                    .map(|t| {
                        let on = s.video_types.contains(&t);
                        (t, on)
                    })
                    .collect();
                for t in &s.video_types {
                    if !all.iter().any(|(k, _)| k == t) {
                        all.push((t.clone(), true));
                    }
                }
                all
            },
            video_type_new: String::new(),
            video_min_size_kb: s.video_min_size_kb,
            video_view: s.video_view,
            video_protected_note: s.video_protected_note,
            menu_items: BROWSER_MENU_ITEMS
                .iter()
                .map(|(id, label)| (*id, *label, !s.browser_menu_hidden.iter().any(|h| h == id)))
                .collect(),
            video_button_excluded_sites: s.video_button_excluded_sites.join("\n"),
            ffmpeg_path: s.ffmpeg_path.clone(),
            ffmpeg_picked: Picked::default(),
            confirm_remove: s.confirm_remove,
            remove_deletes_files: s.remove_deletes_files,
            server_file_date: s.server_file_date,
            resume_ignores_date: s.resume_ignores_date,
            manual_user_agent: s.manual_user_agent.clone(),
            temp_dir: s.temp_dir.clone().unwrap_or_default(),
            temp_picked: Picked::default(),
            browsers: uidm::browsers::Registrar::for_this_user()
                .map(|r| {
                    uidm::browsers::BROWSERS
                        .iter()
                        .map(|b| (b, r.registered(b), r.installed(b)))
                        .filter(|(_, registered, installed)| *registered || *installed)
                        .map(|(b, registered, _)| BrowserDraft {
                            browser: *b,
                            was: registered,
                            on: registered,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            chrome_readers: uidm::browsers::chrome_readers(),
            capture_skip_key: s.capture_skip_key,
            capture_force_key: s.capture_force_key,
            capture_keys_left_click: s.capture_keys_left_click,
            capture_force_skips_pages: s.capture_force_skips_pages,
            capture_excluded_urls: s.capture_excluded_urls.join("\n"),
            offer_exceptions: s.offer_exceptions,
            selection_button: s.selection_button,
            selection_button_sites: s.selection_button_sites.join("\n"),
            server_limits: s.server_limits.iter().map(|l| (l.site.clone(), l.max)).collect(),
            quota_enabled: s.quota_enabled,
            quota_mb: s.quota_mb,
            quota_hours: s.quota_hours,
            quota_warn: s.quota_warn,
            scanner_picked: Picked::default(),
            check_updates: s.check_updates,
            drop_target: s.drop_target,
            progress_on_start: s.progress_on_start,
            progress_speed_tab: s.progress_speed_tab,
            progress_done_tab: s.progress_done_tab,
            schedule_note_hidden: s.schedule_note_hidden,
            tips_on_startup: s.tips_on_startup,
            next_tip: s.next_tip,
            theme: s.theme,
            text_size: s.text_size,
            language: s.language.clone(),
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
            tray_icon: self.tray_icon,
            categories_panel: self.categories_panel,
            start_with_windows: self.start_with_windows,
            notify: self.notify,
            sound_on_done: self.sound_on_done,
            sound_on_failed: self.sound_on_failed,
            sound_on_queue_done: self.sound_on_queue_done,
            sound_done_file: self.sound_files[0].trim().to_string(),
            sound_failed_file: self.sound_files[1].trim().to_string(),
            sound_queue_file: self.sound_files[2].trim().to_string(),
            scan_after_download: self.scan_after_download,
            scanner: self.scanner,
            scanner_command: self.scanner_command.trim().to_string(),
            proxy_mode: self.proxy_mode,
            proxy_http: self.proxy_http.trim().to_string(),
            proxy_https: self.proxy_https.trim().to_string(),
            proxy_ftp: self.proxy_ftp.trim().to_string(),
            ftp_active: self.ftp_active,
            proxy_socks5: self.proxy_socks5.trim().to_string(),
            proxy_socks_version: self.proxy_socks_version,
            proxy_remote_dns: self.proxy_remote_dns,
            proxy_bypass: self.proxy_bypass.trim().to_string(),
            proxy_pac_url: self.proxy_pac_url.trim().to_string(),
            proxy_username: self.proxy_logins[0].username.trim().to_string(),
            proxy_password: self.proxy_logins[0].new_password(),
            proxy_https_username: self.proxy_logins[1].username.trim().to_string(),
            proxy_https_password: self.proxy_logins[1].new_password(),
            proxy_ftp_username: self.proxy_logins[2].username.trim().to_string(),
            proxy_ftp_password: self.proxy_logins[2].new_password(),
            proxy_socks_username: self.proxy_logins[3].username.trim().to_string(),
            proxy_socks_password: self.proxy_logins[3].new_password(),
            proxy_logins_split: false,
            use_browser_proxy: self.use_browser_proxy,
            browser_proxy_after_errors: self.browser_proxy_after_errors,
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
            auto_start_types: parse_extensions(&self.auto_start_types),
            start_while_asking: self.start_while_asking,
            add_only: self.add_only,
            general_remember_folder: self.general_remember_folder,
            general_last_folder: self.general_last_folder.clone().filter(|_| self.general_remember_folder),
            later_queue: self.later_queue,
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
            clipboard_start: self.clipboard_start,
            video_button: self.video_button,
            video_types: self.video_types.iter().filter(|(_, on)| *on).map(|(t, _)| t.clone()).collect(),
            video_min_size_kb: self.video_min_size_kb,
            video_view: self.video_view,
            video_protected_note: self.video_protected_note,
            browser_menu_hidden: self.menu_items.iter().filter(|(_, _, on)| !on).map(|(id, _, _)| id.to_string()).collect(),
            video_button_excluded_sites: sites(&self.video_button_excluded_sites),
            ffmpeg_path: self.ffmpeg_path.trim().to_string(),
            confirm_remove: self.confirm_remove,
            remove_deletes_files: self.remove_deletes_files,
            server_file_date: self.server_file_date,
            resume_ignores_date: self.resume_ignores_date,
            manual_user_agent: self.manual_user_agent.trim().to_string(),
            temp_dir: optional(&self.temp_dir),
            capture_skip_key: self.capture_skip_key,
            capture_force_key: self.capture_force_key,
            capture_keys_left_click: self.capture_keys_left_click,
            capture_force_skips_pages: self.capture_force_skips_pages,
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
            quota_enabled: self.quota_enabled,
            quota_mb: self.quota_mb,
            quota_hours: self.quota_hours,
            quota_warn: self.quota_warn,
            check_updates: self.check_updates,
            drop_target: self.drop_target,
            progress_on_start: self.progress_on_start,
            progress_speed_tab: self.progress_speed_tab,
            progress_done_tab: self.progress_done_tab,
            schedule_note_hidden: self.schedule_note_hidden,
            tips_on_startup: self.tips_on_startup,
            next_tip: self.next_tip,
            theme: self.theme,
            text_size: self.text_size,
            language: self.language.clone(),
            // The daemon keeps its own.
            last_update_check: 0,
        };
        settings.validate()?;
        if let Some(l) = self.logins.iter().find(|l| l.new && l.password.is_empty()) {
            return Err(trf!("The new login for {site} needs a password.", site = l.site.trim()));
        }
        Ok(settings)
    }

    /// Register or remove the browsers whose box changed, keeping the new
    /// state of those that worked.
    fn apply_browsers(&mut self) -> Result<(), String> {
        let changed: Vec<usize> = (0..self.browsers.len()).filter(|&i| self.browsers[i].on != self.browsers[i].was).collect();
        if changed.is_empty() {
            return Ok(());
        }
        let registrar = uidm::browsers::Registrar::for_this_user().map_err(|e| e.to_string())?;
        for i in changed {
            let b = &mut self.browsers[i];
            let done = if b.on { registrar.register(&b.browser) } else { registrar.unregister(&b.browser) };
            done.map_err(|e| trf!("Could not change {browser}: {error}", browser = b.browser.name, error = e))?;
            b.was = b.on;
        }
        Ok(())
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
                clipboard_batch: false,
                login_wanted: VecDeque::new(),
                segments: HashMap::new(),
                started: Vec::new(),
                picked_links: None,
                quota: None,
                zip_listed: None,
                update: None,
                newer_release: None,
                installing: None,
                quota_paused: false,
            })),
            writer: Arc::new(Mutex::new(None)),
            ctx: ctx.clone(),
            url_input: String::new(),
            options: None,
            confirm: None,
            answers: None,
            login: None,
            add_login: None,
            hide_completed: false,
            threat_prompt: None,
            tooltip: String::new(),
            tray_shown: None,
            appearance: None,
            language: None,
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
            layout_unsaved: false,
            properties: Vec::new(),
            remove_prompt: None,
            limit_dialog: None,
            connection_dialogs: Vec::new(),
            progress_windows: Vec::new(),
            links: None,
            zip_preview: None,
            about: None,
            tip: None,
            tip_considered: false,
            drop_shown: false,
            convert: None,
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
        let writer = Arc::clone(&self.writer);
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
                let (types, start) = match &shared.lock().unwrap().settings {
                    Some(s) if s.clipboard_monitor => (s.capture_types.clone(), s.clipboard_start),
                    _ => continue,
                };
                let Some(text) = win::clipboard_text() else { continue };
                let Some(url) = uidm::batch::clipboard_link(&text, &types) else { continue };
                if last_offered.as_deref() == Some(url.as_str()) {
                    continue;
                }
                last_offered = Some(url.clone());
                // Started at once, it goes the way a pasted link does, save-as
                // dialog and all; only the offer is skipped.
                if start && send(&writer, &Incoming::Add(Download { url: url.clone(), ..Download::default() })) {
                    continue;
                }
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
                    if is_download_url(url) {
                        send(
                            &writer,
                            &Incoming::Add(Download {
                                url: url.to_string(),
                                ..Download::default()
                            }),
                        );
                    } else {
                        shared.lock().unwrap().last_error =
                            Some(tr("The clipboard holds no http or https link.").into());
                        win::show();
                    }
                }
                tray::Command::BatchFromClipboard => {
                    shared.lock().unwrap().clipboard_batch = true;
                    win::show();
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
                tray::Command::ToggleDropTarget => {
                    let settings = shared.lock().unwrap().settings.clone();
                    if let Some(s) = settings {
                        send(
                            &writer,
                            &Incoming::SetSettings {
                                settings: Settings {
                                    drop_target: !s.drop_target,
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
            self.shared.lock().unwrap().last_error = Some(trf!("No tray icon: {error}", error = e));
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
            set_link(Link::Lost(tr("the daemon closed the connection").into()));
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
                let mut s = self.shared.lock().unwrap();
                s.completed.pop_front();
                // "Don't show this dialog again": Windows notifications from
                // now on, and the dialogs still waiting go too.
                if std::mem::take(&mut self.hide_completed) {
                    s.completed.clear();
                    if let Some(mut settings) = s.settings.clone() {
                        settings.notify = Notify::Toast;
                        drop(s);
                        send(&self.writer, &Incoming::SetSettings { settings });
                    }
                }
            }
            Action::OpenWith(path) => win::open_with(&path),
            Action::OpenAbout => self.about = Some(false),
            Action::OpenConvert(id) => self.convert = Some((id, uidm::media::convert::Target::Mp3, false)),
            Action::Convert(id, to, delete_original) => {
                send(&self.writer, &Incoming::Convert { id, to, delete_original });
            }
            Action::InstallUpdate => {
                self.shared.lock().unwrap().installing = Some(None);
                self.about = Some(true);
                send(&self.writer, &Incoming::InstallUpdate);
            }
            Action::CheckUpdate => {
                self.shared.lock().unwrap().update = None;
                self.about = Some(true);
                send(&self.writer, &Incoming::CheckUpdate);
            }
            Action::PreviewZip(id) => {
                self.zip_preview = Some(ZipPreview { id, result: None });
                send(&self.writer, &Incoming::PreviewZip { id });
            }
            Action::ShowInFolder(path) => {
                let _ = uidm::platform::reveal(std::path::Path::new(&path));
            }
            Action::Reconnect => self.connect(),
            Action::OpenOptions => {
                if let Some(settings) = &self.shared.lock().unwrap().settings {
                    self.options = Some(OptionsDraft::new(settings));
                }
            }
            Action::OpenLogin(id) => {
                let mut s = self.shared.lock().unwrap();
                if !s.login_wanted.contains(&id) {
                    s.login_wanted.push_front(id);
                }
            }
            Action::AddDownload(download) => {
                send(&self.writer, &Incoming::Add(download));
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
                    s.save_result = Some(Err(tr("not connected to the UIDM daemon").into()));
                }
            }
            Action::QuickSettings(settings) => {
                send(&self.writer, &Incoming::SetSettings { settings });
            }
            Action::Confirm(answer) => {
                send(&self.writer, &Incoming::Confirm(answer));
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
            Action::OpenProgress(id) => {
                let entry = self.shared.lock().unwrap().table.get(id).cloned();
                if let Some(e) = entry.filter(|e| e.status != Status::Done && !self.progress_windows.iter().any(|w| w.id == id)) {
                    self.progress_windows.push(progress::ProgressWindow::new(&e));
                }
            }
            Action::HideProgressTab(tab) => {
                let settings = self.shared.lock().unwrap().settings.clone();
                if let Some(mut s) = settings {
                    match tab {
                        progress::Tab::SpeedLimit => s.progress_speed_tab = false,
                        progress::Tab::OnDone => s.progress_done_tab = false,
                        progress::Tab::Status => return,
                    }
                    self.perform(Action::QuickSettings(s));
                }
            }
            Action::HideScheduleNote => {
                let settings = self.shared.lock().unwrap().settings.clone();
                if let Some(s) = settings {
                    self.perform(Action::QuickSettings(Settings {
                        schedule_note_hidden: true,
                        ..s
                    }));
                }
            }
            Action::SetOnDone(id, on_done) => {
                send(&self.writer, &Incoming::SetOnDone { id, on_done });
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
            Action::CountdownNow => {
                send(&self.writer, &Incoming::CountdownNow);
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
                // Addresses in the list and taken names may need answers
                // first, when Options says to ask.
                let (rows, settings) = {
                    let s = self.shared.lock().unwrap();
                    (s.table.entries().cloned().collect::<Vec<_>>(), s.settings.clone().unwrap_or_default())
                };
                match answers::Prompt::new(items, queue, start_queue, &rows, &settings) {
                    Ok(prompt) => self.answers = Some(prompt),
                    Err(items) => self.add_many(items, queue, start_queue),
                }
            }
            Action::OpenBatch => self.batch = Some(tools::BatchDialog::new()),
            Action::BatchFromClipboard => {
                let links = uidm::batch::links_in(&win::clipboard_text().unwrap_or_default());
                if links.is_empty() {
                    self.shared.lock().unwrap().last_error = Some(tr("The clipboard holds no http, https or ftp links.").into());
                } else {
                    self.batch = Some(tools::BatchDialog::with_list(&links));
                }
            }
            Action::OpenImport => tools::pick_text_file(&self.ctx, tr("Import a download list"), &self.import),
            Action::OpenExport => {
                let queue = match self.filter {
                    Filter::Queue(id) => Some(id),
                    _ => None,
                };
                self.export = Some(tools::ExportDialog::new(!self.selected.is_empty(), queue));
            }
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
            if !tools::export_dialog(ctx, d, rows, &self.selected, &queue_list) {
                self.export = None;
            }
        }
        let events: Vec<GrabEvent> = self.shared.lock().unwrap().grab_events.drain(..).collect();
        if let Some(d) = self.grabber.as_mut() {
            for event in events {
                match event {
                    GrabEvent::Progress(pages, bytes, found, explored) => d.progress(pages, bytes, found, explored),
                    GrabEvent::Done(files, pages, explored, stopped) => d.done(files, pages, explored, stopped),
                }
            }
            if !connected && d.running() {
                d.failed(tr("The connection to the UIDM daemon was lost.").into());
            }
            let settings = self.shared.lock().unwrap().settings.clone().unwrap_or_default();
            let done: HashSet<String> = rows.iter().filter(|e| e.status == Status::Done).map(|e| e.url.clone()).collect();
            if !tools::grab_dialog(ctx, d, &queue_list, &settings, &done, connected, actions) {
                self.grabber = None;
            }
        }
        if std::mem::take(&mut self.shared.lock().unwrap().clipboard_batch) {
            actions.push(Action::BatchFromClipboard);
        }
        if let Some(p) = self.answers.as_mut() {
            match answers::window(ctx, p) {
                Some(true) => {
                    let p = self.answers.take().unwrap();
                    let (queue, start_queue) = (p.queue, p.start_queue);
                    self.add_many(p.into_items(), queue, start_queue);
                }
                Some(false) => self.answers = None,
                None => {}
            }
        }
        let offer = self.shared.lock().unwrap().clipboard.clone();
        if let Some(url) = offer {
            if !tools::clipboard_offer(ctx, &url, connected, actions) {
                self.shared.lock().unwrap().clipboard = None;
            }
        }
    }

    /// The login prompt for the oldest download waiting for a login, with
    /// the others from the same site.
    fn login_window(&mut self, ctx: &egui::Context, rows: &[DownloadEntry], connected: bool, actions: &mut Vec<Action>) {
        let settings = self.shared.lock().unwrap().settings.clone();
        if self.login.is_none() {
            let queued: Vec<u64> = self.shared.lock().unwrap().login_wanted.drain(..).collect();
            // Only those still failed for a login.
            let waiting: Vec<(&DownloadEntry, uidm::engine::LoginWanted)> = queued
                .iter()
                .filter_map(|id| rows.iter().find(|e| e.id == *id && e.status == Status::Failed))
                .filter_map(|e| e.error.as_deref().and_then(uidm::engine::login_wanted).map(|w| (e, w)))
                .collect();
            if let Some((first, wanted)) = waiting.first().cloned() {
                let (same, other): (Vec<_>, Vec<_>) = waiting.iter().partition(|(_, w)| w.host == wanted.host);
                self.shared.lock().unwrap().login_wanted.extend(other.iter().map(|(e, _)| e.id));
                let own_site = same.iter().all(|(e, _)| uidm::net::host_of(&e.url).eq_ignore_ascii_case(&wanted.host));
                let username = settings
                    .as_ref()
                    .and_then(|s| s.site_logins.iter().find(|l| l.site.eq_ignore_ascii_case(&wanted.host)))
                    .map(|l| l.username.clone())
                    .or_else(|| first.login_user.clone())
                    .unwrap_or_default();
                let ids = same.iter().map(|(e, _)| e.id).collect();
                self.login = Some(login::LoginPrompt::new(ids, wanted, own_site, username));
            }
        }
        if let Some(p) = self.login.as_mut() {
            if !login::window(ctx, p, settings.as_ref(), connected, actions) {
                self.login = None;
            }
        }
    }

    /// Add `items` to wait in `queue`, and maybe start it.
    fn add_many(&self, items: Vec<Download>, queue: u64, start_queue: bool) {
        if items.is_empty() {
            return;
        }
        send(&self.writer, &Incoming::AddMany { items, queue: Some(queue) });
        // Sent after on the same connection, so it starts with them.
        if start_queue {
            send(&self.writer, &Incoming::StartQueue { id: queue });
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
        let (queue_list, loaded) = {
            let s = self.shared.lock().unwrap();
            (s.queues.clone(), s.settings.clone())
        };
        let settings = loaded.clone().unwrap_or_default();
        let ask = settings.file_exists == FileExists::Ask;
        if self.confirm.as_ref().map(|c| c.id) != Some(entry.id) {
            let path = entry.path.as_deref().map(Path::new);
            let filename = path
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| display_name(entry));
            let category = settings.listed_under(entry.category.as_deref(), &filename, &entry.url).to_string();
            self.confirm = Some(ConfirmDraft {
                id: entry.id,
                later_queue: MAIN_QUEUE,
                remember_folder: settings.remembers_folder(&category),
                remember_duplicate: false,
                remember_queue: false,
                category,
                filename,
                folder: path
                    .and_then(|p| p.parent())
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                picked: Picked::default(),
                when_taken: FileExists::Rename,
                duplicate: Duplicate::NewName,
                description: entry.description.clone().unwrap_or_default(),
                new_category: None,
            });
        }
        let Some(draft) = self.confirm.as_mut() else {
            return;
        };
        take_picked(&draft.picked, &mut draft.folder);
        let waiting = rows.iter().filter(|e| e.status == Status::Confirming).count();
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

        egui::Window::new(tr("Save download"))
            .id(egui::Id::new("confirm"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::Grid::new("confirm_grid").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                    ui.label(tr("Address"));
                    ui.add(egui::Label::new(&entry.url).truncate())
                        .on_hover_text(&entry.url);
                    ui.end_row();

                    ui.label(tr("File name"));
                    ui.add_enabled(
                        replaced.is_none(),
                        egui::TextEdit::singleline(&mut draft.filename).desired_width(360.0),
                    );
                    ui.end_row();

                    ui.label(tr("Size"));
                    ui.horizontal(|ui| {
                        ui.label(match entry.total {
                            Some(t) => human_bytes(t),
                            None => tr("Unknown").to_string(),
                        });
                        // Started early, while this dialog is open.
                        if entry.done > 0 {
                            ui.weak(trf!("{done} downloaded so far", done = human_bytes(entry.done)));
                        }
                        // A zip on a server that sends ranges can list its
                        // files before it downloads.
                        let zip = draft.filename.trim().to_ascii_lowercase().ends_with(".zip");
                        if zip && entry.resumable {
                            let busy = self.zip_preview.as_ref().is_some_and(|p| p.id == entry.id && p.result.is_none());
                            let preview = ui
                                .add_enabled(connected && !busy, egui::Button::new(tr("Preview")).small())
                                .on_hover_text(tr("List the files inside, reading only the end of the archive"));
                            if preview.clicked() {
                                actions.push(Action::PreviewZip(entry.id));
                            }
                            if busy {
                                ui.spinner();
                            }
                        }
                    });
                    ui.end_row();

                    ui.label(tr("Category"));
                    ui.add_enabled_ui(replaced.is_none(), |ui| {
                        ui.horizontal(|ui| {
                            let before = draft.category.clone();
                            egui::ComboBox::from_id_salt("confirm_category")
                                .selected_text(&draft.category)
                                .width(200.0)
                                .show_ui(ui, |ui| {
                                    for c in &settings.categories {
                                        ui.selectable_value(&mut draft.category, c.name.clone(), &c.name);
                                    }
                                    ui.selectable_value(&mut draft.category, GENERAL.to_string(), GENERAL);
                                });
                            // Picking a category moves the file to its folder.
                            if draft.category != before {
                                draft.folder = settings.category_folder(&draft.category).display().to_string();
                                draft.remember_folder = settings.remembers_folder(&draft.category);
                            }
                            let add = ui
                                .add_enabled(connected && loaded.is_some() && draft.new_category.is_none(), egui::Button::new("+"))
                                .on_hover_text(tr("Add a category"));
                            if add.clicked() {
                                draft.new_category = Some(NewCategory {
                                    extensions: uidm::settings::extension_of(draft.filename.trim()).unwrap_or_default(),
                                    ..NewCategory::default()
                                });
                            }
                        });
                    });
                    ui.end_row();

                    ui.label(tr("Save to"));
                    ui.add_enabled_ui(replaced.is_none(), |ui| {
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut draft.folder).desired_width(280.0));
                            browse_button(ui, tr("Save to"), &draft.folder, &draft.picked);
                        });
                    });
                    ui.end_row();

                    ui.label("");
                    ui.add_enabled(
                        replaced.is_none(),
                        egui::Checkbox::new(
                            &mut draft.remember_folder,
                            trf!("Remember this path for {category}", category = draft.category),
                        ),
                    )
                    .on_hover_text(tr("New files of this category go to the folder last chosen for it"));
                    ui.end_row();

                    ui.label(tr("Description"));
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.description)
                            .hint_text(tr("A note shown in the list"))
                            .desired_width(360.0),
                    );
                    ui.end_row();
                });
                if let Some(old) = other {
                    ui.add_space(4.0);
                    let name = display_name(old);
                    let already = match old.status {
                        Status::Done => trf!("This address is already in the list as {name} (finished).", name = name),
                        Status::Failed => trf!("This address is already in the list as {name} (failed).", name = name),
                        Status::Canceled => trf!("This address is already in the list as {name} (canceled).", name = name),
                        Status::Interrupted => trf!("This address is already in the list as {name} (interrupted).", name = name),
                        Status::Waiting => trf!("This address is already in the list as {name} (waiting).", name = name),
                        Status::Queued | Status::Downloading | Status::Confirming => {
                            trf!("This address is already in the list as {name} (running).", name = name)
                        }
                    };
                    ui.colored_label(ui.visuals().warn_fg_color, already);
                    ui.radio_value(
                        &mut draft.duplicate,
                        Duplicate::NewName,
                        tr("Add it anyway, with a new name if the old file is in the way"),
                    );
                    ui.add_enabled_ui(!other_running, |ui| {
                        ui.radio_value(
                            &mut draft.duplicate,
                            Duplicate::Overwrite,
                            tr("Replace the old download and overwrite its file"),
                        )
                        .on_disabled_hover_text(tr("The old download is still running."));
                    });
                    let existing = match old.status {
                        Status::Done => tr("Don't add it; show the finished download"),
                        s if s.is_retryable() => tr("Don't add it; resume the old download"),
                        _ => tr("Don't add it; the old download is already running"),
                    };
                    ui.radio_value(&mut draft.duplicate, Duplicate::Existing, existing);
                    if let Some(path) = replaced {
                        ui.weak(trf!("{path} will be overwritten.", path = path));
                    }
                    ui.checkbox(&mut draft.remember_duplicate, tr("Remember my selection"))
                        .on_hover_text(tr("Answer the same way for every address already in the list; Options can change it"));
                }
                if taken {
                    ui.add_space(4.0);
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        trf!("{name} already exists in this folder.", name = name),
                    );
                    ui.horizontal(|ui| {
                        for f in [FileExists::Rename, FileExists::Overwrite] {
                            ui.radio_value(&mut draft.when_taken, f, file_exists_label(f));
                        }
                    });
                }
                if waiting > 1 {
                    ui.weak(trf!("{count} more downloads are waiting after this one.", count = waiting - 1));
                }
                // Unless Download later has a queue of its own, ask which.
                let later_set = settings.later_queue.is_some_and(|id| queue_list.iter().any(|q| q.id == id));
                if queue_list.len() > 1 && !later_set {
                    ui.horizontal(|ui| {
                        ui.label(tr("Download later in"));
                        let name = queue_list
                            .iter()
                            .find(|q| q.id == draft.later_queue)
                            .map_or(tr("Main queue"), |q| q.name.as_str());
                        egui::ComboBox::from_id_salt("later_queue")
                            .selected_text(name)
                            .show_ui(ui, |ui| {
                                for q in &queue_list {
                                    ui.selectable_value(&mut draft.later_queue, q.id, &q.name);
                                }
                            });
                        ui.checkbox(&mut draft.remember_queue, tr("Don't ask me again"))
                            .on_hover_text(tr("Download later uses this queue from now on; Options can change it"));
                    });
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(connected, |ui| {
                        // The category goes with it only when it is not the one
                        // the file's type gives anyway.
                        let by_type = settings.category_name(&name, &entry.url);
                        let category = (!draft.category.eq_ignore_ascii_case(by_type)).then(|| draft.category.clone());
                        let description = draft.description.trim();
                        let remember_folder = (replaced.is_none()
                            && draft.remember_folder != settings.remembers_folder(&draft.category))
                        .then_some(draft.remember_folder);
                        let answer = |start: bool| {
                            Action::Confirm(Answer {
                                id: draft.id,
                                filename: Some(draft.filename.trim().to_string()),
                                folder: Some(draft.folder.trim().to_string()),
                                start,
                                queue: (!later_set).then_some(draft.later_queue),
                                file_exists: taken.then_some(draft.when_taken),
                                duplicate: other.map(|_| draft.duplicate),
                                category: category.clone(),
                                description: (!description.is_empty()).then(|| description.to_string()),
                                remember_folder,
                                remember_duplicate: other.is_some() && draft.remember_duplicate,
                                remember_queue: !start && !later_set && draft.remember_queue,
                            })
                        };
                        if other.is_some() && draft.duplicate == Duplicate::Existing {
                            if ui.button(tr("OK")).clicked() {
                                actions.push(answer(true));
                            }
                        } else {
                            if ui.button(tr("Start download")).clicked() {
                                actions.push(answer(true));
                            }
                            if ui.button(tr("Download later")).clicked() {
                                actions.push(answer(false));
                            }
                        }
                        if ui.button(tr("Cancel")).clicked() {
                            actions.push(Action::CancelSaveAs(draft.id));
                        }
                    });
                });
            });

        // The category editor "+" opened, over the dialog. The new category
        // is saved to the settings at once and chosen for this download.
        let Some(new) = draft.new_category.as_mut() else {
            return;
        };
        take_picked(&new.picked, &mut new.folder);
        let (mut added, mut close) = (None, false);
        egui::Window::new(tr("New category"))
            .id(egui::Id::new("confirm_new_category"))
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::Grid::new("new_category_grid").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                    ui.label(tr("Name"));
                    ui.add(egui::TextEdit::singleline(&mut new.name).desired_width(240.0));
                    ui.end_row();

                    ui.label(tr("File types"));
                    ui.add(egui::TextEdit::singleline(&mut new.extensions).hint_text("zip, rar").desired_width(240.0));
                    ui.end_row();

                    ui.label(tr("Folder"));
                    ui.horizontal(|ui| {
                        let hint = trf!("Download folder\\{name}", name = new.name.trim());
                        ui.add(egui::TextEdit::singleline(&mut new.folder).hint_text(hint).desired_width(240.0));
                        browse_button(ui, tr("Category folder"), &new.folder, &new.picked);
                    });
                    ui.end_row();
                });
                if let Some(err) = &new.error {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(connected, egui::Button::new(tr("OK"))).clicked() {
                        match loaded.as_ref().map_or(Err(tr("not connected to the UIDM daemon").to_string()), |s| new.add_to(s)) {
                            Ok(s) => added = Some(s),
                            Err(e) => new.error = Some(e),
                        }
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        close = true;
                    }
                });
            });
        if let Some(s) = added {
            draft.category = new.name.trim().to_string();
            draft.folder = s.category_folder(&draft.category).display().to_string();
            draft.remember_folder = false;
            actions.push(Action::QuickSettings(s));
            close = true;
        }
        if close {
            draft.new_category = None;
        }
    }

    /// Draw the Options dialog while it is open.
    fn options_window(&mut self, ctx: &egui::Context, connected: bool, actions: &mut Vec<Action>) {
        // Pick up the daemon's answer to a save.
        let result = self.shared.lock().unwrap().save_result.take();
        let saving = self.shared.lock().unwrap().saving;
        let queues = self.shared.lock().unwrap().queues.clone();
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
        // A picked scanner keeps the arguments typed after the program.
        let mut scanner = String::new();
        take_picked(&draft.scanner_picked, &mut scanner);
        if !scanner.is_empty() {
            draft.scanner_command = uidm::scan::with_program(&scanner, &draft.scanner_command);
        }
        for (slot, file) in draft.sound_picked.iter().zip(draft.sound_files.iter_mut()) {
            take_picked(slot, file);
        }
        for c in &mut draft.categories {
            take_picked(&c.picked, &mut c.folder);
        }

        let mut open = true;
        let mut close = false;
        egui::Window::new(tr("Options"))
            .id(egui::Id::new("Options"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .min_width(720.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut draft.tab, OptionsTab::General, tr("General"));
                    ui.selectable_value(&mut draft.tab, OptionsTab::Categories, tr("Categories"));
                    ui.selectable_value(&mut draft.tab, OptionsTab::Capture, tr("Browser capture"));
                    ui.selectable_value(&mut draft.tab, OptionsTab::Video, tr("Videos and menus"));
                    ui.selectable_value(&mut draft.tab, OptionsTab::Notifications, tr("When done"));
                    ui.selectable_value(&mut draft.tab, OptionsTab::Connection, tr("Connection"));
                });
                ui.separator();
                match draft.tab {
                    OptionsTab::General => general_tab(ui, draft, &queues),
                    OptionsTab::Categories => categories_tab(ui, draft),
                    OptionsTab::Capture => capture_tab(ui, draft),
                    OptionsTab::Video => video_tab(ui, draft),
                    OptionsTab::Notifications => notifications_tab(ui, draft),
                    OptionsTab::Connection => connection_tab(ui, draft),
                }

                if let Some(err) = &draft.error {
                    ui.add_space(4.0);
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let save = ui.add_enabled(connected && !saving, egui::Button::new(tr("Save")));
                    if save.clicked() {
                        match draft.to_settings().and_then(|s| draft.apply_browsers().map(|()| s)) {
                            Ok(settings) => {
                                draft.error = None;
                                actions.push(Action::SaveSettings(settings));
                            }
                            Err(e) => draft.error = Some(e),
                        }
                    }
                    if ui.button(tr("Cancel")).clicked() {
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

fn theme_label(t: uidm::settings::Theme) -> &'static str {
    match t {
        uidm::settings::Theme::System => tr("Follow Windows"),
        uidm::settings::Theme::Light => tr("Light"),
        uidm::settings::Theme::Dark => tr("Dark"),
    }
}

fn general_tab(ui: &mut egui::Ui, draft: &mut OptionsDraft, queues: &[Queue]) {
    egui::Grid::new("options").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
        ui.label(tr("Download folder"));
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut draft.folder)
                    .hint_text(tr("Your Downloads folder"))
                    .desired_width(280.0),
            );
            browse_button(ui, tr("Download folder"), &draft.folder, &draft.picked);
            if ui.button(tr("Default")).on_hover_text(tr("Use your Downloads folder")).clicked() {
                draft.folder.clear();
            }
        });
        ui.end_row();

        ui.label(tr("Temporary folder"));
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut draft.temp_dir)
                        .hint_text(tr("None: partial files sit next to finished ones"))
                        .desired_width(280.0),
                );
                browse_button(ui, tr("Temporary folder"), &draft.temp_dir, &draft.temp_picked);
                if ui.button(tr("None")).clicked() {
                    draft.temp_dir.clear();
                }
            });
            ui.weak(tr("Unfinished downloads are written here and move to their folder once complete. Paused ones keep their place if you change it."));
        });
        ui.end_row();

        ui.label(tr("Connections per file"));
        let (lo, hi) = CONNECTIONS_RANGE;
        ui.add(egui::DragValue::new(&mut draft.connections).range(lo..=hi));
        ui.end_row();

        ui.label(tr("Downloads at once"));
        let (lo, hi) = MAX_CONCURRENT_RANGE;
        ui.add(egui::DragValue::new(&mut draft.max_concurrent).range(lo..=hi));
        ui.end_row();

        ui.label(tr("Speed limit"));
        ui.horizontal(|ui| {
            ui.checkbox(&mut draft.limit_enabled, tr("Limit to"));
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

        ui.label(tr("Download quota"));
        ui.horizontal(|ui| {
            ui.checkbox(&mut draft.quota_enabled, tr("No more than"));
            ui.add_enabled_ui(draft.quota_enabled, |ui| {
                ui.add(egui::DragValue::new(&mut draft.quota_mb).range(1..=10_000_000));
                ui.label(tr("MB every"));
                let (lo, hi) = QUOTA_HOURS_RANGE;
                ui.add(egui::DragValue::new(&mut draft.quota_hours).range(lo..=hi));
                ui.label(tr("hours"));
            });
        })
        .response
        .on_hover_text(tr("Downloads pause when it is reached, keeping their progress, and resume when the window allows"));
        ui.end_row();

        ui.label("");
        ui.add_enabled(draft.quota_enabled, egui::Checkbox::new(&mut draft.quota_warn, tr("Warn me when 90% of it is used")));
        ui.end_row();

        ui.label(tr("If the file exists"));
        egui::ComboBox::from_id_salt("file_exists")
            .selected_text(file_exists_label(draft.file_exists))
            .show_ui(ui, |ui| {
                for f in [FileExists::Rename, FileExists::Overwrite, FileExists::Ask] {
                    ui.selectable_value(&mut draft.file_exists, f, file_exists_label(f));
                }
            });
        ui.end_row();

        ui.label(tr("If the address is in the list"));
        egui::ComboBox::from_id_salt("if_duplicate")
            .selected_text(duplicate_label(draft.if_duplicate))
            .show_ui(ui, |ui| {
                for d in [Duplicate::Ask, Duplicate::NewName, Duplicate::Overwrite, Duplicate::Existing] {
                    ui.selectable_value(&mut draft.if_duplicate, d, duplicate_label(d));
                }
            });
        ui.end_row();

        ui.label(tr("New downloads"));
        ui.vertical(|ui| {
            ui.checkbox(&mut draft.add_only, tr("Only add them to the Download later queue, without starting them"));
            ui.add_enabled_ui(!draft.add_only, |ui| {
                ui.checkbox(&mut draft.ask_where_to_save, tr("Ask where to save each new download"));
                ui.add_enabled_ui(draft.ask_where_to_save, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(tr("Start these types without asking"));
                        ui.add(
                            egui::TextEdit::singleline(&mut draft.auto_start_types)
                                .hint_text(tr("none, or exe, msi"))
                                .desired_width(180.0),
                        );
                    });
                    ui.checkbox(&mut draft.start_while_asking, tr("Start downloading while the save-as dialog is open"))
                        .on_hover_text(tr("For servers that can resume. What it fetched is kept when you answer, in the folder you choose."));
                });
            });
        });
        ui.end_row();

        ui.label(tr("Download later puts files in"));
        let later = draft
            .later_queue
            .and_then(|id| queues.iter().find(|q| q.id == id))
            .map_or(tr("The queue chosen in the save-as dialog"), |q| q.name.as_str());
        egui::ComboBox::from_id_salt("later_queue_option").selected_text(later).show_ui(ui, |ui| {
            ui.selectable_value(&mut draft.later_queue, None, tr("The queue chosen in the save-as dialog"));
            for q in queues {
                ui.selectable_value(&mut draft.later_queue, Some(q.id), &q.name);
            }
        });
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut draft.resume_at_startup, tr("Resume unfinished downloads at startup"));
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut draft.resume_ignores_date, tr("Ignore file modification time changes when resuming"))
            .on_hover_text(tr("For servers that report a new date on every request. The size, and the ETag when the server sends one, must still match."));
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut draft.server_file_date, tr("Give finished files the date the server reports"))
            .on_hover_text(tr("Uses the server's Last-Modified date. Streams and form downloads keep the time they were written."));
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut draft.start_with_windows, tr("Start with Windows, in the tray"));
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut draft.tray_icon, tr("Show UIDM's icon in the notification area"))
            .on_hover_text(tr("Without it, closing the window quits UIDM; open it again from the Start menu"));
        ui.end_row();

        ui.label("");
        ui.add_enabled(draft.tray_icon, egui::Checkbox::new(&mut draft.close_to_tray, tr("Closing the window keeps UIDM in the tray")));
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut draft.drop_target, tr("Show the drop target, a small window on top that takes dragged links"))
            .on_hover_text(tr("Drag it to move it, double-click it to open UIDM, right-click it to close it"));
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut draft.tips_on_startup, tr("Show tips on startup"));
        ui.end_row();

        ui.label(tr("Download window"));
        ui.vertical(|ui| {
            ui.checkbox(&mut draft.progress_on_start, tr("Open it when a download starts"))
                .on_hover_text(tr("Double-clicking a download's name opens it at any time"));
            ui.checkbox(&mut draft.progress_speed_tab, tr("Show its Speed limit tab"));
            ui.checkbox(&mut draft.progress_done_tab, tr("Show its When done tab"));
        });
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut draft.check_updates, tr("Look for a newer UIDM on GitHub once a week"))
            .on_hover_text(tr("Only a notice; Update now in About downloads and runs the new Setup"));
        ui.end_row();

        ui.label(tr("Theme"));
        egui::ComboBox::from_id_salt("theme")
            .selected_text(theme_label(draft.theme))
            .show_ui(ui, |ui| {
                for t in uidm::settings::Theme::ALL {
                    ui.selectable_value(&mut draft.theme, t, theme_label(t));
                }
            });
        ui.end_row();

        ui.label(tr("Language"));
        let windows = i18n::resolve("");
        let follow = trf!("Follow Windows ({language})", language = windows.name);
        let selected = i18n::LANGUAGES
            .iter()
            .find(|l| l.code == draft.language)
            .map_or(follow.clone(), |l| l.name.to_string());
        egui::ComboBox::from_id_salt("language").selected_text(selected).show_ui(ui, |ui| {
            ui.selectable_value(&mut draft.language, String::new(), follow);
            for l in i18n::LANGUAGES {
                ui.selectable_value(&mut draft.language, l.code.to_string(), l.name);
            }
        });
        ui.end_row();

        ui.label(tr("Text size"));
        let (lo, hi) = TEXT_SIZE_RANGE;
        ui.add(egui::Slider::new(&mut draft.text_size, lo..=hi).step_by(10.0).suffix("%"))
            .on_hover_text(tr("Scales everything in the window"));
        ui.end_row();

        ui.label(tr("Removing downloads"));
        ui.vertical(|ui| {
            ui.checkbox(&mut draft.confirm_remove, tr("Ask first"));
            let text = if draft.confirm_remove {
                tr("Tick the box to delete files by default")
            } else {
                tr("Also send their files to the Recycle Bin")
            };
            ui.checkbox(&mut draft.remove_deletes_files, text);
        });
        ui.end_row();

        ui.label(tr("User-Agent")).on_hover_text(tr("For downloads not started from the browser"));
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut draft.manual_user_agent)
                        .hint_text(tr("UIDM's default"))
                        .desired_width(280.0),
                );
                ui.menu_button(tr("Presets"), |ui| {
                    for (name, agent) in USER_AGENT_PRESETS {
                        if ui.button(name).clicked() {
                            draft.manual_user_agent = agent.to_string();
                            ui.close_menu();
                        }
                    }
                    if ui.button(tr("Default")).clicked() {
                        draft.manual_user_agent.clear();
                        ui.close_menu();
                    }
                });
            });
            ui.weak(tr("Sent with links you add by hand, batches, imports and the site grabber's finds. Downloads from the browser send the browser's own."));
        });
        ui.end_row();

        ui.label("ffmpeg");
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut draft.ffmpeg_path)
                        .hint_text(tr("Empty looks for it on PATH"))
                        .desired_width(280.0),
                );
                browse_file_button(ui, tr("Where is ffmpeg.exe?"), "exe", &draft.ffmpeg_picked);
            });
            let found = Settings {
                ffmpeg_path: draft.ffmpeg_path.trim().to_string(),
                ..Settings::default()
            }
            .ffmpeg();
            match found {
                Some(path) => ui.weak(trf!("Using {path}", path = path.display())),
                None => ui.colored_label(ui.visuals().warn_fg_color, tr("Not found.")),
            };
            ui.weak(tr("Joins streamed videos into one .mp4. Without it they are saved as .ts, or as separate video and audio."));
        });
        ui.end_row();
    });
}

fn connection_tab(ui: &mut egui::Ui, draft: &mut OptionsDraft) {
    egui::Grid::new("connection").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label(tr("Proxy"));
        ui.vertical(|ui| {
            ui.radio_value(&mut draft.proxy_mode, ProxyMode::None, tr("No proxy"));
            ui.radio_value(&mut draft.proxy_mode, ProxyMode::System, tr("Use the Windows proxy setting"))
                .on_hover_text(tr("Including its configuration script or automatic detection"));
            ui.horizontal(|ui| {
                ui.radio_value(&mut draft.proxy_mode, ProxyMode::Script, tr("Use the configuration script at"));
                ui.add_enabled(
                    draft.proxy_mode == ProxyMode::Script,
                    egui::TextEdit::singleline(&mut draft.proxy_pac_url)
                        .hint_text("http://wpad.example.com/proxy.pac")
                        .desired_width(240.0),
                );
            });
            ui.horizontal(|ui| {
                ui.radio_value(&mut draft.proxy_mode, ProxyMode::Manual, tr("Use these proxies:"));
                if ui
                    .button(tr("Get System"))
                    .on_hover_text(tr("Fill in the proxies from the Windows proxy setting"))
                    .clicked()
                {
                    get_system_proxy(draft);
                }
            });
        });
        ui.end_row();
        let manual = draft.proxy_mode == ProxyMode::Manual;
        let [http_login, https_login, ftp_login, socks_login] = &mut draft.proxy_logins;
        for (label, field, hint, login) in [
            (tr("HTTP proxy"), &mut draft.proxy_http, "proxy.example.com:8080", http_login),
            (tr("HTTPS proxy"), &mut draft.proxy_https, tr("Same as HTTP when empty"), https_login),
            (tr("FTP proxy"), &mut draft.proxy_ftp, tr("For ftp:// downloads; logs in as user@host"), ftp_login),
            (tr("SOCKS proxy"), &mut draft.proxy_socks5, tr("For what has no proxy of its own"), socks_login),
        ] {
            ui.label(label);
            ui.add_enabled_ui(manual, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(field).hint_text(hint).desired_width(220.0));
                    ui.add(egui::TextEdit::singleline(&mut login.username).hint_text(tr("User name")).desired_width(110.0));
                    let hint = if login.changed { tr("Password") } else { tr("Password (unchanged)") };
                    let pw = ui.add(
                        egui::TextEdit::singleline(&mut login.password)
                            .password(true)
                            .hint_text(hint)
                            .desired_width(130.0),
                    );
                    if pw.changed() {
                        login.changed = true;
                    }
                });
            });
            ui.end_row();
        }
        ui.label(tr("SOCKS version"));
        ui.add_enabled_ui(manual, |ui| {
            ui.horizontal(|ui| {
                ui.radio_value(&mut draft.proxy_socks_version, SocksVersion::V4, "SOCKS4");
                ui.radio_value(&mut draft.proxy_socks_version, SocksVersion::V5, "SOCKS5");
                let v5 = draft.proxy_socks_version == SocksVersion::V5;
                ui.add_enabled(!v5, egui::Checkbox::new(&mut draft.proxy_remote_dns, tr("The proxy looks up host names (SOCKS4a)")))
                    .on_disabled_hover_text(tr("A SOCKS5 proxy always looks up host names"));
            });
        });
        ui.end_row();
        ui.label(tr("Not for"));
        ui.add_enabled(
            manual,
            egui::TextEdit::singleline(&mut draft.proxy_bypass)
                .hint_text("<local> *.corp.example 10.0.0.1")
                .desired_width(260.0),
        )
        .on_hover_text(tr("Hosts reached directly, separated by spaces or semicolons. * matches anything; <local> is any name without a dot."));
        ui.end_row();
        ui.label("");
        ui.vertical(|ui| {
            ui.checkbox(&mut draft.use_browser_proxy, tr("For downloads from the browser, use the browser's proxy"));
            ui.add_enabled_ui(draft.use_browser_proxy, |ui| {
                ui.radio_value(&mut draft.browser_proxy_after_errors, false, tr("Always"));
                ui.radio_value(
                    &mut draft.browser_proxy_after_errors,
                    true,
                    tr("Only after a download fails: it is tried once more through the browser's proxy"),
                );
            });
        });
        ui.end_row();
        ui.label(tr("FTP"));
        ui.checkbox(&mut draft.ftp_active, tr("Active mode: the server connects back to UIDM for data"))
            .on_hover_text(tr("Passive mode, the default, works through most firewalls and routers"));
        ui.end_row();
    });

    ui.add_space(8.0);
    ui.strong(tr("Connections per server"));
    ui.weak(tr("Some servers refuse clients that open many connections. These limits count every download to the server together."));
    let mut gone = None;
    egui::Grid::new("server_limits").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
        for (i, (site, max)) in draft.server_limits.iter_mut().enumerate() {
            ui.add_sized([220.0, 20.0], egui::TextEdit::singleline(site).hint_text("*.example.com"));
            ui.horizontal(|ui| {
                let (lo, hi) = SERVER_CONNECTIONS_RANGE;
                ui.add(egui::DragValue::new(max).range(lo..=hi));
                ui.label(tr("connections at most"));
            });
            if ui.small_button(tr("Remove")).clicked() {
                gone = Some(i);
            }
            ui.end_row();
        }
    });
    if let Some(i) = gone {
        draft.server_limits.remove(i);
    }
    if ui.button(tr("Add server")).clicked() {
        draft.server_limits.push((String::new(), 2));
    }

    ui.add_space(8.0);
    ui.strong(tr("Site logins"));
    ui.weak(tr("Sent only to a site that asks for a login, and only to that site."));
    let mut remove = None;
    egui::Grid::new("logins").num_columns(4).spacing([8.0, 6.0]).show(ui, |ui| {
        for (i, l) in draft.logins.iter_mut().enumerate() {
            ui.add_sized([180.0, 20.0], egui::TextEdit::singleline(&mut l.site).hint_text("example.com"))
                .on_hover_text(tr("A server, or one of its folders such as example.com/private/*. The longest match wins."));
            ui.add_sized([120.0, 20.0], egui::TextEdit::singleline(&mut l.username).hint_text(tr("User name")));
            let hint = if l.new { tr("Password") } else { tr("Password (unchanged)") };
            ui.add_sized([150.0, 20.0], egui::TextEdit::singleline(&mut l.password).password(true).hint_text(hint));
            if ui.small_button(tr("Remove")).clicked() {
                remove = Some(i);
            }
            ui.end_row();
        }
    });
    if let Some(i) = remove {
        draft.logins.remove(i);
    }
    if ui.button(tr("Add login")).clicked() {
        draft.logins.push(LoginDraft {
            site: String::new(),
            username: String::new(),
            password: String::new(),
            new: true,
        });
    }
}

/// Fill the manual proxies from the Windows proxy setting, as IDM's Get
/// System does. Windows' SOCKS proxies speak SOCKS4. Its configuration
/// script is not read: it picks a proxy per address.
fn get_system_proxy(draft: &mut OptionsDraft) {
    let Some(rules) = uidm::net::system_fixed_proxy() else {
        draft.error = Some(tr("Windows has no proxy server turned on.").to_string());
        return;
    };
    draft.proxy_mode = ProxyMode::Manual;
    draft.proxy_http = rules.http.clone().unwrap_or_default();
    // An HTTPS proxy the same as the HTTP one is left to fall back to it.
    draft.proxy_https = rules.https.clone().filter(|s| rules.http.as_ref() != Some(s)).unwrap_or_default();
    draft.proxy_ftp = rules.ftp.clone().unwrap_or_default();
    draft.proxy_socks5 = rules
        .socks
        .as_deref()
        .map(|s| s.split_once("://").map_or(s, |(_, a)| a).to_string())
        .unwrap_or_default();
    if rules.socks.is_some() {
        draft.proxy_socks_version = SocksVersion::V4;
    }
    draft.proxy_bypass = rules.bypass.join(" ");
    draft.error = None;
}

fn notifications_tab(ui: &mut egui::Ui, draft: &mut OptionsDraft) {

    egui::Grid::new("notifications").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
        ui.label(tr("When a download ends"));
        ui.vertical(|ui| {
            ui.radio_value(&mut draft.notify, Notify::Toast, tr("Show a Windows notification"));
            ui.radio_value(&mut draft.notify, Notify::Dialog, tr("Show a dialog in UIDM"));
            ui.radio_value(&mut draft.notify, Notify::Off, tr("Show nothing"));
        });
        ui.end_row();

        ui.label(tr("Sounds"));
        ui.vertical(|ui| {
            ui.weak(tr("A .wav file for each, or empty for Windows' own sound."));
            egui::Grid::new("sounds").num_columns(4).spacing([8.0, 4.0]).show(ui, |ui| {
                let events = [
                    (Sound::Done, tr("When a download finishes"), &mut draft.sound_on_done),
                    (Sound::Failed, tr("When a download fails"), &mut draft.sound_on_failed),
                    (Sound::QueueDone, tr("When a queue is done"), &mut draft.sound_on_queue_done),
                ];
                for (i, (sound, label, on)) in events.into_iter().enumerate() {
                    ui.checkbox(on, label);
                    ui.add_enabled(
                        *on,
                        egui::TextEdit::singleline(&mut draft.sound_files[i])
                            .hint_text(tr("Windows' sound"))
                            .desired_width(220.0),
                    );
                    ui.add_enabled_ui(*on, |ui| browse_file_button(ui, tr("Choose a sound"), "wav", &draft.sound_picked[i]));
                    if ui.add_enabled(*on, egui::Button::new(tr("Play"))).clicked() {
                        let file = draft.sound_files[i].trim();
                        uidm::notify::play(sound, (!file.is_empty()).then(|| Path::new(file)));
                    }
                    ui.end_row();
                }
            });
        });
        ui.end_row();

        ui.label(tr("Virus scan"));
        ui.vertical(|ui| {
            ui.checkbox(&mut draft.scan_after_download, tr("Scan each finished download"));
            ui.add_enabled_ui(draft.scan_after_download, |ui| {
                ui.radio_value(&mut draft.scanner, Scanner::Defender, "Microsoft Defender");
                ui.radio_value(&mut draft.scanner, Scanner::Custom, tr("This scanner:"));
                ui.add_enabled_ui(draft.scanner == Scanner::Custom, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut draft.scanner_command)
                                .hint_text(r#""C:\Program Files\ClamAV\clamscan.exe" {path}"#)
                                .desired_width(320.0),
                        );
                        browse_file_button(ui, tr("Choose the scanner program"), "exe", &draft.scanner_picked);
                    });
                });
                ui.weak(tr("{path} is replaced by the file. Exit code 0 means clean, 1 a threat."));
            });
        });
        ui.end_row();
    });
}

fn categories_tab(ui: &mut egui::Ui, draft: &mut OptionsDraft) {
    ui.weak(trf!("Each download goes to its category's folder, chosen by its file type and, if the category lists sites, by the site it comes from. Other files are \"{general}\" and go to the download folder.", general = GENERAL));
    ui.add_space(6.0);
    let mut remove = None;
    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
        egui::Grid::new("categories").num_columns(6).spacing([8.0, 8.0]).show(ui, |ui| {
            ui.strong(tr("Name"));
            ui.strong(tr("File types"));
            ui.strong(tr("Folder"));
            ui.strong(tr("Only from sites"));
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
                    let hint = trf!("Download folder\\{name}", name = c.name.trim());
                    ui.add_sized(
                        [180.0, 20.0],
                        egui::TextEdit::singleline(&mut c.folder).hint_text(hint),
                    );
                    browse_button(ui, tr("Category folder"), &c.folder, &c.picked);
                });
                ui.add_sized(
                    [150.0, 20.0],
                    egui::TextEdit::singleline(&mut c.sites).hint_text(tr("Every site")),
                )
                .on_hover_text(tr("Such as *.example.com, separated by commas"));
                ui.checkbox(&mut c.remember_folder, tr("Last folder"))
                    .on_hover_text(tr("Save new files to the folder last chosen for this category in the save-as dialog"));
                if ui.small_button(tr("Remove")).clicked() {
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
    ui.checkbox(
        &mut draft.general_remember_folder,
        trf!("Save new \"{general}\" files to the folder last chosen for them", general = GENERAL),
    );
    ui.add_space(4.0);
    if ui.button(tr("Add category")).clicked() {
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
    ui.weak(tr("These rules decide which browser downloads UIDM takes. The rest stay in the browser."));
    ui.add_space(6.0);
    egui::Grid::new("capture").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
        ui.label(tr("File types"));
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::multiline(&mut draft.capture_types)
                    .hint_text(tr("Empty takes every type"))
                    .desired_rows(3)
                    .desired_width(360.0),
            );
            if ui.button(tr("Default")).on_hover_text(tr("The types UIDM starts with")).clicked() {
                draft.capture_types = uidm::settings::default_capture_types().join(", ");
            }
        });
        ui.end_row();

        ui.label(tr("Minimum size"));
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut draft.capture_min_size_kb).range(0..=CAPTURE_MIN_SIZE_MAX_KB));
            ui.label(tr("KB (0 takes every size)"));
        });
        ui.end_row();

        ui.label(tr("Never from"));
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::multiline(&mut draft.capture_excluded_sites)
                    .hint_text(tr("One site per line, such as *.example.com"))
                    .desired_rows(4)
                    .desired_width(360.0),
            );
            if ui.button(tr("Default")).on_hover_text(tr("No site left out, as UIDM starts")).clicked() {
                draft.capture_excluded_sites.clear();
            }
        });
        ui.end_row();

        ui.label(tr("Never these addresses"));
        ui.vertical(|ui| {
            ui.add(
                egui::TextEdit::multiline(&mut draft.capture_excluded_urls)
                    .hint_text(tr("One address per line; * matches anything, as in https://example.com/files/*"))
                    .desired_rows(3)
                    .desired_width(360.0),
            );
            ui.checkbox(
                &mut draft.offer_exceptions,
                tr("When I cancel the same download twice, offer to stop taking it"),
            );
        });
        ui.end_row();

        ui.label(tr("Browsers"));
        ui.vertical(|ui| {
            ui.weak(tr("Capture downloads from these browsers. Each needs UIDM's extension; restart a browser after a change."));
            if draft.browsers.is_empty() {
                ui.label(tr("No browser was found."));
            }
            for b in &mut draft.browsers {
                ui.checkbox(&mut b.on, b.browser.name);
            }
            if !draft.chrome_readers.is_empty() {
                ui.weak(trf!("{browsers} use Google Chrome's setting.", browsers = draft.chrome_readers.join(", ")));
            }
        });
        ui.end_row();

        ui.label(tr("Special keys"));
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(tr("Hold"));
                key_boxes(ui, &mut draft.capture_skip_key, false);
                ui.label(tr("while clicking a link to keep its download in the browser"));
            });
            ui.horizontal(|ui| {
                ui.label(tr("Hold"));
                key_boxes(ui, &mut draft.capture_force_key, true);
                ui.label(tr("while clicking a link to send it to UIDM, whatever its type"));
            });
            ui.checkbox(
                &mut draft.capture_keys_left_click,
                tr("Only when clicking with the left mouse button, not a middle click or Enter"),
            );
            ui.checkbox(
                &mut draft.capture_force_skips_pages,
                tr("The force keys leave links to web pages, pictures and scripts alone"),
            );
        });
        ui.end_row();

        ui.label(tr("Selected links"));
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(tr("Show a Download with UIDM button by selected text with links"));
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
                        .hint_text(tr("One site per line, such as *.example.com"))
                        .desired_rows(2)
                        .desired_width(360.0),
                );
            }
        });
        ui.end_row();


        ui.label(tr("Clipboard"));
        ui.vertical(|ui| {
            ui.checkbox(
                &mut draft.clipboard_monitor,
                tr("Watch for links I copy whose file type is listed above"),
            );
            ui.add_enabled_ui(draft.clipboard_monitor, |ui| {
                ui.radio_value(&mut draft.clipboard_start, false, tr("Offer to download each one"));
                ui.radio_value(&mut draft.clipboard_start, true, tr("Start downloading it at once"));
            });
        });
        ui.end_row();
    });
}

/// The video button's options and the browser's context menu items.
fn video_tab(ui: &mut egui::Ui, draft: &mut OptionsDraft) {
    egui::Grid::new("video").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
        ui.label(tr("Videos"));
        ui.checkbox(&mut draft.video_button, tr("Show a download button over videos playing in the browser"));
        ui.end_row();

        ui.label(tr("No video button on"));
        ui.add_enabled(
            draft.video_button,
            egui::TextEdit::multiline(&mut draft.video_button_excluded_sites)
                .hint_text(tr("One site per line, such as *.example.com"))
                .desired_rows(2)
                .desired_width(360.0),
        );
        ui.end_row();

        ui.label(tr("Video types"));
        ui.add_enabled_ui(draft.video_button, |ui| {
            ui.vertical(|ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.set_max_width(420.0);
                    for (t, on) in &mut draft.video_types {
                        ui.checkbox(on, t.to_ascii_uppercase());
                    }
                });
                ui.weak(tr("M3U8 and MPD are HLS and DASH streams."));
                ui.horizontal(|ui| {
                    if ui.button(tr("Check all")).clicked() {
                        draft.video_types.iter_mut().for_each(|(_, on)| *on = true);
                    }
                    if ui.button(tr("Clear all")).clicked() {
                        draft.video_types.iter_mut().for_each(|(_, on)| *on = false);
                    }
                    ui.add(egui::TextEdit::singleline(&mut draft.video_type_new).hint_text("ts").desired_width(70.0));
                    if ui.button(tr("Add")).clicked() {
                        for t in parse_extensions(&draft.video_type_new) {
                            match draft.video_types.iter_mut().find(|(k, _)| *k == t) {
                                Some((_, on)) => *on = true,
                                None => draft.video_types.push((t, true)),
                            }
                        }
                        draft.video_type_new.clear();
                    }
                });
            });
        });
        ui.end_row();

        ui.label(tr("Minimum size"));
        ui.add_enabled_ui(draft.video_button, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut draft.video_min_size_kb).range(0..=CAPTURE_MIN_SIZE_MAX_KB));
                ui.label(tr("KB: smaller video and audio files are left out of the list"));
            });
        });
        ui.end_row();

        ui.label(tr("Buttons on pages"));
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.radio_value(&mut draft.video_view, PanelView::Full, tr("Full, with their text"));
                ui.radio_value(&mut draft.video_view, PanelView::Mini, tr("Mini, an arrow"));
            })
            .response
            .on_hover_text(tr("For the video button and the button by selected links"));
            ui.add_enabled(
                draft.video_button,
                egui::Checkbox::new(
                    &mut draft.video_protected_note,
                    tr("Say when a video is protected and cannot be downloaded"),
                ),
            )
            .on_hover_text(tr("Off, the video button does not show on protected videos"));
        });
        ui.end_row();

        ui.label(tr("Context menu"));
        ui.vertical(|ui| {
            ui.weak(tr("The items UIDM adds to the browser's menu for links, pages and videos"));
            for (_, label, on) in &mut draft.menu_items {
                ui.checkbox(on, tr(label));
            }
        });
        ui.end_row();
    });
}

fn selection_label(b: SelectionButton) -> &'static str {
    match b {
        SelectionButton::Off => tr("Nowhere"),
        SelectionButton::Everywhere => tr("On every site"),
        SelectionButton::Sites => tr("On these sites only"),
    }
}

/// A box for each key of a combination; none ticked turns it off. Only
/// forcing offers Insert.
fn key_boxes(ui: &mut egui::Ui, key: &mut CaptureKey, insert: bool) {
    ui.checkbox(&mut key.ctrl, "Ctrl");
    ui.checkbox(&mut key.alt, "Alt");
    ui.checkbox(&mut key.shift, "Shift");
    if insert {
        ui.checkbox(&mut key.insert, "Insert");
    }
}

fn duplicate_label(d: Duplicate) -> &'static str {
    match d {
        Duplicate::Ask => tr("Ask me"),
        Duplicate::NewName => tr("Add it with a new name"),
        Duplicate::Overwrite => tr("Replace the old download"),
        Duplicate::Existing => tr("Use the old download"),
    }
}

fn file_exists_label(f: FileExists) -> &'static str {
    match f {
        FileExists::Rename => tr("Save with a new name"),
        FileExists::Overwrite => tr("Overwrite it"),
        FileExists::Ask => tr("Ask me"),
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
            // A site that wants a login gets the login prompt.
            Outgoing::Failed { id, error, .. } if uidm::engine::login_wanted(error).is_some() => {
                if !s.login_wanted.contains(id) {
                    s.login_wanted.push_back(*id);
                }
                true
            }
            // Options, or the download itself, can ask for the dialog.
            Outgoing::Done { id, .. } if s.dialog_mode() || s.table.get(*id).and_then(|e| e.on_done).is_some_and(|o| o.dialog) => {
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

/// Open a file with its program, as a double-click does.
fn open_file(path: &str) {
    let _ = uidm::platform::open(path.as_ref());
}

/// An http, https or ftp address with no spaces.
fn is_download_url(url: &str) -> bool {
    uidm::batch::is_download_url(url) && !url.contains(char::is_whitespace)
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
        let title = if failed { tr("Download failed") } else { tr("Download complete") };
        egui::Window::new(title)
            .id(egui::Id::new("completed"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let path = e.path.as_deref().map(Path::new);
                egui::Grid::new("completed_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label(tr("File"));
                    ui.strong(display_name(&e));
                    ui.end_row();
                    ui.label(tr("Address"));
                    ui.add(egui::Label::new(&e.url).truncate()).on_hover_text(&e.url);
                    ui.end_row();
                    if let Some(t) = e.total {
                        ui.label(tr("Size"));
                        ui.label(human_bytes(t));
                        ui.end_row();
                    }
                    if let Some(folder) = path.and_then(|p| p.parent()) {
                        ui.label(tr("Saved in"));
                        ui.label(folder.display().to_string());
                        ui.end_row();
                    }
                    if failed {
                        ui.label(tr("Error"));
                        ui.colored_label(ui.visuals().error_fg_color, e.error.clone().unwrap_or_default());
                        ui.end_row();
                    }
                    if let Some(scan) = &e.scan {
                        ui.label(tr("Virus scan"));
                        scan_label(ui, scan);
                        ui.end_row();
                    }
                });
                if waiting > 1 {
                    ui.weak(trf!("{count} more after this one.", count = waiting - 1));
                }
                ui.checkbox(&mut self.hide_completed, tr("Don't show this dialog again"))
                    .on_hover_text(tr("Show a Windows notification instead; Options > When done can bring the dialog back"));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if failed {
                        if ui.add_enabled(connected, egui::Button::new(tr("Retry"))).clicked() {
                            actions.push(Action::Retry(e.id));
                            actions.push(Action::DismissCompleted);
                        }
                    } else if let Some(p) = &e.path {
                        if ui.button(tr("Open")).clicked() {
                            actions.push(Action::Open(e.id));
                            actions.push(Action::DismissCompleted);
                        }
                        if ui.button(tr("Open with...")).on_hover_text(tr("Choose the program to open it with")).clicked() {
                            actions.push(Action::OpenWith(p.clone()));
                            actions.push(Action::DismissCompleted);
                        }
                        if ui.button(tr("Show in folder")).clicked() {
                            actions.push(Action::ShowInFolder(p.clone()));
                            actions.push(Action::DismissCompleted);
                        }
                    }
                    if ui.button(tr("Close")).clicked() {
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
        egui::Window::new(tr("Threat found"))
            .id(egui::Id::new("threat"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(trf!("The virus scan found a threat in {name}.", name = name));
                if !detail.is_empty() {
                    ui.weak(&detail);
                }
                ui.label(tr("Opening it could harm your computer. Open it anyway?"));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(tr("Cancel")).clicked() {
                        cancel = true;
                    }
                    let open = egui::Button::new(egui::RichText::new(tr("Open anyway")).color(ui.visuals().error_fg_color));
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
            ui.weak(tr("Scanning..."));
        }
        Scan::Clean => {
            ui.label(tr("No threat found"));
        }
        Scan::Threat { detail } => {
            ui.colored_label(v.error_fg_color, tr("Threat found")).on_hover_text(detail);
        }
        Scan::Failed { detail } => {
            ui.colored_label(v.warn_fg_color, tr("Scan failed")).on_hover_text(detail);
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

        // The tray icon follows the settings; hiding it brings the window
        // up, since the icon was the way back to a hidden window.
        if let Some(s) = &settings {
            if self.tray_shown != Some(s.tray_icon) {
                tray::set_visible(s.tray_icon);
                if !s.tray_icon {
                    win::show();
                }
                self.tray_shown = Some(s.tray_icon);
            }
        }
        // Close keeps UIDM in the tray unless Options says to quit.
        if ctx.input(|i| i.viewport().close_requested())
            && tray::shown()
            && settings.as_ref().is_none_or(|s| s.close_to_tray)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            win::hide();
        }
        let tooltip = self.shared.lock().unwrap().tooltip();
        if tooltip != self.tooltip {
            tray::set_tooltip(tooltip.clone());
            self.tooltip = tooltip;
        }
        if let Some(s) = &settings {
            if self.appearance != Some((s.theme, s.text_size)) {
                theme::set_appearance(ctx, s.theme, s.text_size);
                self.appearance = Some((s.theme, s.text_size));
            }
            if self.language.as_deref() != Some(s.language.as_str()) {
                i18n::set_language(&s.language);
                tray::relabel();
                self.language = Some(s.language.clone());
            }
            tray::set_speed_limit_checked(s.speed_limit_enabled);
            self.sync_drop_target(s, &mut actions);
        }

        egui::TopBottomPanel::top("add").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading("UIDM");
                ui.add_space(8.0);
                let input = ui.add(
                    egui::TextEdit::singleline(&mut self.url_input)
                        .hint_text(tr("Paste a download URL"))
                        .desired_width((ui.available_width() - 360.0).max(120.0)),
                );
                let entered = input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let url = self.url_input.trim().to_string();
                let can_add = connected && is_download_url(&url);
                // The login fields, when filled in, go with it.
                let login = self.add_login.as_ref().filter(|(u, _)| !u.trim().is_empty()).map(|(u, p)| DownloadLogin {
                    username: u.trim().to_string(),
                    password: Some(p.clone()),
                });
                let clicked = ui.add_enabled(can_add, egui::Button::new(tr("Download"))).clicked();
                if can_add && (clicked || entered) {
                    actions.push(Action::AddDownload(Download {
                        url: url.clone(),
                        login: login.clone(),
                        ..Download::default()
                    }));
                    self.url_input.clear();
                }
                let later = ui
                    .add_enabled(can_add, egui::Button::new(tr("Later")))
                    .on_hover_text(tr("Add it to the main queue without starting it"));
                if can_add && later.clicked() {
                    actions.push(Action::AddDownload(Download {
                        url,
                        login,
                        start: Some(false),
                        ..Download::default()
                    }));
                    self.url_input.clear();
                }
                let mut with_login = self.add_login.is_some();
                if ui
                    .toggle_value(&mut with_login, tr("Login"))
                    .on_hover_text(tr("Send a user name and password with the address"))
                    .changed()
                {
                    self.add_login = with_login.then(Default::default);
                }
                ui.add_enabled_ui(connected, |ui| {
                    ui.menu_button(tr("More"), |ui| {
                        if ui.button(tr("Add many...")).on_hover_text(tr("From a URL pattern or a list")).clicked() {
                            actions.push(Action::OpenBatch);
                            ui.close_menu();
                        }
                        if ui
                            .button(tr("Add batch download from clipboard"))
                            .on_hover_text(tr("Every link in the copied text, to tick and add"))
                            .clicked()
                        {
                            actions.push(Action::BatchFromClipboard);
                            ui.close_menu();
                        }
                        if ui.button(tr("Site grabber...")).on_hover_text(tr("Find the files a site links to")).clicked() {
                            actions.push(Action::OpenGrabber);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button(tr("Import list...")).clicked() {
                            actions.push(Action::OpenImport);
                            ui.close_menu();
                        }
                        if ui.button(tr("Export list...")).clicked() {
                            actions.push(Action::OpenExport);
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button(tr("Find in the list...")).on_hover_text(tr("Ctrl+F")).clicked() {
                            self.find.get_or_insert_with(find::FindBar::new);
                            ui.close_menu();
                        }
                        if ui.button(tr("Manage queues...")).clicked() {
                            actions.push(Action::OpenQueues(None));
                            ui.close_menu();
                        }
                        if ui.button(tr("Columns...")).on_hover_text(tr("Show, hide and order the list's columns")).clicked() {
                            self.columns_dialog = true;
                            ui.close_menu();
                        }
                        if let Some(s) = &settings {
                            let mut hide = !s.categories_panel;
                            if ui.checkbox(&mut hide, tr("Hide categories")).changed() {
                                actions.push(Action::SaveSettings(Settings {
                                    categories_panel: !hide,
                                    ..s.clone()
                                }));
                                ui.close_menu();
                            }
                            let mut no_tray = !s.tray_icon;
                            if ui
                                .checkbox(&mut no_tray, tr("Hide the tray icon"))
                                .on_hover_text(tr("Closing the window then quits UIDM; open it again from the Start menu"))
                                .changed()
                            {
                                actions.push(Action::SaveSettings(Settings {
                                    tray_icon: !no_tray,
                                    ..s.clone()
                                }));
                                ui.close_menu();
                            }
                        }
                    });
                });
                if help::help_menu(ui) {
                    self.tip = Some(help::TipWindow {
                        index: settings.as_ref().map_or(0, |s| s.next_tip),
                        on_startup: settings.as_ref().is_none_or(|s| s.tips_on_startup),
                    });
                }
                if ui.button(tr("About")).clicked() {
                    actions.push(Action::OpenAbout);
                }
                let can_open = connected && settings.is_some();
                if ui.add_enabled(can_open, egui::Button::new(tr("Options"))).clicked() {
                    actions.push(Action::OpenOptions);
                }
            });
            if let Some((user, password)) = self.add_login.as_mut() {
                ui.horizontal(|ui| {
                    ui.label(tr("User name"));
                    ui.add(egui::TextEdit::singleline(user).desired_width(160.0));
                    ui.label(tr("Password"));
                    ui.add(egui::TextEdit::singleline(password).password(true).desired_width(160.0));
                    ui.weak(tr("Used for the addresses you add here until you close it"));
                });
            }
            ui.add_space(6.0);
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                match &link {
                    Link::Connecting => {
                        ui.spinner();
                        ui.label(tr("Connecting to the UIDM daemon..."));
                    }
                    Link::Connected => {
                        ui.label(tr("Connected"));
                    }
                    Link::Lost(why) => {
                        ui.colored_label(ui.visuals().error_fg_color, trf!("Disconnected: {reason}", reason = why));
                        if ui.button(tr("Reconnect")).clicked() {
                            actions.push(Action::Reconnect);
                        }
                    }
                }
                let refreshing = self.shared.lock().unwrap().refresh_waiting;
                if let Some(e) = refreshing.and_then(|id| rows.iter().find(|e| e.id == id)) {
                    ui.separator();
                    ui.label(trf!("Your next browser download gives {name} a new link", name = display_name(e)));
                    if ui.small_button(tr("Stop waiting")).clicked() {
                        actions.push(Action::CancelRefresh);
                    }
                }
                let active = rows.iter().filter(|e| !e.status.is_terminal()).count();
                ui.separator();
                ui.label(trf!("{active} active, {total} total", active = active, total = rows.len()));
                if let Some(err) = &last_error {
                    ui.separator();
                    ui.colored_label(ui.visuals().warn_fg_color, err);
                }
                let newer = self.shared.lock().unwrap().newer_release.clone();
                if let Some(r) = newer {
                    ui.separator();
                    ui.hyperlink_to(trf!("UIDM {version} is available", version = r.version), &r.url);
                    // About checks again and offers Update now.
                    if r.setup.is_some() && ui.add_enabled(connected, egui::Button::new(tr("Update now")).small()).clicked() {
                        actions.push(Action::CheckUpdate);
                    }
                }
                let quota = self.shared.lock().unwrap().quota.clone();
                if let Some(q) = quota {
                    ui.separator();
                    ui.colored_label(ui.visuals().warn_fg_color, q);
                }
                let notice = self.shared.lock().unwrap().notice.clone();
                if let Some(n) = notice {
                    ui.separator();
                    ui.label(n);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let any_done = rows.iter().any(|e| e.status == Status::Done);
                    if ui
                        .add_enabled(connected && any_done, egui::Button::new(tr("Clear finished")))
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
                            .add_enabled(connected, egui::Button::new(trf!("Remove ticked ({count})", count = ticked.len())))
                            .on_hover_text(tr("Running downloads are left alone"))
                            .clicked()
                    {
                        actions.push(Action::Remove(ticked));
                    }
                    if let Some(s) = &settings {
                        ui.separator();
                        let toggle = if s.speed_limit_enabled { tr("Turn off") } else { tr("Limit speed") };
                        let hover = trf!("Limit: {speed}/s", speed = human_bytes(s.speed_limit_kbps * 1024));
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
                                trf!("Speed limit {speed}/s", speed = human_bytes(s.speed_limit_kbps * 1024)),
                            );
                        } else {
                            ui.weak(tr("No speed limit"));
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
                .map_or(GENERAL, |s| s.listed_under(e.category.as_deref(), &display_name(e), &e.url))
                .to_string()
        };
        let queue_list = self.shared.lock().unwrap().queues.clone();
        // With the panel hidden, nothing is left to pick another view with.
        if settings.as_ref().is_some_and(|s| !s.categories_panel) {
            self.filter = Filter::All;
        }
        if let Some(s) = settings.as_ref().filter(|s| s.categories_panel) {
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
                    let all = trf!("All downloads ({count})", count = rows.len());
                    ui.selectable_value(&mut self.filter, Filter::All, all);
                    ui.add_space(4.0);
                    for name in names {
                        let count = rows.iter().filter(|e| category(e) == name).count();
                        let label = format!("{name} ({count})");
                        ui.selectable_value(&mut self.filter, Filter::Category(name), label);
                    }
                    ui.add_space(10.0);
                    ui.weak(tr("Queues"));
                    for q in &queue_list {
                        let label = if q.running {
                            trf!("{name} ({count}, started)", name = q.name, count = q.order.len())
                        } else {
                            format!("{} ({})", q.name, q.order.len())
                        };
                        ui.selectable_value(&mut self.filter, Filter::Queue(q.id), label);
                    }
                    ui.add_space(4.0);
                    if ui.add_enabled(connected, egui::Button::new(tr("Manage queues..."))).clicked() {
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
                        ui.weak(tr("This queue is empty. Use Later, or a download's Queue menu, to add one."));
                    });
                    return;
                }
            }
            if shown.is_empty() && finding {
                ui.centered_and_justified(|ui| {
                    ui.weak(tr("No downloads here match. Press Escape to see them all."));
                });
                return;
            }
            if shown.is_empty() && !rows.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.weak(tr("No downloads in this category."));
                });
                return;
            }
            if rows.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.weak(tr("No downloads yet. Paste a URL above, or download something in your browser."));
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
                            ui.horizontal(|ui| {
                                let title = egui::RichText::new(format!("{}{}", column.title(), self.layout.arrow(column))).strong();
                                let header = ui.add(egui::Button::new(title).frame(false));
                                let header = if current_queue.is_some() {
                                    header.on_hover_text(tr("A queue's downloads stay in its run order"))
                                } else {
                                    header.on_hover_text(tr("Sort by this column; click again to reverse, a third time for the order added"))
                                };
                                if header.clicked() && current_queue.is_none() {
                                    self.layout.click(column);
                                }
                                header.context_menu(|ui| {
                                    if ui.button(tr("Columns...")).clicked() {
                                        self.columns_dialog = true;
                                        ui.close_menu();
                                    }
                                });
                                // The edge that sets the column's width.
                                let (rect, edge) = ui.allocate_exact_size(egui::vec2(6.0, 18.0), egui::Sense::drag());
                                let edge = edge.on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
                                let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
                                ui.painter().vline(rect.center().x, rect.y_range(), stroke);
                                if edge.dragged() {
                                    let now = self.layout.width(column).unwrap_or(ui.min_rect().width());
                                    self.layout.set_width(column, now + edge.drag_delta().x);
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
                                layout: &self.layout,
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
        self.login_window(ctx, &rows, connected, &mut actions);
        self.completed_window(ctx, connected, &mut actions);
        self.threat_window(ctx, &mut actions);
        self.remove_window(ctx, &rows, connected, &mut actions);
        self.limit_window(ctx, &rows, connected, &mut actions);
        self.exception_window(ctx, connected, &mut actions);
        self.zip_window(ctx, &rows);
        self.about_window(ctx, connected, &mut actions);
        // The tip of the day, at startup when the settings ask for it.
        if let (Some(s), false) = (&settings, self.tip_considered) {
            self.tip_considered = true;
            if s.tips_on_startup {
                self.tip = Some(help::TipWindow {
                    index: s.next_tip,
                    on_startup: true,
                });
            }
        }
        if let Some(t) = self.tip.as_mut() {
            if let Some((next_tip, tips_on_startup)) = help::tip_window(ctx, t) {
                self.tip = None;
                if let Some(s) = settings.as_ref().filter(|_| connected) {
                    actions.push(Action::SaveSettings(Settings {
                        next_tip,
                        tips_on_startup,
                        ..s.clone()
                    }));
                }
            }
        }
        self.convert_window(ctx, &rows, connected, &mut actions);
        if let Some(d) = self.shared.lock().unwrap().picked_links.take() {
            self.links = Some(d);
        }
        if let Some(d) = self.links.as_mut() {
            let settings = settings.clone().unwrap_or_default();
            if !links::dialog(ctx, d, &queue_list, &settings, connected, &mut actions) {
                self.links = None;
            }
        }
        let segments = self.shared.lock().unwrap().segments.clone();
        self.connection_dialogs.retain(|id| {
            let entry = rows.iter().find(|e| e.id == *id);
            connections::dialog(ctx, *id, entry, segments.get(id).map(Vec::as_slice))
        });
        let started = std::mem::take(&mut self.shared.lock().unwrap().started);
        if settings.as_ref().is_some_and(|s| s.progress_on_start) {
            actions.extend(started.into_iter().map(Action::OpenProgress));
        }
        let tabs = progress::Tabs {
            speed_limit: settings.as_ref().is_none_or(|s| s.progress_speed_tab),
            on_done: settings.as_ref().is_none_or(|s| s.progress_done_tab),
        };
        self.progress_windows.retain_mut(|w| {
            let entry = rows.iter().find(|e| e.id == w.id);
            let segments = segments.get(&w.id).map(Vec::as_slice);
            progress::window(ctx, w, entry, segments, speeds.get(&w.id).copied(), &tabs, connected, &mut actions)
        });
        self.refresh_window(ctx, &rows, connected, &mut actions);
        self.tool_windows(ctx, &rows, connected, &mut actions);
        if let Some(d) = self.queues_dialog.as_mut() {
            d.schedule_note = !settings.as_ref().is_some_and(|s| s.schedule_note_hidden);
            if !queues::dialog(ctx, d, &queue_list, connected, &mut actions) {
                self.queues_dialog = None;
            }
        }
        let countdown = self.shared.lock().unwrap().countdown.clone();
        if let Some((action, deadline, queue, file)) = countdown {
            if Instant::now() < deadline {
                queues::countdown(ctx, action, deadline, &queue, file.as_deref(), connected, &mut actions);
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
        // Saved once a drag on a header's edge is over, not on every frame
        // of it.
        if self.layout != layout_before {
            self.layout_unsaved = true;
        }
        if self.layout_unsaved && !ctx.input(|i| i.pointer.any_down()) {
            self.layout.save();
            self.layout_unsaved = false;
        }

        for action in actions {
            self.perform(action);
        }
    }
}

impl App {
    /// The Convert dialog: an MP4, or the sound alone as an MP3 or M4A.
    fn convert_window(&mut self, ctx: &egui::Context, rows: &[DownloadEntry], connected: bool, actions: &mut Vec<Action>) {
        use uidm::media::convert::Target;
        let Some((id, to, delete)) = self.convert.as_mut() else {
            return;
        };
        let Some(e) = rows.iter().find(|e| e.id == *id && e.status == Status::Done) else {
            self.convert = None;
            return;
        };
        let name = display_name(e);
        let is_mp4 = name.to_ascii_lowercase().ends_with(".mp4");
        let mut close = false;
        egui::Window::new(tr("Convert"))
            .id(egui::Id::new("convert"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(trf!("Make a new file from {name} with ffmpeg.", name = name));
                if ui.add_enabled(!is_mp4, egui::RadioButton::new(*to == Target::Mp4, tr("An MP4 video"))).clicked() {
                    *to = Target::Mp4;
                }
                if ui.radio(*to == Target::Mp3, tr("Its sound as an MP3")).clicked() {
                    *to = Target::Mp3;
                }
                if ui.radio(*to == Target::M4a, tr("Its sound as an M4A")).clicked() {
                    *to = Target::M4a;
                }
                ui.checkbox(delete, tr("Then send the original to the Recycle Bin"));
                ui.weak(tr("It shows in the list with its progress. Streams are copied when they fit, else encoded."));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(connected, egui::Button::new(tr("Convert"))).clicked() {
                        actions.push(Action::Convert(*id, *to, *delete));
                        close = true;
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        close = true;
                    }
                });
            });
        if close {
            self.convert = None;
        }
    }

    /// Open or close the drop target to match the setting. Closing it with
    /// a right-click turns the setting off.
    fn sync_drop_target(&mut self, s: &Settings, actions: &mut Vec<Action>) {
        let open = droptarget::is_open();
        tray::set_drop_target_checked(s.drop_target && open);
        match (s.drop_target, open, self.drop_shown) {
            (true, false, false) => {
                let writer = Arc::clone(&self.writer);
                let ctx = self.ctx.clone();
                droptarget::open(
                    Arc::new(move |url: String| {
                        // Each dropped link goes through the save-as dialog.
                        send(
                            &writer,
                            &Incoming::Add(Download {
                                url,
                                ask: Some(true),
                                ..Download::default()
                            }),
                        );
                        win::show();
                        ctx.request_repaint();
                    }),
                    Arc::new(win::show),
                );
                self.drop_shown = true;
            }
            (true, false, true) => {
                self.drop_shown = false;
                actions.push(Action::QuickSettings(Settings {
                    drop_target: false,
                    ..s.clone()
                }));
            }
            (false, true, _) => {
                droptarget::close();
                self.drop_shown = false;
            }
            (false, false, _) => self.drop_shown = false,
            (true, true, _) => {}
        }
    }

    /// The About box: the version, the project's page, and a check for a
    /// newer release.
    fn about_window(&mut self, ctx: &egui::Context, connected: bool, actions: &mut Vec<Action>) {
        let Some(checking) = self.about else {
            return;
        };
        let answer = self.shared.lock().unwrap().update.clone();
        let mut open = true;
        egui::Window::new(tr("About UIDM"))
            .id(egui::Id::new("about"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.heading("UIDM");
                ui.label(trf!("Version {version}", version = env!("CARGO_PKG_VERSION")));
                ui.hyperlink_to("github.com/tajbinkhan/uidm", "https://github.com/tajbinkhan/uidm");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let busy = checking && answer.is_none();
                    if ui.add_enabled(connected && !busy, egui::Button::new(tr("Check for updates"))).clicked() {
                        actions.push(Action::CheckUpdate);
                    }
                    if busy {
                        ui.spinner();
                    }
                });
                if checking {
                    match &answer {
                        Some(Ok(Some(r))) => {
                            ui.label(trf!("UIDM {version} is available.", version = r.version));
                            ui.hyperlink_to(tr("Open the release page"), &r.url);
                            if r.setup.is_some() {
                                let installing = self.shared.lock().unwrap().installing.clone();
                                ui.horizontal(|ui| {
                                    let busy = matches!(installing, Some(None));
                                    if ui
                                        .add_enabled(connected && !busy, egui::Button::new(tr("Update now")))
                                        .on_hover_text(tr("Download the new Setup, check it, and run it. UIDM closes for it; downloads stop and can be resumed."))
                                        .clicked()
                                    {
                                        actions.push(Action::InstallUpdate);
                                    }
                                    if busy {
                                        ui.spinner();
                                        ui.label(tr("Downloading the update..."));
                                    }
                                });
                                if let Some(Some(e)) = installing {
                                    ui.colored_label(ui.visuals().error_fg_color, e);
                                }
                            }
                        }
                        Some(Ok(None)) => {
                            ui.label(tr("This is the newest version."));
                        }
                        Some(Err(e)) => {
                            ui.colored_label(ui.visuals().warn_fg_color, e);
                        }
                        None => {}
                    }
                }
            });
        if !open {
            self.about = None;
        }
    }

    /// The files inside a zip download, once listed.
    fn zip_window(&mut self, ctx: &egui::Context, rows: &[DownloadEntry]) {
        if let Some((id, result)) = self.shared.lock().unwrap().zip_listed.take() {
            if let Some(p) = self.zip_preview.as_mut().filter(|p| p.id == id) {
                p.result = Some(result);
            }
        }
        let Some(p) = &self.zip_preview else {
            return;
        };
        let Some(result) = &p.result else {
            return;
        };
        let name = rows.iter().find(|e| e.id == p.id).map(display_name).unwrap_or_default();
        let mut open = true;
        egui::Window::new(trf!("Inside {name}", name = name))
            .id(egui::Id::new("zip_preview"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| match result {
                Err(e) => {
                    ui.colored_label(ui.visuals().warn_fg_color, e);
                }
                Ok(entries) => {
                    let total: u64 = entries.iter().map(|e| e.size).sum();
                    ui.label(trf!("{count} files, {size} unpacked", count = entries.len(), size = human_bytes(total)));
                    if entries.len() == uidm::zip::MAX_ENTRIES {
                        ui.weak(trf!("Showing the first {count}.", count = uidm::zip::MAX_ENTRIES));
                    }
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        egui::Grid::new("zip_entries").striped(true).num_columns(2).spacing([16.0, 2.0]).show(ui, |ui| {
                            for e in entries {
                                ui.add_sized([360.0, 16.0], egui::Label::new(&e.name).truncate());
                                ui.weak(human_bytes(e.size));
                                ui.end_row();
                            }
                        });
                    });
                }
            });
        if !open {
            self.zip_preview = None;
        }
    }

    /// After the save-as dialog of one captured address is canceled twice:
    /// offer to keep its site, or the address, in the browser from now on.
    fn exception_window(&mut self, ctx: &egui::Context, connected: bool, actions: &mut Vec<Action>) {
        let Some((url, dont_show)) = self.exception_offer.as_mut() else {
            return;
        };
        let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
        let mut chosen = None;
        egui::Window::new(tr("Keep it in the browser?"))
            .id(egui::Id::new("exception_offer"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(tr("You canceled this download twice. UIDM can leave it to the browser next time."));
                ui.add(egui::Label::new(egui::RichText::new(url.as_str()).weak()).truncate());
                ui.add_space(6.0);
                ui.checkbox(dont_show, tr("Don't show this again"));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(connected, |ui| {
                        if !host.is_empty() && ui.button(trf!("Don't capture {host}", host = host)).clicked() {
                            chosen = Some(Some(true));
                        }
                        if ui.button(tr("Don't capture this address")).clicked() {
                            chosen = Some(Some(false));
                        }
                    });
                    if ui.button(tr("Keep capturing")).clicked() {
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
        egui::Window::new(tr("Speed limit"))
            .id(egui::Id::new("download_limit"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(trf!("A speed limit for {name} alone.", name = display_name(e)));
                ui.weak(tr("The total limit still applies; the lower one wins. The limit stays when it stops and resumes."));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut d.enabled, tr("Limit to"));
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
                    if ui.add_enabled(connected, egui::Button::new(tr("Save"))).clicked() {
                        let bytes = d.enabled.then(|| (d.value * d.unit.kb() * 1024.0).round() as u64);
                        actions.push(Action::SetSpeedLimit(d.id, bytes));
                        close = true;
                    }
                    if ui.button(tr("Cancel")).clicked() {
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
        let (question, partial) = match p.ids.as_slice() {
            [] => {
                self.remove_prompt = None;
                return;
            }
            [id] => (
                trf!(
                    "Remove {name} from the list?",
                    name = rows.iter().find(|e| e.id == *id).map(display_name).unwrap_or_default()
                ),
                tr("It is not finished, so its partial file goes too.").to_string(),
            ),
            ids => (
                trf!("Remove {count} downloads from the list?", count = ids.len()),
                match unfinished {
                    1 => tr("One of them is not finished; its partial file goes too.").to_string(),
                    n => trf!("{count} of them are not finished; their partial files go too.", count = n),
                },
            ),
        };
        let mut close = false;
        egui::Window::new(tr("Remove from the list"))
            .id(egui::Id::new("remove"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(question);
                ui.add_space(4.0);
                ui.checkbox(&mut p.delete_files, tr("Also send the files to the Recycle Bin"));
                if p.delete_files && unfinished > 0 {
                    ui.colored_label(ui.visuals().warn_fg_color, partial);
                }
                ui.checkbox(&mut p.dont_ask, tr("Don't ask again"))
                    .on_hover_text(tr("Options > General can turn the question back on"));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(connected, egui::Button::new(tr("Remove"))).clicked() {
                        actions.push(Action::RemoveNow {
                            ids: p.ids.clone(),
                            delete_files: p.delete_files,
                            dont_ask: p.dont_ask,
                        });
                        close = true;
                    }
                    if ui.button(tr("Cancel")).clicked() {
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
        egui::Window::new(tr("New link"))
            .id(egui::Id::new("refresh"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(trf!("A fresh link for {name}", name = display_name(entry)));
                ui.weak(tr("The part already downloaded is kept if the new link is for the same file."));
                ui.add_space(6.0);
                if let Some((_, new_url, reason)) = &mismatch {
                    ui.colored_label(ui.visuals().warn_fg_color, reason);
                    ui.label(tr("Start over from the beginning with the new link?"));
                    ui.horizontal(|ui| {
                        if ui.add_enabled(connected, egui::Button::new(tr("Start over"))).clicked() {
                            actions.push(Action::RefreshWith {
                                id,
                                url: new_url.clone(),
                                restart: true,
                            });
                        }
                        if ui.button(tr("Cancel")).clicked() {
                            close = true;
                        }
                    });
                    return;
                }
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(url).hint_text(tr("Paste the new address")).desired_width(360.0));
                    let ok = connected && is_download_url(url.trim());
                    if ui.add_enabled(ok, egui::Button::new(tr("Use this link"))).clicked() {
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
                        ui.label(tr("Waiting for your next download in the browser..."));
                        if ui.button(tr("Stop waiting")).clicked() {
                            actions.push(Action::CancelRefresh);
                        }
                    });
                } else if ui
                    .add_enabled(connected, egui::Button::new(tr("Use my next download in the browser")))
                    .on_hover_text(tr("Start the download again on its web page; UIDM takes the new link from it"))
                    .clicked()
                {
                    actions.push(Action::RefreshFromBrowser(id));
                }
                ui.add_space(6.0);
                if ui.button(tr("Close")).clicked() {
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
    /// Their widths.
    layout: &'a columns::Layout,
    /// The match the find bar moved to.
    found: bool,
    /// Scroll the list to this row.
    scroll: bool,
}

/// A "Queue" menu that puts the download in a queue.
fn queue_menu(ui: &mut egui::Ui, e: &DownloadEntry, queues: &[Queue], actions: &mut Vec<Action>) {
    ui.menu_button(tr("Queue"), |ui| {
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
    if ui.checkbox(&mut ticked, "").on_hover_text(tr("Tick to export it")).changed() {
        actions.push(Action::Select(e.id, ticked));
    }
    for column in row.columns {
        // A column given a width keeps it, cutting what does not fit; text
        // columns cut their text themselves.
        match row.layout.width(*column).filter(|_| !column.is_text()) {
            Some(w) => {
                ui.allocate_ui_with_layout(egui::vec2(w, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_width(w);
                    ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                    draw_cell(ui, *column, e, row, actions);
                });
            }
            None => draw_cell(ui, *column, e, row, actions),
        }
    }
    ui.horizontal(|ui| {
        if let Some((queue, i, len)) = row.place {
            ui.add_enabled_ui(connected, |ui| {
                if ui.add_enabled(i > 0, egui::Button::new(tr("Up")).small()).on_hover_text(tr("Run it earlier")).clicked() {
                    actions.push(Action::MoveToQueue {
                        id: e.id,
                        queue,
                        position: Some(i - 1),
                    });
                }
                if ui
                    .add_enabled(i + 1 < len, egui::Button::new(tr("Down")).small())
                    .on_hover_text(tr("Run it later"))
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
                if ui.small_button(tr("Cancel")).clicked() {
                    actions.push(Action::Cancel(e.id));
                }
            }
            Status::Waiting => {
                if ui.small_button(tr("New link")).on_hover_text(tr("Give it a fresh download link")).clicked() {
                    actions.push(Action::OpenRefresh(e.id));
                }
                if ui.small_button(tr("Start")).clicked() {
                    actions.push(Action::Confirm(Answer::start(e.id)));
                }
                queue_menu(ui, e, row.queues, actions);
                if ui.small_button(tr("Remove")).clicked() {
                    actions.push(Action::Remove(vec![e.id]));
                }
            }
            Status::Failed | Status::Canceled | Status::Interrupted => {
                if e.error.as_deref().and_then(uidm::engine::login_wanted).is_some()
                    && ui.small_button(tr("Log in...")).on_hover_text(tr("Give the user name and password the site asks for")).clicked()
                {
                    actions.push(Action::OpenLogin(e.id));
                }
                if ui.small_button(tr("New link")).on_hover_text(tr("Give it a fresh download link")).clicked() {
                    actions.push(Action::OpenRefresh(e.id));
                }
                if ui.small_button(tr("Retry")).on_hover_text(tr("Go on from where it stopped")).clicked() {
                    actions.push(Action::Retry(e.id));
                }

                queue_menu(ui, e, row.queues, actions);
                if ui.small_button(tr("Remove")).clicked() {
                    actions.push(Action::Remove(vec![e.id]));
                }
            }
            Status::Done => {
                if let Some(path) = &e.path {
                    if ui.small_button(tr("Open")).clicked() {
                        actions.push(Action::Open(e.id));
                    }
                    if ui.small_button(tr("Show in folder")).clicked() {
                        actions.push(Action::ShowInFolder(path.clone()));
                    }
                }

                if ui.small_button(tr("Remove")).clicked() {
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
    ui.menu_button(tr("More"), |ui| {
        if ui.button(tr("Properties...")).on_hover_text(tr("Its details, link, description and login")).clicked() {
            actions.push(Action::OpenProperties(e.id));
            ui.close_menu();
        }
        if let Some(path) = e.path.as_ref().filter(|_| e.status == Status::Done) {
            if ui.button(tr("Open with...")).on_hover_text(tr("Choose the program to open it with")).clicked() {
                actions.push(Action::OpenWith(path.clone()));
                ui.close_menu();
            }
            if uidm::media::convert::is_video(path) {
                let convert = ui
                    .add_enabled(connected, egui::Button::new(tr("Convert...")))
                    .on_hover_text(tr("To an MP4, or its sound to an MP3 or M4A (needs ffmpeg)"));
                if convert.clicked() {
                    actions.push(Action::OpenConvert(e.id));
                    ui.close_menu();
                }
            }
        }
        if e.status != Status::Done
            && ui.button(tr("Download window")).on_hover_text(tr("Its status, speed and time left, with Pause and Cancel")).clicked()
        {
            actions.push(Action::OpenProgress(e.id));
            ui.close_menu();
        }
        if ui.button(tr("Connections...")).on_hover_text(tr("Each connection's range and progress")).clicked() {
            actions.push(Action::OpenConnections(e.id));
            ui.close_menu();
        }
        let limit = ui
            .add_enabled(connected && e.status != Status::Done, egui::Button::new(tr("Speed limit...")))
            .on_hover_text(tr("A limit for this download alone"));
        if limit.clicked() {
            actions.push(Action::OpenSpeedLimit(e.id));
            ui.close_menu();
        }
        let again = matches!(e.status, Status::Done | Status::Failed | Status::Canceled | Status::Interrupted);
        let redo = ui
            .add_enabled(connected && again, egui::Button::new(tr("Download again")))
            .on_hover_text(tr("Start over from the first byte; if the file is still there, the file-exists option decides"));
        if redo.clicked() {
            actions.push(Action::Redownload(e.id));
            ui.close_menu();
        }
    });
}

/// One cell of a download's row.
fn draw_cell(ui: &mut egui::Ui, column: Column, e: &DownloadEntry, row: &Row, actions: &mut Vec<Action>) {
    // A stream counts segments; its size is only known once fetched.
    let parts = e.parts.filter(|[_, all]| *all > 0);
    let running = row.speed.filter(|s| *s > 0.0 && e.status == Status::Downloading);
    let width = row.layout.width(column).unwrap_or(columns::TEXT_WIDTH);
    match column {
        Column::Name => {
            // A fixed-width, left-aligned cell so long names truncate instead
            // of pushing the action buttons off the window.
            ui.allocate_ui_with_layout(
                egui::vec2(width, 18.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_width(width);
                    let mut name = egui::RichText::new(display_name(e));
                    if row.found {
                        let selection = ui.visuals().selection;
                        name = name.background_color(selection.bg_fill).color(selection.stroke.color);
                    }
                    let label = ui
                        .add(egui::Label::new(name).truncate().sense(egui::Sense::click()))
                        .on_hover_text(e.path.as_deref().unwrap_or(&e.url));
                    // Like IDM: a finished file opens, anything else shows
                    // its download window.
                    if label.double_clicked() {
                        actions.push(if e.status == Status::Done {
                            Action::Open(e.id)
                        } else {
                            Action::OpenProgress(e.id)
                        });
                    }
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
                size.on_hover_text(trf!("{done} of {all} segments", done = done, all = all));
            }
        }
        Column::Progress => {
            let bar = egui::ProgressBar::new(columns::fraction(e).unwrap_or(0.0)).desired_width(130.0);
            theme::progress_bar(ui, if e.total.is_some() || e.status == Status::Done {
                bar.show_percentage()
            } else if let Some([done, all]) = parts {
                bar.text(trf!("{done}/{all} segments", done = done, all = all))
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
                Status::Queued => (tr("Queued"), visuals.weak_text_color()),
                Status::Downloading => (tr("Downloading"), visuals.text_color()),
                Status::Done => (tr("Done"), visuals.text_color()),
                Status::Failed => (tr("Failed"), visuals.error_fg_color),
                Status::Canceled => (tr("Canceled"), visuals.warn_fg_color),
                Status::Interrupted => (tr("Interrupted"), visuals.warn_fg_color),
                Status::Confirming => (tr("Confirm"), visuals.weak_text_color()),
                Status::Waiting => (tr("Waiting"), visuals.weak_text_color()),
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
                    ui.weak(tr("(note)")).on_hover_text(note);
                }
                if let Some(limit) = e.speed_limit {
                    ui.weak(tr("(limited)")).on_hover_text(trf!("Limited to {speed}/s", speed = human_bytes(limit)));
                }
            });
        }
        Column::LastTry => {
            ui.label(e.last_try.map(columns::local_date).unwrap_or_default());
        }
        Column::SaveTo => {
            let folder = e.path.as_deref().and_then(|p| Path::new(p).parent()).map(|d| d.display().to_string());
            text_cell(ui, folder.as_deref().unwrap_or(""), width);
        }
        Column::Referrer => {
            text_cell(ui, e.referrer.as_deref().unwrap_or(""), width);
        }
        Column::Description => {
            text_cell(ui, e.description.as_deref().unwrap_or(""), width);
        }
    }
}

/// A long text, cut to `width` with the rest in a tooltip.
fn text_cell(ui: &mut egui::Ui, text: &str, width: f32) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 18.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_width(width);
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
        format!("{n} {}", tr(UNITS[0]))
    } else {
        format!("{value:.1} {}", tr(UNITS[unit]))
    }
}

fn human_duration(secs: f64) -> String {
    let s = secs.round().max(0.0) as u64;
    match s {
        0..=59 => trf!("{seconds}s", seconds = s),
        60..=3599 => trf!("{minutes}m {seconds}s", minutes = s / 60, seconds = s % 60),
        _ => trf!("{hours}h {minutes}m", hours = s / 3600, minutes = (s % 3600) / 60),
    }
}
