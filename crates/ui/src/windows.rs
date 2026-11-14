//! The windows' labels, and opening, closing and hiding them (AGENTS.md
//! section 7.3). Only this module makes labels, and only the shell opens
//! windows.

use std::collections::BTreeSet;

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use udm_domain::download::Status;
use udm_domain::id::DownloadId;

use crate::i18n::Language;
use crate::state::AppState;

/// A window kind's label for one download.
type MakeLabel = fn(DownloadId) -> Label;

/// Every window the shell opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Label {
    Main,
    AddUrl,
    /// Download File Info for one download.
    SaveAs(DownloadId),
    /// A download's progress window.
    Download(DownloadId),
    /// Download complete.
    Done(DownloadId),
    Remove,
    About,
    Options,
    Scheduler,
    Grabber,
    Batch,
    Links,
    /// A download's properties.
    Properties(DownloadId),
    DropTarget,
    Login,
    Countdown,
    Refresh,
    Password,
    PickLinks,
    PickVideos,
    Gallery,
    /// The files inside a zip download.
    ZipPreview,
    /// The offer to keep a download in the browser.
    Exception,
}

impl Label {
    pub fn parse(text: &str) -> Option<Label> {
        match text {
            "main" => return Some(Label::Main),
            "add-url" => return Some(Label::AddUrl),
            "remove" => return Some(Label::Remove),
            "about" => return Some(Label::About),
            "options" => return Some(Label::Options),
            "scheduler" => return Some(Label::Scheduler),
            "grabber" => return Some(Label::Grabber),
            "batch" => return Some(Label::Batch),
            "links" => return Some(Label::Links),
            "drop-target" => return Some(Label::DropTarget),
            "login" => return Some(Label::Login),
            "countdown" => return Some(Label::Countdown),
            "refresh" => return Some(Label::Refresh),
            "password" => return Some(Label::Password),
            "pick-links" => return Some(Label::PickLinks),
            "pick-videos" => return Some(Label::PickVideos),
            "gallery" => return Some(Label::Gallery),
            "zip-preview" => return Some(Label::ZipPreview),
            "exception" => return Some(Label::Exception),
            _ => {}
        }
        let kinds: [(&str, MakeLabel); 4] = [
            ("save-as-", Label::SaveAs),
            ("download-", Label::Download),
            ("done-", Label::Done),
            ("properties-", Label::Properties),
        ];
        kinds.iter().find_map(|(prefix, make)| {
            let id = text.strip_prefix(prefix)?.parse().ok()?;
            Some(make(DownloadId::new(id)))
        })
    }

    /// The page the window loads, one per window kind.
    fn page(self) -> &'static str {
        match self {
            Label::Main => "main.html",
            Label::AddUrl => "add-url.html",
            Label::SaveAs(_) => "save-as.html",
            Label::Download(_) => "download.html",
            Label::Done(_) => "done.html",
            Label::Remove => "remove.html",
            Label::About => "about.html",
            Label::Options => "options.html",
            Label::Scheduler => "scheduler.html",
            Label::Grabber => "grabber.html",
            Label::Batch => "batch.html",
            Label::Links => "links.html",
            Label::Properties(_) => "properties.html",
            Label::DropTarget => "drop-target.html",
            Label::Login => "login.html",
            Label::Countdown => "countdown.html",
            Label::Refresh => "refresh.html",
            Label::Password => "password.html",
            Label::PickLinks => "pick-links.html",
            Label::PickVideos => "pick-videos.html",
            Label::Gallery => "gallery.html",
            Label::ZipPreview => "zip-preview.html",
            Label::Exception => "exception.html",
        }
    }

    /// The window's inner size, as the window it follows has it.
    fn size(self) -> (f64, f64) {
        match self {
            Label::Main => (814.0, 435.0),
            Label::AddUrl => (566.0, 96.0),
            Label::SaveAs(_) => (576.0, 206.0),
            Label::Download(_) => (552.0, 244.0),
            Label::Done(_) => (462.0, 212.0),
            Label::Remove => (423.0, 148.0),
            Label::About => (396.0, 378.0),
            Label::Options => (436.0, 505.0),
            Label::Scheduler => (657.0, 464.0),
            Label::Grabber => (615.0, 388.0),
            Label::Batch => (480.0, 375.0),
            Label::Links => (814.0, 553.0),
            Label::Properties(_) => (483.0, 398.0),
            Label::DropTarget => (48.0, 48.0),
            Label::Login => (400.0, 210.0),
            Label::Countdown => (400.0, 150.0),
            Label::Refresh => (480.0, 200.0),
            Label::Password => (380.0, 160.0),
            Label::PickLinks => (760.0, 480.0),
            Label::PickVideos => (640.0, 440.0),
            Label::Gallery => (700.0, 480.0),
            Label::ZipPreview => (520.0, 400.0),
            Label::Exception => (460.0, 200.0),
        }
    }

    fn title(self, lang: &'static Language) -> String {
        lang.tr(match self {
            Label::Main => "UDM",
            Label::AddUrl => "Enter new address to download",
            Label::SaveAs(_) => "Download File Info",
            Label::Download(_) => "Download status",
            Label::Done(_) => "Download complete",
            Label::Remove => "Confirm deletion of downloads",
            Label::About => "About UDM",
            Label::Options => "UDM Configuration",
            Label::Scheduler => "Scheduler",
            Label::Grabber => "UDM Site Grabber",
            Label::Batch => "Batch download",
            Label::Links => "Batch download from clipboard",
            Label::Properties(_) => "File Properties",
            Label::DropTarget => "UDM",
            Label::Login => "Login",
            Label::Countdown => "UDM",
            Label::Refresh => "New link",
            Label::Password => "Package password",
            Label::PickLinks => "Download links",
            Label::PickVideos => "Download videos",
            Label::Gallery => "Download gallery",
            Label::ZipPreview => "Preview",
            Label::Exception => "Keep it in the browser?",
        })
        .to_string()
    }
}

