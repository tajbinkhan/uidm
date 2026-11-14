//! The Options window's commands: saving it, and its Browse buttons.

use std::path::Path;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use tauri::State;
use udm_application::window::{FileKind, FilePicker};

use udm_application::daemon::Command;

use super::{lang, post};
use crate::actor::{BrowserAction, CurrentLanguage, Inbox, Outbox};
use crate::contract::{DebridChoice, Id, InvokeError, OptionsDraft, PickKind, SystemProxyView, ToolKey};
use crate::errors;
use crate::state::UiMessage;

/// The system's file and folder dialogs (a managed type).
pub type Pickers = Arc<dyn FilePicker>;

/// Options' OK: check the settings and send them to the daemon.
#[tauri::command]
#[specta::specta]
pub async fn save_options(
    window: tauri::WebviewWindow,
    outbox: State<'_, Outbox>,
    current: State<'_, CurrentLanguage>,
    draft: OptionsDraft,
) -> Result<(), InvokeError> {
    let (tx, rx) = mpsc::channel();
    post(&outbox, &current, Inbox::SaveOptions { draft: Box::new(draft), reply: tx })?;
    let offline = errors::offline(lang(&current));
    let result = tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(5)))
        .await
        .map_err(|_| offline.clone())?
        .map_err(|_| offline)?;
    result?;
    let _ = window.destroy();
    Ok(())
}

/// A Browse button: a folder, a sound or a program, starting at `start`.
/// `None` when the user cancels.
#[tauri::command]
#[specta::specta]
pub async fn pick_path(
    pickers: State<'_, Pickers>,
    current: State<'_, CurrentLanguage>,
    kind: PickKind,
    start: String,
) -> Result<Option<String>, InvokeError> {
    let lang = lang(&current);
    let pickers = Arc::clone(&pickers);
    let title = lang.tr(match kind {
        PickKind::Folder => "Choose a folder",
        PickKind::Sound => "Choose a sound",
        PickKind::Program => "Choose a program",
    });
    let title = title.to_string();
    let sounds = FileKind::new(lang.tr("Sounds"), &["wav"]);
    let picked = tauri::async_runtime::spawn_blocking(move || {
        let start = Some(Path::new(start.trim())).filter(|p| p.is_absolute());
        match kind {
            PickKind::Folder => pickers.pick_folder(&title, start),
            PickKind::Sound => pickers.pick_file(&title, &[sounds], start),
            PickKind::Program => pickers.pick_file(&title, &[], start),
        }
    })
    .await
    .map_err(|_| errors::offline(lang))?;
    Ok(picked.map(|p| p.display().to_string()))
}

/// Install or update a tool: yt-dlp, ffmpeg or gallery-dl.
#[tauri::command]
#[specta::specta]
pub async fn install_tool(
    outbox: State<'_, Outbox>,
    current: State<'_, CurrentLanguage>,
    tool: ToolKey,
) -> Result<(), InvokeError> {
    let tool = crate::services::tool_of(tool);
    post(&outbox, &current, Inbox::Send { command: Box::new(Command::InstallTool { tool }), reply: None })?;
    post(&outbox, &current, Inbox::Ui(UiMessage::ToolInstalling(tool)))
}

/// Check account: ask the debrid service about the account, with the key
/// typed in the field, or else the stored one. The answer shows on the page.
#[tauri::command]
#[specta::specta]
pub async fn check_debrid(
    outbox: State<'_, Outbox>,
    current: State<'_, CurrentLanguage>,
    service: DebridChoice,
    key: String,
) -> Result<(), InvokeError> {
    let service = crate::options::debrid_service(service);
    let command = Command::CheckDebrid { service, key: udm_domain::debrid::DebridKey::new(&key) };
    post(&outbox, &current, Inbox::Send { command: Box::new(command), reply: None })?;
    post(&outbox, &current, Inbox::Ui(UiMessage::DebridChecking(service)))
}

/// Ask how remote control stands, as Options' Remote page does while it
/// is open.
#[tauri::command]
#[specta::specta]
pub async fn remote_status(outbox: State<'_, Outbox>, current: State<'_, CurrentLanguage>) -> Result<(), InvokeError> {
    post(&outbox, &current, Inbox::Send { command: Box::new(Command::RemoteStatus), reply: None })
}

