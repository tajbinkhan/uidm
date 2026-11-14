//! The user's settings: what they hold, their defaults, and the rules a
//! valid set follows.
//!
//! The daemon owns them; clients read them with `get_settings` and change
//! them with `set_settings`. Reading and writing `settings.json`, and the
//! checks that need the filesystem, live in `udm_infrastructure::settings`.
//! Where a folder rule falls back to the download folder and none is set, the
//! caller passes the system's Downloads folder as `downloads`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::bytes::ByteCount;
use crate::theme::ThemeName;
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
/// Minutes without keyboard or mouse input before a speed limit that applies
/// only while the computer is in use is lifted, unless the settings say
/// otherwise.
pub const DEFAULT_IDLE_MINUTES: u32 = 5;
/// The remote-control page's port unless the user picks another.
pub const DEFAULT_REMOTE_PORT: u16 = 18443;
/// The ports the remote-control page may use: none of the system's own,
/// which need administrator rights.
pub const REMOTE_PORT_RANGE: (u16, u16) = (1024, 65535);
/// From one minute to four hours.
pub const IDLE_MINUTES_RANGE: (u32, u32) = (1, 240);
/// Up to 10 GB, in KB.
pub const CAPTURE_MIN_SIZE_MAX_KB: u64 = 10 * 1024 * 1024;

/// The category for files no other category claims. It is not stored in the
/// list; its files go straight into the download folder.
pub const GENERAL: &str = "General";

/// The keys held while clicking a link in the browser, to keep the download
/// in the browser or to send the link to UDM: any mix of Alt, Ctrl and
/// Shift, and Insert or Del for forcing (as IDM offers). Stored as text such
/// as `ctrl+shift`, or `none`; files from when one key was allowed say `alt`,
/// `ctrl` or `shift`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct CaptureKey {
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub insert: bool,
    /// The Delete key. Like Insert it is no modifier, so the extension
    /// follows whether it is held by hand.
    pub delete: bool,
}

impl CaptureKey {
    pub const NONE: CaptureKey = CaptureKey { alt: false, ctrl: false, shift: false, insert: false, delete: false };
    pub const ALT: CaptureKey = CaptureKey { alt: true, ..CaptureKey::NONE };
    pub const CTRL: CaptureKey = CaptureKey { ctrl: true, ..CaptureKey::NONE };
    pub const SHIFT: CaptureKey = CaptureKey { shift: true, ..CaptureKey::NONE };

    pub fn is_none(self) -> bool {
        self == CaptureKey::NONE
    }

    /// `Ctrl+Shift`, or "" for no key.
    pub fn label(self) -> String {
        [(self.ctrl, "Ctrl"), (self.alt, "Alt"), (self.shift, "Shift"), (self.insert, "Insert"), (self.delete, "Del")]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, name)| *name)
            .collect::<Vec<_>>()
            .join("+")
    }
}

impl From<CaptureKey> for String {
    fn from(k: CaptureKey) -> String {
        if k.is_none() {
            "none".into()
        } else {
            k.label().to_ascii_lowercase()
        }
    }
}

/// A capture key setting naming a key UDM does not know.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("\"{0}\" is not a key: use alt, ctrl, shift, insert or del")]
pub struct UnknownKey(pub String);

impl TryFrom<String> for CaptureKey {
    type Error = UnknownKey;

    fn try_from(text: String) -> Result<Self, UnknownKey> {
        let mut k = CaptureKey::NONE;
        for part in text.split('+').map(|p| p.trim().to_ascii_lowercase()) {
            match part.as_str() {
                "none" | "" => {}
                "alt" => k.alt = true,
                "ctrl" => k.ctrl = true,
                "shift" => k.shift = true,
                "insert" => k.insert = true,
                "del" | "delete" => k.delete = true,
                other => return Err(UnknownKey(other.to_string())),
            }
        }
        Ok(k)
    }
}

/// Where selecting text with links on a page shows a "Download with UDM"
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

/// How the video button and the selected-links button look on a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelView {
    /// A button with its text.
    #[default]
    Full,
    /// A small round button with an arrow; its text shows on hover.
    Mini,
}

/// The items UDM can add to the browser's context menu, by the names the
/// extension knows them by, with their labels.
pub const BROWSER_MENU_ITEMS: [(&str, &str); 8] = [
    ("page", "Download page with UDM (the page or frame itself)"),
    ("download", "Download with UDM (links, pictures, videos)"),
    ("download_later", "Download later with UDM"),
    ("all_links", "Download all links with UDM"),
    ("selected_links", "Download selected links with UDM"),
    ("last_video", "Download the last video with UDM"),
    ("recent_videos", "The last 10 videos"),
    ("gallery", "Download gallery with UDM (pictures gallery-dl lists)"),
];

/// The video and audio types the video button offers, unless the settings
/// say otherwise. `m3u8`, `mpd` and `f4m` are HLS, DASH and HDS streams.
pub fn default_video_types() -> Vec<String> {
    [
        "mp4", "m4v", "webm", "mkv", "mov", "avi", "wmv", "flv", "3gp", "ogv", "mp3", "m4a", "aac", "ogg", "oga",
        "opus", "flac", "wav", "m3u8", "mpd", "f4m",
    ]
    .map(String::from)
    .to_vec()
}

/// The tab a download's window opens on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressTab {
    /// Download status.
    #[default]
    Status,
    /// Speed limit, when that tab is shown.
    SpeedLimit,
    /// When done, when that tab is shown.
    WhenDone,
}

impl ProgressTab {
    pub const ALL: [ProgressTab; 3] = [ProgressTab::Status, ProgressTab::SpeedLimit, ProgressTab::WhenDone];
}

/// The file types web players' media is taken from without the video
/// button, when that is on, unless the settings say otherwise.
pub fn default_video_auto_types() -> Vec<String> {
    ["mp4", "m4v", "webm", "flv", "mp3", "m4a"].map(String::from).to_vec()
}

/// How UDM tells you a download finished or failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Notify {
    Off,
    /// A Windows notification.
    #[default]
    Toast,
    /// A dialog in the UDM window, as IDM does; a notification when no
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
    /// The proxies set in UDM's options.
    Manual,
    /// The proxy auto-config script at `proxy_pac_url`.
    Script,
}

/// A login for the sites matching `site`. The password lives in Windows
/// Credential Manager; it travels only from a window setting it to the
/// daemon, and is never written to the settings file or sent back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteLogin {
    /// `example.com` (with its subdomains) or a pattern such as
    /// `dl*.example.com`, optionally with a path such as `/private/*` for one
    /// folder of the server.
    pub site: String,
    pub username: String,
    /// A new password to store; `None` keeps the stored one.
    #[serde(default, skip_serializing)]
    pub password: Option<String>,
    /// Also send it where it can be read on the way: as Basic
    /// authentication over plain http, and to FTP servers. Off, it goes
    /// only over https, or as Digest, which never sends the password.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub plain: bool,
}

/// A browser the user added in Options by its program file, beyond the
/// ones UDM knows: shown in the list, registered where its family reads
/// registrations, and restarted by starting its program again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddedBrowser {
    /// The name the list shows, made from the program's name.
    pub name: String,
    /// The browser's program file.
    pub program: String,
}

/// The most connections UDM opens to the hosts matching `site` (such as
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

/// The window's look (see `crate::theme`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(from = "ThemeRead", into = "ThemeWritten")]
pub enum Theme {
    /// Light or dark, as Windows' app mode is, or Windows' contrast colors
    /// while its high-contrast setting is on.
    #[default]
    System,
    Light,
    Dark,
    /// Windows' contrast colors while its high-contrast setting is on, and
    /// UDM's own otherwise.
    HighContrast,
    /// A theme file's theme, by its name.
    Custom(ThemeName),
}

impl Theme {
    /// The built-in themes, as Options lists them before the theme files.
    pub const BUILT_IN: [Theme; 4] = [Theme::System, Theme::Light, Theme::Dark, Theme::HighContrast];
}

/// How `settings.json` keeps the theme: `"system"`, `"light"`, `"dark"`
/// (as before there were theme files) or `"high_contrast"`, or
/// `{"custom": "<name>"}` for a theme file's. What a newer UDM might
/// write, or a name that is not one, reads as following the system rather
/// than failing the whole file.
#[derive(Deserialize)]
#[serde(untagged)]
enum ThemeRead {
    BuiltIn(String),
    Custom { custom: String },
    Other(serde::de::IgnoredAny),
}