impl std::fmt::Display for Label {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Label::Main => write!(f, "main"),
            Label::AddUrl => write!(f, "add-url"),
            Label::SaveAs(id) => write!(f, "save-as-{}", id.get()),
            Label::Download(id) => write!(f, "download-{}", id.get()),
            Label::Done(id) => write!(f, "done-{}", id.get()),
            Label::Remove => write!(f, "remove"),
            Label::About => write!(f, "about"),
            Label::Options => write!(f, "options"),
            Label::Scheduler => write!(f, "scheduler"),
            Label::Grabber => write!(f, "grabber"),
            Label::Batch => write!(f, "batch"),
            Label::Links => write!(f, "links"),
            Label::Properties(id) => write!(f, "properties-{}", id.get()),
            Label::DropTarget => write!(f, "drop-target"),
            Label::Login => write!(f, "login"),
            Label::Countdown => write!(f, "countdown"),
            Label::Refresh => write!(f, "refresh"),
            Label::Password => write!(f, "password"),
            Label::PickLinks => write!(f, "pick-links"),
            Label::PickVideos => write!(f, "pick-videos"),
            Label::Gallery => write!(f, "gallery"),
            Label::ZipPreview => write!(f, "zip-preview"),
            Label::Exception => write!(f, "exception"),
        }
    }
}

/// The windows the state asks for: Download File Info for each download
/// waiting for it, the progress and completion windows it keeps, and
/// Confirm deletion while it asks.
pub fn wanted(state: &AppState) -> BTreeSet<Label> {
    let mut out = BTreeSet::new();
    for e in state.table.entries() {
        if e.status == Status::Confirming && !state.answered.contains(&e.id) {
            out.insert(Label::SaveAs(e.id));
        }
    }
    out.extend(state.progress.iter().map(|id| Label::Download(*id)));
    out.extend(state.done.iter().map(|id| Label::Done(*id)));
    if !state.to_remove.is_empty() {
        out.insert(Label::Remove);
    }
    if state.links.is_some() {
        out.insert(Label::Links);
    }
    out.extend(state.properties.iter().map(|id| Label::Properties(*id)));
    if state.settings.as_ref().is_some_and(|s| s.drop_target) {
        out.insert(Label::DropTarget);
    }
    let a = &state.asks;
    for (on, label) in [
        (!a.login_wanted.is_empty(), Label::Login),
        (a.countdown.is_some(), Label::Countdown),
        (a.refresh.is_some(), Label::Refresh),
        (a.password.is_some(), Label::Password),
        (a.links.is_some(), Label::PickLinks),
        (a.videos.is_some(), Label::PickVideos),
        (a.gallery.is_some(), Label::Gallery),
        (state.files.zip.is_some(), Label::ZipPreview),
        (state.files.exception.is_some(), Label::Exception),
    ] {
        if on {
            out.insert(label);
        }
    }
    out
}

/// Whether a window may go to `url`: the bundled app only, or Vite's dev
/// server while the frontend is not embedded.
fn stays_in_app(url: &tauri::Url) -> bool {
    match (url.scheme(), url.host_str()) {
        ("tauri", Some("localhost")) => true,
        ("http" | "https", Some("tauri.localhost")) => true,
        ("http", Some("localhost")) => cfg!(not(feature = "embed")) && url.port() == Some(1420),
        _ => false,
    }
}

