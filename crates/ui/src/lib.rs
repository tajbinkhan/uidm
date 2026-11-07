//! UIDM's desktop window: its egui views, its state (`AppState`) and the
//! messages that change it. It lists downloads and controls them.
//!
//! The `uidm-gui` binary builds the adapters, puts them in a [`Start`] and
//! calls [`run`]. This crate reaches the rest of UIDM only through the ports
//! in `uidm_application` and `uidm_domain` (AGENTS.md section 3).
//!
//! The window is one more daemon client. A background thread connects (starting the
//! daemon if needed, with the same mutual handshake as the host), asks for the
//! full list and the settings, then applies every event to a `DownloadTable`.
//! The window draws from a copy of that table each frame and sends `add`,
//! `cancel`, `retry`, `remove` and `set_settings` requests over the same
//! connection.
//!
//! It also puts an icon in the tray. Closing the window hides it there (unless
//! Options says to quit), and `--minimized` starts it hidden, which is how
//! "Start with Windows" runs it. Hiding cloaks the window rather than
//! hiding it, since a hidden window gets no frames and every dialog is drawn
//! in its frame: a save-as or download window opens on its own while the
//! main window stays in the tray, as IDM's do. The tray or a click on a
//! notification shows the window through Win32 directly.

mod addurl;
mod answers;
mod banner;
mod categories;
mod columns;
mod connections;
mod dialog;
#[cfg(windows)]
mod droptarget;
#[cfg(not(windows))]
#[path = "droptarget_other.rs"]
mod droptarget;
mod find;
mod help;
mod i18n;
mod links;
mod login;
mod menubar;
mod pending;
mod ports;

pub use ports::Ports;
mod progress;
mod properties;
mod queues;
mod state;
mod table;
mod theme;
mod toolbar;
mod tools;
mod tray;
#[cfg(windows)]
mod win;
#[cfg(not(windows))]
#[path = "win_other.rs"]
mod win;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use categories::Filter;
use eframe::egui;
use i18n::{tr, trf};
use pending::Pending;
#[cfg(windows)]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use state::{AppState, Forget, GrabEvent, Link, Need, UiMessage, Watch};
use uidm_application::daemon::{Command, DaemonClient, DaemonError};
use uidm_domain::download::CommandLine;
use uidm_domain::download::{Answer, Download, DownloadEntry, OnDone, Properties, Scan, Status};
use uidm_domain::grab::GrabRequest;
use uidm_domain::queue::{Queue, WhenDone, MAIN_QUEUE};
use uidm_domain::settings::Sound;
use uidm_domain::settings::{
    parse_extensions, CaptureKey, Category, Duplicate, FileExists, Notify, PanelView, ProgressTab, ProxyMode, Scanner,
    SelectionButton, ServerLimit, Settings, SiteLogin, SocksVersion, BROWSER_MENU_ITEMS, CAPTURE_MIN_SIZE_MAX_KB,
    CONNECTIONS_RANGE, GENERAL, MAX_CONCURRENT_RANGE, QUOTA_HOURS_RANGE, SERVER_CONNECTIONS_RANGE,
    SPEED_LIMIT_RANGE_KBPS, TEXT_SIZE_RANGE, USER_AGENT_PRESETS,
};

/// The `client` this window names in its `hello`, so the daemon knows a
/// window is open to show a save-as dialog. `main` builds the daemon client
/// with it.
pub const WINDOW_CLIENT: &str = "window";

/// The window icon as raw 64x64 RGBA, made from assets/icon-1024.png so no
/// image decoder is needed at run time.
const ICON_RGBA: &[u8] = include_bytes!("../../../assets/icon-64.rgba");
const ICON_SIZE: u32 = 64;

/// How often the grabber's projects and templates are read again, and how
/// often while the grabber is open.
const LIBRARY_EVERY: Duration = Duration::from_secs(5);
const LIBRARY_OPEN_EVERY: Duration = Duration::from_secs(1);

/// How often the clipboard's change counter is checked.
const CLIPBOARD_POLL: Duration = Duration::from_millis(700);

/// A failure gets its dialog after this long, so a download the browser
/// extension takes back (and removes) right away gets none. The daemon waits
/// the same for its notifications.
const FAILED_GRACE: Duration = Duration::from_secs(1);

/// What `main` hands the window: its adapters and how it was started.
pub struct Start {
    /// The adapters behind the views' ports.
    pub ports: Ports,
    /// The connection to the daemon.
    pub daemon: Arc<dyn DaemonClient>,
    /// Start hidden in the tray (`--minimized`), as "Start with Windows"
    /// does.
    pub minimized: bool,
    /// A command line to run, such as a download to add, or why it could
    /// not be read.
    pub command_line: Option<Result<CommandLine, String>>,
    /// A named test copy's name, which the title shows.
    pub instance: Option<String>,
}

/// Open the window and run it until it quits. A command line that a window
/// already open takes ends this copy without a window.
pub fn run(start: Start) -> eframe::Result {
    let Start { ports, daemon, mut minimized, command_line, instance } = start;
    ports::install(ports);
    let mut startup_error = None;
    match command_line {
        None => {}
        Some(Ok(cmd)) => match daemon.run_command_line(cmd) {
            // A window was open and took it; this copy has nothing to do.
            Ok(true) => return Ok(()),
            // It runs once this window connects; its dialogs bring the
            // window forward.
            Ok(false) => minimized = true,
            Err(e) => {
                let error = match e {
                    DaemonError::Closed => tr("the daemon closed the connection").to_string(),
                    DaemonError::Connect(why) | DaemonError::Refused(why) => why,
                };
                startup_error = Some(trf!("Could not pass the command line to UIDM: {error}", error = error));
            }
        },
        Some(Err(e)) => startup_error = Some(e),
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            // A named copy says so, to tell it from the installed one.
            .with_title(match instance {
                Some(name) => format!("UIDM ({name})"),
                None => "UIDM".to_string(),
            })
            // Elsewhere "hidden" is minimized, which needs a visible window.
            .with_visible(!minimized || !cfg!(windows))
            .with_icon(egui::IconData { rgba: ICON_RGBA.to_vec(), width: ICON_SIZE, height: ICON_SIZE })
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
                    menubar::attach(h.hwnd.get());
                    // Started in the tray: made hidden (cloaked) rather than
                    // left invisible, so its dialogs can open.
                    if minimized {
                        win::hide();
                    }
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
            theme::apply(&cc.egui_ctx, crate::ports::ports().system.fonts());
            // Until the settings arrive, follow Windows.
            i18n::set_language("");
            let mut app = App::new(&cc.egui_ctx, daemon);
            if startup_error.is_some() {
                if let Some(e) = startup_error {
                    state::apply(&mut app.state, UiMessage::Error(e));
                }
            }
            menubar::init(&cc.egui_ctx);
            // Without a tray icon there would be no way back to a hidden
            // window.
            if !tray::installed() {
                win::show();
            }
            Ok(Box::new(app))
        }),
    )
}

/// Lock `m`. The window's mutexes hold plain values (a picked file, the
/// tooltip, the threads' `Watch`) that a panicking thread cannot leave
/// half-changed.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a click asked for, applied after drawing.
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
    Convert(u64, uidm_domain::media::Target, bool),
    OpenAbout,
    /// Look for a newer release now.
    CheckUpdate,
    /// Download and run the newer release's Setup.
    InstallUpdate,
    Reconnect,
    /// Stop the daemon and start a new one.
    RestartDaemon,
    OpenOptions,
    /// Open Options at the Connection tab with a new login for this host.
    /// Ask for the login a failed download's site wants.
    OpenLogin(u64),
    /// Add a download as it is (one with a login, from the URL bar).
    AddDownload(Download),
    SaveSettings(Settings),
    /// Send new settings without the Options dialog (the speed limit toggle).
    QuickSettings(Settings),
    /// The site grabber's own choices: add the ticked files on closing, and
    /// save a project whose name is unique without asking.
    GrabOptions {
        add_on_close: bool,
        save_unique_silently: bool,
    },
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
    /// Take a download out of its queue, keeping it in the list.
    RemoveFromQueue(u64),
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
    /// Open a saved grabber project.
    OpenGrabProject(String),
    Grab(GrabRequest),
    CancelGrab,
    /// Ask the servers of the link picker's links for their type and size.
    ProbeLinks {
        request: u64,
        links: Vec<Download>,
    },
    /// The link picker closed: stop asking.
    StopProbeLinks,
    /// A click on a download's name: select only it, or with Ctrl add or
    /// take it away, or with Shift select the run from the last one clicked.
    Click {
        id: u64,
        ctrl: bool,
        shift: bool,
    },
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
    /// Set or clear a download's own limit, in bytes per second, and
    /// whether it stays when the download is stopped and resumed.
    SetSpeedLimit(u64, Option<u64>, bool),
    SetProperties {
        id: u64,
        changes: Properties,
    },
}

struct App {
    /// What the window draws from; only `state::apply` changes it.
    state: AppState,
    /// Messages from the threads outside the frame, drained each frame.
    inbox: Receiver<UiMessage>,
    /// Where those threads send them.
    outbox: Sender<UiMessage>,
    /// What those threads keep from the daemon's events.
    watch: Arc<Mutex<Watch>>,
    /// The tray tooltip as the frame last made it, for the tray to read.
    tooltip_now: Arc<Mutex<String>>,
    /// When the grabber's projects and templates were last asked for.
    library_asked: Option<Instant>,
    /// The connection to the daemon.
    daemon: Arc<dyn DaemonClient>,
    ctx: egui::Context,
    /// Tasks > Add new download, while it is open.
    add_dialog: Option<addurl::AddDialog>,
    /// The Options dialog, while it is open.
    options: Option<OptionsDraft>,
    /// The save-as dialog for the oldest `confirming` download.
    confirm: Option<ConfirmDraft>,
    /// Many downloads waiting for answers before they are added.
    answers: Option<answers::Prompt>,
    /// A batch's questions being worked out on a worker: the prompt, or
    /// the downloads to add when nothing needs asking, with their queue
    /// and whether to start it.
    asking: Vec<Pending<AskedBatch>>,
    /// Batch prompts waiting for the one on screen.
    prompts: VecDeque<answers::Prompt>,
    /// The login prompt on screen.
    login: Option<login::LoginPrompt>,
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
    /// The window was minimized last frame, so minimizing it is noticed once.
    was_minimized: bool,
    /// The theme and text size last applied, so they are only set when the
    /// settings change them (Ctrl+Plus and Ctrl+Minus zoom in between).
    appearance: Option<(uidm_domain::settings::Theme, u32)>,
    /// The language setting last applied.
    language: Option<String>,
    /// Which downloads the list shows.
    filter: Filter,
    /// The categories tree between frames.
    tree: categories::Tree,
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
    /// Downloads > Find Next was chosen; step to the next match.
    find_next: bool,
    /// The download a Shift+click selects from.
    anchor: Option<u64>,
    /// The downloads the list shows, in its order, for Shift+click and Ctrl+A.
    order: Vec<u64>,
    /// The list's columns and sort order, saved when they change.
    layout: columns::Layout,
    /// The Columns dialog is open, with the layout it opened with, which
    /// its Cancel puts back.
    columns_dialog: Option<columns::Layout>,
    /// View > Font > Choose Font is open, with the font being chosen.
    font_dialog: Option<columns::ListFont>,
    /// The list's table between frames.
    table: table::TableState,
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
    /// The download windows in the tray, as last given to it.
    hidden_windows: Vec<(u64, String)>,
    /// The link picker, while it is open.
    links: Option<links::LinksDialog>,
    /// A zip download's preview, asked for or shown.
    zip_preview: Option<ZipPreview>,
    /// The Convert dialog: the video, what it becomes, and whether the
    /// original goes.
    convert: Option<(u64, uidm_domain::media::Target, bool)>,
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
    /// Keep the limit when the download is stopped and resumed.
    remember: bool,
}

/// The files inside a zip download, once the daemon has listed them.
struct ZipPreview {
    id: u64,
    result: Option<Result<Vec<uidm_domain::download::ZipEntry>, String>>,
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
            let start = (!start.is_empty()).then(|| Path::new(&start));
            if let Some(folder) = crate::ports::ports().pickers.pick_folder(title, start) {
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
            let kinds = [uidm_application::window::FileKind::new(ext, &[ext])];
            if let Some(file) = crate::ports::ports().pickers.pick_file(title, &kinds, None) {
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
    /// "Start the queue now": start the queue Download later puts it in,
    /// as IDM's "Start queue processing" does, rather than leave it waiting.
    start_queue: bool,
    /// The name box the queue picker's "+" opened, with its error, if open.
    new_queue: Option<(String, Option<String>)>,
    /// The folder last checked for whether UIDM may write there, and the
    /// answer, so the check runs once per folder typed or picked.
    write_check: Option<(String, bool)>,
    /// Save into a folder UIDM may not write to, with administrator rights.
    elevate: bool,
    /// A queue "+" just created, to select once the daemon lists it (the
    /// daemon picks its id), and when it was sent.
    pending_queue: Option<(String, std::time::Instant)>,
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

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
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
        ProxyLoginDraft { username: username.to_string(), ..ProxyLoginDraft::default() }
    }

    fn new_password(&self) -> Option<String> {
        self.changed.then(|| self.password.clone())
    }
}

/// An ffmpeg field's text and the ffmpeg found for it, if any.
type FfmpegLookup = (String, Option<PathBuf>);

/// A batch's questions, worked out on a worker: the prompt, or the
/// downloads when nothing needs asking; then their queue and whether to
/// start it.
type AskedBatch = (Result<answers::Prompt, Vec<Download>>, u64, bool);

/// What Save's worker did: each registration it made (the browser and
/// whether it is now registered), and the settings to send, or why not.
type SaveChecked = (Vec<(uidm_application::window::BrowserSlot, bool)>, Result<Settings, String>);

/// A browser in Options' list: whether it was registered when the dialog
/// opened, and whether it should be.
struct BrowserDraft {
    /// The browser whose registration it uses: itself, or for an added
    /// browser the one its family reads.
    browser: uidm_application::window::BrowserSlot,
    name: String,
    /// The browser the user added by its program, if it is one.
    added: Option<uidm_domain::settings::AddedBrowser>,
    /// For an added browser, whether its place is known rather than the
    /// place most of its family reads.
    known: bool,
    was: bool,
    on: bool,
    /// Taken out of the list with Remove; it is saved without it.
    removed: bool,
}

/// What Options shows of the browsers, read on a worker: the browsers found
/// or registered, then those the user `added`.
fn read_browsers(added: &[uidm_domain::settings::AddedBrowser]) -> state::BrowsersRead {
    let browsers = &crate::ports::ports().browsers;
    let mut rows: Vec<state::BrowserRow> = browsers
        .known()
        .into_iter()
        .map(|(b, registered)| state::BrowserRow {
            name: b.name.clone(),
            browser: b,
            added: None,
            known: true,
            registered,
        })
        .collect();
    rows.extend(added.iter().filter_map(browser_row));
    state::BrowsersRead { rows, chrome_readers: browsers.chrome_readers() }
}

/// The row for a browser the user `added` by its program, or `None` when
/// UIDM cannot tell what browser it is. Reads the disk: for workers.
fn browser_row(added: &uidm_domain::settings::AddedBrowser) -> Option<state::BrowserRow> {
    let browsers = &crate::ports::ports().browsers;
    let target = browsers.identify(Path::new(&added.program))?;
    let registered = browsers.registered(&target.browser);
    Some(state::BrowserRow {
        browser: target.browser,
        name: added.name.clone(),
        added: Some(added.clone()),
        known: target.known,
        registered,
    })
}

/// Save's checks, on a worker: the settings' checks that look at the disk,
/// then each browser registration that `changes` asks for, in order,
/// stopping at the first that fails.
fn check_and_register(
    settings: Settings,
    changes: Vec<(uidm_application::window::BrowserSlot, String, bool)>,
) -> SaveChecked {
    let ports = crate::ports::ports();
    if let Err(e) = ports.checks.validate(&settings) {
        return (Vec::new(), Err(e.0));
    }
    let mut done = Vec::new();
    for (browser, name, on) in changes {
        if let Err(e) = ports.browsers.set_registered(&browser, on) {
            return (done, Err(trf!("Could not change {browser}: {error}", browser = name, error = e)));
        }
        done.push((browser, on));
    }
    (done, Ok(settings))
}

impl BrowserDraft {
    /// The program of a browser the user added.
    fn program(&self) -> Option<PathBuf> {
        self.added.as_ref().map(|a| PathBuf::from(&a.program))
    }

