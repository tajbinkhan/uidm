//! The state actor (AGENTS.md section 7.1): the one thread that owns
//! [`AppState`]. Commands, the daemon connection, the tray and window
//! events send it [`Inbox`] messages; it applies them, opens and closes the
//! windows the state asks for, and sends each subscribed window its
//! snapshot and then its patches, at most one every 200 ms while the
//! daemon's progress streams in.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use tauri::ipc::Channel;
use tauri::AppHandle;
use udm_application::daemon::{Command, DaemonClient, Event};
use udm_application::window::{
    BrowserRegistry, Clipboard, GrabLibrary, ListFiles, ListFormat, SystemProxy, SystemServices, ThemeFiles,
    WindowFiles,
};
use udm_domain::download::{OnDone, Status};
use udm_domain::grab::{project_login_target, GrabRequest};
use udm_domain::id::{DownloadId, QueueId};
use udm_domain::package::Packaging;
use udm_domain::ports::ProblemReports;
use udm_domain::ports::{Clock, FileOpener, GrabDownloads, SaveFolders, Timestamps};
use udm_domain::settings::Notify;

use crate::contract::{Look, OpenHow, Patch, Update, View};
use crate::i18n::fill;
use crate::projection::{self, main, Ctx};
use crate::state::{apply, AppState, UiMessage};
use crate::windows::{self, Label};
use crate::{actions, contract};

/// The fastest a window gets patches while the list's progress changes.
const PATCH_EVERY: Duration = Duration::from_millis(200);
/// How long after losing the daemon the shell connects again.
const RECONNECT_AFTER: Duration = Duration::from_secs(3);

/// What the actor hears.
pub enum Inbox {
    Ui(UiMessage),
    /// A window's subscription, from its `subscribe` command.
    Subscribe {
        label: Label,
        channel: Channel<Update>,
    },
    /// A window closed, by the user or by the shell.
    Closed(String),
    /// A toolbar or menu action on the main window's selection.
    Act(contract::ListAction),
    /// Send a command to the daemon, telling `reply` whether it went.
    Send {
        command: Box<Command>,
        reply: Option<Sender<bool>>,
    },
    /// The Download File Info dialog's answer.
    Answer {
        answer: Box<udm_domain::download::Answer>,
        reply: Sender<bool>,
    },
    /// Remove the downloads Confirm deletion asked about, and with
    /// `dont_ask` stop asking.
    Remove {
        delete_files: bool,
        dont_ask: bool,
    },
    /// "Don't show this dialog again" in Download complete.
    NoDoneDialogs,
    /// Open a finished download's file or folder.
    Open {
        id: DownloadId,
        how: OpenHow,
    },
    /// A progress window's speed limit.
    Limit {
        id: DownloadId,
        bytes_per_sec: Option<u64>,
        remember: bool,
    },
    /// A progress window's Options on completion.
    OnDone {
        id: DownloadId,
        on_done: Option<OnDone>,
    },
    /// Open a window by a click.
    OpenWindow(contract::OpenWindow),
    /// The Scheduler's Apply for a queue.
    SaveQueue {
        draft: Box<contract::QueueDraft>,
        reply: Sender<Result<(), contract::InvokeError>>,
    },
    /// The Scheduler's New queue.
    CreateQueue {
        name: String,
        reply: Sender<Result<(), contract::InvokeError>>,
    },
    /// The Scheduler's Apply for the download limits.
    SaveQuota {
        quota: contract::QuotaDraft,
        reply: Sender<Result<(), contract::InvokeError>>,
    },
    /// Move a download along its queue.
    MoveQueued {
        id: DownloadId,
        offset: i32,
    },
    /// The grabber's Next on its last step: check the fields, keep a new
    /// password, and start the crawl.
    GrabStart {
        draft: Box<contract::GrabDraft>,
        reply: Sender<Result<(), contract::InvokeError>>,
    },
    /// The crawl after its password was kept.
    GrabBegin(Box<GrabRequest>),
    GrabStop,
    /// Add the ticked results to `queue`, and start it with `start`.
    GrabAdd {
        indices: Vec<u32>,
        queue: QueueId,
        start: bool,
        reply: Sender<Result<(), contract::InvokeError>>,
    },
    /// A template's fields.
    GrabTemplate {
        name: String,
        reply: Sender<Result<contract::GrabDraft, contract::InvokeError>>,
    },
    GrabOpen {
        name: String,
        reply: Sender<Result<contract::GrabOpened, contract::InvokeError>>,
    },
    GrabSave {
        name: String,
        draft: Box<contract::GrabDraft>,
        checked: Vec<bool>,
        schedule: Box<contract::GrabScheduleDraft>,
        reply: Sender<Result<(), contract::InvokeError>>,
    },
    GrabSaveTemplate {
        name: String,
        draft: Box<contract::GrabDraft>,
        reply: Sender<Result<(), contract::InvokeError>>,
    },
    GrabSaveFilters {
        filters: Vec<udm_domain::grab::Filter>,
        reply: Sender<Result<(), contract::InvokeError>>,
    },
    /// "Add checked files ... on closing the grabber".
    GrabAddOnClose(bool),
    /// Find file: select the next match, telling `reply` whether there was
    /// one.
    Find {
        input: Box<contract::FindInput>,
        reply: Sender<bool>,
    },
    /// Export's text for the downloads `scope` names, and how many.
    ExportText {
        scope: contract::ExportScope,
        format: contract::ExportFormat,
        reply: Sender<(String, usize)>,
    },
    /// The speed limiter's settings.
    Limiter {
        draft: contract::LimiterDraft,
        reply: Sender<Result<(), contract::InvokeError>>,
    },
    /// Save category `original` (a new one when `None`), or delete it when
    /// `input` is `None`.
    Category {
        original: Option<String>,
        input: Option<Box<contract::CategoryInput>>,
        reply: Sender<Result<(), contract::InvokeError>>,
    },
    /// Open a category's folder.
    BrowseCategory(String),
    /// Put the selected downloads in `queue`, or take them out of theirs.
    QueueSelection(Option<QueueId>),
    /// A download's properties.
    Properties {
        id: DownloadId,
        input: Box<contract::PropertiesInput>,
        reply: Sender<Result<(), contract::InvokeError>>,
    },
    /// Batch download's or the links window's downloads, for `queue`.
    AddItems {
        items: Vec<udm_domain::download::Download>,
        queue: QueueId,
        start: bool,
        reply: Sender<Result<(), contract::InvokeError>>,
    },
    /// The links window's ticked links.
    AddLinks {
        indices: Vec<u32>,
        queue: QueueId,
        start: bool,
        reply: Sender<Result<(), contract::InvokeError>>,
    },
    /// Show or hide the drop target.
    DropTarget(bool),
    /// The tip of the day closed: the tip to show next, and whether tips
    /// show at startup.
    TipClosed {
        next: u32,
        on_startup: bool,
    },
    /// The login prompt's OK.
    Login {
        user: String,
        password: String,
        save: bool,
        reply: Sender<Result<(), contract::InvokeError>>,
    },
    /// The pickers' Add, from the window `label`.
    AddPicks {
        label: Label,
        input: Box<contract::PickInput>,
        reply: Sender<Result<(), contract::InvokeError>>,
    },
    /// The names the link picker's rename pattern gives the ticked links.
    LinkNames {
        indices: Vec<u32>,
        rename: String,
        reply: Sender<Result<Vec<String>, contract::InvokeError>>,
    },
    /// Open a package's unpacked folder.
    OpenPackage(udm_domain::id::PackageId),
    /// Close the strip offering this release.
    DismissRelease,
    /// Options' OK: the draft to check and send, and whether it went.
    SaveOptions {
        draft: Box<contract::OptionsDraft>,
        reply: Sender<Result<(), contract::InvokeError>>,
    },
    /// Connect to the daemon again.
    Reconnect,
    /// A change to the browsers UDM is registered with.
    Browser(BrowserAction),
    /// Open the folder theme files go in.
    OpenThemesFolder,
    /// Play an event's sound, from `file` or the system's own.
    PlaySound {
        sound: udm_domain::settings::Sound,
        file: String,
    },
    /// Read the system's proxy setting for Get System.
    SystemProxy(Sender<Option<SystemProxy>>),
    /// A button of the report strip, or Help's Report a problem.
    Report(contract::ReportAction),
    /// The clipboard's text changed.
    Clipboard(String),
    /// Check a finished download's file against a checksum.
    Verify {
        id: DownloadId,
        text: String,
        reply: Sender<Result<contract::VerifyResult, contract::InvokeError>>,
    },
    /// Show the system's Open with picker for a finished download's file.
    OpenWith(DownloadId),
    /// Drag a finished download's file out of the window `label`, while
    /// its left button is down.
    DragFile {
        label: Label,
        id: DownloadId,
    },
    /// The tray's Add batch download from clipboard.
    ClipboardBatch,
    /// The tray's Speed Limiter and Show drop target.
    ToggleLimiter,
    ToggleDropTarget,
    /// The answer to the offer to keep a download in the browser.
    Exception {
        choice: contract::ExceptionChoice,
        stop_offering: bool,
    },
}