/// The theme as [`ThemeRead`] reads it back.
#[derive(Serialize)]
#[serde(untagged)]
enum ThemeWritten {
    BuiltIn(&'static str),
    Custom { custom: String },
}

impl From<ThemeRead> for Theme {
    fn from(read: ThemeRead) -> Theme {
        match read {
            ThemeRead::BuiltIn(name) => match name.as_str() {
                "light" => Theme::Light,
                "dark" => Theme::Dark,
                "high_contrast" => Theme::HighContrast,
                _ => Theme::System,
            },
            ThemeRead::Custom { custom } => ThemeName::parse(&custom).map_or(Theme::System, Theme::Custom),
            ThemeRead::Other(_) => Theme::System,
        }
    }
}

impl From<Theme> for ThemeWritten {
    fn from(theme: Theme) -> ThemeWritten {
        match theme {
            Theme::System => ThemeWritten::BuiltIn("system"),
            Theme::Light => ThemeWritten::BuiltIn("light"),
            Theme::Dark => ThemeWritten::BuiltIn("dark"),
            Theme::HighContrast => ThemeWritten::BuiltIn("high_contrast"),
            Theme::Custom(name) => ThemeWritten::Custom { custom: name.as_str().to_string() },
        }
    }
}

/// The window's interface size, in percent of the normal size: text,
/// controls, the download list and every dialog scale along.
pub const TEXT_SIZE_RANGE: (u32, u32) = (80, 200);

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
            && (self.sites.is_empty() || self.sites.iter().any(|s| crate::site::site_matches(s, host)))
    }
}

/// The host of `url`, in lower case, or "" if it has none.
fn host_of(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)).unwrap_or_default()
}

/// The categories a new settings file starts with.
pub fn default_categories() -> Vec<Category> {
    vec![
        Category::new("Compressed", &["zip", "rar", "7z", "tar", "gz", "bz2", "xz"]),
        Category::new("Documents", &["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "rtf", "odt"]),
        Category::new("Music", &["mp3", "flac", "wav", "m4a", "aac", "ogg"]),
        Category::new("Programs", &["exe", "msi", "apk", "msix"]),
        Category::new("Video", &["mp4", "mkv", "avi", "mov", "webm", "wmv"]),
    ]
}

/// File types the extension captures by default: every category's, plus disk
/// images and packages.
pub fn default_capture_types() -> Vec<String> {
    let mut types: Vec<String> = default_categories().into_iter().flat_map(|c| c.extensions).collect();
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
    /// Types that go to `default` whatever category lists them.
    general: Vec<String>,
    /// The download's host, for categories limited to some sites.
    host: String,
}

impl Folders {
    /// Every file goes to `dir`.
    pub fn fixed(dir: PathBuf) -> Self {
        Folders { default: dir, categories: Vec::new(), general: Vec::new(), host: String::new() }
    }

    /// The folder for a file named `filename`.
    pub fn for_name(&self, filename: &str) -> PathBuf {
        let ext = extension_of(filename).filter(|e| !self.general.contains(e));
        self.categories
            .iter()
            .find(|(c, _)| ext.as_ref().is_some_and(|e| c.takes(e, &self.host)))
            .map(|(_, dir)| dir.clone())
            .unwrap_or_else(|| self.default.clone())
    }
}

/// Which sound to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    Done,
    Failed,
    QueueStarted,
    QueueDone,
}

/// Where a package's archives are unpacked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnpackTo {
    /// Into the folder the archives are in.
    Beside,
    /// Into a folder named after the package, beside the archives.
    #[default]
    Subfolder,
    /// Into a folder named after the package, in `unpack_folder`.
    Folder,
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
    /// Turn the speed limit on whenever the daemon starts, whatever it was
    /// when UDM last ran, for users who only want it off for a while.
    #[serde(default)]
    pub speed_limit_on_startup: bool,
    /// While the speed limit is on, the most each download may take, in
    /// KB/s, unless it has a limit of its own. `None` caps only the total.
    #[serde(default)]
    pub speed_limit_per_file_kbps: Option<u64>,
    /// Apply the speed limit only while someone uses the computer: lift it
    /// after `speed_limit_idle_minutes` without keyboard or mouse input, and
    /// apply it again at the next input.
    #[serde(default)]
    pub speed_limit_only_while_active: bool,
    /// Minutes without input before the speed limit is lifted, when it
    /// applies only while the computer is in use.
    pub speed_limit_idle_minutes: u32,
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
    pub later_queue: Option<crate::id::QueueId>,
    /// File types and their folders. Files of no listed type are `General`.
    pub categories: Vec<Category>,
    /// File types that are `General` even when a category lists them, as
    /// IDM's General category has a type list of its own. Empty by default:
    /// General then takes only what no category claims.
    #[serde(default)]
    pub general_extensions: Vec<String>,
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
    /// Minimizing the window hides it in the tray, as IDM does, instead of
    /// leaving it on the taskbar.
    pub minimize_to_tray: bool,
    /// Show UDM's icon in the notification area. Without it, closing the
    /// window quits, and UDM is reached from the Start menu.
    pub tray_icon: bool,
    /// Show the categories and queues panel at the window's left.
    pub categories_panel: bool,
    /// Start the window, hidden in the tray, when you sign in to Windows.
    pub start_with_windows: bool,
    /// Keep Windows from going to sleep while something downloads.
    pub keep_awake: bool,
    pub notify: Notify,
    /// Play a sound when a download finishes.
    pub sound_on_done: bool,
    /// Play a sound when a download fails.
    pub sound_on_failed: bool,
    /// Play a sound when a queue starts a run, by hand or on its schedule.
    #[serde(default)]
    pub sound_on_queue_started: bool,
    /// Play a sound when a started queue has finished its run.
    pub sound_on_queue_done: bool,
    /// The WAV files those sounds play, full paths; empty plays Windows'
    /// own sound for the event.
    pub sound_done_file: String,
    pub sound_failed_file: String,
    #[serde(default)]
    pub sound_queue_started_file: String,
    pub sound_queue_file: String,
    /// Scan each finished download for viruses.
    pub scan_after_download: bool,
    pub scanner: Scanner,
    /// The custom scanner's command line, such as
    /// `"C:\ClamAV\clamscan.exe" --no-summary {path}`.
    pub scanner_command: String,
    pub proxy_mode: ProxyMode,
    /// `host:port` of the manual proxy for http URLs.
    pub proxy_http: String,
    /// `host:port` of the manual proxy for https URLs; empty uses the http
    /// one, with its login.
    pub proxy_https: String,
    /// `host:port` of a manual FTP proxy for ftp:// downloads.
    pub proxy_ftp: String,
    /// `host:port` of a manual SOCKS proxy, for http and https URLs that
    /// have no proxy of their own. (The name is from when only SOCKS5 was
    /// offered.)
    pub proxy_socks5: String,
    /// https URLs use the SOCKS proxy even when an HTTP or HTTPS proxy is
    /// set, so http can go through a web proxy and https through SOCKS.
    #[serde(default)]
    pub proxy_socks_https: bool,
    /// ftp:// downloads use the SOCKS proxy when no FTP proxy is set.
    #[serde(default = "yes")]
    pub proxy_socks_ftp: bool,
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
    /// User name for the manual HTTP proxy, if it needs one.
    pub proxy_username: String,
    /// A new HTTP proxy password to store (empty clears it); `None` keeps
    /// the stored one. Each proxy's password is kept with DPAPI, never in
    /// this file.
    #[serde(default, skip_serializing)]
    pub proxy_password: Option<String>,
    /// User name for the manual HTTPS proxy.
    pub proxy_https_username: String,
    #[serde(default, skip_serializing)]
    pub proxy_https_password: Option<String>,
    /// User name for the manual FTP proxy.
    pub proxy_ftp_username: String,
    #[serde(default, skip_serializing)]
    pub proxy_ftp_password: Option<String>,
    /// User name for the manual SOCKS proxy.
    pub proxy_socks_username: String,
    #[serde(default, skip_serializing)]
    pub proxy_socks_password: Option<String>,
    /// Set on loading a file from before each proxy had its own login: the
    /// HTTPS and SOCKS proxies took the HTTP proxy's user name, and the
    /// daemon gives them its password too.
    #[serde(skip)]
    pub proxy_logins_split: bool,
    /// For a download taken from the browser, use the proxy the browser uses.
    pub use_browser_proxy: bool,
    /// With `use_browser_proxy`, use the browser's proxy only to retry a
    /// download that failed through UDM's own proxy setting.
    pub browser_proxy_after_errors: bool,
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
    /// The video and audio types (lowercase extensions) the video button
    /// offers; `m3u8` and `mpd` stand for HLS and DASH streams.
    pub video_types: Vec<String>,
    /// Whether `video_types` has been through the step that gave files from
    /// before HDS support `f4m` (see `add_hds_type`), so a user who later
    /// turns it off keeps it off.
    #[serde(default)]
    pub video_types_hds: bool,
    /// Video and audio files the browser reports as smaller than this, in
    /// KB, are left out of the video button's list.
    pub video_min_size_kb: u64,
    /// How the video button and the selected-links button look.
    pub video_view: PanelView,
    /// The video button says when a video is protected (DRM). Off, it does
    /// not show on such videos at all.
    pub video_protected_note: bool,
    /// Automatically take media a web player loads, of the types in
    /// `video_auto_types`, as a download, without the video button (IDM's
    /// "automatically start downloading of the following file types from
    /// web players"). Only whole files, not HLS or DASH streams; each
    /// address once per tab.
    #[serde(default)]
    pub video_auto_capture: bool,
    #[serde(default = "default_video_auto_types")]
    pub video_auto_types: Vec<String>,
    /// The context menu items the extension leaves out in Chrome, Edge and
    /// the other Chromium browsers, by the names in `BROWSER_MENU_ITEMS`.
    /// A file from when one list served every browser gives both lists its
    /// items (see `split_browser_menu`).
    #[serde(default)]
    pub browser_menu_hidden_chromium: Vec<String>,
    /// The same, for Firefox.
    #[serde(default)]
    pub browser_menu_hidden_firefox: Vec<String>,
    /// Browsers added in Options by their program file.
    #[serde(default)]
    pub added_browsers: Vec<AddedBrowser>,
    /// ffmpeg.exe, to join streams into .mp4 files. Empty looks on `PATH`.
    pub ffmpeg_path: String,
    /// 7-Zip's program (7z.exe, or 7zz), for 7z and RAR archives and spanned
    /// zips. Empty looks on `PATH` and where 7-Zip installs itself.
    pub seven_zip_path: String,
    /// Unpack a package's archives as soon as all its files are in.
    pub unpack_auto: bool,
    /// Where a package's archives are unpacked.
    pub unpack_to: UnpackTo,
    /// The folder `UnpackTo::Folder` unpacks into, each package in a folder
    /// of its own there.
    pub unpack_folder: String,
    /// Send a package's archives to the Recycle Bin once they are unpacked.
    pub unpack_recycle: bool,
    /// Look for a newer yt-dlp once a day and install it, when UDM
    /// installed yt-dlp (Options > Tools). Off unless the user turns it on.
    #[serde(default)]
    pub ytdlp_auto_update: bool,
    /// Ask before removing downloads from the list.
    pub confirm_remove: bool,
    /// Removing a download also sends its files to the Recycle Bin. The
    /// confirmation's box starts with this, and it applies on its own once
    /// the confirmation is turned off.
    pub remove_deletes_files: bool,
    /// Give a finished file the modified date the server reports for it
    /// (`Last-Modified`) instead of the time it was written.
    pub server_file_date: bool,
    /// Make the server's date the file's creation time too, where the
    /// system lets it be set (Windows and macOS; not Linux).
    #[serde(default)]
    pub server_file_date_created: bool,
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
    /// Held while clicking a link, sends it to UDM whatever its type.
    pub capture_force_key: CaptureKey,
    /// The keys count only on a click with the left mouse button, not a
    /// middle click or Enter on a link.
    pub capture_keys_left_click: bool,
    /// The force key leaves links to web pages, pictures and scripts alone,
    /// so they open as usual.
    pub capture_force_skips_pages: bool,
    /// Downloads, links and videos in the browser's private windows go to
    /// UDM too. Off by default: a private window keeps its downloads out of
    /// UDM's list. Only the window changes it, never the browser itself.
    /// Chrome also needs "Allow in Incognito" turned on for the extension.
    #[serde(default)]
    pub capture_private: bool,
    /// Addresses whose downloads stay in the browser, with `*` matching any
    /// run of characters, such as `https://example.com/files/*.zip`.
    pub capture_excluded_urls: Vec<String>,
    /// Whether `capture_excluded_urls` has been given UDM's default list
    /// (see `add_default_exclusions`), so an address the user removes stays
    /// removed.
    #[serde(default)]
    pub capture_excluded_urls_defaults: bool,
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
    /// Warn when 90% of the quota is used, before downloads stop.
    pub quota_warn: bool,
    /// Look for a newer UDM on GitHub once a week.
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
    /// The download window has a When done tab.
    pub progress_done_tab: bool,
    /// The Speed limit and When done tabs have a Hide tab button.
    #[serde(default = "yes")]
    pub progress_hide_buttons: bool,
    /// The tab the download window opens on; a hidden one opens Download
    /// status instead.
    #[serde(default)]
    pub progress_start_tab: ProgressTab,
    /// The download window opens with its details (the connections' bar
    /// and table) shown.
    #[serde(default)]
    pub progress_start_details: bool,
    /// The Add batch download and link picker dialogs show the panel that
    /// picks the queue and whether to start; off, they add to the main
    /// queue and start as Options say, as IDM can hide it.
    #[serde(default = "yes")]
    pub list_queue_panel: bool,
    /// The note that a queue's schedule needs UDM running was dismissed.
    pub schedule_note_hidden: bool,
    /// Closing the site grabber adds the files ticked in its results to the
    /// list and queue, as its Add button does.
    pub grab_add_on_close: bool,
    /// Closing a grabber project, or starting its crawl, saves it without
    /// asking when no other saved project has its name.
    pub grab_save_unique_silently: bool,
    /// Show a tip when the window opens.
    pub tips_on_startup: bool,
    /// The tip to show next.
    pub next_tip: usize,
    pub theme: Theme,
    /// The window's interface size, in percent; everything in it scales
    /// along. (Named for when it was the Text size option.)
    pub text_size: u32,
    /// The window's language as a code such as `de`; empty follows Windows.
    pub language: String,
    /// Serve the remote-control page to paired phones on the network
    /// (`pairing`). Off unless the user turns it on.
    #[serde(default)]
    pub remote_enabled: bool,
    /// The port the remote-control page listens on.
    #[serde(default = "default_remote_port")]
    pub remote_port: u16,
    /// The addresses of this computer it listens on; empty listens on
    /// every network interface.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_addresses: Vec<String>,
    /// The debrid account links to file hosts go through (experimental).
    /// Its API key is stored sealed beside the settings, never here.
    #[serde(default)]
    pub debrid: crate::debrid::DebridSettings,
}