    /// Whether Options can restart it.
    fn can_restart(&self) -> bool {
        crate::ports::ports().browsers.can_restart(&self.browser, self.program().as_deref())
    }
}

/// The Browsers section's line on the extension's link: connected, when
/// it last was, or not since the daemon started.
fn browser_link_text(connected: bool, last_seen: Option<u64>, now: u64) -> String {
    if connected {
        return tr("The browser extension is connected.").into();
    }
    let Some(seen) = last_seen else {
        return tr("The browser extension has not connected since UIDM started.").into();
    };
    let minutes = now.saturating_sub(seen) / 60;
    match minutes {
        0 => tr("The browser extension last connected less than a minute ago.").into(),
        1 => tr("The browser extension last connected a minute ago.").into(),
        2..=119 => trf!("The browser extension last connected {minutes} minutes ago.", minutes = minutes),
        _ => trf!("The browser extension last connected {hours} hours ago.", hours = minutes / 60),
    }
}

/// One site login being edited. An empty password keeps the stored one.
struct LoginDraft {
    site: String,
    username: String,
    password: String,
    /// Added in this dialog, so it has no stored password yet.
    new: bool,
    /// Also sent over plain http (as Basic) and to FTP servers.
    plain: bool,
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
    minimize_to_tray: bool,
    tray_icon: bool,
    /// Kept as it is; the View menu changes it.
    categories_panel: bool,
    start_with_windows: bool,
    keep_awake: bool,
    notify: Notify,
    sound_on_done: bool,
    sound_on_failed: bool,
    sound_on_queue_done: bool,
    /// The WAV files, in `SOUND_EVENTS` order; empty for Windows' sound.
    sound_on_queue_started: bool,
    sound_files: [String; 4],
    sound_picked: [Picked; 4],
    scan_after_download: bool,
    scanner: Scanner,
    scanner_command: String,
    proxy_mode: ProxyMode,
    proxy_http: String,
    proxy_https: String,
    proxy_ftp: String,
    ftp_active: bool,
    proxy_socks5: String,
    proxy_socks_https: bool,
    proxy_socks_ftp: bool,
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
    limit_on_startup: bool,
    /// The limit for each file, and its value in KB/s, kept while it is off.
    per_file_on: bool,
    per_file_kbps: u64,
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
    /// General's own types, comma-separated.
    general_extensions: String,
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
    video_auto_capture: bool,
    /// Comma-separated.
    video_auto_types: String,
    /// Each context menu item, its label, and whether it shows in Chromium
    /// browsers and in Firefox.
    menu_items: Vec<(&'static str, &'static str, bool, bool)>,
    /// One site per line.
    video_button_excluded_sites: String,
    ffmpeg_path: String,
    /// ffmpeg.exe chosen with Browse.
    ffmpeg_picked: Picked,
    /// The ffmpeg field as it was last looked up, and what the worker
    /// thread found for it.
    ffmpeg_asked: Option<String>,
    ffmpeg_found: Arc<Mutex<Option<FfmpegLookup>>>,
    confirm_remove: bool,
    remove_deletes_files: bool,
    server_file_date: bool,
    server_file_date_created: bool,
    resume_ignores_date: bool,
    manual_user_agent: String,
    /// Empty writes partial files in place.
    temp_dir: String,
    temp_picked: Picked,
    /// The browsers found or registered, with their registration, once a
    /// worker read them.
    browsers: Vec<BrowserDraft>,
    /// Whether that read came.
    browsers_loaded: bool,
    /// The added browsers as saved, kept while the read is awaited.
    added_saved: Vec<uidm_domain::settings::AddedBrowser>,
    /// A picked browser program being identified on a worker.
    adding_browser: Pending<(PathBuf, Option<state::BrowserRow>)>,
    /// Get System's read of the system's proxy, done on a worker.
    system_proxy: Pending<Option<uidm_application::window::SystemProxy>>,
    /// Save's disk checks and browser registrations, done on a worker:
    /// the registrations made, and the settings to send or why not.
    checking: Pending<SaveChecked>,
    /// Browsers found that read Chrome's registration.
    chrome_readers: Vec<String>,
    /// A browser's program chosen with "Add browser...".
    browser_picked: Picked,
    /// What happened to the last browser added or restarted.
    browser_note: Option<String>,
    /// The row whose Restart waits for the user's yes.
    restart_ask: Option<usize>,
    /// A restart's outcome, from its thread.
    restart_done: Arc<Mutex<Option<String>>>,
    /// Whether the extension is connected, and when it last was, from the
    /// daemon; `None` before it says.
    browser_link: Option<(bool, Option<u64>)>,
    capture_skip_key: CaptureKey,
    capture_force_key: CaptureKey,
    capture_keys_left_click: bool,
    capture_force_skips_pages: bool,
    capture_private: bool,
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
    progress_hide_buttons: bool,
    progress_start_tab: ProgressTab,
    progress_start_details: bool,
    list_queue_panel: bool,
    /// Kept as it is; only the Queues dialog changes it.
    schedule_note_hidden: bool,
    /// Kept as they are; only the site grabber changes them.
    grab_add_on_close: bool,
    grab_save_unique_silently: bool,
    tips_on_startup: bool,
    /// Kept as it is; only the tip window changes it.
    next_tip: usize,
    theme: uidm_domain::settings::Theme,
    text_size: u32,
    /// Empty follows Windows.
    language: String,
    error: Option<String>,
    picked: Picked,
}

/// The sites in a one-per-line text field.
fn sites(text: &str) -> Vec<String> {
    text.lines().map(|l| l.trim().to_ascii_lowercase()).filter(|l| !l.is_empty()).collect()
}

impl OptionsDraft {
    fn new(s: &Settings) -> Self {
        let kbps = s.speed_limit_kbps;
        let limit_unit = if kbps >= 1024 && kbps.is_multiple_of(1024) { SpeedUnit::Mb } else { SpeedUnit::Kb };
        OptionsDraft {
            tab: OptionsTab::General,
            close_to_tray: s.close_to_tray,
            minimize_to_tray: s.minimize_to_tray,
            tray_icon: s.tray_icon,
            categories_panel: s.categories_panel,
            start_with_windows: s.start_with_windows,
            keep_awake: s.keep_awake,
            notify: s.notify,
            sound_on_done: s.sound_on_done,
            sound_on_failed: s.sound_on_failed,
            sound_on_queue_done: s.sound_on_queue_done,
            sound_on_queue_started: s.sound_on_queue_started,
            sound_files: [
                s.sound_done_file.clone(),
                s.sound_failed_file.clone(),
                s.sound_queue_started_file.clone(),
                s.sound_queue_file.clone(),
            ],
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
            proxy_socks_https: s.proxy_socks_https,
            proxy_socks_ftp: s.proxy_socks_ftp,
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
                    plain: l.plain,
                })
                .collect(),
            folder: s.download_dir.clone().unwrap_or_default(),
            connections: s.connections,
            max_concurrent: s.max_concurrent,
            limit_enabled: s.speed_limit_enabled,
            limit_value: kbps as f64 / limit_unit.kb(),
            limit_unit,
            limit_on_startup: s.speed_limit_on_startup,
            per_file_on: s.speed_limit_per_file_kbps.is_some(),
            per_file_kbps: s.speed_limit_per_file_kbps.unwrap_or(512),
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
            general_extensions: s.general_extensions.join(", "),
            capture_types: s.capture_types.join(", "),
            capture_min_size_kb: s.capture_min_size_kb,
            capture_excluded_sites: s.capture_excluded_sites.join("\n"),
            clipboard_monitor: s.clipboard_monitor,
            clipboard_start: s.clipboard_start,
            video_button: s.video_button,
            video_types: {
                let mut all: Vec<(String, bool)> = uidm_domain::settings::default_video_types()
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
            video_auto_capture: s.video_auto_capture,
            video_auto_types: s.video_auto_types.join(", "),
            menu_items: BROWSER_MENU_ITEMS
                .iter()
                .map(|(id, label)| {
                    let shown = |hidden: &[String]| !hidden.iter().any(|h| h == id);
                    (*id, *label, shown(&s.browser_menu_hidden_chromium), shown(&s.browser_menu_hidden_firefox))
                })
                .collect(),
            video_button_excluded_sites: s.video_button_excluded_sites.join("\n"),
            ffmpeg_path: s.ffmpeg_path.clone(),
            ffmpeg_picked: Picked::default(),
            ffmpeg_asked: None,
            ffmpeg_found: Arc::default(),
            confirm_remove: s.confirm_remove,
            remove_deletes_files: s.remove_deletes_files,
            server_file_date: s.server_file_date,
            server_file_date_created: s.server_file_date_created,
            resume_ignores_date: s.resume_ignores_date,
            manual_user_agent: s.manual_user_agent.clone(),
            temp_dir: s.temp_dir.clone().unwrap_or_default(),
            temp_picked: Picked::default(),
            // A worker reads them; until then the added ones are kept as saved.
            browsers: Vec::new(),
            browsers_loaded: false,
            added_saved: s.added_browsers.clone(),
            adding_browser: Pending::default(),
            checking: Pending::default(),
            system_proxy: Pending::default(),
            chrome_readers: Vec::new(),
            browser_picked: Picked::default(),
            browser_note: None,
            restart_ask: None,
            restart_done: Arc::default(),
            browser_link: None,
            capture_skip_key: s.capture_skip_key,
            capture_force_key: s.capture_force_key,
            capture_keys_left_click: s.capture_keys_left_click,
            capture_force_skips_pages: s.capture_force_skips_pages,
            capture_private: s.capture_private,
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
            progress_hide_buttons: s.progress_hide_buttons,
            progress_start_tab: s.progress_start_tab,
            progress_start_details: s.progress_start_details,
            list_queue_panel: s.list_queue_panel,
            schedule_note_hidden: s.schedule_note_hidden,
            grab_add_on_close: s.grab_add_on_close,
            grab_save_unique_silently: s.grab_save_unique_silently,
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
            speed_limit_on_startup: self.limit_on_startup,
            speed_limit_per_file_kbps: self.per_file_on.then_some(self.per_file_kbps),
            resume_at_startup: self.resume_at_startup,
            close_to_tray: self.close_to_tray,
            minimize_to_tray: self.minimize_to_tray,
            tray_icon: self.tray_icon,
            categories_panel: self.categories_panel,
            start_with_windows: self.start_with_windows,
            keep_awake: self.keep_awake,
            notify: self.notify,
            sound_on_done: self.sound_on_done,
            sound_on_failed: self.sound_on_failed,
            sound_on_queue_started: self.sound_on_queue_started,
            sound_on_queue_done: self.sound_on_queue_done,
            sound_done_file: self.sound_files[0].trim().to_string(),
            sound_failed_file: self.sound_files[1].trim().to_string(),
            sound_queue_started_file: self.sound_files[2].trim().to_string(),
            sound_queue_file: self.sound_files[3].trim().to_string(),
            scan_after_download: self.scan_after_download,
            scanner: self.scanner,
            scanner_command: self.scanner_command.trim().to_string(),
            proxy_mode: self.proxy_mode,
            proxy_http: self.proxy_http.trim().to_string(),
            proxy_https: self.proxy_https.trim().to_string(),
            proxy_ftp: self.proxy_ftp.trim().to_string(),
            ftp_active: self.ftp_active,
            proxy_socks5: self.proxy_socks5.trim().to_string(),
            proxy_socks_https: self.proxy_socks_https,
            proxy_socks_ftp: self.proxy_socks_ftp,
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
                    plain: l.plain,
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
            general_extensions: parse_extensions(&self.general_extensions),
            capture_types: parse_extensions(&self.capture_types),
            capture_min_size_kb: self.capture_min_size_kb,
            capture_excluded_sites: sites(&self.capture_excluded_sites),
            clipboard_monitor: self.clipboard_monitor,
            clipboard_start: self.clipboard_start,
            video_button: self.video_button,
            video_types: self.video_types.iter().filter(|(_, on)| *on).map(|(t, _)| t.clone()).collect(),
            video_types_hds: true,
            video_min_size_kb: self.video_min_size_kb,
            video_view: self.video_view,
            video_protected_note: self.video_protected_note,
            video_auto_capture: self.video_auto_capture,
            video_auto_types: parse_extensions(&self.video_auto_types),
            browser_menu_hidden_chromium: self.menu_items.iter().filter(|m| !m.2).map(|m| m.0.to_string()).collect(),
            browser_menu_hidden_firefox: self.menu_items.iter().filter(|m| !m.3).map(|m| m.0.to_string()).collect(),
            added_browsers: if self.browsers_loaded {
                self.browsers.iter().filter(|b| !b.removed).filter_map(|b| b.added.clone()).collect()
            } else {
                self.added_saved.clone()
            },
            video_button_excluded_sites: sites(&self.video_button_excluded_sites),
            ffmpeg_path: self.ffmpeg_path.trim().to_string(),
            confirm_remove: self.confirm_remove,
            remove_deletes_files: self.remove_deletes_files,
            server_file_date: self.server_file_date,
            server_file_date_created: self.server_file_date_created,
            resume_ignores_date: self.resume_ignores_date,
            manual_user_agent: self.manual_user_agent.trim().to_string(),
            temp_dir: optional(&self.temp_dir),
            capture_skip_key: self.capture_skip_key,
            capture_force_key: self.capture_force_key,
            capture_keys_left_click: self.capture_keys_left_click,
            capture_force_skips_pages: self.capture_force_skips_pages,
            capture_private: self.capture_private,
            capture_excluded_urls: self
                .capture_excluded_urls
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect(),
            capture_excluded_urls_defaults: true,
            offer_exceptions: self.offer_exceptions,
            selection_button: self.selection_button,
            selection_button_sites: sites(&self.selection_button_sites),
            server_limits: self
                .server_limits
                .iter()
                .map(|(site, max)| ServerLimit { site: site.trim().to_ascii_lowercase(), max: *max })
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
            progress_hide_buttons: self.progress_hide_buttons,
            progress_start_tab: self.progress_start_tab,
            progress_start_details: self.progress_start_details,
            list_queue_panel: self.list_queue_panel,
            schedule_note_hidden: self.schedule_note_hidden,
            grab_add_on_close: self.grab_add_on_close,
            grab_save_unique_silently: self.grab_save_unique_silently,
            tips_on_startup: self.tips_on_startup,
            next_tip: self.next_tip,
            theme: self.theme,
            text_size: self.text_size,
            language: self.language.clone(),
            // The daemon keeps its own.
            last_update_check: 0,
        };
        if let Some(l) = self.logins.iter().find(|l| l.new && l.password.is_empty()) {
            return Err(trf!("The new login for {site} needs a password.", site = l.site.trim()));
        }
        Ok(settings)
    }

