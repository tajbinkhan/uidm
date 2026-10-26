//! User settings, kept in `%APPDATA%\UIDM\settings.json`.
//!
//! The daemon loads them at start and owns them: clients read them with
//! `get_settings` and change them with `set_settings`, and every client hears
//! a `settings` event when they change. A missing file, or a missing field in
//! an older file, means the default for that value, so new options can be
//! added without breaking existing files.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Parallel connections per download unless the settings say otherwise.
pub const DEFAULT_CONNECTIONS: usize = 8;
/// How many downloads run at once unless the settings say otherwise.
pub const DEFAULT_MAX_CONCURRENT: usize = 3;
/// Speed limit offered when the user first turns the limiter on, in KB/s.
pub const DEFAULT_SPEED_LIMIT_KBPS: u64 = 1024;

pub const CONNECTIONS_RANGE: (usize, usize) = (1, 16);
pub const MAX_CONCURRENT_RANGE: (usize, usize) = (1, 10);
/// 1 KB/s up to about 10 GB/s.
pub const SPEED_LIMIT_RANGE_KBPS: (u64, u64) = (1, 10_000_000);

/// What to do when a new download's file name is already taken by a file that
/// is not a partial copy of the same download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileExists {
    /// Save as `name (1).ext`, `name (2).ext`, and so on.
    #[default]
    Rename,
    /// Replace the existing file.
    Overwrite,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Where new downloads are saved. `None` means the user's Downloads folder.
    pub download_dir: Option<String>,
    /// Parallel connections per file.
    pub connections: usize,
    /// Downloads that run at once; the rest wait in the queue.
    pub max_concurrent: usize,
    /// Whether the global speed limit is on. Kept apart from the value so the
    /// quick toggle remembers the last limit.
    pub speed_limit_enabled: bool,
    /// Total speed across every download, in KB/s (1 KB = 1024 bytes).
    pub speed_limit_kbps: u64,
    /// Queue interrupted downloads again when the daemon starts.
    pub resume_at_startup: bool,
    pub file_exists: FileExists,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            download_dir: None,
            connections: DEFAULT_CONNECTIONS,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            speed_limit_enabled: false,
            speed_limit_kbps: DEFAULT_SPEED_LIMIT_KBPS,
            resume_at_startup: false,
            file_exists: FileExists::Rename,
        }
    }
}

impl Settings {
    /// Check every value, returning a message fit to show the user.
    pub fn validate(&self) -> Result<(), String> {
        let (lo, hi) = CONNECTIONS_RANGE;
        if !(lo..=hi).contains(&self.connections) {
            return Err(format!("Connections per file must be between {lo} and {hi}."));
        }
        let (lo, hi) = MAX_CONCURRENT_RANGE;
        if !(lo..=hi).contains(&self.max_concurrent) {
            return Err(format!("Downloads at once must be between {lo} and {hi}."));
        }
        let (lo, hi) = SPEED_LIMIT_RANGE_KBPS;
        if !(lo..=hi).contains(&self.speed_limit_kbps) {
            return Err(format!("The speed limit must be between {lo} and {hi} KB/s."));
        }
        if let Some(dir) = &self.download_dir {
            if dir.trim().is_empty() {
                return Err("The download folder is empty.".to_string());
            }
            if !Path::new(dir).is_absolute() {
                return Err(format!("The download folder must be a full path: {dir}"));
            }
            if Path::new(dir).exists() && !Path::new(dir).is_dir() {
                return Err(format!("The download folder is a file, not a folder: {dir}"));
            }
        }
        Ok(())
    }

    /// The folder new downloads go to.
    pub fn download_dir(&self) -> PathBuf {
        match &self.download_dir {
            Some(dir) => PathBuf::from(dir),
            None => default_downloads_dir(),
        }
    }

    /// The speed limit in bytes per second, or `None` when it is off.
    pub fn speed_limit_bytes(&self) -> Option<u64> {
        self.speed_limit_enabled
            .then(|| self.speed_limit_kbps.saturating_mul(1024))
    }

    /// Read settings from `path`. A missing file gives the defaults; an
    /// unreadable or invalid one is an error, so the caller can say so instead
    /// of silently dropping the user's choices.
    pub fn load(path: &Path) -> io::Result<Settings> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Settings::default()),
            Err(e) => return Err(e),
        };
        let settings: Settings = serde_json::from_slice(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        settings
            .validate()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(settings)
    }

    /// Write settings to `path` through a temp file and a rename, so a crash
    /// never leaves a half-written file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        write_atomic(path, &bytes)
    }
}