fn default_remote_port() -> u16 {
    DEFAULT_REMOTE_PORT
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            download_dir: None,
            connections: DEFAULT_CONNECTIONS,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            speed_limit_enabled: false,
            speed_limit_kbps: DEFAULT_SPEED_LIMIT_KBPS,
            speed_limit_on_startup: false,
            speed_limit_per_file_kbps: None,
            speed_limit_only_while_active: false,
            speed_limit_idle_minutes: DEFAULT_IDLE_MINUTES,
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
            general_extensions: Vec::new(),
            capture_types: default_capture_types(),
            capture_min_size_kb: 0,
            capture_excluded_sites: Vec::new(),
            close_to_tray: true,
            minimize_to_tray: true,
            tray_icon: true,
            categories_panel: true,
            start_with_windows: false,
            keep_awake: true,
            notify: Notify::Toast,
            sound_on_done: false,
            sound_on_failed: false,
            sound_on_queue_started: false,
            sound_on_queue_done: false,
            sound_done_file: String::new(),
            sound_failed_file: String::new(),
            sound_queue_started_file: String::new(),
            sound_queue_file: String::new(),
            scan_after_download: false,
            // Defender is Windows' own; elsewhere the user names a scanner.
            scanner: if cfg!(windows) { Scanner::Defender } else { Scanner::Custom },
            scanner_command: String::new(),
            proxy_mode: ProxyMode::None,
            proxy_http: String::new(),
            proxy_https: String::new(),
            proxy_ftp: String::new(),
            proxy_socks5: String::new(),
            proxy_socks_https: false,
            proxy_socks_ftp: true,
            proxy_socks_version: SocksVersion::V5,
            proxy_remote_dns: true,
            proxy_bypass: "<local>".into(),
            proxy_pac_url: String::new(),
            ftp_active: false,
            proxy_username: String::new(),
            proxy_password: None,
            proxy_https_username: String::new(),
            proxy_https_password: None,
            proxy_ftp_username: String::new(),
            proxy_ftp_password: None,
            proxy_socks_username: String::new(),
            proxy_socks_password: None,
            proxy_logins_split: false,
            use_browser_proxy: false,
            browser_proxy_after_errors: false,
            site_logins: Vec::new(),
            clipboard_monitor: false,
            clipboard_start: false,
            video_button: true,
            video_button_excluded_sites: Vec::new(),
            video_types: default_video_types(),
            video_types_hds: true,
            video_min_size_kb: 128,
            video_view: PanelView::Full,
            video_protected_note: true,
            video_auto_capture: false,
            video_auto_types: default_video_auto_types(),
            browser_menu_hidden_chromium: Vec::new(),
            browser_menu_hidden_firefox: Vec::new(),
            added_browsers: Vec::new(),
            ffmpeg_path: String::new(),
            seven_zip_path: String::new(),
            unpack_auto: false,
            unpack_to: UnpackTo::Subfolder,
            unpack_folder: String::new(),
            unpack_recycle: false,
            ytdlp_auto_update: false,
            confirm_remove: true,
            remove_deletes_files: false,
            server_file_date: true,
            server_file_date_created: false,
            resume_ignores_date: false,
            manual_user_agent: String::new(),
            temp_dir: None,
            capture_skip_key: CaptureKey::ALT,
            capture_force_key: CaptureKey::CTRL,
            capture_keys_left_click: false,
            capture_force_skips_pages: true,
            capture_private: false,
            capture_excluded_urls: default_capture_excluded_urls(),
            capture_excluded_urls_defaults: true,
            offer_exceptions: true,
            selection_button: SelectionButton::Everywhere,
            selection_button_sites: Vec::new(),
            server_limits: Vec::new(),
            quota_enabled: false,
            quota_mb: 1024,
            quota_hours: 24,
            quota_warn: true,
            check_updates: true,
            last_update_check: 0,
            drop_target: false,
            // On, as IDM's "Show download progress dialog" is.
            progress_on_start: true,
            progress_speed_tab: true,
            progress_done_tab: true,
            progress_hide_buttons: true,
            progress_start_tab: ProgressTab::Status,
            progress_start_details: false,
            list_queue_panel: true,
            schedule_note_hidden: false,
            grab_add_on_close: false,
            grab_save_unique_silently: false,
            tips_on_startup: true,
            next_tip: 0,
            theme: Theme::System,
            text_size: 100,
            language: String::new(),
            remote_enabled: false,
            remote_port: DEFAULT_REMOTE_PORT,
            remote_addresses: Vec::new(),
            debrid: crate::debrid::DebridSettings::default(),
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
    /// The WAV file to play for `sound`, if one is set.
    pub fn sound_file(&self, sound: Sound) -> Option<PathBuf> {
        let file = match sound {
            Sound::Done => &self.sound_done_file,
            Sound::Failed => &self.sound_failed_file,
            Sound::QueueStarted => &self.sound_queue_started_file,
            Sound::QueueDone => &self.sound_queue_file,
        };
        Some(PathBuf::from(file.trim())).filter(|p| !p.as_os_str().is_empty())
    }

    /// Check every value, returning why one is not valid.
    pub fn validate(&self) -> Result<(), InvalidSettings> {
        use InvalidSettings as E;
        if self.proxy_mode == ProxyMode::Script && !crate::batch::is_web_url(self.proxy_pac_url.trim()) {
            return Err(E::ScriptNotWeb);
        }
        if self.language.len() > 16 || !self.language.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(E::Language(self.language.clone()));
        }
        let (lo, hi) = TEXT_SIZE_RANGE;
        if !(lo..=hi).contains(&self.text_size) {
            return Err(E::TextSize);
        }
        let (lo, hi) = QUOTA_HOURS_RANGE;
        if !(lo..=hi).contains(&self.quota_hours) {
            return Err(E::QuotaHours);
        }
        if self.quota_mb == 0 {
            return Err(E::QuotaZero);
        }
        let (lo, hi) = SERVER_CONNECTIONS_RANGE;
        for l in &self.server_limits {
            if l.site.trim().is_empty() || l.site.contains(char::is_whitespace) {
                return Err(E::ServerName(l.site.clone()));
            }
            if !(lo..=hi).contains(&l.max) {
                return Err(E::ServerConnections(l.site.clone()));
            }
        }
        if self.capture_skip_key == self.capture_force_key && !self.capture_skip_key.is_none() {
            return Err(E::SameCaptureKeys);
        }
        if self.capture_skip_key.insert || self.capture_skip_key.delete {
            return Err(E::InsertDelSkip);
        }
        if let Some(dir) = self.temp_dir.as_deref().filter(|d| !Path::new(d).is_absolute()) {
            return Err(E::TempNotFull(dir.to_string()));
        }
        if self.manual_user_agent.chars().any(char::is_control) {
            return Err(E::UserAgentLines);
        }
        let (lo, hi) = CONNECTIONS_RANGE;
        if !(lo..=hi).contains(&self.connections) {
            return Err(E::Connections);
        }
        let (lo, hi) = MAX_CONCURRENT_RANGE;
        if !(lo..=hi).contains(&self.max_concurrent) {
            return Err(E::DownloadsAtOnce);
        }
        let (lo, hi) = SPEED_LIMIT_RANGE_KBPS;
        if !(lo..=hi).contains(&self.speed_limit_kbps) {
            return Err(E::SpeedLimit);
        }
        if self.speed_limit_per_file_kbps.is_some_and(|k| !(lo..=hi).contains(&k)) {
            return Err(E::SpeedLimitPerFile);
        }
        let (lo, hi) = IDLE_MINUTES_RANGE;
        if !(lo..=hi).contains(&self.speed_limit_idle_minutes) {
            return Err(E::IdleMinutes);
        }
        let (lo, hi) = REMOTE_PORT_RANGE;
        if !(lo..=hi).contains(&self.remote_port) {
            return Err(E::RemotePort);
        }
        if let Some(bad) = self.remote_addresses.iter().find(|a| a.trim().parse::<std::net::IpAddr>().is_err()) {
            return Err(E::RemoteAddress(bad.clone()));
        }
        if let Some(dir) = &self.download_dir {
            check_folder(SettingsField::DownloadFolder, dir)?;
        }

        let mut names: Vec<String> = Vec::new();
        for c in &self.categories {
            let name = c.name.trim();
            if name.is_empty() {
                return Err(E::CategoryUnnamed);
            }
            if name.eq_ignore_ascii_case(GENERAL) {
                return Err(E::GeneralName);
            }
            if names.contains(&name.to_lowercase()) {
                return Err(E::TwoCategories(name.to_string()));
            }
            names.push(name.to_lowercase());
            if c.extensions.is_empty() {
                return Err(E::NoFileTypes(name.to_string()));
            }
            check_extensions(SettingsField::Category(name.to_string()), &c.extensions)?;
            if let Some(dir) = &c.folder {
                check_folder(SettingsField::CategoryFolder(name.to_string()), dir)?;
            }
        }

        check_extensions(SettingsField::Category(GENERAL.to_string()), &self.general_extensions)?;
        check_extensions(SettingsField::CapturedTypes, &self.capture_types)?;
        check_extensions(SettingsField::VideoTypes, &self.video_types)?;
        if self.video_min_size_kb > CAPTURE_MIN_SIZE_MAX_KB {
            return Err(E::VideoMinSize);
        }
        check_extensions(SettingsField::PlayerTypes, &self.video_auto_types)?;
        if self.video_auto_capture && self.video_auto_types.is_empty() {
            return Err(E::NoPlayerTypes);
        }
        if let Some(item) = self
            .browser_menu_hidden_chromium
            .iter()
            .chain(&self.browser_menu_hidden_firefox)
            .find(|i| !BROWSER_MENU_ITEMS.iter().any(|(id, _)| id == i))
        {
            return Err(E::MenuItem(item.clone()));
        }

        check_extensions(SettingsField::AutoStartTypes, &self.auto_start_types)?;
        if self.capture_min_size_kb > CAPTURE_MIN_SIZE_MAX_KB {
            return Err(E::CaptureMinSize);
        }
        for site in self.capture_excluded_sites.iter().chain(&self.video_button_excluded_sites) {
            if !is_site(site) {
                return Err(E::NotSite(site.clone()));
            }
        }
        let ffmpeg = self.ffmpeg_path.trim();
        if !ffmpeg.is_empty() {
            let path = Path::new(ffmpeg);
            if !path.is_absolute() {
                return Err(E::FfmpegNotFull(ffmpeg.to_string()));
            }
        }
        let seven_zip = self.seven_zip_path.trim();
        if !seven_zip.is_empty() && !Path::new(seven_zip).is_absolute() {
            return Err(E::SevenZipNotFull(seven_zip.to_string()));
        }
        if self.unpack_to == UnpackTo::Folder {
            check_folder(SettingsField::UnpackFolder, &self.unpack_folder)?;
        }
        for (which, addr) in [
            (ProxyEntry::Http, &self.proxy_http),
            (ProxyEntry::Https, &self.proxy_https),
            (ProxyEntry::Ftp, &self.proxy_ftp),
            (ProxyEntry::Socks, &self.proxy_socks5),
        ] {
            if !addr.trim().is_empty() && !is_host_port(addr.trim()) {
                return Err(E::ProxyNotHostPort(which));
            }
        }
        if self.proxy_mode == ProxyMode::Manual
            && self.proxy_http.trim().is_empty()
            && self.proxy_https.trim().is_empty()
            && self.proxy_ftp.trim().is_empty()
            && self.proxy_socks5.trim().is_empty()
        {
            return Err(E::NoProxy);
        }
        let mut sites: Vec<String> = Vec::new();
        for l in &self.site_logins {
            let site = l.site.trim().to_ascii_lowercase();
            let (host, path) = crate::site::split_login_pattern(&site);
            if !is_site(host) || host.ends_with(':') || path.contains(char::is_whitespace) {
                return Err(E::NotLoginSite(l.site.clone()));
            }
            if l.username.trim().is_empty() {
                return Err(E::LoginNoUser(site));
            }
            if sites.contains(&site) {
                return Err(E::TwoLogins(site));
            }
            sites.push(site);
        }
        for file in
            [&self.sound_done_file, &self.sound_failed_file, &self.sound_queue_started_file, &self.sound_queue_file]
        {
            let f = file.trim();
            if !f.is_empty() && (!Path::new(f).is_absolute() || extension_of(f).as_deref() != Some("wav")) {
                return Err(E::SoundNotWav(f.to_string()));
            }
        }
        if self.scan_after_download && self.scanner == Scanner::Custom {
            crate::command::parse_command(&self.scanner_command)?;
        }
        if self.scan_after_download && self.scanner == Scanner::Defender && !cfg!(windows) {
            return Err(E::DefenderNotHere);
        }
        Ok(())
    }

    /// The folder new downloads go to: the one set in Options, otherwise
    /// `downloads`.
    pub fn download_dir(&self, downloads: &Path) -> PathBuf {
        match &self.download_dir {
            Some(dir) => PathBuf::from(dir),
            None => downloads.to_path_buf(),
        }
    }

    /// The category a file named `filename` from `url` belongs to, or `None`
    /// for `General`. A category limited to some sites only takes files from
    /// them.
    pub fn category_of(&self, filename: &str, url: &str) -> Option<&Category> {
        let ext = extension_of(filename)?;
        if self.general_extensions.contains(&ext) {
            return None;
        }
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
            .filter(|l| crate::site::site_matches(&l.site, host))
            .max_by_key(|l| l.site.trim_start_matches("*.").len())
            .map(|l| l.max)
    }

    /// The quota as bytes and a window in minutes, when it is on.
    pub fn quota(&self) -> Option<(ByteCount, u64)> {
        self.quota_enabled.then(|| (ByteCount::mib(self.quota_mb), self.quota_hours.saturating_mul(60)))
    }

    /// Where new downloads go, by category, for files from any site.
    pub fn folders(&self, downloads: &Path) -> Folders {
        self.folders_for("", downloads)
    }

    /// Where new downloads from `url` go, by category.
    pub fn folders_for(&self, url: &str, downloads: &Path) -> Folders {
        Folders {
            categories: self.categories.iter().map(|c| (c.clone(), self.folder_of(c, downloads))).collect(),
            general: self.general_extensions.clone(),
            default: self.general_folder(downloads),
            host: host_of(url),
        }
    }

    /// Where new `General` files go: the folder last chosen for them when
    /// they remember one, otherwise the download folder.
    fn general_folder(&self, downloads: &Path) -> PathBuf {
        match &self.general_last_folder {
            Some(last) if self.general_remember_folder => PathBuf::from(last),
            _ => self.download_dir(downloads),
        }
    }

    /// Where new files of category `c` go: the folder last chosen for it in
    /// the save-as dialog when it remembers one, its own folder, or a folder
    /// named after it inside the download folder.
    fn folder_of(&self, c: &Category, downloads: &Path) -> PathBuf {
        match (&c.last_folder, &c.folder) {
            (Some(last), _) if c.remember_folder => PathBuf::from(last),
            (_, Some(f)) => PathBuf::from(f),
            _ => self.download_dir(downloads).join(c.name.trim()),
        }
    }

    /// The category named `name`, ignoring case.
    pub fn category(&self, name: &str) -> Option<&Category> {
        self.categories.iter().find(|c| c.name.trim().eq_ignore_ascii_case(name.trim()))
    }

    /// Where new files of the category named `name` go; `General`, and a
    /// name no category has, are General's folder.
    pub fn category_folder(&self, name: &str, downloads: &Path) -> PathBuf {
        match self.category(name) {
            Some(c) => self.folder_of(c, downloads),
            None => self.general_folder(downloads),
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
        let (remember, last) =
            match self.categories.iter_mut().find(|c| c.name.trim().eq_ignore_ascii_case(category.trim())) {
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
    /// Remember what the save-as dialog's boxes asked, for a download saved
    /// at `path` from `url` in `category` (or the one its type gives):
    /// whether its category keeps the folder chosen (`remember_folder`),
    /// that folder when it was `picked`, and with `later_queue` the queue
    /// "Download later" uses. Returns whether anything changed.
    pub fn remember_answer(
        &mut self,
        path: Option<&Path>,
        url: &str,
        category: Option<&str>,
        remember_folder: Option<bool>,
        picked: bool,
        later_queue: Option<crate::id::QueueId>,
    ) -> bool {
        let mut changed = false;
        if let Some(path) = path {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let chosen = category.map(str::to_string).unwrap_or_else(|| self.category_name(&name, url).to_string());
            if let Some(on) = remember_folder {
                changed |= self.set_remember_folder(&chosen, on);
            }
            // A category that remembers its folder keeps the one chosen.
            if let (true, Some(dir)) = (picked || remember_folder == Some(true), path.parent()) {
                changed |= self.remember_folder(&chosen, dir);
            }
        }
        if let Some(q) = later_queue {
            changed |= self.later_queue != Some(q);
            self.later_queue = Some(q);
        }
        changed
    }

    pub fn set_remember_folder(&mut self, category: &str, on: bool) -> bool {
        let flag = match self.categories.iter_mut().find(|c| c.name.trim().eq_ignore_ascii_case(category.trim())) {
            Some(c) => &mut c.remember_folder,
            None => &mut self.general_remember_folder,
        };
        let changed = *flag != on;
        *flag = on;
        changed
    }

    /// The speed limit in bytes per second, or `None` when it is off.
    pub fn speed_limit_bytes(&self) -> Option<u64> {
        self.speed_limit_enabled.then(|| self.speed_limit_kbps.saturating_mul(1024))
    }

    /// The limit for each download without one of its own, in bytes per
    /// second, or `None` when the speed limit is off or has no such limit.
    /// Where a package named `name` whose archives are in `archives_dir` is
    /// unpacked (see `unpack_to`). An empty chosen folder unpacks beside the
    /// archives, in a folder named after the package.
    pub fn unpack_folder_for(&self, archives_dir: &Path, name: &crate::package::PackageName) -> PathBuf {
        let chosen = self.unpack_folder.trim();
        match self.unpack_to {
            UnpackTo::Beside => archives_dir.to_path_buf(),
            UnpackTo::Folder if !chosen.is_empty() => Path::new(chosen).join(name.folder_name()),
            UnpackTo::Subfolder | UnpackTo::Folder => archives_dir.join(name.folder_name()),
        }
    }

    pub fn per_file_limit_bytes(&self) -> Option<u64> {
        self.speed_limit_per_file_kbps.filter(|_| self.speed_limit_enabled).map(|k| k.saturating_mul(1024))
    }

    /// How long the computer must go without keyboard or mouse input before
    /// the speed limit is lifted, or `None` when it is not lifted for that
    /// (the limit is off, or applies whether or not the computer is in use).
    pub fn lift_limit_after(&self) -> Option<Duration> {
        (self.speed_limit_enabled && self.speed_limit_only_while_active)
            .then(|| Duration::from_secs(u64::from(self.speed_limit_idle_minutes) * 60))
    }

    /// Whether the speed limit is lifted now that the computer has had no
    /// input for `idle`. `None`, when the system does not say, keeps it.
    pub fn limit_lifted(&self, idle: Option<Duration>) -> bool {
        self.lift_limit_after().is_some_and(|after| idle.is_some_and(|idle| idle >= after))
    }

    /// The limits the engine applies, in bytes per second: the total and
    /// the one for each download without its own. A lifted speed limit
    /// leaves neither; downloads with a limit of their own keep it.
    pub fn engine_limits(&self, lifted: bool) -> EngineLimits {
        if lifted {
            return EngineLimits::default();
        }
        EngineLimits { total: self.speed_limit_bytes(), per_file: self.per_file_limit_bytes() }
    }
}

/// The speed limits the engine applies, in bytes per second; `None` is no
/// limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EngineLimits {
    pub total: Option<u64>,
    pub per_file: Option<u64>,
}

/// Addresses no browser download is taken from as UDM starts: sounds,
/// game assets and update files that sites and browsers fetch on their own.
/// Taken from IDM's `defexclist.txt`, keeping the masks whose sites still
/// answer. With no scheme, a mask matches `http` and `https` alike.
pub const DEFAULT_CAPTURE_EXCLUDED_URLS: &[&str] = &[
    "*.gstatic.com/*/sounds/*.mp3",
    "*.fbcdn.net/rsrc.php/*.mp3",
    "*.fbcdn.net/rsrc.php/*.ogg",
    "web.whatsapp.com/res/*.mp3",
    "www.youtube.com/s/search/audio/*.mp3",
    "*.gvt1.com/edgedl/widevine-cdm/*.zip*",
    "ciscobinary.openh264.org/*.zip",
    "*.download.windowsupdate.com/*",
    "msedge.b.tlu.dl.delivery.mp.microsoft.com/filestreamingservice/files/*",
    "get*.opera.com/pub/opera/*/autoupdate/*.exe",
    "get*.opera.com/pub/opera/*/autoupdate/*.msi",
    "get.geo.opera.com/res/servicefiles/*.zip",
    "api.browser.ya.ru/configs/all_zip?*",
    "www.slimjetbrowser.com/release/*_console.exe",
    "www.bing.com/api/shopping/v1/ux/images/autoApply/*.mp4",
    "img6.wsimg.com/serpimg/img/solutionsimg/*.mp4",
];

/// `DEFAULT_CAPTURE_EXCLUDED_URLS` as a list to store.
pub fn default_capture_excluded_urls() -> Vec<String> {
    DEFAULT_CAPTURE_EXCLUDED_URLS.iter().map(|&u| u.into()).collect()
}

/// Add each default address that `list` lacks, after the user's own, and
/// return whether any was added. Addresses compare without case, as they
/// match.
pub fn add_missing_exclusions(list: &mut Vec<String>) -> bool {
    let before = list.len();
    for &url in DEFAULT_CAPTURE_EXCLUDED_URLS {
        if !list.iter().any(|u| u.trim().eq_ignore_ascii_case(url)) {
            list.push(url.into());
        }
    }
    list.len() > before
}

fn yes() -> bool {
    true
}

/// A host name, optionally with a `*.` wildcard or a port.
fn is_site(site: &str) -> bool {
    !site.is_empty() && site.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '*' | ':'))
}