    /// Whether the browsers registering in row `i`'s place should be
    /// registered: when one of them still in the list is ticked.
    /// Fill the browser list from a worker's read.
    fn load_browsers(&mut self, read: state::BrowsersRead) {
        self.browsers = read
            .rows
            .into_iter()
            .map(|r| BrowserDraft {
                browser: r.browser,
                name: r.name,
                added: r.added,
                known: r.known,
                was: r.registered,
                on: r.registered,
                removed: false,
            })
            .collect();
        self.chrome_readers = read.chrome_readers;
        self.browsers_loaded = true;
    }

    fn wanted(&self, i: usize) -> bool {
        let place = &self.browsers[i].browser;
        self.browsers.iter().any(|b| !b.removed && b.on && b.browser.same_place(place))
    }

    /// The registrations Save makes: each browser whose box changed, or
    /// that was removed from the list, with whether it is to be registered,
    /// and its name for an error. Rows sharing a place (an added browser
    /// using Chrome's, say) change together, so each place is listed once.
    fn browser_changes(&self) -> Vec<(uidm_application::window::BrowserSlot, String, bool)> {
        let mut changes: Vec<(uidm_application::window::BrowserSlot, String, bool)> = Vec::new();
        for (i, b) in self.browsers.iter().enumerate() {
            let on = self.wanted(i);
            // A row sharing a place already listed changes with it.
            if on != b.was && !changes.iter().any(|(place, _, _)| place.same_place(&b.browser)) {
                changes.push((b.browser.clone(), b.name.clone(), on));
            }
        }
        changes
    }

    /// Note the registrations Save's worker made, for every row sharing
    /// each one's place.
    fn registered(&mut self, done: Vec<(uidm_application::window::BrowserSlot, bool)>) {
        for (place, on) in done {
            for b in self.browsers.iter_mut().filter(|b| b.browser.same_place(&place)) {
                b.was = on;
            }
        }
    }

    /// Start a picked browser program's identification on a worker, and
    /// add it once that is done.
    fn add_picked_browser(&mut self, ctx: &egui::Context) {
        let picked = self.browser_picked.lock().unwrap().take();
        if let Some(program) = picked {
            self.adding_browser.start(ctx, move || {
                let added = uidm_domain::settings::AddedBrowser {
                    name: crate::ports::ports().browsers.display_name(&program),
                    program: program.display().to_string(),
                };
                let row = browser_row(&added);
                (program, row)
            });
        }
        if let Some((program, row)) = self.adding_browser.take() {
            self.add_browser(&program, row);
        }
    }

    /// Add the browser at `program`, which a worker identified as `row`,
    /// ticked to be registered when Options is saved.
    fn add_browser(&mut self, program: &Path, row: Option<state::BrowserRow>) {
        let Some(row) = row else {
            self.browser_note = Some(trf!(
                "UIDM cannot tell what browser {program} is. Pick the program of a browser built on Chromium (such as Chrome, Edge, Brave, Vivaldi, Opera or Yandex) or on Firefox.",
                program = program.display()
            ));
            return;
        };
        let row = BrowserDraft {
            browser: row.browser,
            name: row.name,
            added: row.added,
            known: row.known,
            was: row.registered,
            on: true,
            removed: false,
        };
        self.browser_note = Some(match (row.known, row.browser.name == row.name) {
            (true, true) => trf!("{browser} is added; it is registered when you save.", browser = row.name),
            (true, false) => trf!("{browser} is added; it reads {place}'s registration, written when you save.", browser = row.name, place = row.browser.name),
            (false, _) => trf!(
                "{browser} is added. UIDM does not know where it looks for programs like UIDM, so it registers it where {place} does, which most browsers of its kind read.",
                browser = row.name,
                place = row.browser.name
            ),
        });
        self.browsers.push(row);
    }
}

