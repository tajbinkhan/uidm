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

/// A key held while clicking a link in the browser, to keep the download in
/// the browser or to send the link to UIDM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureKey {
    None,
    Alt,
    Ctrl,
    Shift,
}

impl CaptureKey {
    pub const ALL: [CaptureKey; 4] = [CaptureKey::None, CaptureKey::Alt, CaptureKey::Ctrl, CaptureKey::Shift];

    pub fn label(self) -> &'static str {
        match self {
            CaptureKey::None => "No key",
            CaptureKey::Alt => "Alt",
            CaptureKey::Ctrl => "Ctrl",
            CaptureKey::Shift => "Shift",
        }
    }
}

/// Where selecting text with links on a page shows a "Download with UIDM"
/// button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionButton {
    Off,
    #[default]
    Everywhere,
    /// Only on the sites in `selection_button_sites`.
    Sites,
}

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
    /// The proxy auto-config script at `proxy_pac_url`.
    Script,
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

/// The most connections UIDM opens to the hosts matching `site` (such as
/// `*.example.com`), across every download at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerLimit {
    pub site: String,
    pub max: usize,
}

/// The SOCKS version of a manual SOCKS proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SocksVersion {
    V4,
    #[default]
    V5,
}

/// The window's colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    /// Light or dark, as Windows' app mode is.
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::System, Theme::Light, Theme::Dark];
}

/// The window's text size, in percent of the normal size.
pub const TEXT_SIZE_RANGE: (u32, u32) = (80, 150);

/// Allowed lengths of the quota's window, in hours (up to a week).
pub const QUOTA_HOURS_RANGE: (u64, u64) = (1, 168);

/// Allowed values for a server's connection limit.
pub const SERVER_CONNECTIONS_RANGE: (usize, usize) = (1, 16);

/// A kind of file with its own folder, chosen by file extension and,
/// optionally, the site it comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    pub name: String,
    /// Lowercase extensions without the dot, such as `zip`.
    pub extensions: Vec<String>,
    /// Where its files go. `None` means a folder named after the category
    /// inside the download folder.
    #[serde(default)]
    pub folder: Option<String>,
    /// Only files from these sites (such as `*.example.com`) belong to it.
    /// Empty takes its types from every site.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<String>,
    /// New files go to the folder last chosen for this category in the
    /// save-as dialog.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remember_folder: bool,
    /// That folder, once one was chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_folder: Option<String>,
}

impl Category {
    pub fn new(name: &str, extensions: &[&str]) -> Self {
        Category {
            name: name.to_string(),
            extensions: extensions.iter().map(|e| e.to_string()).collect(),
            folder: None,
            sites: Vec::new(),
            remember_folder: false,
            last_folder: None,
        }
    }

    /// Whether a file with extension `ext` from `host` belongs here.
    fn takes(&self, ext: &str, host: &str) -> bool {
        self.extensions.iter().any(|e| e == ext)
            && (self.sites.is_empty() || self.sites.iter().any(|s| crate::net::site_matches(s, host)))
    }
}

/// The host of `url`, in lower case, or "" if it has none.
fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default()
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
    /// Each category's rules and folder, in order.
    categories: Vec<(Category, PathBuf)>,
    /// The download's host, for categories limited to some sites.
    host: String,
}

impl Folders {
    /// Every file goes to `dir`.
    pub fn fixed(dir: PathBuf) -> Self {
        Folders {
            default: dir,
            categories: Vec::new(),
            host: String::new(),
        }
    }

