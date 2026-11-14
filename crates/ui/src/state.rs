//! The window's state (AGENTS.md section 7.1): what every window draws
//! from, the messages that change it, and [`apply`], the one function that
//! does. One thread, the state actor (`actor.rs`), owns it.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use udm_application::daemon::{self, Event, Lost};
use udm_domain::bytes::ByteCount;
use udm_domain::download::{SegmentInfo, Status};
use udm_domain::id::{DownloadId, QueueId};
use udm_domain::queue::Queue;
use udm_domain::settings::{Notify, Settings};
use udm_domain::table::DownloadTable;

use crate::contract::{ColumnKey, SortDir};

/// Ignore speed samples closer together than this; they are too noisy.
const SPEED_MIN_INTERVAL: Duration = Duration::from_millis(400);

/// The narrowest and widest a column can be dragged.
pub const WIDTH_RANGE: (f32, f32) = (24.0, 800.0);

/// The connection to UDM's background service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Connected,
    Lost(String),
}

/// Bytes per second, smoothed across progress events.
#[derive(Debug, Clone, Copy)]
pub struct Speed {
    at: Instant,
    done: ByteCount,
    pub bytes_per_sec: f64,
}

/// Downloads by how far they got, the tree's three top branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    All,
    Unfinished,
    Finished,
}

/// The categories tree's node the list shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Downloads {
        progress: Progress,
        category: Option<String>,
    },
    Queue(QueueId),
    /// A package's downloads.
    Package(udm_domain::id::PackageId),
}

impl Default for Node {
    fn default() -> Self {
        Node::Downloads { progress: Progress::All, category: None }
    }
}

/// The column to sort by, and which way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub column: ColumnKey,
    pub dir: SortDir,
}

/// The list's font, from View > Font.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ListFont {
    /// The fixed-width font rather than the window's own.
    pub mono: bool,
    /// In points, 8 to 24.
    pub size: f32,
}

/// A message under the toolbar.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub text: String,
    pub error: bool,
}

/// Everything the windows draw from. Only [`apply`] changes it.
pub struct AppState {
    pub link: Link,
    pub table: DownloadTable,
    pub settings: Option<Settings>,
    pub queues: Vec<Queue>,
    pub speeds: HashMap<DownloadId, Speed>,
    pub segments: HashMap<DownloadId, Vec<SegmentInfo>>,
    pub daemon_version: Option<String>,
    /// The rows selected in the main window, in the order picked.
    pub selected: Vec<DownloadId>,
    pub node: Node,
    /// The tree's branches that are open, by key.
    pub open_nodes: BTreeSet<String>,
    pub sort: Option<Sort>,
    /// The columns shown, left to right, with their widths.
    pub columns: Vec<(ColumnKey, f32)>,
    pub categories_panel: bool,
    pub notice: Option<Notice>,
    /// The downloads the Confirm deletion dialog asks about.
    pub to_remove: Vec<DownloadId>,
    /// Downloads whose progress window is open.
    pub progress: BTreeSet<DownloadId>,
    /// Finished downloads whose completion dialog is open.
    pub done: BTreeSet<DownloadId>,
    /// Downloads answered in their Download File Info dialog, until the
    /// daemon says they are no longer waiting for it.
    pub answered: BTreeSet<DownloadId>,
    /// The system's languages, best first, for an empty Language setting.
    pub system_languages: Vec<String>,
    /// The site grabber.
    pub grab: crate::grabber::GrabState,
    /// Links to pick from: the clipboard's, or an imported list's.
    pub links: Option<crate::tools::LinkList>,
    /// Downloads whose properties window is open.
    pub properties: BTreeSet<DownloadId>,
    /// The categories panel's width.
    pub panel_width: f32,
    pub list_font: Option<ListFont>,
    pub toolbar_small: bool,
    /// The toolbar's buttons and separators, in order.
    pub toolbar: Vec<String>,
    /// What the layout file holds that this window does not use.
    pub layout_rest: serde_json::Map<String, serde_json::Value>,
    /// The tip of the day was shown, or dismissed, since the window opened.
    pub tip_seen: bool,
    /// What the daemon asks of the window beyond the list.
    pub asks: crate::extras::Asks,
    /// What Options shows beyond the settings.
    pub services: crate::services::Services,
    /// The problem reports to offer.
    pub reports: crate::reports::Offers,
    /// A link the clipboard monitor caught, for Add URL.
    pub clipboard_offer: Option<String>,
    /// The zip preview and the capture exception offer.
    pub files: crate::filework::FileWork,
    /// The system has an Open with picker.
    pub can_open_with: bool,
    /// What the system can do, read once when the window starts.
    pub system: SystemFacts,
    /// Whether the power plan lets programs wake the computer, on mains
    /// power and on battery, as a worker last read it while the Scheduler or
    /// the grabber was open. `None` until read, or where it cannot be.
    pub wake_timers: Option<(bool, bool)>,
    /// What the system calls files of each extension (lower case), as
    /// workers read them for File Properties; `None` where it names none.
    pub type_names: HashMap<String, Option<String>>,
}