/// A change Options makes to the browsers, at once.
#[derive(Debug, Clone)]
pub enum BrowserAction {
    /// Register UDM with the browser in row `index`, or stop.
    Set { index: usize, on: bool },
    /// Add the browser at `program`, registered.
    Add { program: String },
    /// Remove the browser the user added in row `index`.
    Remove { index: usize },
    /// Close the browser in row `index` and start it again.
    Restart { index: usize },
}

/// The actor's mailbox, as the commands hold it (a managed type).
pub type Outbox = Sender<Inbox>;

/// The window's language, by its place in `i18n::LANGUAGES`, for the
/// commands' error messages (a managed type).
pub type CurrentLanguage = Arc<AtomicUsize>;

/// The ports the actor reaches.
pub struct Ports {
    /// The grabber's projects, templates and filters, when their folder can
    /// be found.
    pub library: Option<Arc<dyn GrabLibrary>>,
    /// The downloads a crawl's finds become.
    pub found: Arc<dyn GrabDownloads>,
    /// The system's credential store, for the grabber's logins.
    pub system: Arc<dyn SystemServices>,
    /// Exporting the download list.
    pub lists: Arc<dyn ListFiles>,
    /// The system's Downloads folder, for a category's folder.
    pub folders: Arc<dyn SaveFolders>,
    /// The window's own files: its layout.
    pub window_files: Arc<dyn WindowFiles>,
    /// Registering UDM with the browsers.
    pub browsers: Arc<dyn BrowserRegistry>,
    /// The network adapters, for Options.
    pub adapters: Arc<dyn udm_domain::ports::NetworkAdapters>,
    /// The theme files.
    pub themes: Arc<dyn ThemeFiles>,
    /// Problem reports.
    pub reports: Arc<dyn ProblemReports>,
    /// What the state thread and the windows tell the watchdog.
    pub beats: Arc<crate::watchdog::Beats>,
    /// The clipboard, for the clipboard monitor.
    pub clipboard: Arc<dyn Clipboard>,
    /// Hashing a finished download's file, for Verify checksum.
    pub hashes: Arc<dyn udm_domain::ports::FileHashes>,
    pub language: CurrentLanguage,
    pub daemon: Arc<dyn DaemonClient>,
    pub clock: Arc<dyn Clock>,
    pub times: Arc<dyn Timestamps>,
    pub opener: Arc<dyn FileOpener>,
}

struct Subscriber {
    channel: Channel<Update>,
    seq: u32,
    last: Option<View>,
    look: Option<Look>,
}

struct Actor {
    app: AppHandle,
    ports: Ports,
    outbox: Outbox,
    state: AppState,
    subscribers: HashMap<Label, Subscriber>,
    /// Windows the state asked for and the actor opened.
    opened: BTreeSet<Label>,
    /// Windows the shell is closing, whose closing is not the user's.
    closing: HashSet<Label>,
    titles: HashMap<Label, String>,
    /// The layout file as last read or saved, so it is written only when it
    /// changes.
    layout: Option<Vec<u8>>,
    /// Whether the clipboard monitor is on, for its thread.
    watching_clipboard: Arc<std::sync::atomic::AtomicBool>,
    /// Whether a window that warns about the power plan's wake timers (the
    /// Scheduler, the grabber) is open, for the thread that reads them.
    watching_wake: Arc<std::sync::atomic::AtomicBool>,
    /// The last link the clipboard monitor offered, so a link copied twice
    /// is offered once.
    clipboard_last: Option<String>,
    /// The system's Downloads folder, read once.
    downloads: std::path::PathBuf,
}

/// Why the link picker's ticked links cannot be added, for the user.
fn pick_error(lang: &'static crate::i18n::Language, e: &crate::extras::PickError) -> contract::InvokeError {
    match e {
        crate::extras::PickError::Package(e) => crate::errors::invalid_text(crate::errors::text::package_name(lang, e)),
        crate::extras::PickError::BadRename => {
            crate::errors::invalid(lang, "A file name cannot contain \\ / : \" < > | or ?")
        }
    }
}

/// Start the actor and connect to the daemon. Returns its mailbox.
pub fn spawn(app: AppHandle, ports: Ports) -> Outbox {
    let (tx, rx) = mpsc::channel();
    let outbox = tx.clone();
    let spawned = thread::Builder::new().name("udm-state".into()).spawn(move || {
        let mut actor = Actor {
            app,
            ports,
            outbox,
            state: AppState::default(),
            subscribers: HashMap::new(),
            opened: BTreeSet::new(),
            closing: HashSet::new(),
            titles: HashMap::new(),
            layout: None,
            watching_clipboard: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            watching_wake: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            clipboard_last: None,
            downloads: std::path::PathBuf::new(),
        };
        actor.downloads = actor.ports.folders.downloads_dir();
        // The layout file is small; reading it on this thread delays only
        // the first snapshot.
        let saved = actor.ports.window_files.read(crate::layout::FILE);
        let file = crate::layout::parse(saved.as_deref());
        apply(&mut actor.state, UiMessage::LayoutRead(Box::new(file)));
        actor.layout = crate::layout::bytes(&actor.state);
        actor.connect();
        actor.state.can_open_with = actor.ports.opener.can_open_with();
        // Each asks the system something it answers at once.
        let system = &actor.ports.system;
        let facts = crate::state::SystemFacts {
            can_elevate: system.can_elevate(),
            creation_time: system.can_set_creation_time(),
            portable: system.is_portable(),
        };
        apply(&mut actor.state, UiMessage::SystemRead(facts));
        actor.watch_themes();
        actor.watch_clipboard();
        actor.watch_wake_timers();
        actor.run(rx);
    });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "the window's state thread did not start");
    }
    tx
}

impl Actor {
    fn connect(&self) {
        self.connect_with(false);
    }

    /// Connect, first restarting the daemon if `restart`.
    fn connect_with(&self, restart: bool) {
        let outbox = self.outbox.clone();
        let times = Arc::clone(&self.ports.times);
        let (beats, reports) = (Arc::clone(&self.ports.beats), Arc::clone(&self.ports.reports));
        self.ports.daemon.connect(
            restart,
            Box::new(move |event| {
                match &event {
                    Event::Pong => beats.answered(),
                    Event::Connected => {
                        beats.connected(true);
                        // The reports a crash or an earlier stall left.
                        let (outbox, reports) = (outbox.clone(), Arc::clone(&reports));
                        thread::spawn(move || {
                            let found = udm_application::report::FindNewReports::new(&*reports).run();
                            if !found.is_empty() {
                                let _ = outbox.send(Inbox::Ui(UiMessage::ReportsFound(found)));
                            }
                        });
                    }
                    Event::Lost(_) => beats.connected(false),
                    _ => {}
                }
                let lost = matches!(event, Event::Lost(_));
                let msg = UiMessage::Daemon { event: Box::new(event), at: Instant::now(), unix: times.unix_now() };
                let _ = outbox.send(Inbox::Ui(msg));
                if lost {
                    let outbox = outbox.clone();
                    thread::spawn(move || {
                        thread::sleep(RECONNECT_AFTER);
                        let _ = outbox.send(Inbox::Reconnect);
                    });
                }
            }),
        );
    }

