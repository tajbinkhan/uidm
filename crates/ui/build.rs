//! Reads tauri.conf.json and the capabilities, and lists the app's commands
//! so that each is open only to the windows whose capability grants it
//! (AGENTS.md section 7.5). Gives the window its Windows manifest, and on
//! windows-gnu keeps MinGW's default manifest out of the programs that link
//! this crate.

use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;

/// Every command in `src/commands/`. A command missing here cannot be
/// granted to any window, so calling it fails.
const COMMANDS: &[&str] = &[
    "subscribe",
    "report_frontend_error",
    "heartbeat",
    "report_action",
    "close_window",
    "open_window",
    "quit_app",
    "select_rows",
    "select_node",
    "sort_list",
    "resize_column",
    "toggle_node",
    "toggle_categories",
    "run_list_action",
    "add_download",
    "answer_download",
    "check_save_folder",
    "pick_save_folder",
    "control_download",
    "set_download_limit",
    "set_download_on_done",
    "hide_done_dialogs",
    "remove_downloads",
    "open_download",
    "save_options",
    "pick_path",
    "install_tool",
    "check_debrid",
    "remote_status",
    "pair_phone",
    "remove_device",
    "set_browser",
    "add_browser",
    "remove_browser",
    "restart_browser",
    "open_themes_folder",
    "play_sound",
    "system_proxy",
    "save_queue",
    "create_queue",
    "delete_queue",
    "control_queue",
    "move_queued",
    "remove_queued",
    "save_quota",
    "grab_start",
    "grab_stop",
    "grab_select",
    "grab_new",
    "grab_add",
    "grab_template",
    "grab_open",
    "grab_save",
    "grab_save_template",
    "grab_save_filters",
    "grab_add_on_close",
    "preview_batch",
    "add_batch",
    "open_clipboard_links",
    "add_links",
    "import_list",
    "export_list",
    "find_download",
    "show_properties",
    "save_properties",
    "move_download",
    "set_limiter",
    "set_columns",
    "save_category",
    "browse_category",
    "queue_selection",
    "show_drop_target",
    "set_panel_width",
    "set_list_font",
    "set_toolbar",
    "close_tip",
    "answer_login",
    "answer_countdown",
    "show_refresh",
    "refresh_link",
    "cancel_refresh",
    "unpack_package",
    "ask_package_password",
    "ungroup_package",
    "open_package",
    "add_picks",
    "set_link_filter",
    "link_names",
    "check_update",
    "install_update",
    "dismiss_release",
    "preview_zip",
    "convert_download",
    "verify_checksum",
    "open_with",
    "drag_file",
    "answer_exception",
    "show_download",
    "pick_torrent_files",
    "set_seed_limits",
    "stop_seeding",
    "open_torrent_file",
];

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=windows-app-manifest.xml");
    let manifest = tauri_build::AppManifest::new().commands(COMMANDS);
    // The window's Windows manifest, the only one udm-gui holds.
    let windows = tauri_build::WindowsAttributes::new().app_manifest(include_str!("windows-app-manifest.xml"));
    // A build script that cannot read the configuration has to stop the
    // build: there is no window to build without it.
    #[allow(clippy::expect_used)]
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest).windows_attributes(windows))
        .expect("tauri.conf.json and capabilities/ are valid");
    keep_out_default_manifest()
}

/// On windows-gnu, GCC links MinGW's `default-manifest.o` into every
/// program, beside the manifest above. ld drops the default one only when
/// its language is neutral, which some MinGW builds (WinLibs among them) do
/// not give it, so the program would hold two manifests and ld warns
/// ".rsrc merge failure: multiple non-default manifests". GCC looks for
/// `default-manifest.o` in the `-B` folders first, so this writes an empty
/// object by that name and points this crate's tests at it. udm-app's
/// build.rs does the same for udm-gui with the folder it gets as
/// DEP_UDM_UI_NO_DEFAULT_MANIFEST; UDM's other programs keep MinGW's
/// manifest.
fn keep_out_default_manifest() -> io::Result<()> {
    let var = |name: &str| env::var(name).unwrap_or_default();
    if var("CARGO_CFG_TARGET_OS") != "windows" || var("CARGO_CFG_TARGET_ENV") != "gnu" {
        return Ok(());
    }
    // The machine field of a COFF header; the windows-gnu targets are these two.
    let machine: u16 = match var("CARGO_CFG_TARGET_ARCH").as_str() {
        "x86_64" => 0x8664,
        "x86" => 0x014c,
        _ => return Ok(()),
    };
    let dir = PathBuf::from(var("OUT_DIR")).join("no-default-manifest");
    fs::create_dir_all(&dir)?;
    // A COFF object with no sections and no symbols: its 20-byte header.
    let mut object = [0u8; 20];
    object[..2].copy_from_slice(&machine.to_le_bytes());
    fs::write(dir.join("default-manifest.o"), object)?;
    // GCC adds -B's folder to its file names as given, so it needs the
    // trailing separator.
    println!("cargo:rustc-link-arg=-B{}/", dir.display());
    println!("cargo:no_default_manifest={}", dir.display());
    Ok(())
}