/// What the system can do, which some windows leave off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemFacts {
    /// Download File Info can offer administrator rights for a folder UDM
    /// may not write to.
    pub can_elevate: bool,
    /// A finished file's creation time can be set.
    pub creation_time: bool,
    /// A portable copy, which does not start when the user signs in.
    pub portable: bool,
}

impl Default for AppState {
    fn default() -> Self {
        AppState {
            link: Link::Connecting,
            table: DownloadTable::new(),
            settings: None,
            queues: Vec::new(),
            speeds: HashMap::new(),
            segments: HashMap::new(),
            daemon_version: None,
            selected: Vec::new(),
            node: Node::default(),
            open_nodes: ["all".to_string()].into(),
            sort: None,
            columns: DEFAULT_COLUMNS.to_vec(),
            categories_panel: true,
            notice: None,
            to_remove: Vec::new(),
            progress: BTreeSet::new(),
            done: BTreeSet::new(),
            answered: BTreeSet::new(),
            system_languages: Vec::new(),
            grab: crate::grabber::GrabState::default(),
            links: None,
            properties: BTreeSet::new(),
            panel_width: 143.0,
            list_font: None,
            toolbar_small: false,
            toolbar: crate::layout::TOOLBAR.iter().map(|s| s.to_string()).collect(),
            layout_rest: serde_json::Map::new(),
            tip_seen: false,
            asks: crate::extras::Asks::default(),
            services: crate::services::Services::default(),
            reports: crate::reports::Offers::default(),
            clipboard_offer: None,
            files: crate::filework::FileWork::default(),
            can_open_with: false,
            system: SystemFacts::default(),
            wake_timers: None,
            type_names: HashMap::new(),
        }
    }
}

/// The columns and widths of a new window, in the order of the download
/// manager UDM's list follows.
pub const DEFAULT_COLUMNS: [(ColumnKey, f32); 8] = [
    (ColumnKey::Name, 150.0),
    (ColumnKey::Queue, 22.0),
    (ColumnKey::Size, 70.0),
    (ColumnKey::Status, 72.0),
    (ColumnKey::TimeLeft, 75.0),
    (ColumnKey::Speed, 95.0),
    (ColumnKey::LastTry, 65.0),
    (ColumnKey::Description, 120.0),
];