    fn run(&mut self, rx: Receiver<Inbox>) {
        let mut last = Instant::now() - PATCH_EVERY;
        let mut dirty = false;
        loop {
            let wait = if dirty { PATCH_EVERY.saturating_sub(last.elapsed()) } else { Duration::from_secs(1) };
            let mut urgent = false;
            match rx.recv_timeout(wait) {
                Ok(msg) => {
                    urgent = !matches!(&msg, Inbox::Ui(UiMessage::Daemon { event, .. }) if matches!(**event, Event::Progress { .. }));
                    let beats = Arc::clone(&self.ports.beats);
                    let _work = beats.work();
                    self.handle(msg);
                    dirty = true;
                }
                Err(RecvTimeoutError::Timeout) => {
                    // The countdown's seconds tick by themselves.
                    if self.state.asks.countdown.is_some() {
                        dirty = true;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if dirty && (urgent || last.elapsed() >= PATCH_EVERY) {
                self.reconcile();
                self.publish();
                self.save_layout();
                last = Instant::now();
                dirty = false;
            }
        }
    }

    fn lang(&self) -> &'static crate::i18n::Language {
        projection::language(&self.state)
    }

    fn send(&self, command: Command) -> bool {
        let sent = self.ports.daemon.send(command);
        if !sent {
            tracing::warn!("a command for the daemon was not sent: not connected");
        }
        sent
    }

    /// The login prompt's OK: save the login for the site, or give it to
    /// each download, then retry them.
    fn answer_login(&mut self, user: &str, password: &str, save: bool) -> Result<(), contract::InvokeError> {
        let lang = self.lang();
        let Some((ids, wanted)) = crate::extras::login_group(&self.state) else {
            return Err(crate::errors::gone(lang));
        };
        let view = crate::extras::login_view(&self.state);
        let save = save || view.is_some_and(|v| !v.own_site);
        let user = user.trim().to_string();
        if save {
            let Some(mut settings) = self.state.settings.clone() else {
                return Err(crate::errors::offline(lang));
            };
            let plain = ids
                .iter()
                .filter_map(|id| self.state.table.get(*id))
                .any(|e| !e.url.as_str().to_ascii_lowercase().starts_with("https:"));
            let old = settings.site_logins.iter().position(|l| l.site.eq_ignore_ascii_case(&wanted.host));
            let login = udm_domain::settings::SiteLogin {
                site: wanted.host.clone(),
                username: user.clone(),
                password: Some(password.to_string()),
                plain: plain || old.is_some_and(|i| settings.site_logins[i].plain),
            };
            match old {
                Some(i) => settings.site_logins[i] = login,
                None => settings.site_logins.push(login),
            }
            self.sent(lang, Command::SetSettings { settings })?;
        }
        for id in &ids {
            if !save {
                let login = udm_domain::download::DownloadLogin {
                    username: user.clone(),
                    password: Some(password.to_string()),
                };
                let changes = udm_domain::download::Properties { login: Some(login), ..Default::default() };
                self.send(Command::SetProperties { id: *id, changes });
            }
            self.send(Command::Retry { id: *id });
        }
        apply(&mut self.state, UiMessage::LoginAnswered(ids));
        Ok(())
    }

    /// The pickers' Add: the ticked links, videos or pictures, waiting in a
    /// queue.
    fn add_picks(&mut self, label: Label, input: &contract::PickInput) -> Result<(), contract::InvokeError> {
        let lang = self.lang();
        let queue = QueueId::new(input.queue.0);
        let settings = self.state.settings.clone().unwrap_or_default();
        let a = &self.state.asks;
        let (items, packaging) = match label {
            Label::PickLinks => {
                let l = a.links.as_ref().ok_or_else(|| crate::errors::gone(lang))?;
                let package = input.package.as_deref();
                crate::extras::link_downloads(l, &input.indices, package, &input.rename, &settings, &self.downloads)
                    .map_err(|e| pick_error(lang, &e))?
            }
            Label::PickVideos => {
                let v = a.videos.as_ref().ok_or_else(|| crate::errors::gone(lang))?;
                (crate::extras::video_downloads(v, &input.indices, input.quality), Packaging::None)
            }
            Label::Gallery => {
                let g = a.gallery.as_ref().ok_or_else(|| crate::errors::gone(lang))?;
                let items = crate::extras::gallery_downloads(g, &input.indices, &input.folder)
                    .map_err(|e| crate::errors::invalid_text(lang.tr(&e.to_string()).to_string()))?;
                (items, Packaging::None)
            }
            _ => return Err(crate::errors::gone(lang)),
        };
        if items.is_empty() {
            return Err(crate::errors::invalid(lang, "Tick the files to add."));
        }
        self.sent(lang, Command::AddMany { items, queue: Some(queue), packaging })?;
        if input.start {
            self.send(Command::StartQueue { id: queue });
        }
        let gone = match label {
            Label::PickLinks => UiMessage::WindowClosed("pick-links".into()),
            Label::PickVideos => UiMessage::WindowClosed("pick-videos".into()),
            _ => UiMessage::WindowClosed("gallery".into()),
        };
        apply(&mut self.state, gone);
        Ok(())
    }

    fn set_drop_target(&self, on: bool) {
        if let Some(mut settings) = self.state.settings.clone().filter(|s| s.drop_target != on) {
            settings.drop_target = on;
            self.send(Command::SetSettings { settings });
        }
    }

    /// Add `items` to wait in `queue`, and start it with `start`.
    fn add_items(
        &self,
        items: Vec<udm_domain::download::Download>,
        queue: QueueId,
        start: bool,
    ) -> Result<(), contract::InvokeError> {
        let lang = self.lang();
        if items.is_empty() {
            return Err(crate::errors::invalid(lang, "Tick the files to add."));
        }
        self.sent(lang, Command::AddMany { items, queue: Some(queue), packaging: Packaging::None })?;
        if start {
            self.send(Command::StartQueue { id: queue });
        }
        Ok(())
    }

    /// Save the layout file when it changed. The port writes on a thread of
    /// its own.
    fn save_layout(&mut self) {
        let bytes = crate::layout::bytes(&self.state);
        if bytes.is_some() && bytes != self.layout {
            if let Some(b) = bytes.clone() {
                self.ports.window_files.save(crate::layout::FILE, b);
            }
            self.layout = bytes;
        }
    }

    /// Read the theme files and the system's contrast colors every two
    /// seconds on a thread, so a new or changed file shows; sends them when
    /// they change.
    fn watch_themes(&self) {
        let (outbox, files, system) =
            (self.outbox.clone(), Arc::clone(&self.ports.themes), Arc::clone(&self.ports.system));
        let spawned = thread::Builder::new().name("udm-themes".into()).spawn(move || {
            let mut last = None;
            loop {
                let read = udm_application::themes::ReadThemes::new(&*files, &*system).run();
                if last.as_ref() != Some(&read) {
                    last = Some(read.clone());
                    if outbox.send(Inbox::Ui(UiMessage::ThemesRead(Box::new(read)))).is_err() {
                        return;
                    }
                }
                thread::sleep(Duration::from_secs(2));
            }
        });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "the theme files are not watched");
        }
    }