    /// The folder for a file named `filename`.
    pub fn for_name(&self, filename: &str) -> PathBuf {
        let ext = extension_of(filename);
        self.categories
            .iter()
            .find(|(c, _)| ext.as_ref().is_some_and(|e| c.takes(e, &self.host)))
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
    /// Stop before writing anything and let the user choose in the save-as
    /// dialog. Without a window to answer, the download gets a new name.
    Ask,
}

/// What to do when a new download's address is already in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Duplicate {
    /// Hold it in the save-as dialog until the user picks one of the others.
    /// Without a window to answer, it is added with a new name. Many
    /// downloads added at once skip their duplicates instead of asking.
    #[default]
    Ask,
    /// Add it anyway; if the other download's file is in the way, the new one
    /// gets a numbered name.
    NewName,
    /// Add it in place of the other download: the other one leaves the list
    /// and the new one overwrites its file. If the other one is running, it
    /// is kept and the new one is not added.
    Overwrite,
    /// Do not add it. A finished download shows its completion dialog again;
    /// a stopped one resumes; a running one is left alone.
    Existing,
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
    /// What to do when a new download's address is already in the list.
    pub if_duplicate: Duplicate,
    /// Show a save-as dialog for each new download before it starts.
    pub ask_where_to_save: bool,
    /// File types (lowercase extensions) that start at once without the
    /// save-as dialog, even when it is on.
    pub auto_start_types: Vec<String>,
    /// Start downloading while the save-as dialog is open, keeping what was
    /// fetched when it is answered.
    pub start_while_asking: bool,
    /// New downloads only join the Download later queue; nothing starts
    /// until it is started.
    pub add_only: bool,
    /// New `General` files go to the folder last chosen for them in the
    /// save-as dialog, as a category's can.
    pub general_remember_folder: bool,
    /// That folder, once one was chosen.
    pub general_last_folder: Option<String>,
    /// The queue "Download later" puts downloads in without asking; `None`
    /// asks in the save-as dialog.
    pub later_queue: Option<u64>,
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
    /// `host:port` of a manual FTP proxy for ftp:// downloads.
    pub proxy_ftp: String,
    /// `host:port` of a manual SOCKS proxy for everything; wins when set.
    /// (The name is from when only SOCKS5 was offered.)
    pub proxy_socks5: String,
    pub proxy_socks_version: SocksVersion,
    /// Let the SOCKS proxy look up host names (SOCKS4a, and always for
    /// SOCKS5) instead of looking them up here (SOCKS4).
    pub proxy_remote_dns: bool,
    /// Hosts the manual proxy is not used for, separated by spaces or
    /// semicolons: `*` wildcards, and `<local>` for names without a dot.
    pub proxy_bypass: String,
    /// The proxy auto-config (PAC) script for `ProxyMode::Script`.
    pub proxy_pac_url: String,
    /// FTP data connections come from the server (active mode, PORT)
    /// instead of going to it (passive mode).
    pub ftp_active: bool,
    /// User name for the manual proxy, if it needs one.
    pub proxy_username: String,
    /// A new proxy password to store (empty clears it); `None` keeps the
    /// stored one. Kept with DPAPI, never in this file.
    #[serde(default, skip_serializing)]
    pub proxy_password: Option<String>,
    /// For a download taken from the browser, use the proxy the browser uses.
    pub use_browser_proxy: bool,
    pub site_logins: Vec<SiteLogin>,
    /// Offer to download links copied to the clipboard whose file type is
    /// one of `capture_types`.
    pub clipboard_monitor: bool,
    /// Download such links at once instead of offering them.
    pub clipboard_start: bool,
    /// Show a download button over videos playing in the browser.
    pub video_button: bool,
    /// Sites that get no video button, such as `*.example.com`.
    pub video_button_excluded_sites: Vec<String>,
    /// ffmpeg.exe, to join streams into .mp4 files. Empty looks on `PATH`.
    pub ffmpeg_path: String,
    /// Ask before removing downloads from the list.
    pub confirm_remove: bool,
    /// Removing a download also sends its files to the Recycle Bin. The
    /// confirmation's box starts with this, and it applies on its own once
    /// the confirmation is turned off.
    pub remove_deletes_files: bool,
    /// Give a finished file the modified date the server reports for it
    /// (`Last-Modified`) instead of the time it was written.
    pub server_file_date: bool,
    /// Resume a partial download even when the server reports a new
    /// modification date for the file; its size must still match.
    pub resume_ignores_date: bool,
    /// The User-Agent sent for downloads not started from the browser (added
    /// by hand, in a batch, from a list or by the grabber). Empty sends the
    /// default.
    pub manual_user_agent: String,
    /// Where unfinished downloads are written, a full path; finished files
    /// then move to their real folder. `None` writes them in place.
    pub temp_dir: Option<String>,
    /// Held while clicking a link, keeps its download in the browser.
    pub capture_skip_key: CaptureKey,
    /// Held while clicking a link, sends it to UIDM whatever its type.
    pub capture_force_key: CaptureKey,
    /// Addresses whose downloads stay in the browser, with `*` matching any
    /// run of characters, such as `https://example.com/files/*.zip`.
    pub capture_excluded_urls: Vec<String>,
    /// After the save-as dialog of the same captured address is canceled
    /// twice, offer to stop capturing it.
    pub offer_exceptions: bool,
    pub selection_button: SelectionButton,
    /// With `SelectionButton::Sites`, the sites that get the button, such
    /// as `*.example.com`.
    pub selection_button_sites: Vec<String>,
    /// Servers that get fewer connections, all downloads together.
    pub server_limits: Vec<ServerLimit>,
    /// Download no more than `quota_mb` MB every `quota_hours` hours;
    /// downloads pause when it is reached and resume when the window allows.
    pub quota_enabled: bool,
    pub quota_mb: u64,
    pub quota_hours: u64,
    /// Look for a newer UIDM on GitHub once a week.
    pub check_updates: bool,
    /// When that last happened, in seconds since 1970.
    pub last_update_check: u64,
    /// Show the drop target, a small window that takes dragged links.
    pub drop_target: bool,
    /// Open a download's window, with its status, Pause and Cancel, when it
    /// starts.
    pub progress_on_start: bool,
    /// The download window has a Speed limit tab.
    pub progress_speed_tab: bool,
    pub theme: Theme,
    /// The window's text size, in percent; everything in it scales along.
    pub text_size: u32,
    /// The window's language as a code such as `de`; empty follows Windows.
    pub language: String,
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
            if_duplicate: Duplicate::Ask,
            ask_where_to_save: true,
            auto_start_types: Vec::new(),
            start_while_asking: false,
            add_only: false,
            general_remember_folder: false,
            general_last_folder: None,
            later_queue: None,
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
            proxy_ftp: String::new(),
            proxy_socks5: String::new(),
            proxy_socks_version: SocksVersion::V5,
            proxy_remote_dns: true,
            proxy_bypass: "<local>".into(),
            proxy_pac_url: String::new(),
            ftp_active: false,
            proxy_username: String::new(),
            proxy_password: None,
            use_browser_proxy: false,
            site_logins: Vec::new(),
            clipboard_monitor: false,
            clipboard_start: false,
            video_button: true,
            video_button_excluded_sites: Vec::new(),
            ffmpeg_path: String::new(),
            confirm_remove: true,
            remove_deletes_files: false,
            server_file_date: true,
            resume_ignores_date: false,
            manual_user_agent: String::new(),
            temp_dir: None,
            capture_skip_key: CaptureKey::Alt,
            capture_force_key: CaptureKey::Ctrl,
            capture_excluded_urls: Vec::new(),
            offer_exceptions: true,
            selection_button: SelectionButton::Everywhere,
            selection_button_sites: Vec::new(),
            server_limits: Vec::new(),
            quota_enabled: false,
            quota_mb: 1024,
            quota_hours: 24,
            check_updates: true,
            last_update_check: 0,
            drop_target: false,
            progress_on_start: false,
            progress_speed_tab: true,
            theme: Theme::System,
            text_size: 100,
            language: String::new(),
        }
    }
}

