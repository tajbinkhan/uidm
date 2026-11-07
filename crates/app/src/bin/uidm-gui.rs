//! UIDM's desktop window. The views live in `uidm_ui`; this binary builds
//! the adapters they reach through ports and starts the window
//! (AGENTS.md section 8).

#![windows_subsystem = "windows"]

use std::sync::Arc;

use uidm_infrastructure as infra;

fn main() -> eframe::Result {
    infra::logging::init(infra::logging::Program::Gui);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().unwrap_or_default();
    let ports = uidm_ui::Ports {
        clock: Arc::new(infra::clock::LocalClock),
        times: Arc::new(infra::clock::LocalClock),
        folders: Arc::new(infra::files::DiskFiles),
        files: Arc::new(infra::files::DiskFiles),
        opener: Arc::new(infra::platform::Desktop),
        window_files: Arc::new(infra::window_files::WindowDir::new()),
        grab: infra::grab::projects_dir().ok().map(|dir| Arc::new(infra::grab::ProjectFiles(dir)) as _),
        found: Arc::new(infra::grab::FoundFiles),
        lists: Arc::new(infra::listfile::ListFormats),
        checks: Arc::new(infra::settings::DiskChecks),
        system: Arc::new(infra::system::ThisSystem),
        browsers: Arc::new(infra::browsers::Registry),
        pickers: Arc::new(infra::pickers::NativePicker),
    };
    uidm_ui::run(uidm_ui::Start {
        ports,
        daemon: Arc::new(infra::daemon_client::IpcDaemonClient::new(uidm_ui::WINDOW_CLIENT)),
        minimized: args.iter().any(|a| a == "--minimized"),
        command_line: infra::cli::parse(&args, &cwd).transpose(),
        instance: infra::paths::instance(),
    })
}