    /// Look at the clipboard every 0.7 seconds while the clipboard monitor
    /// is on, and send its text when it changed.
    fn watch_clipboard(&self) {
        let (outbox, clipboard, on) =
            (self.outbox.clone(), Arc::clone(&self.ports.clipboard), Arc::clone(&self.watching_clipboard));
        let spawned = thread::Builder::new().name("udm-clipboard".into()).spawn(move || {
            let mut seen = None;
            loop {
                thread::sleep(Duration::from_millis(700));
                if !on.load(Ordering::Relaxed) {
                    seen = None;
                    continue;
                }
                let now = clipboard.sequence();
                // What was there when the monitor came on is not offered.
                if seen.replace(now).is_none_or(|before| before == now) {
                    continue;
                }
                if let Some(text) = clipboard.text() {
                    if outbox.send(Inbox::Clipboard(text)).is_err() {
                        return;
                    }
                }
            }
        });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "the clipboard is not watched");
        }
    }

    /// Read the power plan's wake timer setting while the Scheduler or the
    /// grabber is open: when one opens, and again every ten seconds, since
    /// the user may change it meanwhile. Sends it when it changes.
    fn watch_wake_timers(&self) {
        let (outbox, system, on) =
            (self.outbox.clone(), Arc::clone(&self.ports.system), Arc::clone(&self.watching_wake));
        let spawned = thread::Builder::new().name("udm-wake-timers".into()).spawn(move || {
            let mut last: Option<(Instant, Option<(bool, bool)>)> = None;
            loop {
                if !on.load(Ordering::Relaxed) {
                    last = None;
                } else if last.as_ref().is_none_or(|(at, _)| at.elapsed() >= Duration::from_secs(10)) {
                    let read = system.wake_timers_allowed();
                    if last.as_ref().is_none_or(|(_, before)| *before != read)
                        && outbox.send(Inbox::Ui(UiMessage::WakeTimersRead(read))).is_err()
                    {
                        return;
                    }
                    last = Some((Instant::now(), read));
                }
                thread::sleep(Duration::from_millis(500));
            }
        });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "the wake timers are not read");
        }
    }

    /// Whether a window that shows the wake timer warning is open.
    fn update_wake_watch(&self) {
        let open = self.subscribers.contains_key(&Label::Scheduler) || self.subscribers.contains_key(&Label::Grabber);
        self.watching_wake.store(open, Ordering::Relaxed);
    }

    /// Ask the system on a worker what it calls the file type of download
    /// `id`, for its File Properties, unless that is known.
    fn read_type_name(&self, id: DownloadId) {
        let Some(ext) = crate::tools::properties_extension(&self.state, id) else { return };
        if self.state.type_names.contains_key(&ext) {
            return;
        }
        let (outbox, system) = (self.outbox.clone(), Arc::clone(&self.ports.system));
        thread::spawn(move || {
            let name = system.file_type_name(&ext);
            let _ = outbox.send(Inbox::Ui(UiMessage::TypeNameRead { ext, name }));
        });
    }

    /// The clipboard's new text: a link of a captured type is started, or
    /// offered in Add URL.
    fn clipboard_text(&mut self, text: &str) {
        let Some(settings) = self.state.settings.as_ref().filter(|s| s.clipboard_monitor) else { return };
        let Some(url) = udm_domain::batch::clipboard_link(text, &settings.capture_types) else { return };
        let url = url.to_string();
        if self.clipboard_last.as_deref() == Some(url.as_str()) {
            return;
        }
        self.clipboard_last = Some(url.clone());
        if settings.clipboard_start {
            let download = udm_domain::download::Download { url: url.as_str().into(), ..Default::default() };
            if self.send(Command::Add(download)) {
                return;
            }
        }
        apply(&mut self.state, UiMessage::ClipboardOffer(url));
        if let Err(e) = windows::open(&self.app, Label::AddUrl, self.lang()) {
            tracing::warn!(error = %e, "Add URL did not open");
        }
    }

    /// Hash a finished download's file on a worker and compare it with the
    /// checksum `text`.
    fn verify(
        &mut self,
        id: DownloadId,
        text: &str,
        reply: Sender<Result<contract::VerifyResult, contract::InvokeError>>,
    ) {
        let lang = self.lang();
        let sum = match udm_domain::checksum::Checksum::parse(text) {
            Ok(sum) => sum,
            Err(_) => {
                let _ = reply.send(Err(crate::errors::invalid(lang, "This is not a checksum UDM knows.")));
                return;
            }
        };
        let Some(path) = self.state.table.get(id).and_then(|e| e.path.clone()) else {
            let _ = reply.send(Err(crate::errors::gone(lang)));
            return;
        };
        let hashes = Arc::clone(&self.ports.hashes);
        thread::spawn(move || {
            let found = udm_application::downloads::VerifyChecksum::new(&*hashes)
                .run(std::path::Path::new(&path), &sum)
                .map(|v| contract::VerifyResult {
                    algorithm: sum.algorithm().to_string(),
                    matches: v.matches,
                    expected: sum.hex(),
                    actual: v.actual,
                })
                .map_err(|e| {
                    crate::errors::invalid_text(fill(
                        lang.tr("The file could not be read: {error}"),
                        &[("error", &e.to_string())],
                    ))
                });
            let _ = reply.send(found);
        });
    }

    /// Read the browsers on a worker, with the ones the user added.
    fn read_browsers(&mut self) {
        let added = self.state.settings.as_ref().map(|s| s.added_browsers.clone()).unwrap_or_default();
        let (outbox, registry) = (self.outbox.clone(), Arc::clone(&self.ports.browsers));
        thread::spawn(move || {
            let read = read_browsers(&*registry, &added);
            let _ = outbox.send(Inbox::Ui(UiMessage::BrowsersRead(Some(Box::new(read)))));
        });
    }

    /// Read the network adapters on a worker.
    fn read_adapters(&mut self) {
        let (outbox, adapters) = (self.outbox.clone(), Arc::clone(&self.ports.adapters));
        thread::spawn(move || {
            let read = adapters.list();
            let _ = outbox.send(Inbox::Ui(UiMessage::AdaptersRead(read)));
        });
    }

    /// A browser change, on a worker; the list is read again after it.
    fn browser_action(&mut self, action: BrowserAction) {
        let lang = self.lang();
        let Some(rows) = self.state.services.browsers.as_ref().map(|b| b.rows.clone()) else { return };
        let Some(mut settings) = self.state.settings.clone() else { return };
        let (outbox, registry) = (self.outbox.clone(), Arc::clone(&self.ports.browsers));
        apply(&mut self.state, UiMessage::BrowserNote(None));
        thread::spawn(move || {
            let note = |text: String, error: bool| {
                let _ = outbox.send(Inbox::Ui(UiMessage::BrowserNote(Some(contract::Banner { text, error }))));
            };
            match action {
                BrowserAction::Set { index, on } => {
                    if let Some(row) = rows.get(index) {
                        if let Err(e) = registry.set_registered(&row.browser, on) {
                            note(crate::errors::files(&e).message, true);
                        }
                    }
                }
                BrowserAction::Add { program } => match registry.identify(std::path::Path::new(&program)) {
                    None => note(
                        fill(
                            lang.tr("UDM cannot tell what browser {program} is. Pick the program of a browser built on Chromium (such as Chrome, Edge, Brave, Vivaldi, Opera or Yandex) or on Firefox."),
                            &[("program", &program)],
                        ),
                        true,
                    ),
                    Some(target) => {
                        if !settings.added_browsers.iter().any(|a| a.program == program) {
                            let name = registry.display_name(std::path::Path::new(&program));
                            settings.added_browsers.push(udm_domain::settings::AddedBrowser { name, program });
                            let command = Command::SetSettings { settings: settings.clone() };
                            let _ = outbox.send(Inbox::Send { command: Box::new(command), reply: None });
                        }
                        if let Err(e) = registry.set_registered(&target.browser, true) {
                            note(crate::errors::files(&e).message, true);
                        }
                    }
                },
                BrowserAction::Remove { index } => {
                    if let Some(row) = rows.get(index).filter(|r| r.added.is_some()) {
                        let program = row.added.as_ref().map(|a| a.program.clone()).unwrap_or_default();
                        settings.added_browsers.retain(|a| a.program != program);
                        let command = Command::SetSettings { settings: settings.clone() };
                        let _ = outbox.send(Inbox::Send { command: Box::new(command), reply: None });
                        // Its place stays registered while another row reads it.
                        let shared = rows.iter().enumerate().any(|(i, r)| i != index && r.browser.same_place(&row.browser));
                        if !shared {
                            if let Err(e) = registry.set_registered(&row.browser, false) {
                                note(crate::errors::files(&e).message, true);
                            }
                        }
                    }
                }
                BrowserAction::Restart { index } => {
                    if let Some(row) = rows.get(index) {
                        let program = row.added.as_ref().map(|a| std::path::PathBuf::from(&a.program));
                        match registry.restart(&row.browser, program.as_deref()) {
                            Ok(()) => note(fill(lang.tr("{browser} was restarted."), &[("browser", &row.name)]), false),
                            Err(e) => note(
                                fill(
                                    lang.tr("Could not restart {browser}: {error}"),
                                    &[("browser", &row.name), ("error", &crate::errors::files(&e).message)],
                                ),
                                true,
                            ),
                        }
                    }
                }
            }
            let read = read_browsers(&*registry, &settings.added_browsers);
            let _ = outbox.send(Inbox::Ui(UiMessage::BrowsersRead(Some(Box::new(read)))));
        });
    }

    /// What a button of the report strip asks.
    fn report_action(&mut self, action: contract::ReportAction) {
        use crate::reports::Offer;
        use contract::ReportAction as A;
        use udm_domain::report::{Kind, Program};
        let first = self.state.reports.first().cloned();
        let (reports, opener) = (Arc::clone(&self.ports.reports), Arc::clone(&self.ports.opener));
        match (action, first) {
            (A::Save, Some(Offer::Froze { draft: Some(report), .. })) => {
                apply(&mut self.state, UiMessage::ReportSaved(report.clone()));
                thread::spawn(move || reports.mark_offered(std::slice::from_ref(&report)));
            }
            (A::Save, Some(Offer::Froze { secs, draft: None })) => self.save_report(
                Program::Window,
                Kind::Freeze,
                format!("A window or the window's state thread stopped for {secs} seconds."),
            ),
            (A::Save, Some(Offer::Silent { secs })) => self.save_report(
                Program::Daemon,
                Kind::Silent,
                format!("The daemon had not answered a ping for {secs} seconds, with its connection open."),
            ),
            (A::Dismiss, first) => {
                if let Some(Offer::Froze { draft: Some(report), .. }) = first {
                    thread::spawn(move || {
                        if let Err(e) = udm_application::report::DiscardReport::new(&*reports).run(&report) {
                            tracing::warn!(error = %e, "the report was not deleted");
                        }
                    });
                }
                apply(&mut self.state, UiMessage::ReportDismissed);
            }
            (A::OpenIssue, Some(Offer::Saved { report, .. })) => {
                thread::spawn(move || {
                    let url = udm_domain::report::issue_url(&reports.this_copy(), Some(&report));
                    opener.open(&url);
                    opener.reveal(&report.path.display().to_string());
                });
            }
            (A::Show, Some(Offer::Saved { report, .. })) => {
                thread::spawn(move || opener.reveal(&report.path.display().to_string()));
            }
            (A::RestartDaemon, _) => {
                apply(&mut self.state, UiMessage::DaemonAnswering);
                self.connect_with(true);
            }
            (A::Reconnect, _) => self.connect(),
            (A::NewIssue, _) => {
                thread::spawn(move || opener.open(&udm_domain::report::issue_url(&reports.this_copy(), None)));
            }
            _ => {}
        }
    }

    /// Write a report on a worker, offering it once written.
    fn save_report(&mut self, program: udm_domain::report::Program, kind: udm_domain::report::Kind, what: String) {
        apply(&mut self.state, UiMessage::ReportSaving);
        let (outbox, reports, times) =
            (self.outbox.clone(), Arc::clone(&self.ports.reports), Arc::clone(&self.ports.times));
        thread::spawn(move || {
            let saved =
                udm_application::report::SaveReport::new(&*reports).run(program, kind, &what, times.unix_now() * 1000);
            let msg = match saved {
                Ok(report) => UiMessage::ReportSaved(report),
                Err(e) => {
                    tracing::warn!(error = %e, "the report was not saved");
                    UiMessage::ReportDismissed
                }
            };
            let _ = outbox.send(Inbox::Ui(msg));
        });
    }

    /// Read the projects folder on a worker.
    fn read_library(&self) {
        let outbox = self.outbox.clone();
        let library = self.ports.library.clone();
        thread::spawn(move || {
            let read = library.map(|l| Box::new(read(&*l)));
            let _ = outbox.send(Inbox::Ui(UiMessage::GrabLibraryRead(read)));
        });
    }

    fn grab_start(&mut self, draft: &contract::GrabDraft, reply: Sender<Result<(), contract::InvokeError>>) {
        let base = self.state.grab.library.as_ref().map_or_else(|| self.state.grab.request.clone(), |l| l.base.clone());
        let request = match crate::grabber::request(draft, &base) {
            Ok(r) => r,
            Err(e) => {
                let _ = reply.send(Err(crate::errors::grab(self.lang(), &e)));
                return;
            }
        };
        let lang = self.lang();
        if !self.state.connected() {
            let _ = reply.send(Err(crate::errors::offline(lang)));
            return;
        }
        let login = request.login_user.clone().filter(|_| !draft.login_password.is_empty());
        let (system, outbox, password) =
            (Arc::clone(&self.ports.system), self.outbox.clone(), draft.login_password.clone());
        // Keeping the password reaches the system's credential store, so it
        // runs on a worker before the crawl starts.
        thread::spawn(move || {
            if let Some(user) = login {
                let target = project_login_target(&request.start, &user);
                if let Err(e) = system.save_credential(&target, &user, &password) {
                    let _ = reply.send(Err(crate::errors::files(&e)));
                    return;
                }
            }
            let _ = outbox.send(Inbox::GrabBegin(Box::new(request)));
            let _ = reply.send(Ok(()));
        });
    }

    fn grab_add(&mut self, indices: &[u32], queue: QueueId, start: bool) -> Result<(), contract::InvokeError> {
        let lang = self.lang();
        let grab = &self.state.grab;
        let settings = self.state.settings.clone().unwrap_or_default();
        let login = grab.login_cookies.as_deref().or(grab.request.login_cookies.as_deref());
        let items: Vec<_> = crate::grabber::ticked(grab, indices)
            .iter()
            .filter_map(|f| self.ports.found.download_for(&grab.request, f, &settings, login))
            .collect();
        if items.is_empty() {
            return Err(crate::errors::invalid(lang, "Tick the files to add."));
        }
        let at_once = grab.request.download_at_once as usize;
        if let Some(q) = self.state.queues.iter().find(|q| q.id == queue && q.max_concurrent != at_once) {
            self.send(Command::SaveQueue { queue: udm_domain::queue::Queue { max_concurrent: at_once, ..q.clone() } });
        }
        self.sent(lang, Command::AddMany { items, queue: Some(queue), packaging: Packaging::None })?;
        if start {
            self.send(Command::StartQueue { id: queue });
        }
        Ok(())
    }

    fn grab_save(
        &mut self,
        name: &str,
        draft: &contract::GrabDraft,
        checked: &[bool],
        schedule: &contract::GrabScheduleDraft,
        reply: Sender<Result<(), contract::InvokeError>>,
    ) {
        let lang = self.lang();
        let Some(library) = self.ports.library.clone() else {
            let _ = reply.send(Err(crate::errors::no_projects(lang)));
            return;
        };
        if name.trim().is_empty() {
            let _ = reply.send(Err(crate::errors::invalid(lang, "Type a name for the project.")));
            return;
        }
        let request = match crate::grabber::request(draft, &self.state.grab.request) {
            Ok(r) => r,
            Err(e) => {
                let _ = reply.send(Err(crate::errors::grab(self.lang(), &e)));
                return;
            }
        };
        let project = match crate::grabber::with_schedule(
            crate::grabber::project(name, request, &self.state.grab, checked),
            schedule,
        ) {
            Ok(p) => p,
            Err(e) => {
                let _ = reply.send(Err(crate::errors::queue(self.lang(), &e)));
                return;
            }
        };
        let (system, outbox, password) =
            (Arc::clone(&self.ports.system), self.outbox.clone(), draft.login_password.clone());
        thread::spawn(move || {
            let result = (|| {
                if let Some(user) = project.request.login_user.clone().filter(|_| !password.is_empty()) {
                    let target = project_login_target(&project.request.start, &user);
                    system.save_credential(&target, &user, &password).map_err(|e| crate::errors::files(&e))?;
                }
                library.save(&project).map_err(|e| crate::errors::files(&e))
            })();
            if result.is_ok() {
                let _ = outbox.send(Inbox::Ui(UiMessage::GrabSaved(Box::new(project))));
                let _ = outbox.send(Inbox::Ui(UiMessage::GrabLibraryRead(Some(Box::new(read(&*library))))));
            }
            let _ = reply.send(result);
        });
    }

    /// Send `command`, as a command's result.
    fn sent(&self, lang: &'static crate::i18n::Language, command: Command) -> Result<(), contract::InvokeError> {
        if self.send(command) {
            Ok(())
        } else {
            Err(crate::errors::offline(lang))
        }
    }

    fn handle(&mut self, msg: Inbox) {
        match msg {
            Inbox::Ui(msg) => {
                // The categories panel is a setting both windows share.
                if matches!(msg, UiMessage::ToggleCategories) {
                    if let Some(mut settings) = self.state.settings.clone() {
                        settings.categories_panel = !self.state.categories_panel;
                        self.send(Command::SetSettings { settings });
                    }
                }
                if let UiMessage::Daemon { event, .. } = &msg {
                    if matches!(**event, Event::Show) {
                        windows::show_main(&self.app, self.lang());
                    }
                    if matches!(**event, Event::Quit) {
                        self.app.exit(0);
                    }
                }
                let picked = matches!(&msg, UiMessage::Daemon { event, .. } if matches!(**event, Event::PickLinks { .. } | Event::PickGallery { .. }));
                apply(&mut self.state, msg);
                // A page's links are probed for their sizes, and a gallery
                // page read, as their windows open.
                if picked {
                    if let Some(l) = self.state.asks.links.as_ref().filter(|l| l.info.is_empty()) {
                        let (request, links) = (l.request, crate::extras::link_probes(l));
                        self.send(Command::ProbeLinks { request, links });
                    }
                    if let Some(g) = self.state.asks.gallery.as_ref().filter(|g| g.result.is_none()) {
                        let (request, page, user_agent) = (g.request, g.page.clone(), g.user_agent.clone());
                        self.send(Command::ReadGallery { request, page, user_agent });
                    }
                }
            }
            Inbox::Subscribe { label, channel } => {
                tracing::info!(window = %label, "window subscribed");
                // Options shows the tools, remote control and the browsers.
                if label == Label::Options {
                    apply(&mut self.state, UiMessage::OptionsOpened);
                    self.send(Command::ListTools);
                    self.send(Command::RemoteStatus);
                    self.read_browsers();
                    self.read_adapters();
                }
                if let Label::Properties(id) = label {
                    self.read_type_name(id);
                }
                self.subscribers.insert(label, Subscriber { channel, seq: 0, last: None, look: None });
                self.update_wake_watch();
            }
            Inbox::Closed(text) => {
                self.ports.beats.forget(&text);
                let Some(label) = Label::parse(&text) else { return };
                self.subscribers.remove(&label);
                self.update_wake_watch();
                self.opened.remove(&label);
                self.titles.remove(&label);
                if self.closing.remove(&label) {
                    return;
                }
                // Closing the drop target hides it until shown again.
                if label == Label::DropTarget {
                    self.set_drop_target(false);
                }
                // Closing the countdown cancels it; closing the links stops
                // their probes.
                if label == Label::Countdown && self.state.asks.countdown.is_some() {
                    self.send(Command::CancelCountdown);
                }
                if label == Label::PickLinks {
                    self.send(Command::StopProbeLinks);
                }
                // Closing Options withdraws a pairing code it showed.
                if label == Label::Options && self.state.services.remote.as_ref().is_some_and(|r| r.pairing.is_some()) {
                    self.send(Command::CancelPairing);
                }
                // Closing Download File Info without answering cancels the
                // download, as its Cancel button does.
                if let Label::SaveAs(id) = label {
                    let waiting = self.state.table.get(id).is_some_and(|e| e.status == Status::Confirming);
                    if waiting && !self.state.answered.contains(&id) {
                        self.send(Command::Cancel { id });
                        apply(&mut self.state, UiMessage::Answered(id));
                        apply(&mut self.state, UiMessage::SaveAsCanceled(id));
                    }
                }
                apply(&mut self.state, UiMessage::WindowClosed(text));
            }
            Inbox::Act(action) => {
                let plan = actions::plan(&self.state, action);
                for m in plan.messages {
                    apply(&mut self.state, m);
                }
                for c in plan.commands {
                    self.send(c);
                }
            }
            Inbox::Send { command, reply } => {
                let sent = self.send(*command);
                if let Some(r) = reply {
                    let _ = r.send(sent);
                }
            }
            Inbox::Answer { answer, reply } => {
                let id = answer.id;
                let sent = self.send(udm_application::daemon::Command::Confirm(*answer));
                if sent {
                    apply(&mut self.state, UiMessage::Answered(id));
                }
                let _ = reply.send(sent);
            }
            Inbox::Remove { delete_files, dont_ask } => {
                for c in actions::remove(&self.state, delete_files) {
                    self.send(c);
                }
                apply(&mut self.state, UiMessage::RemoveClosed);
                if let Some(mut settings) = self.state.settings.clone().filter(|_| dont_ask) {
                    settings.confirm_remove = false;
                    self.send(Command::SetSettings { settings });
                }
            }
            Inbox::NoDoneDialogs => {
                if let Some(mut settings) = self.state.settings.clone() {
                    settings.notify = Notify::Toast;
                    self.send(Command::SetSettings { settings });
                }
            }
            Inbox::Open { id, how } => {
                let Some(path) = self.state.table.get(id).and_then(|e| e.path.clone()) else { return };
                let opener = Arc::clone(&self.ports.opener);
                // Opening starts another program; a worker keeps the state
                // thread free.
                thread::spawn(move || match how {
                    OpenHow::File => opener.open(&path),
                    OpenHow::Folder => match Path::new(&path).exists() {
                        true => opener.reveal(&path),
                        false => {
                            if let Some(dir) = Path::new(&path).parent() {
                                opener.open(&dir.display().to_string());
                            }
                        }
                    },
                });
            }
            Inbox::Limit { id, bytes_per_sec, remember } => {
                self.send(Command::SetSpeedLimit { id, bytes_per_sec, remember: Some(remember) });
            }
            Inbox::OnDone { id, on_done } => {
                self.send(Command::SetOnDone { id, on_done });
            }
            Inbox::OpenWindow(kind) => {
                let label = match kind {
                    contract::OpenWindow::AddUrl => Label::AddUrl,
                    contract::OpenWindow::About => Label::About,
                    // Options edits the daemon's settings, so it waits for them.
                    contract::OpenWindow::Options if self.state.settings.is_none() => return,
                    contract::OpenWindow::Options => Label::Options,
                    contract::OpenWindow::Scheduler => Label::Scheduler,
                    contract::OpenWindow::Batch => Label::Batch,
                    contract::OpenWindow::Main => Label::Main,
                    contract::OpenWindow::Grabber => {
                        if !self.state.grab.library_read {
                            self.read_library();
                        }
                        Label::Grabber
                    }
                };
                if let Err(e) = windows::open(&self.app, label, self.lang()) {
                    tracing::warn!(window = %label, error = %e, "a window did not open");
                }
            }
            Inbox::Browser(action) => self.browser_action(action),
            Inbox::Report(action) => self.report_action(action),
            Inbox::Clipboard(text) => self.clipboard_text(&text),
            Inbox::ClipboardBatch => {
                // The clipboard is read on a worker; its links open the
                // links window, as the menu's command does.
                let (outbox, clipboard) = (self.outbox.clone(), Arc::clone(&self.ports.clipboard));
                thread::spawn(move || {
                    let text: String = clipboard.text().unwrap_or_default().chars().take(1_000_000).collect();
                    let items = crate::tools::links_of(&text);
                    let msg = if items.is_empty() {
                        UiMessage::ClipboardEmpty
                    } else {
                        UiMessage::Links(Some(Box::new(crate::tools::LinkList { file: None, items, rejected: 0 })))
                    };
                    let _ = outbox.send(Inbox::Ui(msg));
                });
            }
            Inbox::ToggleLimiter => {
                if let Some(mut settings) = self.state.settings.clone() {
                    settings.speed_limit_enabled = !settings.speed_limit_enabled;
                    self.send(Command::SetSettings { settings });
                }
            }
            Inbox::ToggleDropTarget => {
                let on = self.state.settings.as_ref().is_some_and(|s| s.drop_target);
                self.set_drop_target(!on);
            }
            Inbox::Verify { id, text, reply } => self.verify(id, &text, reply),
            Inbox::DragFile { label, id } => {
                let path = self.state.table.get(id).filter(|e| e.status == Status::Done).and_then(|e| e.path.clone());
                if let Some(path) = path {
                    crate::drag::start(&self.app, &label.to_string(), path);
                }
            }
            Inbox::OpenWith(id) => {
                if let Some(path) = self.state.table.get(id).and_then(|e| e.path.clone()) {
                    let opener = Arc::clone(&self.ports.opener);
                    thread::spawn(move || opener.open_with(&path));
                }
            }
            Inbox::Exception { choice, stop_offering } => {
                let url = self.state.files.exception.clone();
                let next = url.zip(self.state.settings.as_ref()).and_then(|(url, current)| {
                    crate::filework::exception_settings(current, &url, choice, stop_offering)
                });
                if let Some(settings) = next {
                    self.send(Command::SetSettings { settings });
                }
                apply(&mut self.state, UiMessage::ExceptionAnswered);
            }
            Inbox::OpenThemesFolder => {
                let (themes, opener) = (Arc::clone(&self.ports.themes), Arc::clone(&self.ports.opener));
                thread::spawn(move || match themes.open_folder() {
                    Ok(folder) => opener.open(&folder.display().to_string()),
                    Err(e) => tracing::warn!(error = %e, "the themes folder did not open"),
                });
            }
            Inbox::PlaySound { sound, file } => {
                let system = Arc::clone(&self.ports.system);
                thread::spawn(move || {
                    let file = Some(file.trim()).filter(|f| !f.is_empty()).map(std::path::PathBuf::from);
                    system.play(sound, file.as_deref());
                });
            }
            Inbox::SystemProxy(reply) => {
                let system = Arc::clone(&self.ports.system);
                thread::spawn(move || {
                    let _ = reply.send(system.fixed_proxy());
                });
            }
            Inbox::SaveOptions { draft, reply } => {
                let lang = self.lang();
                let result = match self.state.settings.as_ref().map(|s| crate::options::settings(&draft, s)) {
                    None => Err(crate::errors::offline(lang)),
                    Some(Err(e)) => Err(crate::errors::options(lang, &e)),
                    Some(Ok(mut settings)) => {
                        if let Some(Ok(adapters)) = &self.state.services.adapters {
                            crate::options::renew_adapter(&draft, &mut settings, adapters);
                        }
                        // The debrid key goes on its own, after the
                        // settings, and only when it changes.
                        let key = crate::options::debrid_key(&draft);
                        let sent = self.send(Command::SetSettings { settings })
                            && key.is_none_or(|command| self.send(command));
                        match sent {
                            true => Ok(()),
                            false => Err(crate::errors::offline(lang)),
                        }
                    }
                };
                let _ = reply.send(result);
            }
            Inbox::SaveQueue { draft, reply } => {
                let lang = self.lang();
                let current = self.state.queues.iter().find(|q| q.id.get() == draft.id.0).cloned();
                let result = match current.map(|q| crate::scheduler::queue(&draft, &q)) {
                    None => Err(crate::errors::gone(lang)),
                    Some(Err(e)) => Err(crate::errors::queue(lang, &e)),
                    Some(Ok(queue)) => self.sent(lang, Command::SaveQueue { queue }),
                };
                let _ = reply.send(result);
            }
            Inbox::CreateQueue { name, reply } => {
                let lang = self.lang();
                let queue = udm_domain::queue::Queue::new(udm_domain::id::QueueId::NEW, name.trim());
                let result = match queue.validate() {
                    Err(e) => Err(crate::errors::queue(lang, &e)),
                    Ok(()) => self.sent(lang, Command::SaveQueue { queue }),
                };
                let _ = reply.send(result);
            }
            Inbox::SaveQuota { quota, reply } => {
                let lang = self.lang();
                let result = match self.state.settings.as_ref().map(|s| crate::scheduler::quota_settings(quota, s)) {
                    None => Err(crate::errors::offline(lang)),
                    Some(Err(e)) => Err(crate::errors::settings(lang, &e)),
                    Some(Ok(settings)) => self.sent(lang, Command::SetSettings { settings }),
                };
                let _ = reply.send(result);
            }
            Inbox::MoveQueued { id, offset } => {
                if let Some((queue, position)) = crate::scheduler::moved(&self.state.queues, id, offset) {
                    self.send(Command::MoveToQueue { id, queue, position: Some(position) });
                }
            }
            Inbox::GrabStart { draft, reply } => self.grab_start(&draft, reply),
            Inbox::GrabBegin(request) => {
                apply(&mut self.state, UiMessage::GrabStarted(request.clone()));
                self.send(Command::Grab(*request));
            }
            Inbox::GrabStop => {
                self.send(Command::CancelGrab);
            }
            Inbox::GrabAdd { indices, queue, start, reply } => {
                let _ = reply.send(self.grab_add(&indices, queue, start));
            }
            Inbox::GrabTemplate { name, reply } => {
                let lang = self.lang();
                let template =
                    self.state.grab.library.as_ref().and_then(|l| l.templates.iter().find(|t| t.name == name));
                let _ = reply
                    .send(template.map(|t| crate::grabber::draft(&t.request)).ok_or_else(|| crate::errors::gone(lang)));
            }
            Inbox::GrabOpen { name, reply } => {
                let lang = self.lang();
                let Some(library) = self.ports.library.clone() else {
                    let _ = reply.send(Err(crate::errors::no_projects(lang)));
                    return;
                };
                let outbox = self.outbox.clone();
                thread::spawn(move || {
                    let result = match library.load(&name) {
                        Ok(project) => {
                            let opened = contract::GrabOpened {
                                name: project.name.clone(),
                                draft: crate::grabber::draft(&project.request),
                            };
                            let _ = outbox.send(Inbox::Ui(UiMessage::GrabOpened(Box::new(project))));
                            Ok(opened)
                        }
                        Err(e) => Err(crate::errors::files(&e)),
                    };
                    let _ = reply.send(result);
                });
            }
            Inbox::GrabSave { name, draft, checked, schedule, reply } => {
                self.grab_save(&name, &draft, &checked, &schedule, reply)
            }
            Inbox::GrabSaveTemplate { name, draft, reply } => {
                let lang = self.lang();
                let (Some(library), true) = (self.ports.library.clone(), !name.trim().is_empty()) else {
                    let _ = reply.send(Err(crate::errors::no_projects(lang)));
                    return;
                };
                match crate::grabber::request(&draft, &self.state.grab.request) {
                    Err(e) => {
                        let _ = reply.send(Err(crate::errors::grab(self.lang(), &e)));
                    }
                    Ok(request) => {
                        let outbox = self.outbox.clone();
                        thread::spawn(move || {
                            let result =
                                library.save_template(name.trim(), &request).map_err(|e| crate::errors::files(&e));
                            let _ = outbox.send(Inbox::Ui(UiMessage::GrabLibraryRead(Some(Box::new(read(&*library))))));
                            let _ = reply.send(result);
                        });
                    }
                }
            }
            Inbox::GrabSaveFilters { filters, reply } => {
                let lang = self.lang();
                let Some(library) = self.ports.library.clone() else {
                    let _ = reply.send(Err(crate::errors::no_projects(lang)));
                    return;
                };
                let outbox = self.outbox.clone();
                thread::spawn(move || {
                    let result = library.save_filters(&filters).map_err(|e| crate::errors::files(&e));
                    let _ = outbox.send(Inbox::Ui(UiMessage::GrabLibraryRead(Some(Box::new(read(&*library))))));
                    let _ = reply.send(result);
                });
            }
            Inbox::GrabAddOnClose(on) => {
                if let Some(mut settings) = self.state.settings.clone() {
                    settings.grab_add_on_close = on;
                    self.send(Command::SetSettings { settings });
                }
            }
            Inbox::Find { input, reply } => {
                let found = crate::tools::find(&self.state, &input);
                if let Some(id) = found {
                    apply(&mut self.state, UiMessage::Select(vec![id]));
                }
                let _ = reply.send(found.is_some());
            }
            Inbox::ExportText { scope, format, reply } => {
                let entries = crate::tools::export_entries(&self.state, scope);
                let format = match format {
                    contract::ExportFormat::Ef2 => ListFormat::Ef2,
                    contract::ExportFormat::Text => ListFormat::Text,
                    contract::ExportFormat::Json => ListFormat::Json,
                };
                let _ = reply.send((self.ports.lists.export(format, &entries), entries.len()));
            }
            Inbox::Limiter { draft, reply } => {
                let lang = self.lang();
                let result = match self.state.settings.as_ref().map(|s| crate::tools::limiter_settings(&draft, s)) {
                    None => Err(crate::errors::offline(lang)),
                    Some(Err(e)) => Err(crate::errors::settings(lang, &e)),
                    Some(Ok(settings)) => self.sent(lang, Command::SetSettings { settings }),
                };
                let _ = reply.send(result);
            }
            Inbox::Category { original, input, reply } => {
                let lang = self.lang();
                let result = match self
                    .state
                    .settings
                    .as_ref()
                    .map(|s| crate::tools::with_category(s, original.as_deref(), input.as_deref()))
                {
                    None => Err(crate::errors::offline(lang)),
                    Some(Err(e)) => Err(crate::errors::category(lang, &e)),
                    Some(Ok(settings)) => self.sent(lang, Command::SetSettings { settings }),
                };
                let _ = reply.send(result);
            }
            Inbox::BrowseCategory(name) => {
                let settings = self.state.settings.clone().unwrap_or_default();
                let (folders, opener) = (Arc::clone(&self.ports.folders), Arc::clone(&self.ports.opener));
                thread::spawn(move || {
                    let folder = settings.category_folder(&name, &folders.downloads_dir());
                    opener.open(&folder.display().to_string());
                });
            }
            Inbox::QueueSelection(queue) => {
                for id in self.state.selected.clone() {
                    self.send(match queue {
                        Some(queue) => Command::MoveToQueue { id, queue, position: None },
                        None => Command::RemoveFromQueue { id },
                    });
                }
            }
            Inbox::Properties { id, input, reply } => {
                let lang = self.lang();
                let result = match self.state.table.get(id) {
                    None => Err(crate::errors::gone(lang)),
                    Some(e) => {
                        let changes = crate::tools::properties_changes(&input, e);
                        self.sent(lang, Command::SetProperties { id, changes })
                    }
                };
                let _ = reply.send(result);
            }
            Inbox::AddItems { items, queue, start, reply } => {
                let _ = reply.send(self.add_items(items, queue, start));
            }
            Inbox::AddLinks { indices, queue, start, reply } => {
                let items: Vec<_> = self
                    .state
                    .links
                    .as_ref()
                    .map(|l| indices.iter().filter_map(|i| l.items.get(*i as usize).cloned()).collect())
                    .unwrap_or_default();
                let result = self.add_items(items, queue, start);
                if result.is_ok() {
                    apply(&mut self.state, UiMessage::Links(None));
                }
                let _ = reply.send(result);
            }
            Inbox::DropTarget(on) => self.set_drop_target(on),
            Inbox::Login { user, password, save, reply } => {
                let _ = reply.send(self.answer_login(&user, &password, save));
            }
            Inbox::AddPicks { label, input, reply } => {
                let _ = reply.send(self.add_picks(label, &input));
            }
            Inbox::LinkNames { indices, rename, reply } => {
                let lang = self.lang();
                let names = match self.state.asks.links.as_ref() {
                    Some(l) => crate::extras::renamed_links(l, &indices, &rename).map_err(|e| pick_error(lang, &e)),
                    None => Err(crate::errors::gone(lang)),
                };
                let _ = reply.send(names);
            }
            Inbox::OpenPackage(id) => {
                let folder = self.state.asks.packages.iter().find(|p| p.id == id).and_then(|p| match &p.unpack {
                    udm_domain::package::Unpack::Done { folder, .. } => Some(folder.clone()),
                    _ => p.folder.clone(),
                });
                if let Some(folder) = folder {
                    let opener = Arc::clone(&self.ports.opener);
                    thread::spawn(move || opener.open(&folder));
                }
            }
            Inbox::DismissRelease => {
                if let Some(Ok(Some(r))) = &self.state.asks.update {
                    self.state
                        .layout_rest
                        .insert("dismissed_release".into(), serde_json::Value::String(r.version.clone()));
                }
            }
            Inbox::TipClosed { next, on_startup } => {
                apply(&mut self.state, UiMessage::TipSeen);
                if let Some(mut settings) = self.state.settings.clone() {
                    settings.next_tip = next as usize % crate::tips::TIPS.len();
                    settings.tips_on_startup = on_startup;
                    self.send(Command::SetSettings { settings });
                }
            }
            Inbox::Reconnect => self.connect(),
        }
    }

    /// Open the windows the state asks for and close those it no longer
    /// does.
    fn reconcile(&mut self) {
        let wanted = windows::wanted(&self.state);
        let lang = self.lang();
        for label in wanted.difference(&self.opened.clone()) {
            match windows::open(&self.app, *label, lang) {
                Ok(_) => {
                    self.opened.insert(*label);
                }
                Err(e) => tracing::warn!(window = %label, error = %e, "a window did not open"),
            }
        }
        let stale: Vec<Label> = self.opened.difference(&wanted).copied().collect();
        for label in stale {
            self.closing.insert(label);
            self.opened.remove(&label);
            windows::close(&self.app, label);
        }
    }

    /// The local time's offset from UTC, rounded to the quarter hour.
    fn offset(&self) -> i64 {
        let now = self.ports.clock.now();
        let local = now.day * 1440 + i64::from(now.minute);
        let utc = self.ports.times.unix_now() as i64 / 60;
        ((local - utc) as f64 / 15.0).round() as i64 * 15
    }

    fn publish(&mut self) {
        let today = {
            let (y, m, d) = udm_domain::queue::civil_from_days(self.ports.clock.now().day);
            format!("{y:04}-{m:02}-{d:02}")
        };
        let ctx = Ctx {
            lang: self.lang(),
            offset: self.offset(),
            today,
            now: Instant::now(),
            unix: self.ports.times.unix_now(),
            downloads: self.downloads.clone(),
            flags: !cfg!(windows),
        };
        let index = crate::i18n::LANGUAGES.iter().position(|l| l.code == ctx.lang.code).unwrap_or(0);
        self.ports.language.store(index, Ordering::Relaxed);
        let look = projection::look(&self.state);
        crate::windows::set_zoom(look.zoom);
        let monitor = self.state.settings.as_ref().is_some_and(|s| s.clipboard_monitor);
        self.watching_clipboard.store(monitor, Ordering::Relaxed);
        let (limiter, drop_target) =
            self.state.settings.as_ref().map_or((false, false), |s| (s.speed_limit_enabled, s.drop_target));
        crate::tray::show(&self.app, limiter, drop_target, &projection::main::tooltip(&ctx, &self.state));
        let mut gone = Vec::new();
        let mut titles = Vec::new();
        for (label, sub) in &mut self.subscribers {
            let Some(view) = projection::view(&ctx, &self.state, label) else {
                let _ = sub.channel.send(Update::Gone { seq: sub.seq });
                gone.push(*label);
                continue;
            };
            if let View::Download(d) = &view {
                titles.push((*label, d.title.clone()));
            }
            let update = match &sub.last {
                None => Some(Update::Snapshot { seq: sub.seq, look: look.clone(), view: view.clone() }),
                Some(before) => {
                    let new_look = (sub.look.as_ref() != Some(&look)).then(|| look.clone());
                    let patch = match (before, &view) {
                        (View::Main(a), View::Main(b)) => main::diff(a, b).map(Patch::Main),
                        (a, b) if a != b => Some(Patch::Whole(b.clone())),
                        _ => None,
                    };
                    match (patch, new_look) {
                        (None, None) => None,
                        (Some(patch), look) => Some(Update::Patch { seq: sub.seq, look, patch }),
                        (None, Some(look)) => {
                            Some(Update::Patch { seq: sub.seq, look: Some(look), patch: Patch::Whole(view.clone()) })
                        }
                    }
                }
            };
            if let Some(update) = update {
                if sub.channel.send(update).is_err() {
                    gone.push(*label);
                    continue;
                }
                sub.seq = sub.seq.wrapping_add(1);
            }
            sub.last = Some(view);
            sub.look = Some(look.clone());
        }
        for label in gone {
            self.subscribers.remove(&label);
        }
        for (label, title) in titles {
            if self.titles.get(&label) != Some(&title) {
                windows::set_title(&self.app, label, &title);
                self.titles.insert(label, title);
            }
        }
    }
}

