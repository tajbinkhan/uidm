//! UDM's window (ADR 0002, AGENTS.md section 7): the Tauri shell that owns
//! the window's state, its windows and the tray, and serves the web
//! frontend in `frontend/` its views.

// Tests may use todo!, unimplemented! and unreachable!, which clippy.toml's
// test exemptions do not cover (AGENTS.md section 9).
#![cfg_attr(test, allow(clippy::todo, clippy::unimplemented, clippy::unreachable))]

mod actions;
mod actor;
mod commands;
mod contract;
mod drag;
mod errors;
mod extras;
mod filework;
#[cfg(test)]
mod fixtures;
mod grabber;
mod i18n;
mod layout;
mod options;
mod projection;
mod reports;
mod scheduler;
mod services;
mod state;
#[cfg(test)]
mod testing;
mod theme;
mod tips;
mod tools;
mod tray;
mod watchdog;
mod windows;

use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use tauri::{Manager, RunEvent, WindowEvent};
use udm_application::daemon::DaemonClient;
use udm_application::window::{
    BrowserRegistry, Clipboard, FilePicker, GrabLibrary, ListFiles, SystemServices, ThemeFiles, WindowFiles,
};
use udm_domain::ports::{Clock, DownloadFiles, FileOpener, GrabDownloads, ProblemReports, SaveFolders, Timestamps};

pub use errors::UiError;

/// The `client` this window names in its `hello`, so the daemon knows a
/// window is open to show a save-as dialog. `main` builds the daemon client
/// with it.
pub const WINDOW_CLIENT: &str = "window";

use actor::{CurrentLanguage, Inbox, Outbox};
use windows::Label;

/// What `main` hands the window: the daemon connection and the ports.
pub struct Start {
    pub daemon: Arc<dyn DaemonClient>,
    pub clock: Arc<dyn Clock>,
    pub times: Arc<dyn Timestamps>,
    pub opener: Arc<dyn FileOpener>,
    /// The system's file and folder dialogs, for the Browse buttons.
    pub pickers: Arc<dyn FilePicker>,
    /// The grabber's projects, templates and filters, when their folder
    /// can be found.
    pub library: Option<Arc<dyn GrabLibrary>>,
    /// The downloads a crawl's finds become.
    pub found: Arc<dyn GrabDownloads>,
    /// The operating system's services: the credential store for the
    /// grabber's logins.
    pub system: Arc<dyn SystemServices>,
    /// Importing and exporting the download list.
    pub lists: Arc<dyn ListFiles>,
    /// The system's Downloads folder.
    pub folders: Arc<dyn SaveFolders>,
    /// The write check of the folder Download File Info names.
    pub files: Arc<dyn DownloadFiles>,
    /// The window's own files: its layout and the drop target's place.
    pub window_files: Arc<dyn WindowFiles>,
    /// Registering UDM with the browsers, for Options.
    pub browsers: Arc<dyn BrowserRegistry>,
    /// The computer's network adapters, for Options' Network page.
    pub adapters: Arc<dyn udm_domain::ports::NetworkAdapters>,
    /// The theme files, for the custom themes.
    pub themes: Arc<dyn ThemeFiles>,
    /// Problem reports: the ones a crash left, and the watchdog's.
    pub reports: Arc<dyn ProblemReports>,
    /// The clipboard, for the clipboard monitor.
    pub clipboard: Arc<dyn Clipboard>,
    /// Hashing a finished download's file, for Verify checksum.
    pub hashes: Arc<dyn udm_domain::ports::FileHashes>,
    /// Start in the tray, without showing the main window.
    pub minimized: bool,
    /// Started by a window closing to the tray: wait there, and do not hand
    /// over to the window that is going.
    pub in_tray: bool,
    /// A command line to run, such as a download to add.
    pub command_line: Option<udm_domain::download::CommandLine>,
    /// Why the command line could not be read, to show once the window is up.
    pub startup_error: Option<String>,
}