fn check_folder(what: SettingsField, dir: &str) -> Result<(), InvalidSettings> {
    if dir.trim().is_empty() {
        return Err(InvalidSettings::FolderEmpty(what));
    }
    let path = Path::new(dir);
    if !path.is_absolute() {
        return Err(InvalidSettings::FolderNotFull(what, dir.to_string()));
    }
    Ok(())
}

fn check_extensions(what: SettingsField, exts: &[String]) -> Result<(), InvalidSettings> {
    for e in exts {
        if e.is_empty() || !e.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) {
            return Err(InvalidSettings::BadFileType(what, e.clone()));
        }
    }
    Ok(())
}

/// A part of the settings that a message names.
///
/// The variant names are part of the wire format (the daemon's
/// `error_kind`): renaming one changes what clients read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettingsField {
    DownloadFolder,
    /// The folder of the category with this name.
    CategoryFolder(String),
    /// The category with this name, General included.
    Category(String),
    CapturedTypes,
    VideoTypes,
    PlayerTypes,
    AutoStartTypes,
    /// The folder packages are unpacked into.
    UnpackFolder,
}

impl std::fmt::Display for SettingsField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsField::DownloadFolder => f.write_str("The download folder"),
            SettingsField::CategoryFolder(name) => write!(f, "The folder of \"{name}\""),
            SettingsField::Category(name) => write!(f, "The category \"{name}\""),
            SettingsField::CapturedTypes => f.write_str("The captured file types"),
            SettingsField::VideoTypes => f.write_str("The video types"),
            SettingsField::PlayerTypes => f.write_str("The types taken from web players"),
            SettingsField::AutoStartTypes => f.write_str("The file types that start without asking"),
            SettingsField::UnpackFolder => f.write_str("The folder to unpack into"),
        }
    }
}