/// What the projects folder holds. Reads files, so it runs on a worker.
fn read(library: &dyn GrabLibrary) -> crate::grabber::Library {
    crate::grabber::Library {
        base: library.template(),
        templates: library.templates(),
        filters: library.filters(),
        recent: library.recent(),
    }
}

/// The browsers UDM knows, then the ones the user added that it can tell.
/// Reads the disk: for workers.
fn read_browsers(
    registry: &dyn BrowserRegistry,
    added: &[udm_domain::settings::AddedBrowser],
) -> crate::services::BrowsersRead {
    let mut rows: Vec<crate::services::BrowserRow> = registry
        .known()
        .into_iter()
        .map(|(browser, registered)| crate::services::BrowserRow {
            name: browser.name.clone(),
            can_restart: registry.can_restart(&browser, None),
            browser,
            added: None,
            known: true,
            registered,
        })
        .collect();
    for a in added {
        let program = std::path::Path::new(&a.program);
        if let Some(target) = registry.identify(program) {
            rows.push(crate::services::BrowserRow {
                registered: registry.registered(&target.browser),
                can_restart: registry.can_restart(&target.browser, Some(program)),
                browser: target.browser,
                name: a.name.clone(),
                added: Some(a.clone()),
                known: target.known,
            });
        }
    }
    crate::services::BrowsersRead { rows, chrome_readers: registry.chrome_readers() }
}