/// What changes the state.
#[derive(Debug)]
pub enum UiMessage {
    /// An event from the daemon, with when it came.
    Daemon {
        event: Box<Event>,
        at: Instant,
        unix: u64,
    },
    Select(Vec<DownloadId>),
    SelectAll,
    SelectNode(Node),
    ToggleNode(String),
    Sort(ColumnKey),
    Resize(ColumnKey, f32),
    ToggleCategories,
    /// Ask whether to remove the selected downloads.
    AskRemove,
    /// The Confirm deletion dialog closed, answered or not.
    RemoveClosed,
    ShowProgress(DownloadId),
    /// The user closed one of the shell's windows by its label.
    WindowClosed(String),
    /// The Download File Info dialog of a download was answered.
    Answered(DownloadId),
    SystemLanguages(Vec<String>),
    /// A crawl for this request began.
    GrabStarted(Box<udm_domain::grab::GrabRequest>),
    /// The projects folder was read, or there is none.
    GrabLibraryRead(Option<Box<crate::grabber::Library>>),
    /// A project was opened, or saved.
    GrabOpened(Box<udm_domain::grab::Project>),
    GrabSaved(Box<udm_domain::grab::Project>),
    /// A new project, without results.
    GrabReset,
    /// The grabber's results tree's node.
    GrabNode(String),
    /// Links to pick from, or none.
    Links(Option<Box<crate::tools::LinkList>>),
    ShowProperties(DownloadId),
    /// The column chooser's choice.
    Columns(Vec<(ColumnKey, f32)>),
    /// The layout file, read when the window starts.
    LayoutRead(Box<crate::layout::LayoutFile>),
    PanelWidth(f32),
    ListFont(Option<ListFont>),
    Toolbar {
        small: bool,
        buttons: Vec<String>,
    },
    /// The tip of the day was closed.
    TipSeen,
    /// Open the New link window for a download.
    ShowRefresh(DownloadId),
    /// These downloads got their login.
    LoginAnswered(Vec<DownloadId>),
    /// Ask a package's password.
    AskPassword(udm_domain::id::PackageId),
    /// The update check, or Install now, began.
    UpdateChecking,
    Installing,
    /// Installing a tool began.
    ToolInstalling(udm_domain::tools::Tool),
    /// Check account asked the debrid service about the account.
    DebridChecking,
    /// The browsers a worker read; `None` while they are read again.
    BrowsersRead(Option<Box<crate::services::BrowsersRead>>),
    /// What a browser change said.
    BrowserNote(Option<crate::contract::Banner>),
    /// The theme files and the system's contrast colors.
    ThemesRead(Box<udm_application::themes::ThemesRead>),
    /// Reports a crash or an earlier stall left, newest first.
    ReportsFound(Vec<udm_domain::report::SavedReport>),
    /// A window or the state thread stood still; the report written then.
    Froze {
        lasted_ms: u64,
        draft: Option<udm_domain::report::SavedReport>,
    },
    /// The daemon has not answered a ping for these seconds.
    DaemonSilent(u64),
    DaemonAnswering,
    ReportSaving,
    ReportSaved(udm_domain::report::SavedReport),
    /// The first report offer goes.
    ReportDismissed,
    /// The clipboard monitor caught a link to offer.
    ClipboardOffer(String),
    /// List the files inside a waiting zip download.
    PreviewZip(DownloadId),
    /// A captured download's save-as dialog was canceled.
    SaveAsCanceled(DownloadId),
    /// The offer to keep a download in the browser was answered.
    ExceptionAnswered,
    /// The tray's clipboard batch found no link.
    ClipboardEmpty,
    /// The command line given at start could not be passed on, and why.
    StartupError(String),
    /// What the system can do, read at start.
    SystemRead(SystemFacts),
    /// The power plan's wake timers, on mains power and on battery.
    WakeTimersRead(Option<(bool, bool)>),
    /// The link picker's filters changed.
    LinkFilter(crate::contract::LinkFilter),
    /// What the system calls files with extension `ext` (lower case).
    TypeNameRead {
        ext: String,
        name: Option<String>,
    },
}

impl AppState {
    /// Whether finished downloads get the completion dialog.
    pub fn dialog_mode(&self) -> bool {
        self.settings.as_ref().is_some_and(|s| s.notify == Notify::Dialog)
    }

    pub fn connected(&self) -> bool {
        self.link == Link::Connected
    }

