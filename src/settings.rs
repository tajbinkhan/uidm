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
/// Up to 10 GB, in KB.
pub const CAPTURE_MIN_SIZE_MAX_KB: u64 = 10 * 1024 * 1024;

/// The category for files no other category claims. It is not stored in the
/// list; its files go straight into the download folder.
pub const GENERAL: &str = "General";

/// How UIDM tells you a download finished or failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Notify {
    Off,
    /// A Windows notification.
    #[default]
    Toast,
    /// A dialog in the UIDM window, as IDM does; a notification when no
    /// window is open.
    Dialog,
}

/// Which virus scanner checks finished downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scanner {
    /// Microsoft Defender's command-line scanner.
    #[default]
    Defender,
    /// `scanner_command`, with `{path}` replaced by the file.
    Custom,
}

/// Which proxy downloads go through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyMode {
    /// Connect directly.
    #[default]
    None,
    /// The proxy set in Windows' Internet settings.
    System,
    /// The proxies set in UIDM's options.
    Manual,
}

/// A login for the sites matching `site`. The password lives in Windows
/// Credential Manager; it travels only from a window setting it to the
/// daemon, and is never written to the settings file or sent back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteLogin {
    /// `example.com` (with its subdomains) or a pattern such as `dl*.example.com`.
    pub site: String,
    pub username: String,
    /// A new password to store; `None` keeps the stored one.
    #[serde(default, skip_serializing)]
    pub password: Option<String>,
}

/// A kind of file with its own folder, chosen by file extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    pub name: String,
    /// Lowercase extensions without the dot, such as `zip`.
    pub extensions: Vec<String>,
    /// Where its files go. `None` means a folder named after the category
    /// inside the download folder.
    #[serde(default)]
    pub folder: Option<String>,
}

impl Category {
    fn new(name: &str, extensions: &[&str]) -> Self {
        Category {
            name: name.to_string(),
            extensions: extensions.iter().map(|e| e.to_string()).collect(),
            folder: None,
        }
    }
}