/// One of the manual proxies.
///
/// The variant names are part of the wire format (the daemon's
/// `error_kind`): renaming one changes what clients read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProxyEntry {
    Http,
    Https,
    Ftp,
    Socks,
}

impl std::fmt::Display for ProxyEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ProxyEntry::Http => "The HTTP proxy",
            ProxyEntry::Https => "The HTTPS proxy",
            ProxyEntry::Ftp => "The FTP proxy",
            ProxyEntry::Socks => "The SOCKS proxy",
        })
    }
}

/// Why settings are not valid (see [`Settings::validate`]). The window
/// shows these, translated.
///
/// The variant names are part of the wire format (the daemon's
/// `error_kind`): renaming one changes what clients read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum InvalidSettings {
    #[error("The configuration script must be an http or https address.")]
    ScriptNotWeb,
    #[error("\"{0}\" is not a language code, such as de or pt-BR.")]
    Language(String),
    #[error("The interface size must be between {lo}% and {hi}%.", lo = TEXT_SIZE_RANGE.0, hi = TEXT_SIZE_RANGE.1)]
    TextSize,
    #[error("The quota's window must be between {lo} and {hi} hours.", lo = QUOTA_HOURS_RANGE.0, hi = QUOTA_HOURS_RANGE.1)]
    QuotaHours,
    #[error("The quota must be at least 1 MB.")]
    QuotaZero,
    #[error("\"{0}\" is not a server name, such as files.example.com.")]
    ServerName(String),
    #[error("The connections for {0} must be between {lo} and {hi}.", lo = SERVER_CONNECTIONS_RANGE.0, hi = SERVER_CONNECTIONS_RANGE.1)]
    ServerConnections(String),
    #[error("The keys that skip and force capture must differ.")]
    SameCaptureKeys,
    #[error("Insert and Del can only force capture, not skip it.")]
    InsertDelSkip,
    #[error("The temporary folder must be a full path: {0}")]
    TempNotFull(String),
    #[error("The User-Agent must be one line of text.")]
    UserAgentLines,
    #[error("Connections per file must be between {lo} and {hi}.", lo = CONNECTIONS_RANGE.0, hi = CONNECTIONS_RANGE.1)]
    Connections,
    #[error("Downloads at once must be between {lo} and {hi}.", lo = MAX_CONCURRENT_RANGE.0, hi = MAX_CONCURRENT_RANGE.1)]
    DownloadsAtOnce,
    #[error("The speed limit must be between {lo} and {hi} KB/s.", lo = SPEED_LIMIT_RANGE_KBPS.0, hi = SPEED_LIMIT_RANGE_KBPS.1)]
    SpeedLimit,
    #[error("The speed limit per file must be between {lo} and {hi} KB/s.", lo = SPEED_LIMIT_RANGE_KBPS.0, hi = SPEED_LIMIT_RANGE_KBPS.1)]
    SpeedLimitPerFile,
    #[error("The time away must be between {lo} and {hi} minutes.", lo = IDLE_MINUTES_RANGE.0, hi = IDLE_MINUTES_RANGE.1)]
    IdleMinutes,
    #[error("The remote-control port must be between {lo} and {hi}.", lo = REMOTE_PORT_RANGE.0, hi = REMOTE_PORT_RANGE.1)]
    RemotePort,
    /// An address to listen on that is not an IP address.
    #[error("\"{0}\" is not an IP address of this computer, such as 192.168.1.20.")]
    RemoteAddress(String),
    #[error("{0} is empty.")]
    FolderEmpty(SettingsField),
    #[error("{0} must be a full path: {1}")]
    FolderNotFull(SettingsField, String),
    #[error("Every category needs a name.")]
    CategoryUnnamed,
    #[error("\"{name}\" is kept for files of every other type.", name = GENERAL)]
    GeneralName,
    #[error("There are two categories named \"{0}\".")]
    TwoCategories(String),
    #[error("The category \"{0}\" has no file types.")]
    NoFileTypes(String),
    #[error("{0} has an invalid file type \"{1}\". Use letters and digits, such as zip or mp4.")]
    BadFileType(SettingsField, String),
    #[error("The minimum video size is too large.")]
    VideoMinSize,
    #[error("List the types to take from web players, such as mp4.")]
    NoPlayerTypes,
    #[error("\"{0}\" is not a context menu item.")]
    MenuItem(String),
    #[error("The minimum size to capture is too large.")]
    CaptureMinSize,
    #[error("\"{0}\" is not a site. Use a host name such as example.com or *.example.com.")]
    NotSite(String),
    #[error("ffmpeg must be the full path of ffmpeg.exe: {0}")]
    FfmpegNotFull(String),
    #[error("7-Zip must be the full path of its program, such as 7z.exe: {0}")]
    SevenZipNotFull(String),
    #[error("{0} must be host:port, such as proxy.example.com:8080.")]
    ProxyNotHostPort(ProxyEntry),
    #[error("Set at least one proxy, or choose another proxy option.")]
    NoProxy,
    #[error("\"{0}\" is not a site. Use a host name such as example.com, or one with a folder such as example.com/private/*.")]
    NotLoginSite(String),
    #[error("The login for {0} needs a user name.")]
    LoginNoUser(String),
    #[error("There are two logins for {0}.")]
    TwoLogins(String),
    #[error("A sound must be the full path of a .wav file: {0}")]
    SoundNotWav(String),
    #[error(transparent)]
    Scanner(#[from] crate::command::CommandError),
    #[error("Microsoft Defender is only on Windows. Choose a scanner program such as clamscan.")]
    DefenderNotHere,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A Windows full path as a full path on this system: `C:\a\b` stays
    /// on Windows and is `/a/b` elsewhere.
    fn full(windows: &str) -> String {
        if cfg!(windows) {
            windows.to_string()
        } else {
            windows[2..].replace('\\', "/")
        }
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
    fn private_windows_are_not_captured_by_default() {
        assert!(!Settings::default().capture_private);
        let old: Settings = serde_json::from_str(r#"{"connections":4}"#).unwrap();
        assert!(!old.capture_private, "a settings file from before the option");
        let on: Settings = serde_json::from_str(r#"{"capture_private":true}"#).unwrap();
        assert!(on.capture_private);
    }

    #[test]
    fn the_speed_limit_is_lifted_only_after_the_minutes_away() {
        let mins = |m: u64| Some(Duration::from_secs(m * 60));
        let mut s = Settings {
            speed_limit_enabled: true,
            speed_limit_per_file_kbps: Some(200),
            speed_limit_only_while_active: true,
            speed_limit_idle_minutes: 10,
            ..Settings::default()
        };
        assert_eq!(s.lift_limit_after(), mins(10));
        assert!(!s.limit_lifted(mins(9)));
        assert!(s.limit_lifted(mins(10)));
        assert!(!s.limit_lifted(None), "an unknown idle time keeps the limit");
        assert_eq!(s.engine_limits(true), EngineLimits { total: None, per_file: None });
        assert_eq!(
            s.engine_limits(false),
            EngineLimits { total: Some(s.speed_limit_kbps * 1024), per_file: Some(200 * 1024) }
        );

        s.speed_limit_only_while_active = false;
        assert_eq!(s.lift_limit_after(), None);
        assert!(!s.limit_lifted(mins(60)), "the limit applies whether or not the computer is in use");
        s.speed_limit_only_while_active = true;
        s.speed_limit_enabled = false;
        assert!(!s.limit_lifted(mins(60)), "there is no limit to lift");
    }

    #[test]
    fn themes_read_the_old_format_and_keep_theme_files_by_name() {
        // Before theme files, the theme was one of three words.
        for (json, theme) in [("system", Theme::System), ("light", Theme::Light), ("dark", Theme::Dark)] {
            let s: Settings = serde_json::from_str(&format!(r#"{{"theme":"{json}","text_size":150}}"#)).unwrap();
            assert_eq!((s.theme, s.text_size), (theme, 150));
        }
        let ocean = Theme::Custom(ThemeName::parse("Ocean").unwrap());
        for theme in [Theme::HighContrast, ocean.clone(), Theme::Dark] {
            let s = Settings { theme: theme.clone(), ..Settings::default() };
            let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
            assert_eq!(back.theme, theme);
        }
        let json = serde_json::to_value(Settings { theme: ocean, ..Settings::default() }).unwrap();
        assert_eq!(json["theme"], serde_json::json!({"custom": "Ocean"}));
        let json = serde_json::to_value(Settings { theme: Theme::HighContrast, ..Settings::default() }).unwrap();
        assert_eq!(json["theme"], "high_contrast");
        // A newer UDM's theme, or a name that is not one, follows the
        // system instead of failing the whole file.
        for odd in [r#""sepia""#, r#"{"custom":""}"#, r#"{"gradient":[1,2]}"#, "7"] {
            let s: Settings = serde_json::from_str(&format!(r#"{{"theme":{odd},"connections":4}}"#)).unwrap();
            assert_eq!((s.theme, s.connections), (Theme::System, 4), "{odd}");
        }
    }

    #[test]
    fn older_settings_keep_remote_control_off_and_odd_ports_and_addresses_are_refused() {
        let old: Settings = serde_json::from_str(r#"{"connections":4,"language":"de"}"#).unwrap();
        assert!(!old.remote_enabled);
        assert_eq!(old.remote_port, DEFAULT_REMOTE_PORT);
        assert!(old.remote_addresses.is_empty());
        assert!(!Settings::default().remote_enabled, "off by default");
        let saved = serde_json::to_value(Settings::default()).unwrap();
        assert!(saved.get("remote_addresses").is_none(), "every interface is the field left out");
        for port in [0, 80, 1023] {
            let s = Settings { remote_port: port, ..Settings::default() };
            assert_eq!(s.validate(), Err(InvalidSettings::RemotePort));
        }
        let s = Settings { remote_addresses: vec!["192.168.1.20".into(), "fe80::1".into()], ..Settings::default() };
        assert_eq!(s.validate(), Ok(()));
        let s = Settings { remote_addresses: vec!["my-pc.local".into()], ..Settings::default() };
        assert_eq!(s.validate(), Err(InvalidSettings::RemoteAddress("my-pc.local".into())));
        let on: Settings =
            serde_json::from_str(r#"{"remote_enabled":true,"remote_port":20000,"remote_addresses":["10.0.0.2"]}"#)
                .unwrap();
        assert_eq!((on.remote_enabled, on.remote_port, on.remote_addresses), (true, 20000, vec!["10.0.0.2".into()]));
    }

    #[test]
    fn older_settings_apply_the_limit_all_the_time_and_odd_idle_times_are_refused() {
        let old: Settings = serde_json::from_str(r#"{"speed_limit_enabled":true}"#).unwrap();
        assert!(!old.speed_limit_only_while_active);
        assert_eq!(old.speed_limit_idle_minutes, DEFAULT_IDLE_MINUTES);
        for minutes in [0, 241] {
            let s = Settings { speed_limit_idle_minutes: minutes, ..Settings::default() };
            assert_eq!(s.validate(), Err(InvalidSettings::IdleMinutes));
        }
        assert_eq!(Settings { speed_limit_idle_minutes: 240, ..Settings::default() }.validate(), Ok(()));
    }

    #[test]
    fn packages_unpack_where_the_settings_say() {
        let name = crate::package::PackageName::parse("Show: S1").unwrap();
        let dir = Path::new("/dl/Compressed");
        let mut s = Settings::default();
        assert_eq!(s.unpack_folder_for(dir, &name), dir.join("Show_ S1"), "a folder of its own by default");
        s.unpack_to = UnpackTo::Beside;
        assert_eq!(s.unpack_folder_for(dir, &name), dir);
        s.unpack_to = UnpackTo::Folder;
        assert_eq!(s.unpack_folder_for(dir, &name), dir.join("Show_ S1"), "no folder chosen");
        assert_eq!(s.validate(), Err(InvalidSettings::FolderEmpty(SettingsField::UnpackFolder)));
        s.unpack_folder = "relative".into();
        assert!(matches!(s.validate(), Err(InvalidSettings::FolderNotFull(SettingsField::UnpackFolder, _))));
        let unpacked = std::env::temp_dir().join("Unpacked");
        s.unpack_folder = unpacked.display().to_string();
        assert_eq!(s.validate(), Ok(()));
        assert_eq!(s.unpack_folder_for(dir, &name), unpacked.join("Show_ S1"));
        s.seven_zip_path = "7z.exe".into();
        assert_eq!(s.validate(), Err(InvalidSettings::SevenZipNotFull("7z.exe".into())));
    }

    #[test]
    fn older_settings_do_not_unpack_on_their_own() {
        let old: Settings = serde_json::from_str("{}").unwrap();
        assert!(!old.unpack_auto && !old.unpack_recycle);
        assert_eq!((old.unpack_to, old.seven_zip_path.as_str()), (UnpackTo::Subfolder, ""));
    }

    #[test]
    fn the_limit_per_file_applies_only_while_the_speed_limit_is_on() {
        let mut s = Settings { speed_limit_per_file_kbps: Some(200), ..Settings::default() };
        assert_eq!(s.per_file_limit_bytes(), None);
        s.speed_limit_enabled = true;
        assert_eq!(s.per_file_limit_bytes(), Some(200 * 1024));
        s.speed_limit_per_file_kbps = None;
        assert_eq!(s.per_file_limit_bytes(), None);
        // Settings saved before these options load with them off.
        let old: Settings = serde_json::from_str(r#"{"speed_limit_enabled":false}"#).unwrap();
        assert!(!old.speed_limit_on_startup);
        assert_eq!(old.speed_limit_per_file_kbps, None);
    }

    #[test]
    fn restoring_exclusions_keeps_the_users_own() {
        let mut list = vec!["https://dl.net/keep/*".to_string(), DEFAULT_CAPTURE_EXCLUDED_URLS[0].to_string()];
        assert!(add_missing_exclusions(&mut list));
        assert_eq!(list[0], "https://dl.net/keep/*");
        assert_eq!(list.len(), DEFAULT_CAPTURE_EXCLUDED_URLS.len() + 1);
        assert!(!add_missing_exclusions(&mut list));
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
        for (per_file, valid) in [(None, true), (Some(0), false), (Some(1), true), (Some(10_000_001), false)] {
            let s = Settings { speed_limit_per_file_kbps: per_file, ..Settings::default() };
            assert_eq!(s.validate().is_ok(), valid, "per file {per_file:?}");
        }
        for (text_size, valid) in [(79, false), (80, true), (150, true), (200, true), (201, false)] {
            let s = Settings { text_size, ..Settings::default() };
            assert_eq!(s.validate().is_ok(), valid, "text_size {text_size}");
        }
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
        let bad =
            Settings { server_limits: vec![ServerLimit { site: "x.net".into(), max: 40 }], ..Settings::default() };
        assert!(bad.validate().is_err());
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
            proxy_https_password: Some("https-secret".into()),
            proxy_ftp_password: Some("ftp-secret".into()),
            proxy_socks_password: Some("socks-secret".into()),
            site_logins: vec![SiteLogin {
                site: "example.com".into(),
                username: "ann".into(),
                password: Some("login-secret".into()),
                plain: false,
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
            plain: false,
        };
        assert!(with(&|s| s.site_logins = vec![login("a.com", "u")]).is_ok());
        assert!(with(&|s| s.site_logins = vec![login("a.com", " ")]).is_err());
        assert!(with(&|s| s.site_logins = vec![login("https://a.com/", "u")]).is_err());
        assert!(with(&|s| s.site_logins = vec![login("a.com/private/*", "u")]).is_ok());
        assert!(with(&|s| s.site_logins = vec![login("a.com/my files", "u")]).is_err());
        assert!(with(&|s| s.site_logins = vec![login("a.com", "u"), login("a.com/x", "v")]).is_ok());
        assert!(with(&|s| s.site_logins = vec![login("a.com", "u"), login("A.com", "v")]).is_err());
    }

    /// Options lists exactly the items the extension's menu has, so each can
    /// be left out (the page item came later, in #150).
    #[test]
    fn the_menu_items_are_the_extensions() {
        let worker = include_str!("../../../extension/chromium/background.js");
        let start = worker.find("const MENU_ITEMS = {").unwrap();
        let end = start + worker[start..].find("\n};").unwrap();
        let in_extension: Vec<&str> = worker[start..end]
            .lines()
            .skip(1)
            .filter_map(|l| l.strip_prefix("  ")?.split_once(':').map(|(name, _)| name))
            .filter(|name| !name.starts_with(' '))
            .collect();
        let in_options: Vec<&str> = BROWSER_MENU_ITEMS.iter().map(|(id, _)| *id).collect();
        assert_eq!(in_extension, in_options);
        let page = Settings { browser_menu_hidden_chromium: vec!["page".into()], ..Settings::default() };
        assert!(page.validate().is_ok());
    }

    #[test]
    fn new_options_have_idms_defaults_and_old_files_load() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(s.progress_hide_buttons && s.list_queue_panel && s.proxy_socks_ftp);
        assert!(
            !s.progress_start_details && !s.proxy_socks_https && !s.server_file_date_created && !s.video_auto_capture
        );
        assert_eq!(s.progress_start_tab, ProgressTab::Status);
        assert_eq!(s.video_auto_types, default_video_auto_types());
        let tab: Settings = serde_json::from_str(r#"{"progress_start_tab":"speed_limit"}"#).unwrap();
        assert_eq!(tab.progress_start_tab, ProgressTab::SpeedLimit);
        let none = Settings { video_auto_capture: true, video_auto_types: Vec::new(), ..Settings::default() };
        assert!(none.validate().is_err(), "auto capture needs types");
    }

    #[test]
    fn capture_keys_are_combinations() {
        let read = |json: &str| serde_json::from_str::<Settings>(json).unwrap();
        // Files from when one key was allowed.
        assert_eq!(read(r#"{"capture_skip_key":"shift"}"#).capture_skip_key, CaptureKey::SHIFT);
        assert_eq!(read(r#"{"capture_force_key":"none"}"#).capture_force_key, CaptureKey::NONE);
        let both = read(r#"{"capture_force_key":"Shift+ctrl+insert"}"#).capture_force_key;
        assert_eq!(both, CaptureKey { ctrl: true, shift: true, insert: true, ..CaptureKey::NONE });
        assert_eq!(String::from(both), "ctrl+shift+insert");
        assert_eq!(both.label(), "Ctrl+Shift+Insert");
        assert!(serde_json::from_str::<Settings>(r#"{"capture_skip_key":"win"}"#).is_err());
        let json = serde_json::to_string(&Settings::default()).unwrap();
        assert!(json.contains(r#""capture_skip_key":"alt""#), "{json}");

        let with = |skip: CaptureKey, force: CaptureKey| {
            Settings { capture_skip_key: skip, capture_force_key: force, ..Settings::default() }.validate()
        };
        assert!(with(CaptureKey::ALT, CaptureKey { alt: true, shift: true, ..CaptureKey::NONE }).is_ok());
        assert!(with(CaptureKey::CTRL, CaptureKey::CTRL).is_err());
        assert!(with(CaptureKey::NONE, CaptureKey::NONE).is_ok());
        assert!(with(CaptureKey { insert: true, ..CaptureKey::NONE }, CaptureKey::CTRL).is_err());

        // Del forces capture, alone or with modifiers, as IDM's can.
        let del = read(r#"{"capture_force_key":"del"}"#).capture_force_key;
        assert_eq!(del, CaptureKey { delete: true, ..CaptureKey::NONE });
        assert_eq!(read(r#"{"capture_force_key":"ctrl+Delete"}"#).capture_force_key.label(), "Ctrl+Del");
        assert_eq!(String::from(del), "del");
        assert!(with(CaptureKey::ALT, del).is_ok());
        assert!(with(del, CaptureKey::CTRL).is_err(), "Del cannot skip");
    }

    #[test]
    fn sounds_are_wav_files_or_the_system_sound() {
        let with = |file: &str| Settings { sound_failed_file: file.into(), ..Settings::default() };
        assert!(with("").validate().is_ok());
        assert!(with(&full(r"C:\Windows\Media\chord.WAV")).validate().is_ok());
        assert!(with(&full(r"C:\music\song.mp3")).validate().is_err());
        assert!(with("chord.wav").validate().is_err());
        assert_eq!(with("").sound_file(Sound::Failed), None);
        let a = full(r"C:\a.wav");
        assert_eq!(with(&a).sound_file(Sound::Failed), Some(PathBuf::from(&a)));
        assert_eq!(with(&a).sound_file(Sound::Done), None);
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
        let off = Settings { scan_after_download: false, ..custom("") };
        assert!(off.validate().is_ok());
    }
}