/// The interface size windows open at, in percent. The actor sets it from
/// the settings each time it publishes; a window opened before the first
/// settings arrive opens at 100%.
static ZOOM: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(100);

/// Open later windows at this interface size, in percent.
pub fn set_zoom(percent: u32) {
    ZOOM.store(percent, std::sync::atomic::Ordering::Relaxed);
}

/// A torrent's download window, with its tabs of files, peers, trackers,
/// pieces and seeding, and its Download File Info, with its file list.
const TORRENT_DOWNLOAD: (f64, f64) = (700.0, 400.0);
const TORRENT_SAVE_AS: (f64, f64) = (576.0, 432.0);

/// The size window `label` opens at: a torrent's windows are larger.
pub fn size(state: &AppState, label: Label) -> (f64, f64) {
    let torrent = |id: DownloadId| state.table.get(id).is_some_and(|e| e.torrent.is_some());
    match label {
        Label::Download(id) if torrent(id) => TORRENT_DOWNLOAD,
        Label::SaveAs(id) if torrent(id) => TORRENT_SAVE_AS,
        _ => label.size(),
    }
}

/// Open the window `label`, or bring it forward when it is open.
pub fn open(app: &AppHandle, label: Label, lang: &'static Language) -> Result<WebviewWindow, tauri::Error> {
    open_sized(app, label, lang, label.size())
}

/// Open the window `label` at `size` (before the interface size), or bring
/// it forward when it is open.
pub fn open_sized(
    app: &AppHandle,
    label: Label,
    lang: &'static Language,
    size: (f64, f64),
) -> Result<WebviewWindow, tauri::Error> {
    if let Some(w) = app.get_webview_window(&label.to_string()) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(w);
    }
    let zoom = f64::from(ZOOM.load(std::sync::atomic::Ordering::Relaxed).clamp(50, 400)) / 100.0;
    let (width, height) = (size.0 * zoom, size.1 * zoom);
    let main = matches!(
        label,
        Label::Main | Label::Grabber | Label::Links | Label::PickLinks | Label::PickVideos | Label::Gallery
    );
    let builder = WebviewWindowBuilder::new(app, label.to_string(), WebviewUrl::App(label.page().into()))
        .title(label.title(lang))
        .inner_size(width, height)
        .resizable(main)
        .maximizable(main)
        .minimizable(main || matches!(label, Label::Download(_)))
        .on_navigation(stays_in_app)
        .devtools(cfg!(debug_assertions))
        .visible(true);
    let builder = if main { builder.min_inner_size(520.0, 300.0) } else { builder.center() };
    // The drop target floats over other windows without a frame, and takes
    // links dropped on it as the page's own drops.
    let builder = if label == Label::DropTarget {
        builder.decorations(false).always_on_top(true).skip_taskbar(true).minimizable(false).disable_drag_drop_handler()
    } else {
        builder
    };
    builder.build()
}

/// Close the window `label`, if it is open.
pub fn close(app: &AppHandle, label: Label) {
    if let Some(w) = app.get_webview_window(&label.to_string()) {
        let _ = w.destroy();
    }
}

/// Show the main window, opening it when it is not.
pub fn show_main(app: &AppHandle, lang: &'static Language) {
    if let Err(e) = open(app, Label::Main, lang) {
        tracing::warn!(error = %e, "the main window did not open");
    }
}

/// Set a window's title, such as a progress window's percentage.
pub fn set_title(app: &AppHandle, label: Label, title: &str) {
    if let Some(w) = app.get_webview_window(&label.to_string()) {
        let _ = w.set_title(title);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_round_trip() {
        for label in [
            Label::Main,
            Label::AddUrl,
            Label::SaveAs(DownloadId::new(3)),
            Label::Download(DownloadId::new(4)),
            Label::Done(DownloadId::new(5)),
            Label::Remove,
            Label::About,
            Label::Options,
            Label::Scheduler,
            Label::Grabber,
            Label::Batch,
            Label::Links,
            Label::Properties(DownloadId::new(6)),
            Label::DropTarget,
            Label::Login,
            Label::Countdown,
            Label::Refresh,
            Label::Password,
            Label::PickLinks,
            Label::PickVideos,
            Label::Gallery,
            Label::ZipPreview,
            Label::Exception,
        ] {
            assert_eq!(Label::parse(&label.to_string()), Some(label));
        }
        assert_eq!(Label::parse("download-x"), None);
    }

    #[test]
    fn windows_never_leave_the_app() {
        let ok = |u: &str| stays_in_app(&tauri::Url::parse(u).unwrap());
        assert!(ok("tauri://localhost/main.html"));
        assert!(ok("http://tauri.localhost/main.html"));
        assert!(!ok("https://example.com/"));
        assert!(!ok("file:///etc/passwd"));
    }
}