/// The categories a new settings file starts with.
pub fn default_categories() -> Vec<Category> {
    vec![
        Category::new("Compressed", &["zip", "rar", "7z", "tar", "gz", "bz2", "xz"]),
        Category::new(
            "Documents",
            &["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "rtf", "odt"],
        ),
        Category::new("Music", &["mp3", "flac", "wav", "m4a", "aac", "ogg"]),
        Category::new("Programs", &["exe", "msi", "apk", "msix"]),
        Category::new("Video", &["mp4", "mkv", "avi", "mov", "webm", "wmv"]),
    ]
}

/// File types the extension captures by default: every category's, plus disk
/// images and packages.
pub fn default_capture_types() -> Vec<String> {
    let mut types: Vec<String> = default_categories()
        .into_iter()
        .flat_map(|c| c.extensions)
        .collect();
    types.extend(["iso", "img", "dmg", "deb", "rpm", "bin"].map(String::from));
    types
}

/// The lowercase extension of a file name, without the dot. A name that only
/// starts with a dot (`.bashrc`) has none.
pub fn extension_of(filename: &str) -> Option<String> {
    let ext = Path::new(filename).extension()?.to_str()?;
    (!ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// Turn user input such as `.ZIP, mp4 ;7z` into `["zip", "mp4", "7z"]`.
pub fn parse_extensions(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in text.split(|c: char| c == ',' || c == ';' || c.is_whitespace()) {
        let ext = part.trim().trim_start_matches('.').to_ascii_lowercase();
        if !ext.is_empty() && !out.contains(&ext) {
            out.push(ext);
        }
    }
    out
}

/// Where each new download's file goes: a category's folder chosen by the
/// final file name, or one fixed folder (a folder the user picked).
#[derive(Debug, Clone, PartialEq)]
pub struct Folders {
    default: PathBuf,
    categories: Vec<(Vec<String>, PathBuf)>,
}

impl Folders {
    /// Every file goes to `dir`.
    pub fn fixed(dir: PathBuf) -> Self {
        Folders {
            default: dir,
            categories: Vec::new(),
        }
    }

    /// The folder for a file named `filename`.
    pub fn for_name(&self, filename: &str) -> PathBuf {
        let ext = extension_of(filename);
        self.categories
            .iter()
            .find(|(exts, _)| ext.as_ref().is_some_and(|e| exts.contains(e)))
            .map(|(_, dir)| dir.clone())
            .unwrap_or_else(|| self.default.clone())
    }
}

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
    /// Show a save-as dialog for each new download before it starts.
    pub ask_where_to_save: bool,
    /// File types and their folders. Files of no listed type are `General`.
    pub categories: Vec<Category>,
    /// Extensions the browser extension takes from the browser. Empty means
    /// every type.
    pub capture_types: Vec<String>,
    /// Files the browser reports as smaller than this, in KB, stay in the
    /// browser. 0 captures every size.
    pub capture_min_size_kb: u64,
    /// Sites whose downloads stay in the browser, such as `*.example.com`.
    pub capture_excluded_sites: Vec<String>,
    /// Closing the window hides it in the tray instead of quitting.
    pub close_to_tray: bool,
    /// Start the window, hidden in the tray, when you sign in to Windows.
    pub start_with_windows: bool,
    pub notify: Notify,
    /// Play a sound when a download finishes.
    pub sound_on_done: bool,
    /// Play a sound when a download fails.
    pub sound_on_failed: bool,
    /// Scan each finished download for viruses.
    pub scan_after_download: bool,
    pub scanner: Scanner,
    /// The custom scanner's command line, such as
    /// `"C:\ClamAV\clamscan.exe" --no-summary {path}`.
    pub scanner_command: String,
    pub proxy_mode: ProxyMode,
    /// `host:port` of the manual proxy for http URLs.
    pub proxy_http: String,
    /// `host:port` of the manual proxy for https URLs; empty uses the http one.
    pub proxy_https: String,
    /// `host:port` of a manual SOCKS5 proxy for everything; wins when set.
    pub proxy_socks5: String,
    /// User name for the manual proxy, if it needs one.
    pub proxy_username: String,
    /// A new proxy password to store (empty clears it); `None` keeps the
    /// stored one. Kept with DPAPI, never in this file.
    #[serde(default, skip_serializing)]
    pub proxy_password: Option<String>,
    /// For a download taken from the browser, use the proxy the browser uses.
    pub use_browser_proxy: bool,
    pub site_logins: Vec<SiteLogin>,
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
            ask_where_to_save: true,
            categories: default_categories(),
            capture_types: default_capture_types(),
            capture_min_size_kb: 0,
            capture_excluded_sites: Vec::new(),
            close_to_tray: true,
            start_with_windows: false,
            notify: Notify::Toast,
            sound_on_done: false,
            sound_on_failed: false,
            scan_after_download: false,
            scanner: Scanner::Defender,
            scanner_command: String::new(),
            proxy_mode: ProxyMode::None,
            proxy_http: String::new(),
            proxy_https: String::new(),
            proxy_socks5: String::new(),
            proxy_username: String::new(),
            proxy_password: None,
            use_browser_proxy: false,
            site_logins: Vec::new(),
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
            check_folder("The download folder", dir)?;
        }

        let mut names: Vec<String> = Vec::new();
        for c in &self.categories {
            let name = c.name.trim();
            if name.is_empty() {
                return Err("Every category needs a name.".to_string());
            }
            if name.eq_ignore_ascii_case(GENERAL) {
                return Err(format!("\"{GENERAL}\" is kept for files of every other type."));
            }
            if names.contains(&name.to_lowercase()) {
                return Err(format!("There are two categories named \"{name}\"."));
            }
            names.push(name.to_lowercase());
            if c.extensions.is_empty() {
                return Err(format!("The category \"{name}\" has no file types."));
            }
            check_extensions(&format!("The category \"{name}\""), &c.extensions)?;
            if let Some(dir) = &c.folder {
                check_folder(&format!("The folder of \"{name}\""), dir)?;
            }
        }

        check_extensions("The captured file types", &self.capture_types)?;
        if self.capture_min_size_kb > CAPTURE_MIN_SIZE_MAX_KB {
            return Err("The minimum size to capture is too large.".to_string());
        }
        for site in &self.capture_excluded_sites {
            let valid = !site.is_empty()
                && site
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '*' | ':'));
            if !valid {
                return Err(format!(
                    "\"{site}\" is not a site. Use a host name such as example.com or *.example.com."
                ));
            }
        }
        for (what, addr) in [
            ("The HTTP proxy", &self.proxy_http),
            ("The HTTPS proxy", &self.proxy_https),
            ("The SOCKS5 proxy", &self.proxy_socks5),
        ] {
            if !addr.trim().is_empty() && !is_host_port(addr.trim()) {
                return Err(format!("{what} must be host:port, such as proxy.example.com:8080."));
            }
        }
        if self.proxy_mode == ProxyMode::Manual
            && self.proxy_http.trim().is_empty()
            && self.proxy_https.trim().is_empty()
            && self.proxy_socks5.trim().is_empty()
        {
            return Err("Set at least one proxy, or choose another proxy option.".to_string());
        }
        let mut sites: Vec<String> = Vec::new();
        for l in &self.site_logins {
            let site = l.site.trim().to_ascii_lowercase();
            let valid = !site.is_empty()
                && site
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '*' | ':'));
            if !valid {
                return Err(format!(
                    "\"{}\" is not a site. Use a host name such as example.com.",
                    l.site
                ));
            }
            if l.username.trim().is_empty() {
                return Err(format!("The login for {site} needs a user name."));
            }
            if sites.contains(&site) {
                return Err(format!("There are two logins for {site}."));
            }
            sites.push(site);
        }
        if self.scan_after_download && self.scanner == Scanner::Custom {
            crate::scan::parse_command(&self.scanner_command)?;
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

    /// The category a file named `filename` belongs to, or `None` for
    /// `General`.
    pub fn category_of(&self, filename: &str) -> Option<&Category> {
        let ext = extension_of(filename)?;
        self.categories.iter().find(|c| c.extensions.contains(&ext))
    }

    /// The name of `filename`'s category, `General` when none claims it.
    pub fn category_name(&self, filename: &str) -> &str {
        self.category_of(filename).map_or(GENERAL, |c| c.name.as_str())
    }

    /// Where new downloads go, by category.
    pub fn folders(&self) -> Folders {
        let base = self.download_dir();
        Folders {
            categories: self
                .categories
                .iter()
                .map(|c| {
                    let dir = match &c.folder {
                        Some(f) => PathBuf::from(f),
                        None => base.join(c.name.trim()),
                    };
                    (c.extensions.clone(), dir)
                })
                .collect(),
            default: base,
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

fn check_folder(what: &str, dir: &str) -> Result<(), String> {
    if dir.trim().is_empty() {
        return Err(format!("{what} is empty."));
    }
    let path = Path::new(dir);
    if !path.is_absolute() {
        return Err(format!("{what} must be a full path: {dir}"));
    }
    if path.exists() && !path.is_dir() {
        return Err(format!("{what} is a file, not a folder: {dir}"));
    }
    Ok(())
}

fn check_extensions(what: &str, exts: &[String]) -> Result<(), String> {
    for e in exts {
        if e.is_empty() || !e.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) {
            return Err(format!(
                "{what} has an invalid file type \"{e}\". Use letters and digits, such as zip or mp4."
            ));
        }
    }
    Ok(())
}

/// `host:port` with a numeric port, or `[v6]:port`.
fn is_host_port(s: &str) -> bool {
    let Some((host, port)) = s.rsplit_once(':') else {
        return false;
    };
    !host.is_empty()
        && !host.contains(char::is_whitespace)
        && !host.contains('/')
        && port.parse::<u16>().is_ok_and(|p| p > 0)
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
            ask_where_to_save: false,
            categories: vec![Category::new("Books", &["epub"])],
            capture_types: vec!["zip".into()],
            capture_min_size_kb: 100,
            capture_excluded_sites: vec!["*.example.com".into()],
            close_to_tray: false,
            start_with_windows: true,
            notify: Notify::Dialog,
            sound_on_done: true,
            sound_on_failed: true,
            scan_after_download: true,
            scanner: Scanner::Custom,
            scanner_command: "C:\\scan.exe {path}".into(),
            proxy_mode: ProxyMode::Manual,
            proxy_http: "proxy:3128".into(),
            proxy_https: String::new(),
            proxy_socks5: String::new(),
            proxy_username: "me".into(),
            proxy_password: None,
            use_browser_proxy: true,
            site_logins: vec![SiteLogin {
                site: "files.example.com".into(),
                username: "ann".into(),
                password: None,
            }],
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

    #[test]
    fn category_matching() {
        let s = Settings::default();
        assert_eq!(s.category_name("setup.EXE"), "Programs");
        assert_eq!(s.category_name("Report.Final.PDF"), "Documents");
        assert_eq!(s.category_name("archive.tar.gz"), "Compressed");
        assert_eq!(s.category_name("movie.mkv"), "Video");
        assert_eq!(s.category_name("README"), GENERAL);
        assert_eq!(s.category_name(".bashrc"), GENERAL);
        assert_eq!(s.category_name("photo.jpg"), GENERAL);
        assert_eq!(s.category_name("trailing."), GENERAL);
    }

    #[test]
    fn category_folders() {
        let base = std::env::temp_dir().join("uidm-dl");
        let mut s = Settings {
            download_dir: Some(base.display().to_string()),
            ..Settings::default()
        };
        let custom = std::env::temp_dir().join("uidm-music");
        s.categories[2].folder = Some(custom.display().to_string());
        let f = s.folders();
        assert_eq!(f.for_name("a.ZIP"), base.join("Compressed"));
        assert_eq!(f.for_name("song.mp3"), custom);
        assert_eq!(f.for_name("noext"), base);
        assert_eq!(f.for_name("photo.jpg"), base);

        let fixed = Folders::fixed(base.join("picked"));
        assert_eq!(fixed.for_name("a.zip"), base.join("picked"));
    }

    #[test]
    fn parse_extension_lists() {
        assert_eq!(parse_extensions(".ZIP, mp4 ;7z  zip"), ["zip", "mp4", "7z"]);
        assert!(parse_extensions(" , ").is_empty());
    }

    #[test]
    fn category_and_capture_validation() {
        let with = |f: &dyn Fn(&mut Settings)| {
            let mut s = Settings::default();
            f(&mut s);
            s.validate()
        };
        assert!(with(&|s| s.categories[0].name = " ".into()).is_err());
        assert!(with(&|s| s.categories[0].name = "general".into()).is_err());
        assert!(with(&|s| s.categories[1].name = "Compressed".into()).is_err());
        assert!(with(&|s| s.categories[0].extensions.clear()).is_err());
        assert!(with(&|s| s.categories[0].extensions = vec!["Zip".into()]).is_err());
        assert!(with(&|s| s.categories[0].folder = Some("rel".into())).is_err());
        assert!(with(&|s| s.capture_types = vec!["a b".into()]).is_err());
        assert!(with(&|s| s.capture_types.clear()).is_ok());
        assert!(with(&|s| s.capture_excluded_sites = vec!["*.example.com".into()]).is_ok());
        assert!(with(&|s| s.capture_excluded_sites = vec!["http://x/".into()]).is_err());
        assert!(with(&|s| s.categories.clear()).is_ok());
    }

    #[test]
    fn passwords_never_reach_the_file() {
        let s = Settings {
            proxy_password: Some("proxy-secret".into()),
            site_logins: vec![SiteLogin {
                site: "example.com".into(),
                username: "ann".into(),
                password: Some("login-secret".into()),
            }],
            ..Settings::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("secret"), "{json}");
        assert!(json.contains("\"username\":\"ann\""));
    }

    #[test]
    fn proxy_and_login_validation() {
        let with = |f: &dyn Fn(&mut Settings)| {
            let mut s = Settings::default();
            f(&mut s);
            s.validate()
        };
        assert!(with(&|s| s.proxy_http = "proxy:3128".into()).is_ok());
        assert!(with(&|s| s.proxy_http = "[::1]:3128".into()).is_ok());
        assert!(with(&|s| s.proxy_http = "proxy".into()).is_err());
        assert!(with(&|s| s.proxy_http = "proxy:99999".into()).is_err());
        assert!(with(&|s| s.proxy_socks5 = "http://p:1".into()).is_err());
        assert!(with(&|s| s.proxy_mode = ProxyMode::Manual).is_err(), "manual with no proxy");
        let login = |site: &str, user: &str| SiteLogin {
            site: site.into(),
            username: user.into(),
            password: None,
        };
        assert!(with(&|s| s.site_logins = vec![login("a.com", "u")]).is_ok());
        assert!(with(&|s| s.site_logins = vec![login("a.com", " ")]).is_err());
        assert!(with(&|s| s.site_logins = vec![login("https://a.com/", "u")]).is_err());
        assert!(with(&|s| s.site_logins = vec![login("a.com", "u"), login("A.com", "v")]).is_err());
    }

    #[test]
    fn custom_scanner_needs_a_command_with_the_path() {
        let custom = |cmd: &str| Settings {
            scan_after_download: true,
            scanner: Scanner::Custom,
            scanner_command: cmd.into(),
            ..Settings::default()
        };
        assert!(custom("").validate().is_err());
        assert!(custom("scan.exe").validate().is_err());
        assert!(custom("\"C:\\Program Files\\AV\\scan.exe\" /file {path}").validate().is_ok());
        // Not checked while scanning is off or Defender is used.
        let off = Settings {
            scan_after_download: false,
            ..custom("")
        };
        assert!(off.validate().is_ok());
    }
}
