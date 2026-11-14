//! The torrent controls: ticking files in Download File Info and the
//! download window, a torrent's seeding limits and Stop seeding, and the
//! main window's Open torrent file.

use std::sync::{mpsc, Arc};
use std::time::Duration;

use tauri::State;
use udm_application::daemon::Command;
use udm_application::window::{FileKind, ReadError};
use udm_domain::download::Download;
use udm_domain::torrent::{TorrentError, MAX_SENT_METAINFO_BYTES};

use super::options::Pickers;
use super::tools::Lists;
use super::{ask, label_of, lang, post};
use crate::actor::{CurrentLanguage, Inbox, Outbox};
use crate::contract::{InvokeError, SeedLimitsInput};
use crate::errors;
use crate::state::UiMessage;
use crate::windows::Label;

/// Tick (`chosen`) or untick a torrent's files `indexes`: in Download File
/// Info before it starts, or in the download window, where the torrent
/// takes the new choice at once.
#[tauri::command]
#[specta::specta]
pub async fn pick_torrent_files(
    window: tauri::WebviewWindow,
    outbox: State<'_, Outbox>,
    current: State<'_, CurrentLanguage>,
    indexes: Vec<u32>,
    chosen: bool,
) -> Result<(), InvokeError> {
    let indexes: Vec<usize> = indexes.into_iter().map(|i| i as usize).collect();
    match label_of(&window, &current)? {
        Label::SaveAs(id) => post(&outbox, &current, Inbox::Ui(UiMessage::PickTorrentFiles { id, indexes, chosen })),
        Label::Download(id) => {
            let (tx, rx) = mpsc::channel();
            post(&outbox, &current, Inbox::ChooseTorrentFiles { id, indexes, chosen, reply: tx })?;
            let lang = lang(&current);
            tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(5)))
                .await
                .map_err(|_| errors::offline(lang))?
                .map_err(|_| errors::offline(lang))?
        }
        _ => Err(errors::gone(lang(&current))),
    }
}

/// The download id of the torrent download window that called.
fn torrent_window(
    window: &tauri::WebviewWindow,
    current: &State<'_, CurrentLanguage>,
) -> Result<udm_domain::id::DownloadId, InvokeError> {
    match label_of(window, current)? {
        Label::Download(id) => Ok(id),
        _ => Err(errors::gone(lang(current))),
    }
}

/// The download window's seeding limits: the torrent's own, or the
/// settings'.
#[tauri::command]
#[specta::specta]
pub async fn set_seed_limits(
    window: tauri::WebviewWindow,
    outbox: State<'_, Outbox>,
    current: State<'_, CurrentLanguage>,
    limits: SeedLimitsInput,
) -> Result<(), InvokeError> {
    let id = torrent_window(&window, &current)?;
    let limits = crate::torrents::seed_limits(&limits).map_err(|e| errors::limits(lang(&current), &e))?;
    ask(&outbox, &current, |reply| Inbox::Send {
        command: Box::new(Command::SetSeedLimits { id, limits }),
        reply: Some(reply),
    })
    .await
}

/// The download window's Stop seeding.
#[tauri::command]
#[specta::specta]
pub async fn stop_seeding(
    window: tauri::WebviewWindow,
    outbox: State<'_, Outbox>,
    current: State<'_, CurrentLanguage>,
) -> Result<(), InvokeError> {
    let id = torrent_window(&window, &current)?;
    ask(&outbox, &current, |reply| Inbox::Send { command: Box::new(Command::StopSeeding { id }), reply: Some(reply) })
        .await
}

/// Tasks > Open torrent file: a `.torrent` file the user picks, read on a
/// worker and sent to the daemon, which adds its torrent as it adds any
/// download. Nothing when the user cancels.
#[tauri::command]
#[specta::specta]
pub async fn open_torrent_file(
    outbox: State<'_, Outbox>,
    current: State<'_, CurrentLanguage>,
    pickers: State<'_, Pickers>,
    lists: State<'_, Lists>,
) -> Result<(), InvokeError> {
    let lang = lang(&current);
    let (pickers, lists) = (Arc::clone(&pickers), Arc::clone(&lists));
    let kinds = [FileKind::new(lang.tr("Torrent files"), &["torrent"]), FileKind::new(lang.tr("All files"), &["*"])];
    let title = lang.tr("Open a torrent file").to_string();
    let read = tauri::async_runtime::spawn_blocking(move || {
        let path = pickers.pick_file(&title, &kinds, None)?;
        // It travels to the daemon in one message, which has a size limit.
        Some(lists.read_bytes(&path, MAX_SENT_METAINFO_BYTES as u64))
    })
    .await
    .map_err(|_| errors::offline(lang))?;
    let Some(read) = read else { return Ok(()) };
    let metainfo = read.map_err(|e| match e {
        ReadError::TooBig { .. } => {
            errors::invalid_text(errors::text::torrent(lang, &TorrentError::TooBig { max: MAX_SENT_METAINFO_BYTES }))
        }
        ReadError::Failed(e) => errors::list(&e),
    })?;
    let command = Command::AddTorrentFile { metainfo, download: Download::default() };
    ask(&outbox, &current, |reply| Inbox::Send { command: Box::new(command), reply: Some(reply) }).await
}