impl App {
    fn new(ctx: &egui::Context, daemon: Arc<dyn DaemonClient>) -> Self {
        let (outbox, inbox) = channel();
        let mut app = App {
            state: AppState::default(),
            inbox,
            outbox,
            watch: Arc::default(),
            tooltip_now: Arc::default(),
            library_asked: None,
            daemon,
            ctx: ctx.clone(),
            add_dialog: None,
            options: None,
            confirm: None,
            answers: None,
            asking: Vec::new(),
            prompts: VecDeque::new(),
            login: None,
            hide_completed: false,
            threat_prompt: None,
            tooltip: String::new(),
            tray_shown: None,
            was_minimized: false,
            appearance: None,
            language: None,
            filter: Filter::default(),
            tree: categories::Tree::default(),
            queues_dialog: None,
            refresh: None,
            batch: None,
            export: None,
            grabber: None,
            import: tools::Loaded::default(),
            selected: HashSet::new(),
            find_next: false,
            anchor: None,
            order: Vec::new(),
            find: None,
            layout: columns::Layout::load(),
            columns_dialog: None,
            font_dialog: None,
            layout_unsaved: false,
            table: table::TableState::default(),
            properties: Vec::new(),
            remove_prompt: None,
            limit_dialog: None,
            connection_dialogs: Vec::new(),
            progress_windows: Vec::new(),
            hidden_windows: Vec::new(),
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

    /// Read the grabber's projects and templates again on a worker, every
    /// few seconds, and every second while the grabber is open.
    fn refresh_library(&mut self) {
        let every = if self.grabber.is_some() { LIBRARY_OPEN_EVERY } else { LIBRARY_EVERY };
        if self.library_asked.is_some_and(|t| t.elapsed() < every) {
            return;
        }
        self.library_asked = Some(Instant::now());
        let (outbox, ctx) = (self.outbox.clone(), self.ctx.clone());
        thread::spawn(move || {
            let library = crate::ports::ports().grab.as_ref();
            let read = state::GrabLibrary {
                projects: library.map(|l| l.recent()).unwrap_or_default(),
                templates: library.map(|l| l.templates()).unwrap_or_else(uidm_domain::grab::built_in_templates),
                template: library.map(|l| l.template()).unwrap_or_default(),
                filters: library.map(|l| l.filters()).unwrap_or_else(uidm_domain::grab::default_filters),
            };
            if outbox.send(UiMessage::GrabLibraryRead(read)).is_ok() {
                ctx.request_repaint();
            }
        });
    }

    /// Open the project the category tree asked for, once a worker read
    /// it. An open grabber asks to save its own project first.
    fn open_grab_project(&mut self) {
        match state::take_grab_opened(&mut self.state) {
            Some(Ok(p)) => {
                let d = tools::GrabDialog::open(p, self.state.grab_filters.clone());
                match self.grabber.as_mut() {
                    Some(open) => open.switch_to(d),
                    None => self.grabber = Some(d),
                }
            }
            Some(Err(e)) => {
                state::apply(&mut self.state, UiMessage::Error(e));
            }
            None => {}
        }
    }

    /// Go on with each batch whose questions a worker worked out: add it
    /// when nothing needs asking, else queue its prompt; prompts show one
    /// at a time, in order.
    fn take_asked_batches(&mut self) {
        let mut i = 0;
        while i < self.asking.len() {
            match self.asking[i].take() {
                Some((Ok(prompt), _, _)) => {
                    self.prompts.push_back(prompt);
                    self.asking.remove(i);
                }
                Some((Err(items), queue, start_queue)) => {
                    self.add_many(items, queue, start_queue);
                    self.asking.remove(i);
                }
                None if self.asking[i].busy() => i += 1,
                None => {
                    self.asking.remove(i);
                }
            }
        }
        if self.answers.is_none() {
            self.answers = self.prompts.pop_front();
        }
    }

    /// Apply what the threads outside the frame sent since the last frame.
    fn drain_inbox(&mut self) {
        while let Ok(msg) = self.inbox.try_recv() {
            state::apply(&mut self.state, msg);
        }
    }

    /// Watch the clipboard for download links while Options asks for it,
    /// and offer each new one once. Polls the clipboard's change counter,
    /// which costs nothing, and reads the text only when it changes.
    fn watch_clipboard(&self) {
        let (watch, outbox) = (Arc::clone(&self.watch), self.outbox.clone());
        let daemon = Arc::clone(&self.daemon);
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
                let settings = lock(&watch).settings.clone();
                let (types, start) = match &settings {
                    Some(s) if s.clipboard_monitor => (s.capture_types.clone(), s.clipboard_start),
                    _ => continue,
                };
                let Some(text) = win::clipboard_text() else { continue };
                let Some(url) = uidm_domain::batch::clipboard_link(&text, &types) else { continue };
                if last_offered.as_deref() == Some(url.as_str()) {
                    continue;
                }
                last_offered = Some(url.clone());
                // Started at once, it goes the way a pasted link does, save-as
                // dialog and all; only the offer is skipped.
                if start && daemon.send(Command::Add(Download { url: url.clone(), ..Download::default() })) {
                    continue;
                }
                let _ = outbox.send(UiMessage::ClipboardOffer(url));
                win::show();
                ctx.request_repaint();
            }
        });
    }

    /// Put UIDM in the tray. Its menu acts at once, without waiting for a
    /// frame, since the window may be hidden.
    fn install_tray(&mut self) {
        let (watch, outbox) = (Arc::clone(&self.watch), self.outbox.clone());
        let daemon = Arc::clone(&self.daemon);
        let ctx = self.ctx.clone();
        let on_command = Arc::new(move |command: tray::Command| {
            match command {
                tray::Command::Open => win::show(),
                tray::Command::AddFromClipboard => {
                    let text = win::clipboard_text().unwrap_or_default();
                    let url = text.trim();
                    if is_download_url(url) {
                        daemon.send(Command::Add(Download { url: url.to_string(), ..Download::default() }));
                    } else {
                        let _ = outbox.send(UiMessage::Error(tr("The clipboard holds no http or https link.").into()));
                        win::show();
                    }
                }
                tray::Command::BatchFromClipboard => {
                    let _ = outbox.send(UiMessage::ClipboardBatch);
                    win::show();
                }
                tray::Command::PauseAll => {
                    daemon.send(Command::PauseAll);
                }
                tray::Command::ResumeAll => {
                    daemon.send(Command::ResumeAll);
                }
                tray::Command::ToggleSpeedLimit => {
                    let settings = lock(&watch).settings.clone();
                    if let Some(s) = settings {
                        let on = !s.speed_limit_enabled;
                        tray::set_speed_limit_checked(on);
                        daemon.send(Command::SetSettings { settings: Settings { speed_limit_enabled: on, ..s } });
                    }
                }
                tray::Command::ToggleDropTarget => {
                    let settings = lock(&watch).settings.clone();
                    if let Some(s) = settings {
                        daemon.send(Command::SetSettings { settings: Settings { drop_target: !s.drop_target, ..s } });
                    }
                }
                // The daemon keeps downloading and exits on its own once idle.
                tray::Command::Exit => std::process::exit(0),
            }
            ctx.request_repaint();
        });
        let shown = Arc::clone(&self.tooltip_now);
        let tooltip = Arc::new(move || lock(&shown).clone());
        let rgba = self.layout.tray_style.apply(ICON_RGBA);
        if let Err(e) = tray::install(rgba, ICON_SIZE, on_command, tooltip) {
            state::apply(&mut self.state, UiMessage::Error(trf!("No tray icon: {error}", error = e)));
        }
    }

    /// Connect on a background thread, then read events until the daemon goes.
    fn connect(&mut self) {
        self.connect_with(false);
    }

    /// Connect, first restarting the daemon if `restart`.
    fn connect_with(&mut self, restart: bool) {
        let (watch, outbox, ctx) = (Arc::clone(&self.watch), self.outbox.clone(), self.ctx.clone());
        state::apply(&mut self.state, UiMessage::Connecting);
        self.daemon.connect(
            restart,
            Box::new(move |event| {
                let need = {
                    let mut w = lock(&watch);
                    w.observe(&event);
                    w.need(&event)
                };
                let _ = outbox.send(UiMessage::Daemon(event));
                match need {
                    Need::Nothing => {}
                    // A click on a notification brings the window up; a
                    // dialog shows on its own, without it.
                    Need::Show => win::show(),
                    Need::Wake => win::wake(),
                    Need::Quit => std::process::exit(0),
                    Need::WakeIfStillFailed(id) => {
                        let (watch, outbox, ctx) = (Arc::clone(&watch), outbox.clone(), ctx.clone());
                        thread::spawn(move || {
                            thread::sleep(FAILED_GRACE);
                            if lock(&watch).still_failed(id) {
                                let _ = outbox.send(UiMessage::FailedSettled(id));
                                win::wake();
                                ctx.request_repaint();
                            }
                        });
                    }
                }
                ctx.request_repaint();
            }),
        );
    }

    fn perform(&mut self, action: Action) {
        match action {
            Action::Add(url) => {
                self.daemon.send(Command::Add(Download { url, ..Download::default() }));
            }
            Action::CancelSaveAs(id) => {
                self.daemon.send(Command::Cancel { id });
                let (entry, settings) = {
                    let s = &self.state;
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
                let settings = self.state.settings.clone();
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
                self.daemon.send(Command::Cancel { id });
            }
            Action::Retry(id) => {
                self.daemon.send(Command::Retry { id });
            }
            Action::Redownload(id) => {
                self.daemon.send(Command::Redownload { id });
            }
            Action::Remove(ids) => self.ask_remove(ids),
            Action::RemoveNow { ids, delete_files, dont_ask } => {
                for id in &ids {
                    self.daemon.send(Command::Remove { id: *id, delete_files });
                    self.selected.remove(id);
                }
                let settings = self.state.settings.clone();
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
                    let s = &self.state;
                    s.table.entries().filter(|e| e.status == Status::Done).map(|e| e.id).collect()
                };
                self.ask_remove(done);
            }
            Action::Open(id) => {
                let entry = self.state.table.get(id).cloned();
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
                state::apply(&mut self.state, UiMessage::Forget(Forget::Completed));
                // "Don't show this dialog again": Windows notifications from
                // now on, and the dialogs still waiting go too.
                if std::mem::take(&mut self.hide_completed) {
                    state::apply(&mut self.state, UiMessage::Forget(Forget::AllCompleted));
                    if let Some(mut settings) = self.state.settings.clone() {
                        settings.notify = Notify::Toast;
                        self.daemon.send(Command::SetSettings { settings });
                    }
                }
            }
            Action::OpenWith(path) => win::open_with(&path),
            Action::OpenAbout => self.about = Some(false),
            Action::OpenConvert(id) => self.convert = Some((id, uidm_domain::media::Target::Mp3, false)),
            Action::Convert(id, to, delete_original) => {
                self.daemon.send(Command::Convert { id, to, delete_original });
            }
            Action::InstallUpdate => {
                state::apply(&mut self.state, UiMessage::InstallStarted);
                self.about = Some(true);
                self.daemon.send(Command::InstallUpdate);
            }
            Action::CheckUpdate => {
                state::apply(&mut self.state, UiMessage::UpdateCheckStarted);
                self.about = Some(true);
                self.daemon.send(Command::CheckUpdate);
            }
            Action::PreviewZip(id) => {
                self.zip_preview = Some(ZipPreview { id, result: None });
                self.daemon.send(Command::PreviewZip { id });
            }
            Action::ShowInFolder(path) => {
                crate::ports::ports().opener.reveal(&path);
            }
            Action::Reconnect => self.connect(),
            Action::RestartDaemon => {
                state::apply(&mut self.state, UiMessage::DaemonRestarting);
                self.connect_with(true);
            }
            Action::OpenOptions => {
                if let Some(settings) = &self.state.settings {
                    let added = settings.added_browsers.clone();
                    self.options = Some(OptionsDraft::new(settings));
                    let (outbox, ctx) = (self.outbox.clone(), self.ctx.clone());
                    thread::spawn(move || {
                        let _ = outbox.send(UiMessage::BrowsersRead(read_browsers(&added)));
                        ctx.request_repaint();
                    });
                }
            }
            Action::OpenLogin(id) => {
                state::apply(&mut self.state, UiMessage::LoginAsked(id));
            }
            Action::AddDownload(download) => {
                self.daemon.send(Command::Add(download));
            }
            Action::SaveSettings(settings) => {
                state::apply(&mut self.state, UiMessage::SavingSettings);
                if !self.daemon.send(Command::SetSettings { settings }) {
                    state::apply(&mut self.state, UiMessage::SaveFailed(tr("not connected to the UIDM daemon").into()));
                }
            }
            Action::QuickSettings(settings) => {
                self.daemon.send(Command::SetSettings { settings });
            }
            Action::GrabOptions { add_on_close, save_unique_silently } => {
                let settings = self.state.settings.clone();
                if let Some(s) = settings {
                    self.perform(Action::QuickSettings(Settings {
                        grab_add_on_close: add_on_close,
                        grab_save_unique_silently: save_unique_silently,
                        ..s
                    }));
                }
            }
            Action::Confirm(answer) => {
                self.daemon.send(Command::Confirm(answer));
            }
            Action::StartQueue(id) => {
                self.daemon.send(Command::StartQueue { id });
            }
            Action::StopQueue(id) => {
                self.daemon.send(Command::StopQueue { id });
            }
            Action::SaveQueue(queue) => {
                self.daemon.send(Command::SaveQueue { queue });
            }
            Action::DeleteQueue(id) => {
                if self.filter == Filter::Queue(id) {
                    self.filter = Filter::default();
                }
                self.daemon.send(Command::DeleteQueue { id });
            }
            Action::MoveToQueue { id, queue, position } => {
                self.daemon.send(Command::MoveToQueue { id, queue, position });
            }
            Action::RemoveFromQueue(id) => {
                self.daemon.send(Command::RemoveFromQueue { id });
            }
            Action::SetQueueOnce(id, when_done) => {
                self.daemon.send(Command::SetQueueOnce { id, when_done });
            }
            Action::OpenProperties(id) => {
                let entry = self.state.table.get(id).cloned();
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
                // One already open comes back from the tray, or to the front.
                match self.progress_windows.iter_mut().find(|w| w.id == id) {
                    Some(w) if w.hidden => w.restore(),
                    Some(w) => {
                        win::raise_window(&w.title);
                    }
                    None => self.open_download_window(id, true, false),
                }
            }
            Action::HideProgressTab(tab) => {
                let settings = self.state.settings.clone();
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
                let settings = self.state.settings.clone();
                if let Some(s) = settings {
                    self.perform(Action::QuickSettings(Settings { schedule_note_hidden: true, ..s }));
                }
            }
            Action::SetOnDone(id, on_done) => {
                self.daemon.send(Command::SetOnDone { id, on_done });
            }
            Action::OpenSpeedLimit(id) => {
                let entry = self.state.table.get(id).cloned();
                if let Some(e) = entry {
                    let kbps = e.speed_limit.map_or(512, |b| (b / 1024).max(1));
                    let unit = if kbps >= 1024 && kbps.is_multiple_of(1024) { SpeedUnit::Mb } else { SpeedUnit::Kb };
                    self.limit_dialog = Some(LimitDialog {
                        id,
                        enabled: e.speed_limit.is_some(),
                        value: kbps as f64 / unit.kb(),
                        unit,
                        remember: e.limit_remember,
                    });
                }
            }
            Action::SetSpeedLimit(id, bytes_per_sec, remember) => {
                self.daemon.send(Command::SetSpeedLimit { id, bytes_per_sec, remember: Some(remember) });
            }
            Action::SetProperties { id, changes } => {
                self.daemon.send(Command::SetProperties { id, changes });
            }
            Action::OpenRefresh(id) => {
                state::apply(&mut self.state, UiMessage::Forget(Forget::RefreshMismatch));
                self.refresh = Some((id, String::new()));
            }
            Action::RefreshWith { id, url, restart } => {
                state::apply(&mut self.state, UiMessage::Forget(Forget::RefreshMismatch));
                self.daemon.send(Command::RefreshLink {
                    id,
                    download: Some(Download { url, ..Download::default() }),
                    restart,
                });
            }
            Action::RefreshFromBrowser(id) => {
                self.daemon.send(Command::RefreshLink { id, download: None, restart: false });
            }
            Action::CancelRefresh => {
                self.daemon.send(Command::CancelRefresh);
            }
            Action::CancelCountdown => {
                self.daemon.send(Command::CancelCountdown);
            }
            Action::CountdownNow => {
                self.daemon.send(Command::CountdownNow);
            }
            Action::OpenQueues(select) => {
                let list = self.state.queues.clone();
                self.queues_dialog = Some(queues::QueuesDialog::open(&list, select));
            }
            Action::AddMany { items, queue, start_queue } => {
                // Addresses in the list and taken names may need answers
                // first, when Options says to ask.
                // Which names are taken is read from disk, so on a worker.
                let (rows, settings) = {
                    let s = &self.state;
                    (s.table.entries().cloned().collect::<Vec<_>>(), s.settings.clone().unwrap_or_default())
                };
                let mut asking = Pending::default();
                asking.start(&self.ctx, move || {
                    (answers::Prompt::new(items, queue, start_queue, &rows, &settings), queue, start_queue)
                });
                self.asking.push(asking);
            }
            Action::OpenBatch => self.batch = Some(tools::BatchDialog::new()),
            Action::BatchFromClipboard => {
                let links = uidm_domain::batch::links_in(&win::clipboard_text().unwrap_or_default());
                if links.is_empty() {
                    state::apply(
                        &mut self.state,
                        UiMessage::Error(tr("The clipboard holds no http, https or ftp links.").into()),
                    );
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
                    self.grabber =
                        Some(tools::GrabDialog::new(&self.state.grab_template, self.state.grab_filters.clone()));
                }
            }
            Action::OpenGrabProject(name) => {
                // Read on a worker; `open_grab_project` opens it once it is.
                let (outbox, ctx) = (self.outbox.clone(), self.ctx.clone());
                thread::spawn(move || {
                    let read = match crate::ports::ports().grab.as_deref() {
                        Some(lib) => {
                            lib.load(&name).map_err(|e| trf!("Could not open {name}: {error}", name = name, error = e))
                        }
                        None => Err(tr("There is no projects folder.").to_string()),
                    };
                    if outbox.send(UiMessage::GrabProjectRead(read)).is_ok() {
                        ctx.request_repaint();
                    }
                });
            }
            Action::Grab(req) => {
                state::apply(&mut self.state, UiMessage::Forget(Forget::GrabEvents));
                self.daemon.send(Command::Grab(req));
            }
            Action::ProbeLinks { request, links } => {
                self.daemon.send(Command::ProbeLinks { request, links });
            }
            Action::StopProbeLinks => {
                self.daemon.send(Command::StopProbeLinks);
            }
            Action::CancelGrab => {
                self.daemon.send(Command::CancelGrab);
            }
            Action::Click { id, ctrl, shift } => {
                let at = |x: u64| self.order.iter().position(|i| *i == x);
                match self.anchor.and_then(at).zip(at(id)).filter(|_| shift) {
                    Some((from, to)) => {
                        if !ctrl {
                            self.selected.clear();
                        }
                        let (lo, hi) = (from.min(to), from.max(to));
                        self.selected.extend(self.order[lo..=hi].iter().copied());
                    }
                    None => {
                        if !ctrl {
                            self.selected.clear();
                            self.selected.insert(id);
                        } else if !self.selected.remove(&id) {
                            self.selected.insert(id);
                        }
                        self.anchor = Some(id);
                    }
                }
            }
        }
    }

    /// The Batch, Export and site grabber dialogs, and the clipboard offer.
    fn tool_windows(
        &mut self,
        ctx: &egui::Context,
        rows: &[DownloadEntry],
        connected: bool,
        actions: &mut Vec<Action>,
    ) {
        let queue_list = self.state.queues.clone();
        if let Some(read) = self.import.lock().unwrap().take() {
            let opened = read.and_then(|(name, text)| {
                crate::ports::ports()
                    .lists
                    .import(&text)
                    .map_err(|e| e.0)
                    .map(|(items, rejected)| tools::BatchDialog::imported(name, items, rejected))
            });
            match opened {
                Ok(d) => self.batch = Some(d),
                Err(e) => {
                    state::apply(&mut self.state, UiMessage::Error(e));
                }
            }
        }
        if let Some(d) = self.batch.as_mut() {
            let queue_panel = self.state.settings.as_ref().is_none_or(|s| s.list_queue_panel);
            if !tools::batch_dialog(ctx, d, &queue_list, queue_panel, connected, actions) {
                self.batch = None;
            }
        }
        self.selected.retain(|id| rows.iter().any(|e| e.id == *id));
        if let Some(d) = self.export.as_mut() {
            if !tools::export_dialog(ctx, d, rows, &self.selected, &queue_list) {
                self.export = None;
            }
        }
        let events: Vec<GrabEvent> = state::take_grab_events(&mut self.state);
        if let Some(d) = self.grabber.as_mut() {
            for event in events {
                match event {
                    GrabEvent::Progress(pages, bytes, found, explored, added) => {
                        d.progress(pages, bytes, found, explored, added)
                    }
                    GrabEvent::Done(files, pages, explored, stopped, cookies) => {
                        d.done(files, pages, explored, stopped, cookies)
                    }
                }
            }
            if !connected && d.running() {
                d.failed(tr("The connection to the UIDM daemon was lost.").into());
            }
            let settings = self.state.settings.clone().unwrap_or_default();
            let lists = tools::GrabLists {
                queues: &queue_list,
                projects: &self.state.grab_projects,
                templates: &self.state.grab_templates,
                template: &self.state.grab_template,
                filters: &self.state.grab_filters,
            };
            if !tools::grab_dialog(ctx, d, &lists, &settings, rows, connected, actions) {
                self.grabber = None;
            }
        }
        if state::take_clipboard_batch(&mut self.state) {
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
        let offer = self.state.clipboard.clone();
        if let Some(url) = offer {
            if !tools::clipboard_offer(ctx, &url, connected, actions) {
                state::apply(&mut self.state, UiMessage::Forget(Forget::ClipboardOffer));
            }
        }
    }

    /// The login prompt for the oldest download waiting for a login, with
    /// the others from the same site.
    fn login_window(
        &mut self,
        ctx: &egui::Context,
        rows: &[DownloadEntry],
        connected: bool,
        actions: &mut Vec<Action>,
    ) {
        let settings = self.state.settings.clone();
        if self.login.is_none() {
            let queued: Vec<u64> = state::take_login_wanted(&mut self.state);
            // Only those still failed for a login.
            let waiting: Vec<(&DownloadEntry, uidm_domain::download::LoginWanted)> = queued
                .iter()
                .filter_map(|id| rows.iter().find(|e| e.id == *id && e.status == Status::Failed))
                .filter_map(|e| e.error.as_deref().and_then(uidm_domain::download::login_wanted).map(|w| (e, w)))
                .collect();
            if let Some((first, wanted)) = waiting.first().cloned() {
                let (same, other): (Vec<_>, Vec<_>) = waiting.iter().partition(|(_, w)| w.host == wanted.host);
                state::apply(&mut self.state, UiMessage::LoginsWaiting(other.iter().map(|(e, _)| e.id).collect()));
                let own_site =
                    same.iter().all(|(e, _)| uidm_domain::site::host_of(&e.url).eq_ignore_ascii_case(&wanted.host));
                let username = settings
                    .as_ref()
                    .and_then(|s| s.site_logins.iter().find(|l| l.site.eq_ignore_ascii_case(&wanted.host)))
                    .map(|l| l.username.clone())
                    .or_else(|| first.login_user.clone())
                    .unwrap_or_default();
                let ids = same.iter().map(|(e, _)| e.id).collect();
                let mut prompt = login::LoginPrompt::new(ids, wanted, own_site, username);
                prompt.plain = same.iter().any(|(e, _)| !e.url.to_ascii_lowercase().starts_with("https:"));
                self.login = Some(prompt);
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
        self.daemon.send(Command::AddMany { items, queue: Some(queue) });
        // Sent after on the same connection, so it starts with them.
        if start_queue {
            self.daemon.send(Command::StartQueue { id: queue });
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
            let s = &self.state;
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
                start_queue: false,
                new_queue: None,
                pending_queue: None,
                write_check: None,
                elevate: false,
                category,
                filename,
                folder: path.and_then(|p| p.parent()).map(|p| p.display().to_string()).unwrap_or_default(),
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
        // Select a queue "+" created, once the daemon lists it.
        if let Some((name, sent)) = &draft.pending_queue {
            if let Some(id) = queues::named(&queue_list, name) {
                draft.later_queue = id;
                draft.pending_queue = None;
            } else if !connected || sent.elapsed() > PENDING_QUEUE_WAIT {
                // The daemon refused it or the link dropped: open the name
                // box again with the name, so "+" never stays stuck.
                let name = name.clone();
                draft.pending_queue = None;
                draft.new_queue = Some((name, Some(tr("UIDM did not add the queue. Try again.").to_string())));
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
        }
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
        let replaced = other.filter(|_| draft.duplicate == Duplicate::Overwrite).and_then(|o| o.path.as_deref());
        // Under Ask, a name taken by a file in the folder needs an answer,
        // unless the duplicate question already covers it.
        let name = uidm_domain::filename::sanitize_filename(draft.filename.trim());
        let taken = ask
            && other.is_none()
            && !name.is_empty()
            && crate::ports::ports().files.is_file(&Path::new(draft.folder.trim()).join(&name));
        // A folder UIDM may not write to needs administrator rights, or
        // another folder.
        let folder = draft.folder.trim().to_string();
        let denied = match &draft.write_check {
            Some((checked, denied)) if *checked == folder => *denied,
            _ => {
                let denied = Path::new(&folder).is_absolute() && !crate::ports::ports().can_write(Path::new(&folder));
                draft.write_check = Some((folder, denied));
                denied
            }
        };
        let blocked = denied && !draft.elevate;

        crate::dialog::Window::new(tr("Save download"))
            .id(egui::Id::new("confirm"))
            .opening(crate::dialog::Opening::OnTop)
            .taskbar(true)
            .dlu_width(380.0)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::Grid::new("confirm_grid").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                    ui.label(tr("Address"));
                    ui.add(egui::Label::new(&entry.url).truncate()).on_hover_text(&entry.url);
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
                            let busy =
                                self.zip_preview.as_ref().is_some_and(|p| p.id == entry.id && p.result.is_none());
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
                                draft.folder = settings.category_folder(&draft.category, &crate::ports::ports().downloads_dir()).display().to_string();
                                draft.remember_folder = settings.remembers_folder(&draft.category);
                            }
                            let add = ui
                                .add_enabled(
                                    connected && loaded.is_some() && draft.new_category.is_none(),
                                    egui::Button::new("+"),
                                )
                                .on_hover_text(tr("Add a category"));
                            if add.clicked() {
                                draft.new_category = Some(NewCategory {
                                    extensions: uidm_domain::settings::extension_of(draft.filename.trim()).unwrap_or_default(),
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
                        Status::Canceled => {
                            trf!("This address is already in the list as {name} (canceled).", name = name)
                        }
                        Status::Interrupted => {
                            trf!("This address is already in the list as {name} (interrupted).", name = name)
                        }
                        Status::Waiting => {
                            trf!("This address is already in the list as {name} (waiting).", name = name)
                        }
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
                    ui.checkbox(&mut draft.remember_duplicate, tr("Remember my selection")).on_hover_text(tr(
                        "Answer the same way for every address already in the list; Options can change it",
                    ));
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
                if denied {
                    ui.add_space(4.0);
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        tr("UIDM may not save files in this folder. Saving there needs administrator rights."),
                    );
                    ui.horizontal(|ui| {
                        if crate::ports::ports().system.can_elevate() {
                            ui.checkbox(&mut draft.elevate, tr("Save there with administrator rights"))
                                .on_hover_text(tr(
                                    "The download runs as you; Windows asks once it is done, before the file is moved there",
                                ));
                        }
                        browse_button(ui, tr("Save to"), &draft.folder, &draft.picked);
                    });
                }
                if waiting > 1 {
                    ui.weak(trf!("{count} more downloads are waiting after this one.", count = waiting - 1));
                }
                // Unless Download later has a queue of its own, ask which,
                // even with only the main queue, so "+" can add another.
                let later_set = settings.later_queue.is_some_and(|id| queue_list.iter().any(|q| q.id == id));
                if !later_set {
                    ui.horizontal(|ui| {
                        ui.label(tr("Download later in"));
                        let name = queue_list
                            .iter()
                            .find(|q| q.id == draft.later_queue)
                            .map_or(tr("Main queue"), |q| q.name.as_str());
                        egui::ComboBox::from_id_salt("later_queue").selected_text(name).show_ui(ui, |ui| {
                            for q in &queue_list {
                                ui.selectable_value(&mut draft.later_queue, q.id, &q.name);
                            }
                        });
                        let add = ui
                            .add_enabled(
                                connected && draft.new_queue.is_none() && draft.pending_queue.is_none(),
                                egui::Button::new("+"),
                            )
                            .on_hover_text(tr("Add a queue"));
                        if add.clicked() {
                            draft.new_queue = Some((String::new(), None));
                        }
                        ui.checkbox(&mut draft.remember_queue, tr("Don't ask me again"))
                            .on_hover_text(tr("Download later uses this queue from now on; Options can change it"));
                    });
                    // The name box "+" opened; the queue is saved at once and
                    // chosen for this download when the daemon lists it.
                    let mut close_new = false;
                    if let Some((name, error)) = draft.new_queue.as_mut() {
                        ui.horizontal(|ui| {
                            ui.label(tr("Queue name"));
                            let edit = ui.add(egui::TextEdit::singleline(name).desired_width(180.0));
                            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if ui.add_enabled(connected, egui::Button::new(tr("Add"))).clicked() || (enter && connected)
                            {
                                match queues::new_named(name, &queue_list) {
                                    Ok(q) => {
                                        draft.pending_queue = Some((q.name.clone(), std::time::Instant::now()));
                                        actions.push(Action::SaveQueue(q));
                                        close_new = true;
                                    }
                                    Err(e) => *error = Some(e),
                                }
                            }
                            if ui.button(tr("Cancel")).clicked() {
                                close_new = true;
                            }
                        });
                        if let Some(e) = error {
                            ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                        }
                    }
                    if close_new {
                        draft.new_queue = None;
                    }
                    if let Some((name, _)) = &draft.pending_queue {
                        ui.weak(trf!("Adding the queue {name}...", name = name));
                    }
                }
                ui.checkbox(&mut draft.start_queue, tr("Start the queue now"))
                    .on_hover_text(tr("With Download later, start the queue it waits in at once"));
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
                                elevate: denied && draft.elevate,
                            })
                        };
                        if other.is_some() && draft.duplicate == Duplicate::Existing {
                            if ui.button(tr("OK")).clicked() {
                                actions.push(answer(true));
                            }
                        } else {
                            let hint = tr("Choose another folder, or save there with administrator rights");
                            if ui.add_enabled(!blocked, egui::Button::new(tr("Start download"))).on_disabled_hover_text(hint).clicked() {
                                actions.push(answer(true));
                            }
                            if ui.add_enabled(!blocked, egui::Button::new(tr("Download later"))).on_disabled_hover_text(hint).clicked() {
                                actions.push(answer(false));
                                // Sent after the answer, so the download is
                                // already waiting in the queue it starts.
                                if draft.start_queue {
                                    let queue = match settings.later_queue {
                                        Some(id) if later_set => id,
                                        _ => draft.later_queue,
                                    };
                                    actions.push(Action::StartQueue(queue));
                                }
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
        crate::dialog::Window::new(tr("New category"))
            .id(egui::Id::new("confirm_new_category"))
            .dlu_width(322.0)
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
                        match loaded
                            .as_ref()
                            .map_or(Err(tr("not connected to the UIDM daemon").to_string()), |s| new.add_to(s))
                        {
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
            draft.folder =
                s.category_folder(&draft.category, &crate::ports::ports().downloads_dir()).display().to_string();
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
        if let Some(read) = state::take_browsers_read(&mut self.state) {
            if let Some(draft) = self.options.as_mut() {
                draft.load_browsers(read);
            }
        }
        // Pick up the daemon's answer to a save.
        let result = state::take_save_result(&mut self.state);
        let saving = self.state.saving;
        let queues = self.state.queues.clone();
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
        draft.add_picked_browser(ctx);
        if let Some(rules) = draft.system_proxy.take() {
            use_system_proxy(draft, rules);
        }
        if let Some((done, result)) = draft.checking.take() {
            draft.registered(done);
            match result {
                Ok(settings) => {
                    draft.error = None;
                    actions.push(Action::SaveSettings(settings));
                }
                Err(e) => draft.error = Some(e),
            }
        }
        if let Some(note) = draft.restart_done.lock().unwrap().take() {
            draft.browser_note = Some(note);
        }
        draft.browser_link = self.state.browser_link;
        take_picked(&draft.ffmpeg_picked, &mut draft.ffmpeg_path);
        // A picked scanner keeps the arguments typed after the program.
        let mut scanner = String::new();
        take_picked(&draft.scanner_picked, &mut scanner);
        if !scanner.is_empty() {
            draft.scanner_command = uidm_domain::command::with_program(&scanner, &draft.scanner_command);
        }
        for (slot, file) in draft.sound_picked.iter().zip(draft.sound_files.iter_mut()) {
            take_picked(slot, file);
        }
        for c in &mut draft.categories {
            take_picked(&c.picked, &mut c.folder);
        }

        let mut open = true;
        let mut close = false;
        crate::dialog::Window::new(tr("Options"))
            .id(egui::Id::new("Options"))
            .dlu_width(282.0)
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
                // Every page is the same height, as on a property sheet, so
                // the dialog keeps its size between tabs; a longer page
                // scrolls, with the tabs and buttons left in place.
                egui::ScrollArea::vertical()
                    .id_salt(("options_page", draft.tab))
                    .min_scrolled_height(OPTIONS_PAGE_HEIGHT)
                    .max_height(OPTIONS_PAGE_HEIGHT)
                    .auto_shrink([false, false])
                    .show(ui, |ui| match draft.tab {
                        OptionsTab::General => general_tab(ui, draft, &queues),
                        OptionsTab::Categories => categories_tab(ui, draft),
                        OptionsTab::Capture => capture_tab(ui, draft),
                        OptionsTab::Video => video_tab(ui, draft),
                        OptionsTab::Notifications => notifications_tab(ui, draft),
                        OptionsTab::Connection => connection_tab(ui, draft),
                    });

                if let Some(err) = &draft.error {
                    ui.add_space(4.0);
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let busy = saving || draft.checking.busy();
                    let save = ui.add_enabled(connected && !busy, egui::Button::new(tr("Save")));
                    if save.clicked() {
                        match draft.to_settings() {
                            Ok(settings) => {
                                let changes = draft.browser_changes();
                                draft.checking.start(ui.ctx(), move || check_and_register(settings, changes));
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

/// The height of an Options page, which scrolls past it.
const OPTIONS_PAGE_HEIGHT: f32 = 420.0;

fn theme_label(t: uidm_domain::settings::Theme) -> &'static str {
    match t {
        uidm_domain::settings::Theme::System => tr("Follow Windows"),
        uidm_domain::settings::Theme::Light => tr("Light"),
        uidm_domain::settings::Theme::Dark => tr("Dark"),
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

        ui.label("");
        ui.add_enabled_ui(draft.limit_enabled, |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut draft.per_file_on, tr("Limit each file to"));
                let (lo, hi) = SPEED_LIMIT_RANGE_KBPS;
                ui.add_enabled(draft.per_file_on, egui::DragValue::new(&mut draft.per_file_kbps).range(lo..=hi).suffix(" KB/s"));
            })
            .response
            .on_hover_text(tr("Downloads with a speed limit of their own keep it instead"));
        });
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut draft.limit_on_startup, tr("Always turn the speed limit on when UIDM starts"));
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
        let created = ui.add_enabled(
            crate::ports::ports().system.can_set_creation_time(),
            egui::Checkbox::new(&mut draft.server_file_date_created, tr("Use the server's date for the file's creation time too")),
        );
        created
            .on_hover_text(tr("Windows and macOS keep a creation time that can be set; Linux does not."))
            .on_disabled_hover_text(tr("This system keeps no creation time that can be set; Windows and macOS do."));
        ui.end_row();

        ui.label("");
        ui.checkbox(
            &mut draft.start_with_windows,
            if cfg!(windows) { tr("Start with Windows, in the tray") } else { tr("Start when I sign in, in the tray") },
        );
        ui.end_row();

        ui.label("");
        ui.add_enabled(cfg!(windows), egui::Checkbox::new(&mut draft.keep_awake, tr("Keep the computer awake while downloading")))
            .on_hover_text(tr("Windows does not go to sleep on its own while a download or a site grab runs. The screen can still turn off."))
            .on_disabled_hover_text(tr("Only on Windows for now"));
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut draft.tray_icon, tr("Show UIDM's icon in the notification area"))
            .on_hover_text(tr("Without it, closing the window quits UIDM; open it again from the Start menu"));
        ui.end_row();

        ui.label("");
        ui.add_enabled(draft.tray_icon, egui::Checkbox::new(&mut draft.close_to_tray, tr("Closing the window keeps UIDM in the tray")));
        ui.end_row();

        ui.label("");
        ui.add_enabled(draft.tray_icon, egui::Checkbox::new(&mut draft.minimize_to_tray, tr("Minimizing the window hides it in the tray")));
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
            ui.checkbox(&mut draft.progress_hide_buttons, tr("Show the Hide tab buttons on those tabs"));
            ui.horizontal(|ui| {
                ui.label(tr("Open it on"));
                egui::ComboBox::from_id_salt("progress_start_tab")
                    .selected_text(tr(progress_tab_label(draft.progress_start_tab)))
                    .show_ui(ui, |ui| {
                        for t in ProgressTab::ALL {
                            ui.selectable_value(&mut draft.progress_start_tab, t, tr(progress_tab_label(t)));
                        }
                    })
                    .response
                    .on_hover_text(tr("A hidden tab opens Download status instead"));
            });
            ui.checkbox(&mut draft.progress_start_details, tr("Open it with its details shown"));
        });
        ui.end_row();

        ui.label(tr("Adding many"));
        ui.checkbox(&mut draft.list_queue_panel, tr("Show the queue panel in the batch download and link list dialogs"))
            .on_hover_text(tr("Off, their downloads go to the main queue without starting it"));
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut draft.check_updates, tr("Look for a newer UIDM on GitHub once a week"))
            .on_hover_text(tr("Only a notice; Update now in About downloads and runs the new Setup"));
        ui.end_row();

        ui.label(tr("Theme"));
        egui::ComboBox::from_id_salt("theme")
            .selected_text(theme_label(draft.theme))
            .show_ui(ui, |ui| {
                for t in uidm_domain::settings::Theme::ALL {
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
            let asked = draft.ffmpeg_path.trim().to_string();
            if draft.ffmpeg_asked.as_deref() != Some(asked.as_str()) {
                draft.ffmpeg_asked = Some(asked.clone());
                let (slot, ctx) = (Arc::clone(&draft.ffmpeg_found), ui.ctx().clone());
                thread::spawn(move || {
                    let found = crate::ports::ports().checks.ffmpeg(&Settings { ffmpeg_path: asked.clone(), ..Settings::default() });
                    *slot.lock().unwrap() = Some((asked, found));
                    ctx.request_repaint();
                });
            }
            let found = draft.ffmpeg_found.lock().unwrap().clone();
            match found.filter(|(path, _)| draft.ffmpeg_asked.as_deref() == Some(path.as_str())) {
                Some((_, Some(path))) => {
                    ui.weak(trf!("Using {path}", path = path.display()));
                }
                Some((_, None)) => {
                    ui.colored_label(ui.visuals().warn_fg_color, tr("Not found."));
                }
                // Still looking.
                None => {}
            }
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
                    draft.system_proxy.start(ui.ctx(), || crate::ports::ports().system.fixed_proxy());
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
        ui.label(tr("SOCKS also for"));
        ui.add_enabled_ui(manual, |ui| {
            ui.vertical(|ui| {
                ui.checkbox(&mut draft.proxy_socks_https, tr("https addresses, even with an HTTP or HTTPS proxy set"))
                    .on_hover_text(tr("So plain http goes through the HTTP proxy and https through SOCKS"));
                ui.checkbox(&mut draft.proxy_socks_ftp, tr("ftp:// downloads, when no FTP proxy is set"))
                    .on_hover_text(tr("Data then always uses passive mode, since the server cannot connect back through the proxy"));
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
    egui::Grid::new("logins").num_columns(5).spacing([8.0, 6.0]).show(ui, |ui| {
        for (i, l) in draft.logins.iter_mut().enumerate() {
            ui.add_sized([180.0, 20.0], egui::TextEdit::singleline(&mut l.site).hint_text("example.com"))
                .on_hover_text(tr("A server, or one of its folders such as example.com/private/*. The longest match wins."));
            ui.add_sized([120.0, 20.0], egui::TextEdit::singleline(&mut l.username).hint_text(tr("User name")));
            let hint = if l.new { tr("Password") } else { tr("Password (unchanged)") };
            ui.add_sized([150.0, 20.0], egui::TextEdit::singleline(&mut l.password).password(true).hint_text(hint));
            ui.checkbox(&mut l.plain, tr("Plain connections")).on_hover_text(tr(
                "Also send it over plain http and to FTP servers, where it can be read on the way. Off, it goes only over https.",
            ));
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
            plain: false,
        });
    }
}

/// Fill the manual proxies from the Windows proxy setting `rules`, which a
/// worker read, as IDM's Get System does. Windows' SOCKS proxies speak
/// SOCKS4. Its configuration script is not read: it picks a proxy per
/// address.
fn use_system_proxy(draft: &mut OptionsDraft, rules: Option<uidm_application::window::SystemProxy>) {
    let Some(rules) = rules else {
        draft.error = Some(
            if cfg!(windows) {
                tr("Windows has no proxy server turned on.")
            } else {
                tr("No proxy is set in http_proxy, https_proxy or all_proxy.")
            }
            .to_string(),
        );
        return;
    };
    draft.proxy_mode = ProxyMode::Manual;
    draft.proxy_http = rules.http.clone().unwrap_or_default();
    // An HTTPS proxy the same as the HTTP one is left to fall back to it.
    draft.proxy_https = rules.https.clone().filter(|s| rules.http.as_ref() != Some(s)).unwrap_or_default();
    draft.proxy_ftp = rules.ftp.clone().unwrap_or_default();
    draft.proxy_socks5 =
        rules.socks.as_deref().map(|s| s.split_once("://").map_or(s, |(_, a)| a).to_string()).unwrap_or_default();
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
                    (Sound::QueueStarted, tr("When a queue starts"), &mut draft.sound_on_queue_started),
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
                    ui.add_enabled_ui(*on, |ui| {
                        browse_file_button(ui, tr("Choose a sound"), "wav", &draft.sound_picked[i])
                    });
                    if ui.add_enabled(*on, egui::Button::new(tr("Play"))).clicked() {
                        let file = draft.sound_files[i].trim();
                        crate::ports::ports().system.play(sound, (!file.is_empty()).then(|| Path::new(file)));
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
                ui.add_sized([160.0, 20.0], egui::TextEdit::singleline(&mut c.extensions).hint_text("zip, rar"));
                ui.horizontal(|ui| {
                    let hint = trf!("Download folder\\{name}", name = c.name.trim());
                    ui.add_sized([180.0, 20.0], egui::TextEdit::singleline(&mut c.folder).hint_text(hint));
                    browse_button(ui, tr("Category folder"), &c.folder, &c.picked);
                });
                ui.add_sized([150.0, 20.0], egui::TextEdit::singleline(&mut c.sites).hint_text(tr("Every site")))
                    .on_hover_text(tr("Such as *.example.com, separated by commas"));
                ui.checkbox(&mut c.remember_folder, tr("Last folder")).on_hover_text(tr(
                    "Save new files to the folder last chosen for this category in the save-as dialog",
                ));
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
    ui.horizontal(|ui| {
        ui.label(trf!("\"{general}\" file types", general = GENERAL));
        ui.add_sized(
            [260.0, 20.0],
            egui::TextEdit::singleline(&mut draft.general_extensions).hint_text(tr("Only what no category takes")),
        )
        .on_hover_text(tr("These types go to the download folder even when a category lists them"));
    });
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
    ui.horizontal_wrapped(|ui| {
        ui.weak(tr("These rules decide which browser downloads UIDM takes. The rest stay in the browser."));
        ui.hyperlink_to(tr("How capture rules work"), help::CAPTURE);
    });
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
                draft.capture_types = uidm_domain::settings::default_capture_types().join(", ");
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
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut draft.capture_excluded_urls)
                        .hint_text(tr("One address per line; * matches anything, as in https://example.com/files/*"))
                        .desired_rows(3)
                        .desired_width(360.0),
                );
                if ui
                    .button(tr("Restore defaults"))
                    .on_hover_text(tr("Add back the site sounds and update files UIDM leaves out as it starts, keeping your own addresses"))
                    .clicked()
                {
                    let mut list: Vec<String> = draft
                        .capture_excluded_urls
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(String::from)
                        .collect();
                    uidm_domain::settings::add_missing_exclusions(&mut list);
                    draft.capture_excluded_urls = list.join("
");
                }
            });
            ui.checkbox(
                &mut draft.offer_exceptions,
                tr("When I cancel the same download twice, offer to stop taking it"),
            );
        });
        ui.end_row();

        ui.label(tr("Browsers"));
        ui.vertical(|ui| {
            ui.weak(tr("Capture downloads from these browsers. Each needs UIDM's extension; restart a browser after a change."));
            if let Some((connected, last_seen)) = draft.browser_link {
                ui.label(browser_link_text(connected, last_seen, crate::ports::ports().unix_now()));
            }
            if draft.browsers_loaded && draft.browsers.iter().all(|b| b.removed) {
                ui.label(tr("No browser was found."));
            }
            let mut toggled = None;
            for (i, b) in draft.browsers.iter_mut().enumerate().filter(|(_, b)| !b.removed) {
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut b.on, &b.name).changed() {
                        toggled = Some(i);
                    }
                    let can_restart = b.can_restart();
                    let restart = ui.add_enabled(can_restart, egui::Button::new(tr("Restart")).small());
                    let restart = if can_restart {
                        restart.on_hover_text(tr("Close this browser and start it again, so it sees a change"))
                    } else {
                        restart.on_disabled_hover_text(tr("UIDM does not know this browser's program; add it with Add browser... to restart it"))
                    };
                    if restart.clicked() {
                        draft.restart_ask = Some(i);
                    }
                    if b.added.is_some() && ui.small_button(tr("Remove")).clicked() {
                        b.removed = true;
                    }
                });
                if let Some(a) = &b.added {
                    let note = if !b.known {
                        trf!("{program}; registered where {place} looks, which most browsers of its kind read", program = a.program, place = b.browser.name)
                    } else if b.browser.name != b.name {
                        trf!("{program}; uses {place}'s registration", program = a.program, place = b.browser.name)
                    } else {
                        a.program.clone()
                    };
                    ui.indent(("added_browser", i), |ui| ui.weak(note));
                }
            }
            // Browsers sharing a place are registered together, so their
            // boxes move together.
            if let Some(i) = toggled {
                let (place, on) = (draft.browsers[i].browser.clone(), draft.browsers[i].on);
                for b in draft.browsers.iter_mut().filter(|b| b.browser.same_place(&place)) {
                    b.on = on;
                }
            }
            if !draft.chrome_readers.is_empty() {
                ui.weak(trf!("{browsers} use Google Chrome's setting.", browsers = draft.chrome_readers.join(", ")));
            }
            if ui.button(tr("Add browser...")).on_hover_text(tr("Choose the program of a browser that is not in the list")).clicked() {
                let (slot, ctx) = (Arc::clone(&draft.browser_picked), ui.ctx().clone());
                thread::spawn(move || {
                    if let Some(file) = crate::ports::ports().pickers.pick_file(tr("Add browser"), &[], None) {
                        *slot.lock().unwrap() = Some(file);
                        ctx.request_repaint();
                    }
                });
            }
            if let Some(i) = draft.restart_ask {
                let name = draft.browsers[i].name.clone();
                ui.horizontal_wrapped(|ui| {
                    ui.label(trf!("Restart {browser}? Unsaved work in it may be lost.", browser = name));
                    if ui.button(tr("Restart")).clicked() {
                        draft.restart_ask = None;
                        if draft.browsers[i].can_restart() {
                            draft.browser_note = Some(trf!("Restarting {browser}...", browser = name));
                            let (done, ctx) = (Arc::clone(&draft.restart_done), ui.ctx().clone());
                            let (slot, program) = (draft.browsers[i].browser.clone(), draft.browsers[i].program());
                            thread::spawn(move || {
                                let restarted = crate::ports::ports().browsers.restart(&slot, program.as_deref());
                                *done.lock().unwrap() = Some(match restarted {
                                    Ok(()) => trf!("{browser} was restarted.", browser = name),
                                    Err(e) => trf!("Could not restart {browser}: {error}", browser = name, error = e),
                                });
                                ctx.request_repaint();
                            });
                        }
                    }
                    if ui.button(tr("Cancel")).clicked() {
                        draft.restart_ask = None;
                    }
                });
            }
            if let Some(note) = &draft.browser_note {
                ui.weak(note);
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

        ui.label(tr("Private windows"));
        ui.checkbox(&mut draft.capture_private, tr("Capture downloads in private windows too"))
            .on_hover_text(tr(
                "Downloads, links and videos in private (incognito) windows go to UIDM as well. The browser must also let the extension in: in Chrome and Edge turn on \"Allow in Incognito\" in the UIDM extension's details, in Firefox \"Run in Private Windows\".",
            ));
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

        ui.label(tr("Web players"));
        ui.vertical(|ui| {
            ui.checkbox(
                &mut draft.video_auto_capture,
                tr("Download these types from web players at once, without the video button:"),
            )
            .on_hover_text(tr("Whole files a page's player loads, each address once per tab; not HLS or DASH streams"));
            ui.add_enabled(
                draft.video_auto_capture,
                egui::TextEdit::singleline(&mut draft.video_auto_types).hint_text("mp4, mp3, flv").desired_width(300.0),
            );
        });
        ui.end_row();

        ui.label(tr("Context menu"));
        ui.vertical(|ui| {
            ui.weak(tr("The items UIDM adds to the browser's menu for links, pages and videos"));
            egui::Grid::new("menu_items").num_columns(3).spacing([12.0, 4.0]).show(ui, |ui| {
                ui.label("");
                ui.strong("Chrome, Edge");
                ui.strong("Firefox");
                ui.end_row();
                for (id, label, chromium, firefox) in &mut draft.menu_items {
                    ui.label(tr(label));
                    ui.checkbox(chromium, "").on_hover_text(tr("Chrome, Edge and other Chromium browsers"));
                    ui.push_id(*id, |ui| ui.checkbox(firefox, ""));
                    ui.end_row();
                }
            });
        });
        ui.end_row();
    });
}

fn progress_tab_label(t: ProgressTab) -> &'static str {
    match t {
        ProgressTab::Status => "Download status",
        ProgressTab::SpeedLimit => "Speed limit",
        ProgressTab::WhenDone => "When done",
    }
}

fn selection_label(b: SelectionButton) -> &'static str {
    match b {
        SelectionButton::Off => tr("Nowhere"),
        SelectionButton::Everywhere => tr("On every site"),
        SelectionButton::Sites => tr("On these sites only"),
    }
}

/// A box for each key of a combination; none ticked turns it off. Only
/// forcing offers Insert and Del, which are no modifiers.
fn key_boxes(ui: &mut egui::Ui, key: &mut CaptureKey, insert: bool) {
    ui.checkbox(&mut key.ctrl, "Ctrl");
    ui.checkbox(&mut key.alt, "Alt");
    ui.checkbox(&mut key.shift, "Shift");
    if insert {
        ui.checkbox(&mut key.insert, "Insert");
        ui.checkbox(&mut key.delete, "Del");
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

/// How long the save-as dialog waits for the daemon to list a queue its "+"
/// added before it gives up on it.
const PENDING_QUEUE_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Apply one daemon message, and wake the window when it needs the user: a
/// save-as dialog, a completion dialog, or a notification click, which also
/// brings the main window up.
/// Open a file with its program, as a double-click does.
fn open_file(path: &str) {
    crate::ports::ports().opener.open(path);
}

/// An http, https or ftp address with no spaces.
/// Running, or about to: what Stop Download stops.
fn is_running(s: Status) -> bool {
    matches!(s, Status::Queued | Status::Downloading | Status::Confirming)
}

/// Waiting, stopped or failed: what Download Now starts.
fn is_startable(s: Status) -> bool {
    matches!(s, Status::Waiting | Status::Failed | Status::Canceled | Status::Interrupted)
}

/// Finished or stopped: what Redownload starts over.
fn is_finished(s: Status) -> bool {
    s.is_terminal() && s != Status::Waiting
}

fn is_download_url(url: &str) -> bool {
    uidm_domain::batch::is_download_url(url) && !url.contains(char::is_whitespace)
}

impl App {
    /// The completion dialog for the oldest finished or failed download.
    fn completed_window(&mut self, ctx: &egui::Context, connected: bool, actions: &mut Vec<Action>) {
        let next = {
            loop {
                let s = &self.state;
                match s.completed.front().copied() {
                    // Removed since: skip it.
                    Some((id, _)) if s.table.get(id).is_none() => {
                        state::apply(&mut self.state, UiMessage::Forget(Forget::Completed));
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
        crate::dialog::Window::new(title)
            .id(egui::Id::new("completed"))
            .opening(crate::dialog::Opening::OnTop)
            .taskbar(true)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let path = e.path.as_deref().map(Path::new);
                ui.horizontal_top(|ui| {
                    // Only Windows drags a file out so far (see `win::drag_file`).
                    if let Some(p) = e.path.as_deref().filter(|_| !failed && cfg!(windows)) {
                        file_icon(ui, p);
                    }
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
                });
                if waiting > 1 {
                    ui.weak(trf!("{count} more after this one.", count = waiting - 1));
                }
                ui.checkbox(&mut self.hide_completed, tr("Don't show this dialog again")).on_hover_text(tr(
                    "Show a Windows notification instead; Options > When done can bring the dialog back",
                ));
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
                        if ui
                            .button(tr("Open with..."))
                            .on_hover_text(tr("Choose the program to open it with"))
                            .clicked()
                        {
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
        crate::dialog::Window::new(tr("Threat found"))
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
                    let open =
                        egui::Button::new(egui::RichText::new(tr("Open anyway")).color(ui.visuals().error_fg_color));
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

/// The completion dialog's file icon, a page with a folded corner, which
/// drags the finished file out to a folder or another program, as IDM's
/// does.
fn file_icon(ui: &mut egui::Ui, path: &str) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(32.0, 36.0), egui::Sense::drag());
    let color = ui.style().interact(&response).fg_stroke.color;
    let page = egui::Rect::from_center_size(rect.center(), egui::vec2(24.0, 30.0));
    let fold = 8.0;
    let stroke = egui::Stroke::new(1.0_f32, color);
    let outline = vec![
        page.left_top(),
        egui::pos2(page.right() - fold, page.top()),
        egui::pos2(page.right(), page.top() + fold),
        page.right_bottom(),
        page.left_bottom(),
    ];
    let painter = ui.painter();
    painter.add(egui::Shape::convex_polygon(outline, ui.visuals().extreme_bg_color, stroke));
    painter.add(egui::Shape::line(
        vec![
            egui::pos2(page.right() - fold, page.top()),
            egui::pos2(page.right() - fold, page.top() + fold),
            egui::pos2(page.right(), page.top() + fold),
        ],
        stroke,
    ));
    let lines = egui::Stroke::new(1.0_f32, color.gamma_multiply(0.5));
    for i in 0..4 {
        let y = page.top() + fold + 4.0 + i as f32 * 4.0;
        painter.hline((page.left() + 5.0)..=(page.right() - 5.0), y, lines);
    }
    let cursor = if response.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab };
    let response = response.on_hover_cursor(cursor).on_hover_text(tr("Drag the file to a folder or another program"));
    table::drag_out(&response, path);
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
        self.drain_inbox();
        self.refresh_library();
        self.open_grab_project();
        self.take_asked_batches();
        // Copy what this frame needs, then draw.
        let (link, rows, speeds, last_error, settings) = {
            let s = &self.state;
            let speeds: HashMap<u64, f64> = s.speeds.iter().map(|(id, sp)| (*id, sp.bytes_per_sec)).collect();
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
        // Minimizing hides it in the tray too, as IDM does; showing it again
        // restores it. Elsewhere hiding is minimizing, so there is nothing to
        // do.
        let minimized = ctx.input(|i| i.viewport().minimized) == Some(true);
        if cfg!(windows)
            && minimized
            && !self.was_minimized
            && tray::shown()
            && settings.as_ref().is_none_or(|s| s.minimize_to_tray)
        {
            win::hide();
        }
        self.was_minimized = minimized;
        let tooltip = self.state.tooltip();
        if tooltip != self.tooltip {
            tray::set_tooltip(tooltip.clone());
            *lock(&self.tooltip_now) = tooltip.clone();
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

        self.menu_bar(ctx, connected, &rows, settings.as_ref(), &mut actions);
        if let Some(d) = self.add_dialog.as_mut() {
            match addurl::show(ctx, d, connected) {
                addurl::Outcome::Open => {}
                addurl::Outcome::Add(download) => {
                    actions.push(Action::AddDownload(*download));
                    self.add_dialog = None;
                }
                addurl::Outcome::Close => self.add_dialog = None,
            }
        }

        // What the window has to say, under the toolbar.
        let (refreshing, notice, quota, release) = {
            let sh = &self.state;
            (sh.refresh_waiting, sh.notice.clone(), sh.quota.clone(), sh.newer_release.clone())
        };
        let notes = banner::Notes {
            connection: match &link {
                Link::Connected => banner::Connection::Up,
                Link::Connecting => banner::Connection::Connecting,
                Link::Lost(why) => banner::Connection::Lost(why),
            },
            refreshing: refreshing.and_then(|id| rows.iter().find(|e| e.id == id)).map(display_name),
            error: last_error.as_deref(),
            stale_daemon: self.state.stale_daemon,
            notice: notice.as_deref(),
            quota: quota.as_deref(),
            // Closed for this version, it stays closed until a newer one.
            release: release.as_ref().filter(|r| self.layout.shows_release(&r.version)).map(|r| banner::Release {
                version: &r.version,
                url: &r.url,
                installable: r.setup.is_some(),
            }),
        };
        if notes.any() {
            let dismissed = egui::TopBottomPanel::top("banner")
                .show_separator_line(false)
                .show(ctx, |ui| {
                    ui.add_space(3.0);
                    let d = banner::show(ui, &notes, connected, &mut actions);
                    ui.add_space(3.0);
                    d
                })
                .inner;
            if dismissed.error {
                state::apply(&mut self.state, UiMessage::Forget(Forget::Error));
            }
            if dismissed.notice {
                state::apply(&mut self.state, UiMessage::Forget(Forget::Notice));
            }
            if let Some(r) = release.filter(|_| dismissed.release) {
                self.layout.dismissed_release = Some(r.version);
                self.layout_unsaved = true;
            }
        }

        // Category of each row, from its file name, and the tree to filter by
        // it. Until the daemon sends its settings the tree uses the defaults.
        let shown_settings = settings.clone().unwrap_or_default();
        let listed: Vec<String> = rows
            .iter()
            .map(|e| shown_settings.listed_under(e.category.as_deref(), &display_name(e), &e.url).to_string())
            .collect();
        let queue_list = self.state.queues.clone();
        // With the panel hidden, nothing is left to pick another view with.
        if !shown_settings.categories_panel {
            self.filter = Filter::default();
        }
        let gone = match &self.filter {
            Filter::Downloads { category: Some(name), .. } => {
                !shown_settings.categories.iter().any(|c| c.name == *name) && !listed.contains(name)
            }
            Filter::Queue(id) => connected && !queue_list.iter().any(|q| q.id == *id),
            Filter::Downloads { category: None, .. } => false,
        };
        if gone {
            self.filter = Filter::default();
        }
        if shown_settings.categories_panel {
            let panel = categories::Panel {
                categories: shown_settings.categories.iter().map(|c| c.name.clone()).collect(),
                listed: &listed,
                queues: &queue_list,
                connected,
                projects: &self.state.grab_projects,
            };
            let width = self.layout.categories_width.unwrap_or(categories::DEFAULT_WIDTH);
            let shown = egui::SidePanel::left("categories")
                .resizable(true)
                .default_width(width)
                .width_range(100.0..=400.0)
                .show(ctx, |ui| categories::show(ui, &panel, &mut self.tree, &mut self.filter, &mut actions));
            if shown.inner.hide {
                self.menu_command(menubar::Command::ToggleCategories, &rows, settings.as_ref(), &mut actions);
            }
            let dragged = shown.response.rect.width();
            if (dragged - width).abs() >= 1.0 {
                self.layout.categories_width = Some(dragged);
                self.layout_unsaved = true;
            }
        }
        let current_queue = match &self.filter {
            Filter::Queue(id) => queue_list.iter().find(|q| q.id == *id).cloned(),
            _ => None,
        };
        let shown: Vec<&DownloadEntry> = match (&self.filter, &current_queue) {
            (Filter::Queue(_), Some(q)) => q.order.iter().filter_map(|id| rows.iter().find(|e| e.id == *id)).collect(),
            _ => rows.iter().zip(&listed).filter(|(e, c)| self.filter.keeps(e, c)).map(|(e, _)| e).collect(),
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
        self.order = shown.iter().map(|e| e.id).collect();
        self.selected.retain(|id| rows.iter().any(|e| e.id == *id));
        if !ctx.wants_keyboard_input() {
            let (all, delete, enter) = ctx.input_mut(|i| {
                (
                    i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::A)),
                    i.key_pressed(egui::Key::Delete),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if all {
                self.selected = self.order.iter().copied().collect();
            }
            if delete {
                self.menu_command(menubar::Command::RemoveTicked, &rows, settings.as_ref(), &mut actions);
            }
            // Like a double-click: a finished file opens, anything else
            // shows its download window.
            let one = rows.iter().find(|e| self.selected.len() == 1 && self.selected.contains(&e.id));
            if let Some(e) = one.filter(|_| enter) {
                actions.push(if e.status == Status::Done { Action::Open(e.id) } else { Action::OpenProgress(e.id) });
            }
        }
        let layout_before = self.layout.clone();
        let finding = self.find.as_ref().is_some_and(|b| b.active());
        if let Some(bar) = self.find.as_mut() {
            let ids: Vec<u64> = shown.iter().map(|e| e.id).collect();
            find::step_keys(ctx, bar, &ids);
            if std::mem::take(&mut self.find_next) {
                bar.step(&ids, false);
            }
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
            }
            // An empty list keeps its headers, with a note under them.
            let empty = shown.is_empty().then(|| {
                if finding {
                    tr("No downloads here match. Press Escape to see them all.")
                } else if current_queue.is_some() {
                    tr("This queue is empty. Use Download Later, or a download's Move to queue menu, to add one.")
                } else if !rows.is_empty() {
                    tr("No downloads in this category.")
                } else {
                    tr("No downloads yet. Choose Tasks > Add new download, or download something in your browser.")
                }
            });
            let area = ui.max_rect();
            let list = table::List {
                shown: &shown,
                speeds: &speeds,
                selected: &self.selected,
                queues: &queue_list,
                queue: current_queue.as_ref(),
                connected,
                found: current,
                scroll,
            };
            if table::show(ui, &list, &mut self.layout, &mut self.table, &mut actions).open_columns {
                self.columns_dialog.get_or_insert_with(|| self.layout.clone());
            }
            if let Some(note) = empty {
                ui.painter().text(
                    area.center(),
                    egui::Align2::CENTER_CENTER,
                    note,
                    egui::TextStyle::Body.resolve(ui.style()),
                    ui.visuals().weak_text_color(),
                );
            }
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
                self.tip = Some(help::TipWindow { index: s.next_tip, on_startup: true });
            }
        }
        if let Some(t) = self.tip.as_mut() {
            if let Some((next_tip, tips_on_startup)) = help::tip_window(ctx, t) {
                self.tip = None;
                if let Some(s) = settings.as_ref().filter(|_| connected) {
                    actions.push(Action::SaveSettings(Settings { next_tip, tips_on_startup, ..s.clone() }));
                }
            }
        }
        self.convert_window(ctx, &rows, connected, &mut actions);
        if let Some(d) = state::take_picked_links(&mut self.state) {
            self.links = Some(d);
        }
        let link_info = state::take_link_info(&mut self.state);
        if let Some(d) = self.links.as_mut() {
            for (request, index, probed) in link_info {
                d.heard(request, index, probed);
            }
            let settings = settings.clone().unwrap_or_default();
            if !links::dialog(ctx, d, &queue_list, &settings, connected, &mut actions) {
                self.links = None;
            }
        }
        let segments = self.state.segments.clone();
        self.connection_dialogs.retain(|id| {
            let entry = rows.iter().find(|e| e.id == *id);
            connections::dialog(ctx, *id, entry, segments.get(id).map(Vec::as_slice))
        });
        let started = state::take_started(&mut self.state);
        let can_hide = tray::shown();
        if settings.as_ref().is_some_and(|s| s.progress_on_start) {
            // A window that opens as its download starts stays behind the
            // program in front; one a queue started opens in the tray, as
            // IDM's do.
            for (id, by_queue) in started {
                self.open_download_window(id, false, by_queue && can_hide);
            }
        }
        let tabs = progress::Tabs {
            speed_limit: settings.as_ref().is_none_or(|s| s.progress_speed_tab),
            on_done: settings.as_ref().is_none_or(|s| s.progress_done_tab),
            hide_buttons: settings.as_ref().is_none_or(|s| s.progress_hide_buttons),
            can_hide,
        };
        // Without the tray, a hidden window could not be found again.
        if !can_hide {
            self.progress_windows.iter_mut().filter(|w| w.hidden).for_each(progress::ProgressWindow::restore);
        }
        for id in tray::take_restored() {
            if let Some(w) = self.progress_windows.iter_mut().find(|w| w.id == id) {
                w.restore();
            }
        }
        self.progress_windows.retain_mut(|w| {
            let entry = rows.iter().find(|e| e.id == w.id);
            let segments = segments.get(&w.id).map(Vec::as_slice);
            progress::window(ctx, w, entry, segments, speeds.get(&w.id).copied(), &tabs, connected, &mut actions)
        });
        // The windows hidden in the tray, for the downloads tray icon.
        let in_tray: Vec<(u64, String)> =
            self.progress_windows.iter().filter(|w| w.hidden).map(|w| (w.id, w.title.clone())).collect();
        if in_tray != self.hidden_windows {
            tray::set_hidden_windows(in_tray.clone(), ctx);
            self.hidden_windows = in_tray;
        }
        self.refresh_window(ctx, &rows, connected, &mut actions);
        self.tool_windows(ctx, &rows, connected, &mut actions);
        if let Some(d) = self.queues_dialog.as_mut() {
            d.schedule_note = !settings.as_ref().is_some_and(|s| s.schedule_note_hidden);
            if !queues::dialog(ctx, d, &queue_list, connected, &mut actions) {
                self.queues_dialog = None;
            }
        }
        let countdown = self.state.countdown.clone();
        if let Some((action, deadline, queue, file)) = countdown {
            if Instant::now() < deadline {
                queues::countdown(ctx, action, deadline, &queue, file.as_deref(), connected, &mut actions);
            } else {
                state::apply(&mut self.state, UiMessage::Forget(Forget::Countdown));
            }
        }
        self.options_window(ctx, connected, &mut actions);
        if let Some(before) = &self.columns_dialog {
            if !columns::dialog(ctx, &mut self.layout, before) {
                self.columns_dialog = None;
            }
        }
        if let Some(draft) = self.font_dialog.as_mut() {
            match columns::font_dialog(ctx, draft) {
                columns::FontChoice::Open => {}
                columns::FontChoice::Cancel => self.font_dialog = None,
                columns::FontChoice::Apply(font) => {
                    self.layout.list_font = Some(font);
                    self.font_dialog = None;
                }
            }
        }
        self.properties.retain_mut(|d| {
            let entry = rows.iter().find(|e| e.id == d.id);
            let queued = queue_list.iter().any(|q| q.order.contains(&d.id));
            properties::dialog(ctx, d, entry, queued, connected, &mut actions)
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
    fn convert_window(
        &mut self,
        ctx: &egui::Context,
        rows: &[DownloadEntry],
        connected: bool,
        actions: &mut Vec<Action>,
    ) {
        use uidm_domain::media::Target;
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
        crate::dialog::Window::new(tr("Convert"))
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
    /// The menu bar: put it on the window, and do what was chosen from it.
    fn menu_bar(
        &mut self,
        ctx: &egui::Context,
        connected: bool,
        rows: &[DownloadEntry],
        settings: Option<&Settings>,
        actions: &mut Vec<Action>,
    ) {
        let queues = self.state.queues.clone();
        let ticked: Vec<Status> = rows.iter().filter(|e| self.selected.contains(&e.id)).map(|e| e.status).collect();
        let state = menubar::State {
            connected,
            settings: settings.map(|s| menubar::Checks {
                drop_target: s.drop_target,
                speed_limit: s.speed_limit_enabled,
                categories: s.categories_panel,
                tray_icon: s.tray_icon,
                dark_mode: s.theme != uidm_domain::settings::Theme::Light,
                language: s.language.clone(),
            }),
            ticked: menubar::Ticked {
                any: ticked.iter().any(|s| s.is_terminal()),
                running: ticked.iter().any(|s| is_running(*s)),
                startable: ticked.iter().any(|s| is_startable(*s)),
                finished: ticked.iter().any(|s| is_finished(*s)),
            },
            queues: queues.iter().map(|q| (q.id, q.name.clone(), q.running)).collect(),
            sort: self.layout.sort,
            toolbar_small: self.layout.toolbar_small,
            list_font: self.layout.list_font.is_some(),
            tray_style: self.layout.tray_style,
            dark: ctx.style().visuals.dark_mode,
        };
        let mut commands = menubar::take();
        if menubar::native() {
            menubar::sync(&state);
        } else {
            egui::TopBottomPanel::top("menu").show(ctx, |ui| commands.extend(menubar::show(ui, &state)));
        }
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(3.0);
            commands.extend(toolbar::show(ui, &state, self.layout.toolbar_small));
            ui.add_space(3.0);
        });
        for command in commands {
            self.menu_command(command, rows, settings, actions);
        }
    }

    fn menu_command(
        &mut self,
        command: menubar::Command,
        rows: &[DownloadEntry],
        settings: Option<&Settings>,
        actions: &mut Vec<Action>,
    ) {
        use menubar::Command as C;
        let selected = &self.selected;
        let ticked = |keep: fn(Status) -> bool| -> Vec<u64> {
            rows.iter().filter(|e| selected.contains(&e.id) && keep(e.status)).map(|e| e.id).collect()
        };
        // A setting changed from the menu, sent without the Options dialog.
        let changed = |change: &dyn Fn(&mut Settings)| {
            settings.map(|s| {
                let mut s = s.clone();
                change(&mut s);
                Action::QuickSettings(s)
            })
        };
        match command {
            C::AddUrl => self.add_dialog = Some(addurl::AddDialog::new(win::clipboard_text())),
            C::AddBatch => actions.push(Action::OpenBatch),
            C::BatchFromClipboard => actions.push(Action::BatchFromClipboard),
            C::Grabber => actions.push(Action::OpenGrabber),
            C::ToggleDropTarget => actions.extend(changed(&|s| s.drop_target = !s.drop_target)),
            C::Export => actions.push(Action::OpenExport),
            C::Import => actions.push(Action::OpenImport),
            // The daemon keeps downloading and exits on its own once idle.
            C::Exit => std::process::exit(0),
            C::StopTicked => actions.extend(ticked(is_running).into_iter().map(Action::Cancel)),
            C::RemoveTicked => {
                let ids = ticked(Status::is_terminal);
                if !ids.is_empty() {
                    actions.push(Action::Remove(ids));
                }
            }
            C::StartTicked => {
                for e in rows.iter().filter(|e| selected.contains(&e.id)) {
                    match e.status {
                        Status::Waiting => actions.push(Action::Confirm(Answer::start(e.id))),
                        s if is_startable(s) => actions.push(Action::Retry(e.id)),
                        _ => {}
                    }
                }
            }
            C::RedownloadTicked => actions.extend(ticked(is_finished).into_iter().map(Action::Redownload)),
            C::PauseAll => {
                self.daemon.send(Command::PauseAll);
            }
            C::ResumeAll => {
                self.daemon.send(Command::ResumeAll);
            }
            C::DeleteCompleted => actions.push(Action::ClearFinished),
            C::Find => self.find.get_or_insert_with(find::FindBar::new).focus = true,
            C::FindNext => match self.find {
                Some(_) => self.find_next = true,
                None => self.find = Some(find::FindBar::new()),
            },
            C::Scheduler => actions.push(Action::OpenQueues(None)),
            C::StartQueue(id) => actions.push(Action::StartQueue(id)),
            C::StopQueue(id) => actions.push(Action::StopQueue(id)),
            C::SpeedLimit(on) => actions.extend(changed(&|s| s.speed_limit_enabled = on)),
            C::Options => actions.push(Action::OpenOptions),
            C::ToggleCategories => actions.extend(changed(&|s| s.categories_panel = !s.categories_panel)),
            C::Arrange(sort) => {
                self.layout.sort = sort;
                self.layout_unsaved = true;
            }
            C::ToolbarSmall(small) => {
                self.layout.toolbar_small = small;
                self.layout_unsaved = true;
            }
            C::ChooseFont => self.font_dialog = Some(self.layout.list_font.unwrap_or(columns::ListFont::DEFAULT)),
            C::ResetFont => {
                self.layout.list_font = None;
                self.layout_unsaved = true;
            }
            C::TrayIcon(None) => actions.extend(changed(&|s| s.tray_icon = false)),
            // Picking a style also shows the icon if it was hidden.
            C::TrayIcon(Some(style)) => {
                if self.layout.tray_style != style {
                    self.layout.tray_style = style;
                    self.layout_unsaved = true;
                    tray::set_icon(style.apply(ICON_RGBA), ICON_SIZE);
                }
                if settings.is_some_and(|s| !s.tray_icon) {
                    actions.extend(changed(&|s| s.tray_icon = true));
                }
            }
            C::Columns => {
                self.columns_dialog.get_or_insert_with(|| self.layout.clone());
            }
            C::ToggleDarkMode => actions.extend(changed(&|s| {
                s.theme = match s.theme {
                    uidm_domain::settings::Theme::Light => uidm_domain::settings::Theme::System,
                    _ => uidm_domain::settings::Theme::Light,
                }
            })),
            C::Language(code) => actions.extend(changed(&|s| s.language = code.to_string())),
            C::Help(url) => self.ctx.open_url(egui::OpenUrl::new_tab(url)),
            C::Tip => {
                self.tip = Some(help::TipWindow {
                    index: settings.map_or(0, |s| s.next_tip),
                    on_startup: settings.is_none_or(|s| s.tips_on_startup),
                })
            }
            C::CheckUpdate => actions.push(Action::CheckUpdate),
            C::About => actions.push(Action::OpenAbout),
        }
    }

    /// Open download `id`'s window unless it has one or is done. It takes
    /// the focus when the user asked for it, and opens hidden in the tray
    /// for one a queue started.
    fn open_download_window(&mut self, id: u64, focus: bool, hidden: bool) {
        if self.progress_windows.iter().any(|w| w.id == id) {
            return;
        }
        let entry = self.state.table.get(id).cloned();
        if let Some(e) = entry.filter(|e| e.status != Status::Done) {
            let settings = self.state.settings.clone().unwrap_or_default();
            self.progress_windows.push(progress::ProgressWindow::new(&e, &settings, focus, hidden));
        }
    }

    fn sync_drop_target(&mut self, s: &Settings, actions: &mut Vec<Action>) {
        let open = droptarget::is_open();
        tray::set_drop_target_checked(s.drop_target && open);
        match (s.drop_target, open, self.drop_shown) {
            (true, false, false) => {
                let daemon = Arc::clone(&self.daemon);
                let ctx = self.ctx.clone();
                droptarget::open(
                    Arc::new(move |url: String| {
                        // Each dropped link goes through the save-as dialog.
                        daemon.send(Command::Add(Download { url, ask: Some(true), ..Download::default() }));
                        win::show();
                        ctx.request_repaint();
                    }),
                    Arc::new(win::show),
                );
                self.drop_shown = true;
            }
            (true, false, true) => {
                self.drop_shown = false;
                actions.push(Action::QuickSettings(Settings { drop_target: false, ..s.clone() }));
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
        let answer = self.state.update.clone();
        let mut open = true;
        crate::dialog::Window::new(tr("About UIDM"))
            .id(egui::Id::new("about"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.heading("UIDM");
                ui.label(trf!("Version {version}", version = env!("CARGO_PKG_VERSION")));
                ui.label(tr("Copyright © 2026 UIDM contributors. Released under the MIT License."));
                ui.hyperlink_to("github.com/tajbinkhan/uidm", help::HOME);
                ui.horizontal(|ui| {
                    ui.label(tr("Support:"));
                    ui.hyperlink_to(tr("report a problem or ask a question"), help::SUPPORT);
                });
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
                                let installing = self.state.installing.clone();
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
        if let Some((id, result)) = state::take_zip_listed(&mut self.state) {
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
        crate::dialog::Window::new(trf!("Inside {name}", name = name))
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
                    if entries.len() == uidm_domain::download::MAX_ZIP_ENTRIES {
                        ui.weak(trf!("Showing the first {count}.", count = uidm_domain::download::MAX_ZIP_ENTRIES));
                    }
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        egui::Grid::new("zip_entries").striped(true).num_columns(2).spacing([16.0, 2.0]).show(
                            ui,
                            |ui| {
                                for e in entries {
                                    ui.add_sized([360.0, 16.0], egui::Label::new(&e.name).truncate());
                                    ui.weak(human_bytes(e.size));
                                    ui.end_row();
                                }
                            },
                        );
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
        crate::dialog::Window::new(tr("Keep it in the browser?"))
            .id(egui::Id::new("exception_offer"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(tr("You canceled this download twice. UIDM can leave it to the browser next time."));
                ui.add(egui::Label::new(egui::RichText::new(url.as_str()).weak()).truncate());
                ui.hyperlink_to(tr("How capture rules work"), help::CAPTURE)
                    .on_hover_text(tr("Which browser downloads UIDM takes, and how to keep others in the browser"));
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
                actions.push(Action::Exception { url: url.clone(), add, stop_offering: *dont_show });
            }
            self.exception_offer = None;
        }
    }

    /// The speed limit dialog for one download.
    fn limit_window(
        &mut self,
        ctx: &egui::Context,
        rows: &[DownloadEntry],
        connected: bool,
        actions: &mut Vec<Action>,
    ) {
        let Some(d) = self.limit_dialog.as_mut() else {
            return;
        };
        let Some(e) = rows.iter().find(|e| e.id == d.id) else {
            self.limit_dialog = None;
            return;
        };
        let mut close = false;
        crate::dialog::Window::new(tr("Speed limit"))
            .id(egui::Id::new("download_limit"))
            .dlu_width(302.0)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(trf!("A speed limit for {name} alone.", name = display_name(e)));
                ui.weak(tr("The total limit still applies; the lower one wins."));
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
                ui.add_enabled(d.enabled, egui::Checkbox::new(&mut d.remember, tr("Remember on stop and resume")));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(connected, egui::Button::new(tr("Save"))).clicked() {
                        let bytes = d.enabled.then(|| (d.value * d.unit.kb() * 1024.0).round() as u64);
                        actions.push(Action::SetSpeedLimit(d.id, bytes, d.remember));
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
        let settings = self.state.settings.clone().unwrap_or_default();
        if settings.confirm_remove {
            self.remove_prompt =
                Some(RemovePrompt { ids, delete_files: settings.remove_deletes_files, dont_ask: false });
        } else {
            self.perform(Action::RemoveNow { ids, delete_files: settings.remove_deletes_files, dont_ask: false });
        }
    }

    /// The confirmation before removing downloads, with a box to send their
    /// files to the Recycle Bin too.
    fn remove_window(
        &mut self,
        ctx: &egui::Context,
        rows: &[DownloadEntry],
        connected: bool,
        actions: &mut Vec<Action>,
    ) {
        let Some(p) = self.remove_prompt.as_mut() else {
            return;
        };
        // One that started again since is left alone.
        p.ids.retain(|id| rows.iter().any(|e| e.id == *id && e.status.is_terminal()));
        let unfinished = rows.iter().filter(|e| p.ids.contains(&e.id) && e.status != Status::Done).count();
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
        crate::dialog::Window::new(tr("Remove from the list"))
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
    fn refresh_window(
        &mut self,
        ctx: &egui::Context,
        rows: &[DownloadEntry],
        connected: bool,
        actions: &mut Vec<Action>,
    ) {
        let Some((id, url)) = self.refresh.as_mut() else {
            return;
        };
        let id = *id;
        let (waiting, mismatch) = {
            let s = &self.state;
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
        crate::dialog::Window::new(tr("New link"))
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
                            actions.push(Action::RefreshWith { id, url: new_url.clone(), restart: true });
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
                        actions.push(Action::RefreshWith { id, url: url.trim().to_string(), restart: false });
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
            state::apply(&mut self.state, UiMessage::Forget(Forget::RefreshMismatch));
        }
    }
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