/// `%APPDATA%\UIDM\settings.json`.
pub fn settings_path() -> io::Result<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "APPDATA is not set"))?;
    Ok(PathBuf::from(base).join("UIDM").join("settings.json"))
}

/// The current user's Downloads folder, or the temp dir if it can't be found.
pub fn default_downloads_dir() -> PathBuf {
    if let Ok(profile) = std::env::var("USERPROFILE") {
        let dir = PathBuf::from(profile).join("Downloads");
        if dir.is_dir() {
            return dir;
        }
    }
    std::env::temp_dir()
}

/// Write `bytes` to `path` by writing a sibling temp file and renaming it over
/// the target. Creates the parent folder if needed.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    fs::write(&tmp, bytes)?;
    // Rename onto an existing file can fail on Windows; retry after removing.
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) => {
            let _ = fs::remove_file(path);
            fs::rename(&tmp, path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("uidm_settings_{}_{name}", std::process::id()))
    }

    #[test]
    fn missing_file_gives_defaults() {
        let s = Settings::load(&temp_path("missing.json")).unwrap();
        assert_eq!(s, Settings::default());
        assert_eq!(s.connections, DEFAULT_CONNECTIONS);
        assert_eq!(s.max_concurrent, DEFAULT_MAX_CONCURRENT);
        assert_eq!(s.speed_limit_bytes(), None);
    }

    #[test]
    fn missing_fields_take_defaults() {
        let s: Settings = serde_json::from_str(r#"{"connections":4}"#).unwrap();
        assert_eq!(s.connections, 4);
        assert_eq!(s.max_concurrent, DEFAULT_MAX_CONCURRENT);
        assert_eq!(s.file_exists, FileExists::Rename);
    }

    #[test]
    fn save_load_roundtrip() {
        let path = temp_path("roundtrip.json");
        let s = Settings {
            download_dir: Some("C:\\Downloads\\UIDM".into()),
            connections: 12,
            max_concurrent: 5,
            speed_limit_enabled: true,
            speed_limit_kbps: 500,
            resume_at_startup: true,
            file_exists: FileExists::Overwrite,
        };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
        assert_eq!(s.speed_limit_bytes(), Some(500 * 1024));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn invalid_file_is_an_error() {
        let path = temp_path("invalid.json");
        fs::write(&path, b"{ not json").unwrap();
        assert!(Settings::load(&path).is_err());
        fs::write(&path, br#"{"connections":99}"#).unwrap();
        assert!(Settings::load(&path).is_err());
        fs::remove_file(&path).ok();
    }

    #[test]
    fn validation_bounds() {
        let ok = Settings::default();
        assert!(ok.validate().is_ok());

        for (connections, valid) in [(0, false), (1, true), (16, true), (17, false)] {
            let s = Settings { connections, ..Settings::default() };
            assert_eq!(s.validate().is_ok(), valid, "connections {connections}");
        }
        for (max_concurrent, valid) in [(0, false), (1, true), (10, true), (11, false)] {
            let s = Settings { max_concurrent, ..Settings::default() };
            assert_eq!(s.validate().is_ok(), valid, "max_concurrent {max_concurrent}");
        }
        let s = Settings { speed_limit_kbps: 0, ..Settings::default() };
        assert!(s.validate().is_err());
    }

    #[test]
    fn validation_of_folder() {
        let relative = Settings {
            download_dir: Some("Downloads".into()),
            ..Settings::default()
        };
        assert!(relative.validate().is_err());

        let empty = Settings {
            download_dir: Some("  ".into()),
            ..Settings::default()
        };
        assert!(empty.validate().is_err());

        let file = temp_path("not-a-dir.txt");
        fs::write(&file, b"x").unwrap();
        let s = Settings {
            download_dir: Some(file.display().to_string()),
            ..Settings::default()
        };
        assert!(s.validate().is_err());
        fs::remove_file(&file).ok();

        let dir = Settings {
            download_dir: Some(std::env::temp_dir().display().to_string()),
            ..Settings::default()
        };
        assert!(dir.validate().is_ok());
    }
}
