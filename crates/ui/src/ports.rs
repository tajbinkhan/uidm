//! The ports the window's views reach beyond the daemon connection: the
//! clock, the save folders, opening files and the window's own files.
//!
//! `main` installs them once, before the window opens, with the adapters
//! from `uidm_infrastructure`. Views read them through [`ports`]. This is a
//! step on the way to #199, where views get what they need through
//! `AppState` instead.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use uidm_application::window::{
    BrowserRegistry, FilePicker, GrabLibrary, ListFiles, SettingsChecks, SystemServices, WindowFiles,
};
use uidm_domain::ports::{Clock, DownloadFiles, FileOpener, GrabDownloads, SaveFolders, Timestamps};
use uidm_domain::queue::Moment;

/// What the views reach through ports.
pub struct Ports {
    /// The local date and time.
    pub clock: Arc<dyn Clock>,
    /// The time as seconds since 1970.
    pub times: Arc<dyn Timestamps>,
    /// The system's Downloads folder.
    pub folders: Arc<dyn SaveFolders>,
    /// Whether UIDM may write to a folder.
    pub files: Arc<dyn DownloadFiles>,
    /// Opening and showing files.
    pub opener: Arc<dyn FileOpener>,
    /// The window's layout and drop target files.
    pub window_files: Arc<dyn WindowFiles>,
    /// The grabber's projects, templates and filters, when their folder
    /// can be found.
    pub grab: Option<Arc<dyn GrabLibrary>>,
    /// The downloads a crawl's finds become.
    pub found: Arc<dyn GrabDownloads>,
    /// Exporting and importing the download list.
    pub lists: Arc<dyn ListFiles>,
    /// The settings' checks that look at the disk.
    pub checks: Arc<dyn SettingsChecks>,
    /// The operating system: its proxy, wake timers, sounds and
    /// credential store.
    pub system: Arc<dyn SystemServices>,
    /// Registering UIDM with the browsers.
    pub browsers: Arc<dyn BrowserRegistry>,
    /// The system's file and folder dialogs, called on workers.
    pub pickers: Arc<dyn FilePicker>,
}

impl Ports {
    /// The local date and time now.
    pub fn now(&self) -> Moment {
        self.clock.now()
    }

    /// Seconds since 1970 now.
    pub fn unix_now(&self) -> u64 {
        self.times.unix_now()
    }

    /// The system's Downloads folder.
    pub fn downloads_dir(&self) -> PathBuf {
        self.folders.downloads_dir()
    }

    /// Whether UIDM may write to `folder` without administrator rights.
    pub fn can_write(&self, folder: &Path) -> bool {
        self.files.can_write(folder)
    }
}

static PORTS: OnceLock<Ports> = OnceLock::new();

/// Install the ports. Only the first call counts.
pub fn install(ports: Ports) {
    let _ = PORTS.set(ports);
}

/// The ports `main` installed.
pub fn ports() -> &'static Ports {
    #[cfg(test)]
    return PORTS.get_or_init(fakes::ports);
    // `main` installs the ports before it opens the window, and only the
    // window's frames and the threads it starts call this.
    #[cfg(not(test))]
    #[allow(clippy::expect_used)]
    PORTS.get().expect("the ports are installed before the window opens")
}

/// Stand-ins for the tests: UTC as the local time, a Downloads folder in
/// the temporary folder, every folder writable, no files opened or saved,
/// no grabber library, empty lists and a system without proxies, wake
/// timers or sounds, no browsers, and pickers the user always cancels.
#[cfg(test)]
mod fakes {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    use uidm_application::window::{
        BrowserSlot, FileKind, IdentifiedBrowser, ListError, ListFormat, SystemFonts, SystemProxy,
    };
    use uidm_domain::download::{Download, DownloadEntry};
    use uidm_domain::grab::{GrabFile, GrabRequest};
    use uidm_domain::ports::{FilesError, SettingsError};
    use uidm_domain::settings::{Settings, Sound};

    struct Utc;

