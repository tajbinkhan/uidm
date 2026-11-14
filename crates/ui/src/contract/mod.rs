//! Every type that crosses into the webview (AGENTS.md section 7.2). Each
//! derives `serde` and `specta::Type`, and `tauri-specta` writes them into
//! `frontend/src/bindings.ts`. They are mapped here from the application's
//! and the domain's types, which know nothing of them.

mod dialogs;
mod extras;
mod filework;
mod grabber;
mod main;
mod options;
mod scheduler;
mod tools;
mod torrent;

pub use dialogs::*;
pub use extras::*;
pub use filework::*;
pub use grabber::*;
pub use main::*;
pub use options::*;
pub use scheduler::*;
pub use tools::*;
pub use torrent::*;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use specta::Type;

/// A download's or queue's number. Ids stay far below 2^53, so the
/// webview reads them as plain numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Type)]
#[serde(transparent)]
pub struct Id(#[specta(type = specta_typescript::Number)] pub u64);

/// One CSS custom property, such as `--fg: #09090b`.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct CssVar {
    pub name: String,
    pub value: String,
}

/// The theme's variables. Following the system, the frontend takes
/// `light` or `dark` by the system's mode; otherwise both are the same.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Theme {
    pub light: Vec<CssVar>,
    pub dark: Vec<CssVar>,
    pub follow_system: bool,
}

/// What every window draws with: the theme and the language.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Look {
    pub theme: Theme,
    /// The language's BCP 47 tag, for the page's `lang`.
    pub language: String,
    pub right_to_left: bool,
    /// The interface size, in percent: every window zooms by it.
    pub zoom: u32,
    /// The translation table: English text to the language's. `t()` falls
    /// back to the English for text the table lacks.
    pub text: BTreeMap<String, String>,
}

/// What a window's subscription carries: a whole view first, then the
/// changes, numbered so a window can tell it missed one and subscribe
/// again.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Update {
    Snapshot {
        seq: u32,
        look: Look,
        view: View,
    },
    Patch {
        seq: u32,
        look: Option<Look>,
        patch: Patch,
    },
    /// The window's subject is gone (its download was removed); the shell
    /// closes it.
    Gone {
        seq: u32,
    },
}

/// A window's whole view, by its kind.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(tag = "window", content = "view", rename_all = "camelCase")]
pub enum View {
    Main(MainView),
    AddUrl(AddUrlView),
    SaveAs(SaveAsView),
    Download(DownloadView),
    Done(DoneView),
    Remove(RemoveView),
    About(AboutView),
    Options(Box<OptionsView>),
    Scheduler(SchedulerView),
    Grabber(Box<GrabberView>),
    Batch(BatchView),
    Links(LinksView),
    Properties(PropertiesView),
    DropTarget(DropView),
    Login(LoginView),
    Countdown(CountdownView),
    Refresh(RefreshView),
    Password(PasswordView),
    PickLinks(PickLinksView),
    PickVideos(PickVideosView),
    Gallery(GalleryView),
    ZipPreview(ZipView),
    Exception(ExceptionView),
}

/// A change to a view. The main window's list, and a torrent's lists,
/// change row by row; the other windows are sent whole.
#[derive(Debug, Clone, Serialize, Type)]
#[serde(tag = "window", content = "patch", rename_all = "camelCase")]
pub enum Patch {
    Main(MainPatch),
    /// A download window: its torrent's lists change row by row.
    Download(Box<DownloadPatch>),
    /// Download File Info: its torrent's files change row by row.
    SaveAs(Box<SaveAsPatch>),
    Whole(View),
}

/// Why a command failed: its kind, and a message already in the user's
/// language (AGENTS.md section 9).
#[derive(Debug, Clone, PartialEq, Serialize, Type, thiserror::Error)]
#[serde(rename_all = "camelCase")]
#[error("{message}")]
pub struct InvokeError {
    pub kind: ErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ErrorKind {
    /// What the window sent does not parse (an address that is not one).
    Invalid,
    /// UDM's background service is not connected.
    Offline,
    /// The window or its download is gone.
    Gone,
}

/// The windows the frontend may ask the shell to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum OpenWindow {
    AddUrl,
    About,
    Options,
    Scheduler,
    Grabber,
    Batch,
    /// Bring the main window forward, as the drop target's double click does.
    Main,
}