    fn sample_speed(&mut self, id: DownloadId, done: ByteCount, now: Instant) {
        let s = self.speeds.entry(id).or_insert(Speed { at: now, done, bytes_per_sec: 0.0 });
        let dt = now.saturating_duration_since(s.at);
        if dt < SPEED_MIN_INTERVAL || done < s.done {
            return;
        }
        let instant = (done - s.done).as_f64() / dt.as_secs_f64();
        s.bytes_per_sec = if s.bytes_per_sec == 0.0 { instant } else { 0.7 * s.bytes_per_sec + 0.3 * instant };
        s.at = now;
        s.done = done;
    }

    /// A running download's speed, when it has one.
    pub fn speed(&self, id: DownloadId) -> Option<f64> {
        self.speeds.get(&id).map(|s| s.bytes_per_sec).filter(|s| *s > 0.0)
    }
}

/// Apply `msg` to `state`. The one function that changes the state.
pub fn apply(state: &mut AppState, msg: UiMessage) {
    match msg {
        UiMessage::Daemon { event, at, unix } => apply_event(state, *event, at, unix),
        UiMessage::Select(ids) => {
            state.selected = ids.into_iter().filter(|id| state.table.get(*id).is_some()).collect();
        }
        UiMessage::SelectAll => {
            state.selected = crate::projection::main::shown(state).iter().map(|e| e.id).collect();
        }
        UiMessage::SelectNode(node) => {
            if state.node != node {
                state.node = node;
                state.selected.clear();
            }
        }
        UiMessage::ToggleNode(key) => {
            if !state.open_nodes.remove(&key) {
                state.open_nodes.insert(key);
            }
        }
        UiMessage::Sort(column) => {
            if column == ColumnKey::Queue {
                return;
            }
            state.sort = match state.sort {
                Some(s) if s.column == column && s.dir == SortDir::Ascending => {
                    Some(Sort { column, dir: SortDir::Descending })
                }
                Some(s) if s.column == column => None,
                _ => Some(Sort { column, dir: SortDir::Ascending }),
            };
        }
        UiMessage::Resize(column, width) => {
            if let Some(c) = state.columns.iter_mut().find(|(k, _)| *k == column) {
                c.1 = width.clamp(WIDTH_RANGE.0, WIDTH_RANGE.1);
            }
        }
        UiMessage::ToggleCategories => state.categories_panel = !state.categories_panel,
        UiMessage::AskRemove => {
            if !state.selected.is_empty() {
                state.to_remove = state.selected.clone();
            }
        }
        UiMessage::RemoveClosed => state.to_remove.clear(),
        UiMessage::ShowProgress(id) => {
            if state.table.get(id).is_some_and(|e| e.status != Status::Done) {
                state.progress.insert(id);
            }
        }
        UiMessage::WindowClosed(label) => match crate::windows::Label::parse(&label) {
            Some(crate::windows::Label::Download(id)) => {
                state.progress.remove(&id);
            }
            Some(crate::windows::Label::Done(id)) => {
                state.done.remove(&id);
            }
            Some(crate::windows::Label::Remove) => state.to_remove.clear(),
            Some(crate::windows::Label::AddUrl) => state.clipboard_offer = None,
            Some(crate::windows::Label::ZipPreview) => state.files.zip = None,
            Some(crate::windows::Label::Exception) => state.files.exception = None,
            Some(crate::windows::Label::Links) => state.links = None,
            Some(crate::windows::Label::Login) => {
                // Closing it answers the first site's downloads with nothing.
                if let Some((ids, _)) = crate::extras::login_group(state) {
                    state.asks.login_wanted.retain(|id| !ids.contains(id));
                }
            }
            Some(crate::windows::Label::Refresh) => {
                state.asks.refresh = None;
                state.asks.refresh_mismatch = None;
            }
            Some(crate::windows::Label::Password) => state.asks.password = None,
            Some(crate::windows::Label::PickLinks) => state.asks.links = None,
            Some(crate::windows::Label::PickVideos) => state.asks.videos = None,
            Some(crate::windows::Label::Gallery) => state.asks.gallery = None,
            Some(crate::windows::Label::Properties(id)) => {
                state.properties.remove(&id);
            }
            _ => {}
        },
        UiMessage::Answered(id) => {
            state.answered.insert(id);
        }
        UiMessage::SystemLanguages(tags) => state.system_languages = tags,
        UiMessage::GrabStarted(request) => state.grab.started(*request),
        UiMessage::GrabLibraryRead(library) => {
            state.grab.library = library.map(|l| *l);
            state.grab.library_read = true;
        }
        UiMessage::GrabOpened(project) => state.grab.opened(*project),
        UiMessage::GrabSaved(project) => state.grab.project = Some(*project),
        UiMessage::GrabReset => state.grab.reset(),
        UiMessage::GrabNode(key) => state.grab.node = key,
        UiMessage::Links(list) => state.links = list.map(|l| *l),
        UiMessage::ShowProperties(id) => {
            if state.table.get(id).is_some() {
                state.properties.insert(id);
            }
        }
        UiMessage::LayoutRead(file) => crate::layout::apply(state, &file),
        UiMessage::PanelWidth(w) => state.panel_width = w.clamp(100.0, 400.0),
        UiMessage::ListFont(font) => state.list_font = font.map(|f| ListFont { size: f.size.clamp(8.0, 24.0), ..f }),
        UiMessage::Toolbar { small, buttons } => {
            state.toolbar_small = small;
            let known = |id: &String| id == crate::layout::SEPARATOR || crate::layout::TOOLBAR.contains(&id.as_str());
            state.toolbar = buttons.into_iter().filter(known).take(64).collect();
        }
        UiMessage::TipSeen => state.tip_seen = true,
        UiMessage::ShowRefresh(id) => {
            if crate::extras::can_refresh(state, id) {
                state.asks.refresh = Some(id);
                state.asks.refresh_mismatch = None;
            }
        }
        UiMessage::LoginAnswered(ids) => state.asks.login_wanted.retain(|id| !ids.contains(id)),
        UiMessage::AskPassword(id) => state.asks.password = Some(id),
        UiMessage::UpdateChecking => {
            state.asks.checking = true;
            state.asks.update = None;
        }
        UiMessage::Installing => state.asks.installing = Some(None),
        UiMessage::ToolInstalling(tool) => {
            if !state.services.tool_busy.contains(&tool) {
                state.services.tool_busy.push(tool);
            }
            state.services.tool_done = None;
        }
        UiMessage::DebridChecking => {
            state.services.debrid_checking = true;
            state.services.debrid = None;
        }
        UiMessage::BrowsersRead(read) => state.services.browsers = read.map(|r| *r),
        UiMessage::BrowserNote(note) => state.services.browser_note = note,
        UiMessage::ThemesRead(read) => state.services.themes = *read,
        UiMessage::ReportsFound(found) => state.reports.found(found),
        UiMessage::Froze { lasted_ms, draft } => state.reports.froze(lasted_ms, draft),
        UiMessage::DaemonSilent(secs) => state.reports.silent(secs),
        UiMessage::DaemonAnswering => state.reports.answering(),
        UiMessage::ReportSaving => state.reports.saving(),
        UiMessage::ReportSaved(report) => state.reports.saved(report),
        UiMessage::ReportDismissed => state.reports.dismiss(),
        UiMessage::ClipboardOffer(url) => state.clipboard_offer = Some(url),
        UiMessage::PreviewZip(id) => state.files.zip = Some(crate::filework::ZipPreview { id, result: None }),
        UiMessage::SaveAsCanceled(id) => crate::filework::canceled(state, id),
        UiMessage::ExceptionAnswered => state.files.exception = None,
        UiMessage::ClipboardEmpty => {
            let text =
                crate::projection::language(state).tr("The clipboard holds no http, https or ftp link.").to_string();
            state.notice = Some(Notice { text, error: false });
        }
        UiMessage::StartupError(text) => state.notice = Some(Notice { text, error: false }),
        UiMessage::SystemRead(facts) => state.system = facts,
        UiMessage::LinkFilter(filter) => {
            if let Some(l) = state.asks.links.as_mut() {
                l.filter = filter;
            }
        }
        UiMessage::WakeTimersRead(read) => state.wake_timers = read,
        UiMessage::TypeNameRead { ext, name } => {
            state.type_names.insert(ext, name);
        }
        UiMessage::Columns(columns) => {
            if !columns.is_empty() {
                state.columns = columns;
            }
        }
    }
}