    fn unix() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
    }

    impl Clock for Utc {
        fn now(&self) -> Moment {
            let minutes = unix() / 60;
            Moment { day: (minutes / 1440) as i64, minute: (minutes % 1440) as u32 }
        }
    }

    impl Timestamps for Utc {
        fn unix_now(&self) -> u64 {
            unix()
        }
    }

    struct Temp;

    impl SaveFolders for Temp {
        fn downloads_dir(&self) -> PathBuf {
            std::env::temp_dir().join("uidm-test-downloads")
        }
    }

    impl DownloadFiles for Temp {
        fn recycle(&self, _: Option<&str>, _: Option<&str>) -> Result<(), FilesError> {
            Ok(())
        }
        fn forget_resume(&self, _: &[&str]) {}
        fn move_finished(&self, _: &Path, _: &Path) -> Result<(), FilesError> {
            Ok(())
        }
        fn move_partial(&self, _: &Path, _: &Path) -> Result<(), FilesError> {
            Ok(())
        }
        fn exists(&self, path: &Path) -> bool {
            path.exists()
        }
        fn is_file(&self, path: &Path) -> bool {
            path.is_file()
        }
        fn can_write(&self, _: &Path) -> bool {
            true
        }
    }

    impl FileOpener for Temp {
        fn open(&self, _: &str) {}
        fn reveal(&self, _: &str) {}
    }

    impl WindowFiles for Temp {
        fn read(&self, _: &str) -> Option<Vec<u8>> {
            None
        }
        fn save(&self, _: &str, _: Vec<u8>) {}
    }

    impl GrabDownloads for Temp {
        fn download_for(&self, _: &GrabRequest, f: &GrabFile, _: &Settings, _: Option<&str>) -> Option<Download> {
            Some(Download { url: f.url.clone(), ..Download::default() })
        }
    }

    impl ListFiles for Temp {
        fn read(&self, path: &Path) -> Result<String, ListError> {
            Err(ListError(format!("{} is not read in tests", path.display())))
        }
        fn write(&self, path: &Path, _: &str) -> Result<(), ListError> {
            Err(ListError(format!("{} is not written in tests", path.display())))
        }
        fn import(&self, _: &str) -> Result<(Vec<Download>, usize), ListError> {
            Ok((Vec::new(), 0))
        }
        fn export(&self, _: ListFormat, _: &[&DownloadEntry]) -> String {
            String::new()
        }
    }

    impl SettingsChecks for Temp {
        fn validate(&self, settings: &Settings) -> Result<(), SettingsError> {
            settings.validate().map_err(SettingsError)
        }
        fn ffmpeg(&self, _: &Settings) -> Option<PathBuf> {
            None
        }
    }

    impl SystemServices for Temp {
        fn fonts(&self) -> SystemFonts {
            SystemFonts::default()
        }
        fn fixed_proxy(&self) -> Option<SystemProxy> {
            None
        }
        fn wake_timers_allowed(&self) -> Option<(bool, bool)> {
            None
        }
        fn can_elevate(&self) -> bool {
            false
        }
        fn can_set_creation_time(&self) -> bool {
            false
        }
        fn play(&self, _: Sound, _: Option<&Path>) {}
        fn save_credential(&self, _: &str, _: &str, _: &str) -> Result<(), FilesError> {
            Ok(())
        }
    }

    impl FilePicker for Temp {
        fn pick_folder(&self, _: &str, _: Option<&Path>) -> Option<PathBuf> {
            None
        }
        fn pick_file(&self, _: &str, _: &[FileKind], _: Option<&Path>) -> Option<PathBuf> {
            None
        }
        fn save_file(&self, _: &str, _: &str, _: &[FileKind]) -> Option<PathBuf> {
            None
        }
    }

    impl BrowserRegistry for Temp {
        fn known(&self) -> Vec<(BrowserSlot, bool)> {
            Vec::new()
        }
        fn identify(&self, _: &Path) -> Option<IdentifiedBrowser> {
            None
        }
        fn registered(&self, _: &BrowserSlot) -> bool {
            false
        }
        fn set_registered(&self, _: &BrowserSlot, _: bool) -> Result<(), FilesError> {
            Ok(())
        }
        fn display_name(&self, program: &Path) -> String {
            program.display().to_string()
        }
        fn chrome_readers(&self) -> Vec<String> {
            Vec::new()
        }
        fn can_restart(&self, _: &BrowserSlot, _: Option<&Path>) -> bool {
            false
        }
        fn restart(&self, _: &BrowserSlot, _: Option<&Path>) -> Result<(), FilesError> {
            Ok(())
        }
    }

    pub fn ports() -> Ports {
        Ports {
            clock: Arc::new(Utc),
            times: Arc::new(Utc),
            folders: Arc::new(Temp),
            files: Arc::new(Temp),
            opener: Arc::new(Temp),
            window_files: Arc::new(Temp),
            grab: None,
            found: Arc::new(Temp),
            lists: Arc::new(Temp),
            checks: Arc::new(Temp),
            system: Arc::new(Temp),
            browsers: Arc::new(Temp),
            pickers: Arc::new(Temp),
        }
    }
}