/// Browser User-Agents to offer for downloads added by hand. Servers mostly
/// look at the browser's name, so these change little between versions.
pub const USER_AGENT_PRESETS: [(&str, &str); 3] = [
    (
        "Chrome",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36",
    ),
    (
        "Edge",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0",
    ),
    (
        "Firefox",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:144.0) Gecko/20100101 Firefox/144.0",
    ),
];

impl Settings {
    /// Check every value, returning a message fit to show the user.
    pub fn validate(&self) -> Result<(), String> {
        if self.proxy_mode == ProxyMode::Script && !crate::batch::is_web_url(self.proxy_pac_url.trim()) {
            return Err("The configuration script must be an http or https address.".into());
        }
        if self.language.len() > 16 || !self.language.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(format!("\"{}\" is not a language code, such as de or pt-BR.", self.language));
        }
        let (lo, hi) = TEXT_SIZE_RANGE;
        if !(lo..=hi).contains(&self.text_size) {
            return Err(format!("The text size must be between {lo}% and {hi}%."));
        }
        let (lo, hi) = QUOTA_HOURS_RANGE;
        if !(lo..=hi).contains(&self.quota_hours) {
            return Err(format!("The quota's window must be between {lo} and {hi} hours."));
        }
        if self.quota_mb == 0 {
            return Err("The quota must be at least 1 MB.".into());
        }
        let (lo, hi) = SERVER_CONNECTIONS_RANGE;
        for l in &self.server_limits {
            if l.site.trim().is_empty() || l.site.contains(char::is_whitespace) {
                return Err(format!("\"{}\" is not a server name, such as files.example.com.", l.site));
            }
            if !(lo..=hi).contains(&l.max) {
                return Err(format!("The connections for {} must be between {lo} and {hi}.", l.site));
            }
        }
        if self.capture_skip_key == self.capture_force_key && self.capture_skip_key != CaptureKey::None {
            return Err("The keys that skip and force capture must differ.".into());
        }
        if let Some(dir) = self.temp_dir.as_deref().filter(|d| !Path::new(d).is_absolute()) {
            return Err(format!("The temporary folder must be a full path: {dir}"));
        }
        if self.manual_user_agent.chars().any(char::is_control) {
            return Err("The User-Agent must be one line of text.".into());
        }
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
        check_extensions("The file types that start without asking", &self.auto_start_types)?;
        if self.capture_min_size_kb > CAPTURE_MIN_SIZE_MAX_KB {
            return Err("The minimum size to capture is too large.".to_string());
        }
        for site in self.capture_excluded_sites.iter().chain(&self.video_button_excluded_sites) {
            if !is_site(site) {
                return Err(format!(
                    "\"{site}\" is not a site. Use a host name such as example.com or *.example.com."
                ));
            }
        }
        let ffmpeg = self.ffmpeg_path.trim();
        if !ffmpeg.is_empty() {
            let path = Path::new(ffmpeg);
            if !path.is_absolute() || path.is_dir() {
                return Err(format!("ffmpeg must be the full path of ffmpeg.exe: {ffmpeg}"));
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
            if !is_site(&site) {
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

    /// The category a file named `filename` from `url` belongs to, or `None`
    /// for `General`. A category limited to some sites only takes files from
    /// them.
    pub fn category_of(&self, filename: &str, url: &str) -> Option<&Category> {
        let ext = extension_of(filename)?;
        let host = host_of(url);
        self.categories.iter().find(|c| c.takes(&ext, &host))
    }

    /// The name of the category of `filename` from `url`, `General` when
    /// none claims it.
    pub fn category_name(&self, filename: &str, url: &str) -> &str {
        self.category_of(filename, url).map_or(GENERAL, |c| c.name.as_str())
    }

    /// The category a download is listed under: the one `chosen` for it in
    /// the save-as dialog while that category still exists, otherwise the
    /// one `filename` from `url` belongs to.
    pub fn listed_under<'a>(&'a self, chosen: Option<&str>, filename: &str, url: &str) -> &'a str {
        match chosen {
            Some(c) if c.trim().eq_ignore_ascii_case(GENERAL) => GENERAL,
            Some(c) => self.category(c).map_or_else(|| self.category_name(filename, url), |c| c.name.as_str()),
            None => self.category_name(filename, url),
        }
    }

    /// Whether a file named `filename` starts without the save-as dialog.
    pub fn starts_without_asking(&self, filename: &str) -> bool {
        extension_of(filename).is_some_and(|e| self.auto_start_types.contains(&e))
    }

    /// The connection limit for `host`, if a server limit covers it: the
    /// most specific matching entry wins.
    pub fn server_limit(&self, host: &str) -> Option<usize> {
        self.server_limits
            .iter()
            .filter(|l| crate::net::site_matches(&l.site, host))
            .max_by_key(|l| l.site.trim_start_matches("*.").len())
            .map(|l| l.max)
    }

    /// The quota as bytes and a window in minutes, when it is on.
    pub fn quota(&self) -> Option<(u64, u64)> {
        self.quota_enabled.then(|| (self.quota_mb * 1024 * 1024, self.quota_hours * 60))
    }

    /// Where new downloads go, by category, for files from any site.
    pub fn folders(&self) -> Folders {
        self.folders_for("")
    }

    /// Where new downloads from `url` go, by category.
    pub fn folders_for(&self, url: &str) -> Folders {
        Folders {
            categories: self.categories.iter().map(|c| (c.clone(), self.folder_of(c))).collect(),
            default: self.general_folder(),
            host: host_of(url),
        }
    }

    /// Where new `General` files go: the folder last chosen for them when
    /// they remember one, otherwise the download folder.
    fn general_folder(&self) -> PathBuf {
        match &self.general_last_folder {
            Some(last) if self.general_remember_folder => PathBuf::from(last),
            _ => self.download_dir(),
        }
    }

    /// Where new files of category `c` go: the folder last chosen for it in
    /// the save-as dialog when it remembers one, its own folder, or a folder
    /// named after it inside the download folder.
    fn folder_of(&self, c: &Category) -> PathBuf {
        match (&c.last_folder, &c.folder) {
            (Some(last), _) if c.remember_folder => PathBuf::from(last),
            (_, Some(f)) => PathBuf::from(f),
            _ => self.download_dir().join(c.name.trim()),
        }
    }

    /// The category named `name`, ignoring case.
    pub fn category(&self, name: &str) -> Option<&Category> {
        self.categories.iter().find(|c| c.name.trim().eq_ignore_ascii_case(name.trim()))
    }

    /// Where new files of the category named `name` go; `General`, and a
    /// name no category has, are General's folder.
    pub fn category_folder(&self, name: &str) -> PathBuf {
        match self.category(name) {
            Some(c) => self.folder_of(c),
            None => self.general_folder(),
        }
    }

    /// Whether the category named `name` (or `General`) keeps the folder
    /// last chosen for it.
    pub fn remembers_folder(&self, name: &str) -> bool {
        match self.category(name) {
            Some(c) => c.remember_folder,
            None => self.general_remember_folder,
        }
    }

    /// Remember `folder` as the last one chosen for the category named
    /// `category` (`General`, or a name no category has, for General), if
    /// it remembers its folder. Returns whether anything changed.
    pub fn remember_folder(&mut self, category: &str, folder: &Path) -> bool {
        let folder = folder.display().to_string();
        let (remember, last) = match self.categories.iter_mut().find(|c| c.name.trim().eq_ignore_ascii_case(category.trim())) {
            Some(c) => (c.remember_folder, &mut c.last_folder),
            None => (self.general_remember_folder, &mut self.general_last_folder),
        };
        if !remember || last.as_deref() == Some(folder.as_str()) {
            return false;
        }
        *last = Some(folder);
        true
    }

    /// Turn remembering the last folder on or off for the category named
    /// `category` (or General). Returns whether anything changed.
    pub fn set_remember_folder(&mut self, category: &str, on: bool) -> bool {
        let flag = match self.categories.iter_mut().find(|c| c.name.trim().eq_ignore_ascii_case(category.trim())) {
            Some(c) => &mut c.remember_folder,
            None => &mut self.general_remember_folder,
        };
        let changed = *flag != on;
        *flag = on;
        changed
    }

    /// ffmpeg.exe: the one set in Options if it exists, otherwise the first
    /// on `PATH`.
    pub fn ffmpeg(&self) -> Option<PathBuf> {
        let set = self.ffmpeg_path.trim();
        if !set.is_empty() {
            return Some(PathBuf::from(set)).filter(|p| p.is_file());
        }
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join("ffmpeg.exe"))
            .find(|p| p.is_file())
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

/// A host name, optionally with a `*.` wildcard or a port.
fn is_site(site: &str) -> bool {
    !site.is_empty()
        && site
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '*' | ':'))
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
        assert_eq!(s.if_duplicate, Duplicate::Ask);
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
            if_duplicate: Duplicate::Existing,
            ask_where_to_save: false,
            auto_start_types: vec!["exe".into()],
            start_while_asking: true,
            add_only: true,
            general_remember_folder: true,
            general_last_folder: Some("D:\\Misc".into()),
            later_queue: Some(4),
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
            proxy_ftp: "ftpproxy:21".into(),
            proxy_socks5: String::new(),
            proxy_socks_version: SocksVersion::V4,
            proxy_remote_dns: false,
            proxy_bypass: "<local> *.corp.net".into(),
            proxy_pac_url: "http://wpad.corp.net/proxy.pac".into(),
            ftp_active: true,
            proxy_username: "me".into(),
            proxy_password: None,
            use_browser_proxy: true,
            site_logins: vec![SiteLogin {
                site: "files.example.com".into(),
                username: "ann".into(),
                password: None,
            }],
            clipboard_monitor: true,
            clipboard_start: true,
            video_button: false,
            video_button_excluded_sites: vec!["video.example.com".into()],
            ffmpeg_path: "C:\\ffmpeg\\bin\\ffmpeg.exe".into(),
            confirm_remove: false,
            remove_deletes_files: true,
            server_file_date: false,
            resume_ignores_date: true,
            manual_user_agent: USER_AGENT_PRESETS[0].1.into(),
            temp_dir: Some("D:\\UIDM temp".into()),
            capture_skip_key: CaptureKey::Shift,
            capture_force_key: CaptureKey::None,
            capture_excluded_urls: vec!["https://example.com/files/*".into()],
            offer_exceptions: false,
            selection_button: SelectionButton::Sites,
            selection_button_sites: vec!["*.example.org".into()],
            server_limits: vec![ServerLimit {
                site: "*.mirror.net".into(),
                max: 2,
            }],
            quota_enabled: true,
            quota_mb: 500,
            quota_hours: 6,
            check_updates: false,
            last_update_check: 1_700_000_000,
            drop_target: true,
            progress_on_start: true,
            progress_speed_tab: false,
            theme: Theme::Dark,
            text_size: 125,
            language: "de".into(),
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
        for (text_size, valid) in [(79, false), (80, true), (150, true), (151, false)] {
            let s = Settings { text_size, ..Settings::default() };
            assert_eq!(s.validate().is_ok(), valid, "text_size {text_size}");
        }
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
        assert_eq!(s.category_name("setup.EXE", ""), "Programs");
        assert_eq!(s.category_name("Report.Final.PDF", ""), "Documents");
        assert_eq!(s.category_name("archive.tar.gz", ""), "Compressed");
        assert_eq!(s.category_name("movie.mkv", ""), "Video");
        assert_eq!(s.category_name("README", ""), GENERAL);
        assert_eq!(s.category_name(".bashrc", ""), GENERAL);
        assert_eq!(s.category_name("photo.jpg", ""), GENERAL);
        assert_eq!(s.category_name("trailing.", ""), GENERAL);
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
    fn server_limits_pick_the_most_specific_entry() {
        let s = Settings {
            server_limits: vec![
                ServerLimit { site: "*.mirror.net".into(), max: 4 },
                ServerLimit { site: "slow.mirror.net".into(), max: 1 },
            ],
            ..Settings::default()
        };
        assert_eq!(s.server_limit("dl.mirror.net"), Some(4));
        assert_eq!(s.server_limit("slow.mirror.net"), Some(1));
        assert_eq!(s.server_limit("example.com"), None);
        assert!(s.validate().is_ok());
        let bad = Settings {
            server_limits: vec![ServerLimit { site: "x.net".into(), max: 40 }],
            ..Settings::default()
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn a_category_can_be_limited_to_some_sites() {
        let base = std::env::temp_dir().join("uidm-dl");
        let mut s = Settings {
            download_dir: Some(base.display().to_string()),
            ..Settings::default()
        };
        let mut work = Category::new("Work", &["pdf"]);
        work.sites = vec!["*.corp.example".into()];
        s.categories.insert(0, work);
        assert_eq!(s.category_name("a.pdf", "https://files.corp.example/a.pdf"), "Work");
        assert_eq!(s.category_name("a.pdf", "https://elsewhere.net/a.pdf"), "Documents");
        assert_eq!(s.folders_for("https://corp.example/x").for_name("a.pdf"), base.join("Work"));
        assert_eq!(s.folders_for("https://elsewhere.net/x").for_name("a.pdf"), base.join("Documents"));
    }

    #[test]
    fn a_category_remembers_the_last_folder_chosen() {
        let mut s = Settings::default();
        let chosen = std::env::temp_dir().join("uidm-chosen");
        assert!(!s.remember_folder("Compressed", &chosen), "off by default");
        s.categories[0].remember_folder = true;
        assert!(s.remember_folder("compressed", &chosen));
        assert_eq!(s.folders().for_name("b.zip"), chosen);
        assert_eq!(s.category_folder("Compressed"), chosen);
        assert!(!s.remember_folder("Compressed", &chosen), "unchanged");
    }

    #[test]
    fn a_chosen_category_wins_while_it_exists() {
        let s = Settings::default();
        assert_eq!(s.listed_under(None, "a.zip", "https://x/a.zip"), "Compressed");
        assert_eq!(s.listed_under(Some("music"), "a.zip", "https://x/a.zip"), "Music");
        assert_eq!(s.listed_under(Some("General"), "a.zip", "https://x/a.zip"), GENERAL);
        assert_eq!(s.listed_under(Some("Gone"), "a.zip", "https://x/a.zip"), "Compressed");
    }

    #[test]
    fn each_category_has_a_folder() {
        let mut s = Settings::default();
        let base = std::env::temp_dir().join("uidm-base");
        s.download_dir = Some(base.display().to_string());
        s.categories[1].folder = Some("D:\\Docs".into());
        assert_eq!(s.category_folder("Music"), base.join("Music"));
        assert_eq!(s.category_folder("documents"), PathBuf::from("D:\\Docs"));
        assert_eq!(s.category_folder(GENERAL), base);
        assert_eq!(s.category_folder("No such"), base);
    }

    #[test]
    fn general_can_remember_its_folder_too() {
        let mut s = Settings::default();
        let base = std::env::temp_dir().join("uidm-base");
        s.download_dir = Some(base.display().to_string());
        let chosen = std::env::temp_dir().join("uidm-general");
        assert!(!s.remember_folder(GENERAL, &chosen), "off by default");
        assert!(s.set_remember_folder(GENERAL, true));
        assert!(s.remembers_folder(GENERAL));
        assert!(s.remember_folder(GENERAL, &chosen));
        assert_eq!(s.folders().for_name("notes.xyz"), chosen);
        assert_eq!(s.category_folder(GENERAL), chosen);
        // The other categories keep their folders inside the download folder.
        assert_eq!(s.folders().for_name("a.zip"), base.join("Compressed"));
        assert!(s.set_remember_folder(GENERAL, false));
        assert_eq!(s.folders().for_name("notes.xyz"), base);
    }

    #[test]
    fn old_categories_load_without_the_new_fields() {
        let c: Category = serde_json::from_str(r#"{"name":"Books","extensions":["epub"],"folder":null}"#).unwrap();
        assert_eq!(c, Category::new("Books", &["epub"]));
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