/// Open the window and run it until the user quits from the tray.
pub fn run(start: Start) -> Result<(), UiError> {
    let builder = commands::builder();
    let invoke = builder.invoke_handler();
    let language: CurrentLanguage = Arc::new(AtomicUsize::new(0));
    let Start {
        daemon,
        clock,
        times,
        opener,
        pickers,
        library,
        found,
        system,
        lists,
        folders,
        files,
        window_files,
        browsers,
        adapters,
        themes,
        reports,
        clipboard,
        hashes,
        mut minimized,
        in_tray,
        command_line,
        mut startup_error,
    } = start;
    // Started again while a window is open, the daemon brings that one
    // forward or runs the command line there, and this copy opens no second
    // window. A command line kept for the next
    // window runs once this one connects; its dialogs bring it forward.
    let given = command_line.as_ref().is_some_and(|c| !c.is_empty());
    let handed = if in_tray { Ok(false) } else { daemon.run_command_line(command_line.unwrap_or_default()) };
    match handed {
        Ok(true) => return Ok(()),
        Ok(false) => minimized |= given,
        Err(e) => {
            let error = match e {
                udm_application::daemon::DaemonError::Closed => "the daemon closed the connection".to_string(),
                udm_application::daemon::DaemonError::Connect(why) => why,
                udm_application::daemon::DaemonError::Refused(failure) => {
                    errors::failure(&i18n::LANGUAGES[0], &failure)
                }
            };
            startup_error = Some(i18n::fill(
                i18n::LANGUAGES[0].tr("Could not pass the command line to UDM: {error}"),
                &[("error", &error)],
            ));
        }
    }
    let beats = Arc::new(watchdog::Beats::default());
    let app = tauri::Builder::default()
        // A second copy hands over and exits; this one comes forward.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            windows::show_main(app, &i18n::LANGUAGES[0]);
        }))
        .invoke_handler(invoke)
        .setup(move |app| {
            builder.mount_events(app);
            let ports = actor::Ports {
                library,
                found,
                system,
                lists: Arc::clone(&lists),
                window_files,
                folders,
                browsers,
                adapters,
                themes,
                reports: Arc::clone(&reports),
                beats: Arc::clone(&beats),
                clipboard,
                hashes,
                language: Arc::clone(&language),
                daemon,
                clock,
                times,
                opener,
            };
            let watched = watchdog::Watched {
                beats: Arc::clone(&beats),
                daemon: Arc::clone(&ports.daemon),
                reports,
                times: Arc::clone(&ports.times),
            };
            let outbox: Outbox = actor::spawn(app.handle().clone(), ports);
            watchdog::start(watched, outbox.clone());
            if let Some(error) = startup_error.take() {
                let _ = outbox.send(Inbox::Ui(state::UiMessage::StartupError(error)));
            }
            app.manage::<commands::Heart>(beats);
            app.manage(outbox.clone());
            app.manage(language);
            app.manage::<commands::Pickers>(pickers);
            app.manage::<commands::Lists>(lists);
            app.manage::<commands::Files>(files);
            let lang = &i18n::LANGUAGES[0];
            if let Err(e) = tray::install(app.handle(), outbox, lang) {
                // Linux desktops without a tray still get the window.
                tracing::warn!(error = %e, "no tray icon");
            }
            if !minimized {
                windows::open(app.handle(), Label::Main, lang)?;
            }
            Ok(())
        })
        .on_window_event(|window, event| match event {
            // Closing the main window puts it in the tray; quitting goes
            // through the tray's menu.
            WindowEvent::CloseRequested { api, .. } if window.label() == "main" => {
                api.prevent_close();
                let _ = window.hide();
            }
            WindowEvent::Destroyed => {
                if let Some(outbox) = window.try_state::<Outbox>() {
                    let _ = outbox.send(Inbox::Closed(window.label().to_string()));
                }
            }
            _ => {}
        })
        .build(tauri::generate_context!())?;
    app.run(|_, event| {
        // The last window closing does not quit: UDM stays in the tray.
        if let RunEvent::ExitRequested { api, code: None, .. } = event {
            api.prevent_exit();
        }
    });
    Ok(())
}

/// Write the TypeScript bindings to `path`.
pub fn export_bindings(path: &std::path::Path) -> Result<(), UiError> {
    let ts = specta_typescript::Typescript::default().header("// Generated by tauri-specta from crates/ui. Do not edit; run\n// `UDM_WRITE_BINDINGS=1 cargo test -p udm-ui bindings` instead.\n// @ts-nocheck");
    commands::builder().export(ts, path).map_err(|e| UiError::Bindings(e.to_string()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    /// The committed bindings match the code (AGENTS.md section 7.2). With
    /// `UDM_WRITE_BINDINGS=1` the test writes them instead.
    #[test]
    fn bindings_are_current() {
        let committed = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../frontend/src/bindings.ts");
        if std::env::var_os("UDM_WRITE_BINDINGS").is_some() {
            super::export_bindings(&committed).unwrap();
            return;
        }
        let fresh = std::env::temp_dir().join(format!("udm-bindings-{}.ts", std::process::id()));
        super::export_bindings(&fresh).unwrap();
        let (a, b) =
            (std::fs::read_to_string(&fresh).unwrap(), std::fs::read_to_string(&committed).unwrap_or_default());
        let _ = std::fs::remove_file(&fresh);
        assert!(
            a == b,
            "frontend/src/bindings.ts is out of date: run UDM_WRITE_BINDINGS=1 cargo test -p udm-ui bindings"
        );
    }
}
