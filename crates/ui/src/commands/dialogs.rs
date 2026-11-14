//! The dialogs' commands: Add URL, Download File Info, Confirm deletion and
//! Download complete.

use std::path::Path;
use std::sync::Arc;

use tauri::State;
use udm_application::daemon::Command;
use udm_domain::address::Address;
use udm_domain::download::{Answer, Download, DownloadLogin};
use udm_domain::filename::FileName;
use udm_domain::ports::DownloadFiles;

use super::options::Pickers;
use super::{ask, label_of, lang, post, reply_of};
use crate::actor::{CurrentLanguage, Inbox, Outbox};
use crate::contract::{AnswerInput, DuplicateChoice, FolderAccess, InvokeError, LoginInput};
use crate::errors;
use crate::projection::dialogs::{save_folder_denied, save_in};
use crate::windows::Label;
use udm_domain::settings::Duplicate;

/// Add the address typed in Add URL, with its login, and close the dialog.
/// The password goes to the daemon only; no view carries it back.
#[tauri::command]
#[specta::specta]
pub async fn add_download(
    window: tauri::WebviewWindow,
    outbox: State<'_, Outbox>,
    current: State<'_, CurrentLanguage>,
    address: String,
    login: Option<LoginInput>,
) -> Result<(), InvokeError> {
    let address = Address::parse(address.trim()).map_err(|e| errors::address(lang(&current), &e))?;
    let login = login
        .filter(|l| !l.user.trim().is_empty())
        .map(|l| DownloadLogin { username: l.user.trim().to_string(), password: Some(l.password) });
    let download = Download { url: address.into(), login, ..Download::default() };
    ask(&outbox, &current, |reply| Inbox::Send { command: Box::new(Command::Add(download)), reply: Some(reply) })
        .await?;
    let _ = window.destroy();
    Ok(())
}

/// The download files' port, for the write check of the folder a download
/// is saved in (a managed type).
pub type Files = Arc<dyn DownloadFiles>;

/// Whether the calling Download File Info gives a folder alone: its
/// download is a torrent of one file, which keeps its name. The shell
/// decides, from the torrent's files.
async fn folder_only(
    window: &tauri::WebviewWindow,
    outbox: &State<'_, Outbox>,
    current: &State<'_, CurrentLanguage>,
) -> Result<bool, InvokeError> {
    match label_of(window, current)? {
        Label::SaveAs(id) => reply_of(outbox, current, |reply| Inbox::FolderOnly { id, reply }).await,
        _ => Ok(false),
    }
}

/// Whether UDM may save in the folder Download File Info's Save As names
/// (for a torrent of one file, the folder it holds), checked on a worker
/// since it writes a probe file there.
#[tauri::command]
#[specta::specta]
pub async fn check_save_folder(
    window: tauri::WebviewWindow,
    outbox: State<'_, Outbox>,
    files: State<'_, Files>,
    current: State<'_, CurrentLanguage>,
    save_as: String,
) -> Result<FolderAccess, InvokeError> {
    let folder_only = folder_only(&window, &outbox, &current).await?;
    let files = Arc::clone(&files);
    let denied = tauri::async_runtime::spawn_blocking(move || denied(&save_as, folder_only, |f| files.can_write(f)))
        .await
        .map_err(|_| errors::offline(lang(&current)))?;
    Ok(FolderAccess { denied })
}

/// Whether saving at `save_as` (a folder alone with `folder_only`) needs
/// administrator rights.
fn denied(save_as: &str, folder_only: bool, can_write: impl Fn(&Path) -> bool) -> bool {
    if folder_only {
        let folder = Path::new(save_as.trim());
        folder.is_absolute() && !can_write(folder)
    } else {
        save_folder_denied(save_as, can_write)
    }
}

