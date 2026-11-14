//! UDM's desktop window (ADR 0002). The shell lives in `udm_ui`; this
//! binary builds the adapters it reaches through ports and starts it
//! (AGENTS.md section 8).

#![windows_subsystem = "windows"]
// Tests may use todo!, unimplemented! and unreachable!, which clippy.toml's
// test exemptions do not cover (AGENTS.md section 9).
#![cfg_attr(test, allow(clippy::todo, clippy::unimplemented, clippy::unreachable))]

use std::sync::Arc;

use anyhow::Context;
use udm_infrastructure as infra;

fn main() -> anyhow::Result<()> {
    // Before the log, which would make UDM's data folder first.
    let moved = infra::from_old_names::move_from_old_names();
    infra::logging::init(infra::logging::Program::Gui);
    infra::crash::install(infra::logging::Program::Gui);
    moved.log();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().unwrap_or_default();
    let (command_line, startup_error) = match infra::cli::parse(&args, &cwd) {
        Ok(command_line) => (command_line, None),
        Err(e) => (None, Some(e.to_string())),
    };
    udm_ui::run(udm_ui::Start {
        daemon: Arc::new(infra::daemon_client::IpcDaemonClient::new(udm_ui::WINDOW_CLIENT)),
        clock: Arc::new(infra::clock::LocalClock),
        times: Arc::new(infra::clock::LocalClock),
        opener: Arc::new(infra::platform::Desktop),
        pickers: Arc::new(infra::pickers::NativePicker),
        library: infra::grab::projects_dir().ok().map(|dir| Arc::new(infra::grab::ProjectFiles(dir)) as _),
        found: Arc::new(infra::grab::FoundFiles),
        system: Arc::new(infra::system::ThisSystem),
        lists: Arc::new(infra::listfile::ListFormats),
        folders: Arc::new(infra::files::DiskFiles),
        files: Arc::new(infra::files::DiskFiles),
        window_files: Arc::new(infra::window_files::WindowDir::new()),
        browsers: Arc::new(infra::browsers::Registry),
        adapters: Arc::new(infra::adapters::SystemAdapters),
        themes: Arc::new(infra::themes::ThemeDir::new()),
        reports: Arc::new(infra::crash::ReportFiles::new()),
        clipboard: Arc::new(infra::clipboard::SystemClipboard),
        hashes: Arc::new(infra::hash::DiskHashes),
        minimized: args.iter().any(|a| a == "--minimized" || a == infra::cli::IN_TRAY),
        in_tray: args.iter().any(|a| a == infra::cli::IN_TRAY),
        command_line,
        startup_error,
    })
    .context("the window did not start")
}