/// Show a pairing code for a phone (`on`), or withdraw it.
#[tauri::command]
#[specta::specta]
pub async fn pair_phone(
    outbox: State<'_, Outbox>,
    current: State<'_, CurrentLanguage>,
    on: bool,
) -> Result<(), InvokeError> {
    let command = if on { Command::StartPairing } else { Command::CancelPairing };
    post(&outbox, &current, Inbox::Send { command: Box::new(command), reply: None })
}

/// Unpair a phone at once.
#[tauri::command]
#[specta::specta]
pub async fn remove_device(
    outbox: State<'_, Outbox>,
    current: State<'_, CurrentLanguage>,
    id: Id,
) -> Result<(), InvokeError> {
    let command = Command::RemoveDevice { id: udm_domain::id::DeviceId::new(id.0) };
    post(&outbox, &current, Inbox::Send { command: Box::new(command), reply: None })
}

/// Register UDM with a browser in Options' list, or stop.
#[tauri::command]
#[specta::specta]
pub async fn set_browser(
    outbox: State<'_, Outbox>,
    current: State<'_, CurrentLanguage>,
    index: u32,
    on: bool,
) -> Result<(), InvokeError> {
    post(&outbox, &current, Inbox::Browser(BrowserAction::Set { index: index as usize, on }))
}

/// Add a browser by its program, picked with Browse.
#[tauri::command]
#[specta::specta]
pub async fn add_browser(
    outbox: State<'_, Outbox>,
    current: State<'_, CurrentLanguage>,
    program: String,
) -> Result<(), InvokeError> {
    let program = program.trim().to_string();
    if !Path::new(&program).is_absolute() {
        return Err(errors::invalid(lang(&current), "Choose the program of a browser that is not in the list"));
    }
    post(&outbox, &current, Inbox::Browser(BrowserAction::Add { program }))
}

/// Remove a browser the user added.
#[tauri::command]
#[specta::specta]
pub async fn remove_browser(
    outbox: State<'_, Outbox>,
    current: State<'_, CurrentLanguage>,
    index: u32,
) -> Result<(), InvokeError> {
    post(&outbox, &current, Inbox::Browser(BrowserAction::Remove { index: index as usize }))
}

/// Close a browser and start it again, so it sees a change.
#[tauri::command]
#[specta::specta]
pub async fn restart_browser(
    outbox: State<'_, Outbox>,
    current: State<'_, CurrentLanguage>,
    index: u32,
) -> Result<(), InvokeError> {
    post(&outbox, &current, Inbox::Browser(BrowserAction::Restart { index: index as usize }))
}

/// Open the folder theme files go in.
#[tauri::command]
#[specta::specta]
pub async fn open_themes_folder(
    outbox: State<'_, Outbox>,
    current: State<'_, CurrentLanguage>,
) -> Result<(), InvokeError> {
    post(&outbox, &current, Inbox::OpenThemesFolder)
}

/// Play the sound of event `index` (download complete, failed, queue
/// started, queue done), from `file` or the system's own.
#[tauri::command]
#[specta::specta]
pub async fn play_sound(
    outbox: State<'_, Outbox>,
    current: State<'_, CurrentLanguage>,
    index: u32,
    file: String,
) -> Result<(), InvokeError> {
    use udm_domain::settings::Sound;
    let sound = match index {
        0 => Sound::Done,
        1 => Sound::Failed,
        2 => Sound::QueueStarted,
        _ => Sound::QueueDone,
    };
    post(&outbox, &current, Inbox::PlaySound { sound, file })
}

/// Get System: the system's proxies, as the manual fields take them.
#[tauri::command]
#[specta::specta]
pub async fn system_proxy(
    outbox: State<'_, Outbox>,
    current: State<'_, CurrentLanguage>,
) -> Result<SystemProxyView, InvokeError> {
    let lang = lang(&current);
    let (tx, rx) = mpsc::channel();
    post(&outbox, &current, Inbox::SystemProxy(tx))?;
    let offline = errors::offline(lang);
    let found = tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(10)))
        .await
        .map_err(|_| offline.clone())?
        .map_err(|_| offline)?;
    crate::options::system_proxy(found.as_ref()).ok_or_else(|| errors::invalid(lang, crate::options::NO_SYSTEM_PROXY))
}
