//! The dialogs' and download windows' contract.

use serde::{Deserialize, Serialize};
use specta::Type;

use super::{FileIcon, Id, QueueItem};

/// Add URL ("Enter new address to download").
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AddUrlView {
    pub connected: bool,
    /// A link the clipboard monitor caught, to fill in.
    pub address: Option<String>,
}

/// A login the Add URL dialog sends with the address.
#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LoginInput {
    pub user: String,
    pub password: String,
}

/// Download File Info: where a probed download goes before it starts.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SaveAsView {
    pub id: Id,
    /// A zip on a server that sends ranges can list its files first.
    pub can_preview: bool,
    pub url: String,
    pub categories: Vec<String>,
    pub category: String,
    /// The file's folder and name, joined.
    pub save_as: String,
    /// The folders the list remembers, for the Save As box's list.
    pub folders: Vec<String>,
    pub remember_folder: bool,
    /// The folder "Remember this path" keeps for the category.
    pub remembered: String,
    pub size: String,
    pub icon: FileIcon,
    pub description: String,
    pub queues: Vec<QueueItem>,
    /// Another download in the list has the same address.
    pub duplicate: Option<DuplicateView>,
    /// The system can ask for administrator rights to save in a folder UDM
    /// may not write to.
    pub can_elevate: bool,
}

/// The Duplicate download link question.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateView {
    /// The other one is running, so it cannot be replaced.
    pub other_running: bool,
}

/// The Download File Info dialog's answer.
#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AnswerInput {
    pub category: String,
    pub save_as: String,
    pub remember_folder: bool,
    pub description: String,
    /// Start now, or wait in a queue ("Download Later").
    pub start: bool,
    /// The answer to the Duplicate download link question, when it asked.
    pub duplicate: Option<super::DuplicateChoice>,
    pub remember_duplicate: bool,
    /// Save there with administrator rights, when UDM may not write to the
    /// folder.
    pub elevate: bool,
}

/// Whether UDM may save files in a folder Download File Info names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct FolderAccess {
    /// Saving there needs administrator rights.
    pub denied: bool,
}

/// One connection's range, for the progress window's strip and table.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SegmentView {
    /// Where the range starts and how far it got, as shares of the file.
    #[specta(type = specta_typescript::Number)]
    pub start: f32,
    #[specta(type = specta_typescript::Number)]
    pub reached: f32,
    pub downloaded: String,
    pub info: String,
}

/// The speed limiter tab.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LimitView {
    pub on: bool,
    pub kb_per_sec: u32,
    pub remember: bool,
}

/// The Options on completion tab.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct OnDoneView {
    pub dialog: bool,
    pub then: Then,
    pub force_close: bool,
}

/// What happens once a download is done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum Then {
    Nothing,
    Exit,
    Sleep,
    Hibernate,
    ShutDown,
}

/// A download's progress window.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DownloadView {
    pub id: Id,
    pub title: String,
    pub address: String,
    pub status: String,
    pub size: String,
    pub downloaded: String,
    pub speed: String,
    pub time_left: String,
    pub resume: String,
    /// The server's country, its flag and code; empty when unknown.
    pub country: String,
    #[specta(type = specta_typescript::Number)]
    pub fraction: f32,
    pub running: bool,
    pub can_resume: bool,
    pub save_to: String,
    pub segments: Vec<SegmentView>,
    pub limit: LimitView,
    pub on_done: OnDoneView,
    pub speed_tab: bool,
    pub done_tab: bool,
    pub details: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum DownloadControl {
    Pause,
    Resume,
    /// Stop it and close the window; it stays in the list.
    Cancel,
}

#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LimitInput {
    pub on: bool,
    pub kb_per_sec: u32,
    pub remember: bool,
}

/// Download complete.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DoneView {
    pub id: Id,
    pub failed: bool,
    pub headline: String,
    pub size_line: String,
    pub address: String,
    pub saved_as: String,
    pub can_open: bool,
    /// ... and the system has an Open with picker for it.
    pub can_open_with: bool,
    /// Its file can be dragged out of the window (Windows).
    pub drag_out: bool,
}

/// Confirm deletion of downloads.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RemoveView {
    pub count: u32,
    /// Some of them are finished and have files to delete.
    pub has_files: bool,
}

/// About UDM.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AboutView {
    pub version: String,
    pub daemon: String,
    /// What the update check said, or is doing.
    pub update: String,
    /// A newer release can be installed.
    pub can_install: bool,
    /// What Update now does, for its tooltip: run Windows' Setup, or put
    /// the new copy in place of this one.
    pub install_title: String,
    pub checking: bool,
    pub connected: bool,
}

/// The Options on completion tab's choices.
#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct OnDoneInput {
    pub dialog: bool,
    pub then: Then,
    pub force_close: bool,
}
