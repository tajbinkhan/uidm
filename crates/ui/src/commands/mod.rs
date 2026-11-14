//! The commands the windows call (AGENTS.md section 7.1). Each is thin: it
//! takes contract types, parses them into domain types, hands the work to
//! the state actor and maps the result to a contract type or an
//! [`InvokeError`]. None runs on the main thread, and none reaches the
//! daemon or the disk itself.

// `#[tauri::command]` expands an async command returning `Result` into code
// with an `unreachable!` of its own; the commands' bodies have none.
#![allow(clippy::unreachable)]

mod common;
mod dialogs;
mod download;
mod extras;
mod grabber;
mod main;
mod options;
mod scheduler;
mod tools;
mod torrent;

pub use common::Heart;
pub use dialogs::Files;
pub use options::Pickers;
pub use tools::Lists;

use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use tauri::State;

use crate::actor::{CurrentLanguage, Inbox, Outbox};
use crate::contract::InvokeError;
use crate::errors;
use crate::i18n::{Language, LANGUAGES};
use crate::windows::Label;

/// The commands, for the invoke handler and the bindings.
pub fn builder() -> tauri_specta::Builder<tauri::Wry> {
    tauri_specta::Builder::<tauri::Wry>::new().commands(tauri_specta::collect_commands![
        common::subscribe,
        common::report_frontend_error,
        common::heartbeat,
        common::report_action,
        common::close_window,
        common::open_window,
        common::quit_app,
        main::select_rows,
        main::select_node,
        main::sort_list,
        main::resize_column,
        main::toggle_node,
        main::toggle_categories,
        main::run_list_action,
        main::open_download,
        main::show_download,
        dialogs::add_download,
        dialogs::answer_download,
        dialogs::check_save_folder,
        dialogs::pick_save_folder,
        dialogs::remove_downloads,
        dialogs::hide_done_dialogs,
        download::control_download,
        download::set_download_limit,
        download::set_download_on_done,
        options::save_options,
        options::pick_path,
        options::install_tool,
        options::check_debrid,
        options::remote_status,
        options::pair_phone,
        options::remove_device,
        options::set_browser,
        options::add_browser,
        options::remove_browser,
        options::restart_browser,
        options::open_themes_folder,
        options::play_sound,
        options::system_proxy,
        scheduler::save_queue,
        scheduler::create_queue,
        scheduler::delete_queue,
        scheduler::control_queue,
        scheduler::move_queued,
        scheduler::remove_queued,
        scheduler::save_quota,
        grabber::grab_start,
        grabber::grab_stop,
        grabber::grab_select,
        grabber::grab_new,
        grabber::grab_add,
        grabber::grab_template,
        grabber::grab_open,
        grabber::grab_save,
        grabber::grab_save_template,
        grabber::grab_save_filters,
        grabber::grab_add_on_close,
        tools::preview_batch,
        tools::add_batch,
        tools::open_clipboard_links,
        tools::add_links,
        tools::import_list,
        tools::export_list,
        tools::find_download,
        tools::show_properties,
        tools::save_properties,
        tools::move_download,
        tools::set_limiter,
        tools::set_columns,
        tools::save_category,
        tools::browse_category,
        tools::queue_selection,
        tools::show_drop_target,
        tools::set_panel_width,
        tools::set_list_font,
        tools::set_toolbar,
        tools::close_tip,
        extras::answer_login,
        extras::answer_countdown,
        extras::show_refresh,
        extras::refresh_link,
        extras::cancel_refresh,
        extras::unpack_package,
        extras::ask_package_password,
        extras::ungroup_package,
        extras::open_package,
        extras::add_picks,
        extras::set_link_filter,
        extras::link_names,
        extras::check_update,
        extras::install_update,
        extras::dismiss_release,
        extras::preview_zip,
        extras::convert_download,
        extras::verify_checksum,
        extras::open_with,
        extras::drag_file,
        extras::answer_exception,
        torrent::pick_torrent_files,
        torrent::set_seed_limits,
        torrent::stop_seeding,
        torrent::open_torrent_file,
    ])
}

/// The language the window speaks now, for error messages.
fn lang(current: &State<'_, CurrentLanguage>) -> &'static Language {
    LANGUAGES.get(current.load(Ordering::Relaxed)).unwrap_or(&LANGUAGES[0])
}

/// Hand `msg` to the state actor.
fn post(outbox: &State<'_, Outbox>, current: &State<'_, CurrentLanguage>, msg: Inbox) -> Result<(), InvokeError> {
    outbox.send(msg).map_err(|_| errors::offline(lang(current)))
}

/// Hand the actor a message with a reply, and wait for it off the main
/// thread: whether the daemon took the command.
async fn ask(
    outbox: &State<'_, Outbox>,
    current: &State<'_, CurrentLanguage>,
    make: impl FnOnce(mpsc::Sender<bool>) -> Inbox,
) -> Result<(), InvokeError> {
    let (tx, rx) = mpsc::channel();
    post(outbox, current, make(tx))?;
    let sent = tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(5)).unwrap_or(false))
        .await
        .unwrap_or(false);
    if sent {
        Ok(())
    } else {
        Err(errors::offline(lang(current)))
    }
}

/// The label of the window that called, parsed.
fn label_of(window: &tauri::WebviewWindow, current: &State<'_, CurrentLanguage>) -> Result<Label, InvokeError> {
    Label::parse(window.label()).ok_or_else(|| errors::gone(lang(current)))
}