fn apply_event(state: &mut AppState, event: Event, at: Instant, unix: u64) {
    daemon::apply(&mut state.table, &event, unix);
    crate::extras::take(state, &event, at);
    crate::services::take(state, &event);
    crate::filework::take(state, &event);
    match event {
        Event::Connected => {
            state.link = Link::Connected;
            if state.notice.as_ref().is_some_and(|n| n.error) {
                state.notice = None;
            }
        }
        Event::Lost(lost) => {
            state.link = Link::Lost(match lost {
                Lost::Closed => String::new(),
                Lost::Failed(why) => why,
            });
            state.speeds.clear();
        }
        Event::Hello { version, .. } => state.daemon_version = Some(version),
        Event::Settings { settings } => {
            state.categories_panel = settings.categories_panel;
            state.settings = Some(settings);
        }
        Event::Queues { items } => state.queues = items,
        Event::Started { id, .. } => {
            state.speeds.remove(&id);
            state.done.remove(&id);
            if state.settings.as_ref().is_some_and(|s| s.progress_on_start) {
                state.progress.insert(id);
            }
        }
        Event::Progress { id, done, segments, .. } => {
            state.sample_speed(id, done, at);
            if let Some(segments) = segments {
                state.segments.insert(id, segments);
            }
        }
        Event::Done { id, .. } => {
            state.speeds.remove(&id);
            let opened = state.progress.remove(&id);
            let asked = state.table.get(id).and_then(|e| e.on_done).is_some_and(|o| o.dialog);
            if state.dialog_mode() || asked || opened && asked {
                state.done.insert(id);
            }
        }
        Event::ShowDone { id } => {
            state.done.insert(id);
        }
        Event::Failed { id, .. } | Event::Canceled { id, .. } => {
            state.speeds.remove(&id);
        }
        Event::Removed { id } => {
            state.speeds.remove(&id);
            state.segments.remove(&id);
            state.selected.retain(|s| *s != id);
            state.to_remove.retain(|s| *s != id);
            state.progress.remove(&id);
            state.done.remove(&id);
            state.answered.remove(&id);
            state.properties.remove(&id);
        }
        Event::Downloads { .. } => {
            let table = &state.table;
            state.selected.retain(|id| table.get(*id).is_some());
            state.progress.retain(|id| table.get(*id).is_some());
            state.done.retain(|id| table.get(*id).is_some());
        }
        Event::GrabProgress { pages, found, explored, added, .. } => state.grab.progress(pages, found, explored, added),
        Event::GrabDone { files, pages, explored, stopped, login_cookies } => {
            state.grab.done(files, pages, explored, stopped, login_cookies)
        }
        Event::Error { error } | Event::Rejected { error, .. } => {
            state.notice =
                Some(Notice { text: crate::errors::failure(crate::projection::language(state), &error), error: true });
        }
        _ => {}
    }
    // An answered download leaves `answered` once it stops waiting for the
    // dialog, so a later dialog for it can open again.
    let table = &state.table;
    state.answered.retain(|id| table.get(*id).is_some_and(|e| e.status == Status::Confirming));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn added(state: &mut AppState, id: u64) {
        let event = Event::Added { id: DownloadId::new(id), url: format!("https://e.com/{id}.zip").as_str().into() };
        apply(state, UiMessage::Daemon { event: Box::new(event), at: Instant::now(), unix: 0 });
    }

    fn event(state: &mut AppState, event: Event) {
        apply(state, UiMessage::Daemon { event: Box::new(event), at: Instant::now(), unix: 0 });
    }

    #[test]
    fn selection_keeps_only_downloads_in_the_list() {
        let mut s = AppState::default();
        added(&mut s, 1);
        apply(&mut s, UiMessage::Select(vec![DownloadId::new(1), DownloadId::new(9)]));
        assert_eq!(s.selected, [DownloadId::new(1)]);
        event(&mut s, Event::Removed { id: DownloadId::new(1) });
        assert!(s.selected.is_empty());
    }

    #[test]
    fn sorting_a_column_goes_up_down_and_off() {
        let mut s = AppState::default();
        apply(&mut s, UiMessage::Sort(ColumnKey::Size));
        assert_eq!(s.sort, Some(Sort { column: ColumnKey::Size, dir: SortDir::Ascending }));
        apply(&mut s, UiMessage::Sort(ColumnKey::Size));
        assert_eq!(s.sort, Some(Sort { column: ColumnKey::Size, dir: SortDir::Descending }));
        apply(&mut s, UiMessage::Sort(ColumnKey::Size));
        assert_eq!(s.sort, None);
        apply(&mut s, UiMessage::Sort(ColumnKey::Queue));
        assert_eq!(s.sort, None);
    }

    #[test]
    fn columns_resize_within_bounds() {
        let mut s = AppState::default();
        apply(&mut s, UiMessage::Resize(ColumnKey::Name, 5000.0));
        assert_eq!(s.columns[0], (ColumnKey::Name, WIDTH_RANGE.1));
    }

    #[test]
    fn removing_asks_about_the_selection_only() {
        let mut s = AppState::default();
        apply(&mut s, UiMessage::AskRemove);
        assert!(s.to_remove.is_empty());
        added(&mut s, 1);
        apply(&mut s, UiMessage::Select(vec![DownloadId::new(1)]));
        apply(&mut s, UiMessage::AskRemove);
        assert_eq!(s.to_remove, [DownloadId::new(1)]);
        apply(&mut s, UiMessage::WindowClosed("remove".into()));
        assert!(s.to_remove.is_empty());
    }

    #[test]
    fn a_finished_download_gets_its_dialog_in_dialog_mode() {
        let mut s = AppState::default();
        let settings = Settings { notify: Notify::Dialog, ..Settings::default() };
        event(&mut s, Event::Settings { settings });
        added(&mut s, 1);
        let id = DownloadId::new(1);
        apply(&mut s, UiMessage::ShowProgress(id));
        assert!(s.progress.contains(&id));
        event(&mut s, Event::Done { id, url: "https://e.com/1.zip".into(), path: "/tmp/1.zip".into(), note: None });
        assert!(!s.progress.contains(&id));
        assert!(s.done.contains(&id));
        apply(&mut s, UiMessage::WindowClosed(format!("done-{}", id.get())));
        assert!(s.done.is_empty());
    }

    #[test]
    fn progress_makes_a_speed() {
        let mut s = AppState::default();
        added(&mut s, 1);
        let id = DownloadId::new(1);
        let url = "https://e.com/1.zip";
        let start = Instant::now();
        let progress = |done: u64| Event::Progress {
            id,
            url: url.into(),
            done: ByteCount::new(done),
            total: Some(ByteCount::new(10_000_000)),
            parts: None,
            segments: None,
        };
        apply(&mut s, UiMessage::Daemon { event: Box::new(progress(0)), at: start, unix: 0 });
        let later = start + Duration::from_secs(1);
        apply(&mut s, UiMessage::Daemon { event: Box::new(progress(1_000_000)), at: later, unix: 0 });
        assert_eq!(s.speed(id).map(|b| b.round()), Some(1_000_000.0));
    }

    #[test]
    fn losing_the_daemon_is_remembered() {
        let mut s = AppState::default();
        event(&mut s, Event::Connected);
        assert!(s.connected());
        event(&mut s, Event::Lost(Lost::Failed("refused".into())));
        assert_eq!(s.link, Link::Lost("refused".into()));
    }
}