/// Download File Info's Browse: another folder for the file, keeping its
/// name; for a torrent of one file, the folder alone. `None` when the user
/// cancels.
#[tauri::command]
#[specta::specta]
pub async fn pick_save_folder(
    window: tauri::WebviewWindow,
    outbox: State<'_, Outbox>,
    pickers: State<'_, Pickers>,
    current: State<'_, CurrentLanguage>,
    save_as: String,
) -> Result<Option<String>, InvokeError> {
    let folder_only = folder_only(&window, &outbox, &current).await?;
    let lang = lang(&current);
    let pickers = Arc::clone(&pickers);
    let title = lang.tr("Save to").to_string();
    let start = save_as.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        let start = Path::new(start.trim());
        let start = if folder_only { Some(start) } else { start.parent() }.filter(|p| p.is_absolute());
        pickers.pick_folder(&title, start)
    })
    .await
    .map_err(|_| errors::offline(lang))?;
    Ok(picked.map(|folder| {
        let folder = folder.display().to_string();
        if folder_only {
            folder
        } else {
            save_in(&save_as, &folder)
        }
    }))
}

/// Download File Info's Start Download or Download Later.
#[tauri::command]
#[specta::specta]
pub async fn answer_download(
    window: tauri::WebviewWindow,
    outbox: State<'_, Outbox>,
    current: State<'_, CurrentLanguage>,
    files: State<'_, Files>,
    answer: AnswerInput,
) -> Result<(), InvokeError> {
    let Label::SaveAs(id) = label_of(&window, &current)? else {
        return Err(errors::gone(lang(&current)));
    };
    // Administrator rights only for a folder that needs them, checked
    // again here on a worker: the webview's word is not taken for it.
    let folder_only = folder_only(&window, &outbox, &current).await?;
    let elevate = answer.elevate && {
        let (files, save_as) = (Arc::clone(&files), answer.save_as.clone());
        tauri::async_runtime::spawn_blocking(move || denied(&save_as, folder_only, |f| files.can_write(f)))
            .await
            .map_err(|_| errors::offline(lang(&current)))?
    };
    let path = Path::new(answer.save_as.trim());
    // A torrent of one file keeps its name: the dialog gives its folder.
    let (filename, folder) = if folder_only {
        let folder = path.display().to_string();
        if folder.is_empty() {
            return Err(errors::invalid(lang(&current), "Type the folder to save in."));
        }
        (None, Some(folder))
    } else {
        let name = path
            .file_name()
            .and_then(|n| FileName::parse(&n.to_string_lossy()).ok())
            .ok_or_else(|| errors::invalid(lang(&current), "Type a name for the file."))?;
        (Some(name.into_string()), path.parent().map(|p| p.display().to_string()).filter(|f| !f.is_empty()))
    };
    let answer = Answer {
        filename,
        folder,
        start: answer.start,
        category: Some(answer.category.chars().take(200).collect()),
        description: Some(answer.description.chars().take(2000).collect()),
        remember_folder: Some(answer.remember_folder),
        duplicate: answer.duplicate.map(|d| match d {
            DuplicateChoice::Ask => Duplicate::Ask,
            DuplicateChoice::NewName => Duplicate::NewName,
            DuplicateChoice::Overwrite => Duplicate::Overwrite,
            DuplicateChoice::Existing => Duplicate::Existing,
        }),
        remember_duplicate: answer.remember_duplicate,
        elevate,
        ..Answer::start(id)
    };
    reply_of(&outbox, &current, |reply| Inbox::Answer { answer: Box::new(answer), reply }).await?
}

/// Confirm deletion's Yes.
#[tauri::command]
#[specta::specta]
pub async fn remove_downloads(
    outbox: State<'_, Outbox>,
    current: State<'_, CurrentLanguage>,
    delete_files: bool,
    dont_ask: bool,
) -> Result<(), InvokeError> {
    post(&outbox, &current, Inbox::Remove { delete_files, dont_ask })
}

/// Download complete's "Don't show this dialog again".
#[tauri::command]
#[specta::specta]
pub async fn hide_done_dialogs(
    outbox: State<'_, Outbox>,
    current: State<'_, CurrentLanguage>,
) -> Result<(), InvokeError> {
    post(&outbox, &current, Inbox::NoDoneDialogs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_torrent_of_one_file_checks_its_folder_itself() {
        let locked = if cfg!(windows) { r"C:\Program Files" } else { "/usr/local" };
        let can_write = |f: &Path| f != Path::new(locked);
        assert!(denied(locked, true, can_write), "the folder alone");
        assert!(!denied(locked, false, can_write), "its parent, for a file there");
        assert!(!denied("relative", true, |_| false), "a relative path names no folder yet");
    }
}
