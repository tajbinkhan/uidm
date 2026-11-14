//! The main window's contract: the download list, the categories tree, the
//! toolbar's states, and what its commands take.

use serde::{Deserialize, Serialize};
use specta::Type;

use super::Id;

/// Everything the main window draws.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MainView {
    /// The rows, already filtered by the tree's node and sorted.
    pub rows: Vec<Row>,
    pub chrome: MainChrome,
}

/// The main window apart from its rows.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MainChrome {
    pub title: String,
    pub columns: Vec<ColumnHead>,
    pub tree: Vec<TreeNode>,
    pub categories_panel: bool,
    pub selected: Vec<Id>,
    pub can: Can,
    pub queues: Vec<QueueItem>,
    /// A message under the toolbar: the daemon is away, or what failed.
    pub banner: Option<Banner>,
    pub limiter: super::LimiterDraft,
    /// Every column, for the column chooser.
    pub all_columns: Vec<super::ColumnChoice>,
    /// What the column chooser's Reset puts back.
    pub default_columns: Vec<super::ColumnChoice>,
    /// The categories, for their properties sheets.
    pub categories: Vec<super::CategoryInput>,
    pub drop_target: bool,
    /// The categories panel's width.
    pub panel_width: u32,
    /// The list's font: the fixed-width one or the window's, and its size.
    pub list_mono: bool,
    pub list_size: u32,
    pub toolbar_small: bool,
    /// The toolbar's buttons and separators, in order.
    pub toolbar: Vec<String>,
    /// The tips of the day, the next one's place, and whether they show at
    /// startup.
    pub tips: Vec<String>,
    pub next_tip: u32,
    pub tips_on_startup: bool,
    /// Show the tip now: the window just opened with tips on startup.
    pub show_tip: bool,
    /// A newer release's version, for the strip offering it, unless that
    /// strip was closed for it.
    pub newer: Option<String>,
    /// That release has a setup Update now can run.
    pub newer_installable: bool,
    /// What each package's menu offers, by the package's tree node.
    pub packages: Vec<PackageMenu>,
    /// The strip offering a problem report.
    pub report: Option<ReportStrip>,
    /// The connection was lost: the banner offers Reconnect and Restart
    /// daemon.
    pub reconnect: bool,
    /// The checksum the one selected download was added with, for Verify
    /// checksum to start from.
    pub checksum: String,
}

/// A package's menu in the categories tree.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PackageMenu {
    pub id: Id,
    /// Unpack, or Unpack again once it was.
    pub unpack: bool,
    pub again: bool,
    /// Unpack with password.
    pub password: bool,
    /// Open unpacked folder.
    pub opened: bool,
    pub connected: bool,
}

/// A change to the main window: the rows that changed, the new order when
/// rows came, went or moved, and the rest when it changed.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MainPatch {
    pub rows: Vec<Row>,
    pub order: Option<Vec<Id>>,
    pub chrome: Option<MainChrome>,
}

/// One download in the list, each column's text already formatted.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub id: Id,
    pub name: String,
    pub icon: FileIcon,
    /// Waiting in a queue (the Q column's mark).
    pub queued: bool,
    /// Finished with its file saved: Enter and a double click open it
    /// rather than its download window.
    pub finished: bool,
    pub size: String,
    pub status: String,
    pub tone: Tone,
    /// From 0 to 1, when known.
    #[specta(type = Option<specta_typescript::Number>)]
    pub fraction: Option<f32>,
    pub progress: String,
    pub time_left: String,
    pub speed: String,
    pub last_try: String,
    pub description: String,
    pub save_to: String,
    pub page: String,
    pub referrer: String,
    /// The server's country: its flag and ISO code, such as "🇺🇸 US"; empty
    /// when unknown.
    pub country: String,
}

/// The kind of file a row holds, for its icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum FileIcon {
    Archive,
    Document,
    Music,
    Program,
    Video,
    Image,
    Other,
}

/// How a row's status reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum Tone {
    Normal,
    Running,
    Failed,
}

/// The list's columns, in the order the window saves them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ColumnKey {
    Name,
    Queue,
    Size,
    Progress,
    Speed,
    TimeLeft,
    Status,
    LastTry,
    SaveTo,
    Page,
    Referrer,
    Description,
    Country,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum SortDir {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ColumnHead {
    pub key: ColumnKey,
    pub title: String,
    pub width: u32,
    pub sort: Option<SortDir>,
    pub sortable: bool,
}

/// One line of the categories tree, flattened, with its depth.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TreeNode {
    /// What `select_node` and `toggle_node` take back.
    pub key: String,
    pub label: String,
    pub depth: u8,
    pub icon: TreeIcon,
    /// Open or shut, for a node with children; `None` for a leaf.
    pub open: Option<bool>,
    pub selected: bool,
    /// More about the node, shown when the pointer rests on it: why a
    /// package was not unpacked.
    pub tip: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum TreeIcon {
    AllDownloads,
    Category(FileIcon),
    Unfinished,
    Finished,
    Grabber,
    Queues,
    Queue,
}

/// Which toolbar buttons and menu items work for the selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Can {
    pub resume: bool,
    pub stop: bool,
    pub stop_all: bool,
    pub resume_all: bool,
    pub delete: bool,
    pub delete_completed: bool,
    pub open: bool,
    pub redownload: bool,
    pub progress: bool,
    /// One stopped download is selected, whose link can be replaced.
    pub refresh: bool,
    /// One finished video is selected, to convert.
    pub convert: bool,
    /// One finished download with its file is selected, to check.
    pub verify: bool,
    /// ... and the system has an Open with picker for it.
    pub open_with: bool,
    /// A finished row's file can be dragged out of the window (Windows).
    pub drag_out: bool,
    pub connected: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub id: Id,
    pub name: String,
    pub running: bool,
}

/// How a strip in the main window reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum StripTone {
    Info,
    Warn,
}

/// The strip offering a problem report, and what its buttons can do.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ReportStrip {
    pub text: String,
    pub tone: StripTone,
    /// How many more reports wait in the folder.
    pub more: String,
    /// Save a report.
    pub save: bool,
    /// Restart the daemon.
    pub restart: bool,
    /// Open an issue, and show the report.
    pub issue: bool,
    /// The report is being written.
    pub busy: bool,
}

/// What the report strip's buttons do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ReportAction {
    Save,
    Dismiss,
    OpenIssue,
    Show,
    RestartDaemon,
    /// Help's Report a problem: a new issue without a report.
    NewIssue,
    /// The lost connection's banner: connect again.
    Reconnect,
}

#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Banner {
    pub text: String,
    pub error: bool,
}

/// What the toolbar, the menus and the list's own menu ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Type)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum ListAction {
    /// Start or resume the selected downloads.
    Resume,
    /// Stop the selected downloads.
    Stop,
    StopAll,
    ResumeAll,
    /// Ask to remove the selected downloads.
    Delete,
    /// Remove every finished download from the list.
    DeleteCompleted,
    Redownload,
    StartQueue {
        id: Id,
    },
    StopQueue {
        id: Id,
    },
    /// Select every row shown.
    SelectAll,
}

/// How to open a finished download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum OpenHow {
    File,
    Folder,
}
