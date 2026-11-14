//! The Options window's contract: the settings it edits as plain fields,
//! and what it chooses from. The window edits a copy and sends it back
//! whole with `save_options`; the shell parses and checks it.
//!
//! No password crosses to the webview: the password fields come empty, and
//! an empty one sent back keeps the stored password.

use serde::{Deserialize, Serialize};
use specta::Type;

use super::Id;

/// A value and what a list shows for it.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub value: String,
    pub label: String,
}

/// The Options window.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct OptionsView {
    pub draft: OptionsDraft,
    pub themes: Vec<Choice>,
    pub languages: Vec<Choice>,
    /// The items UDM can add to the browsers' context menus.
    pub menu_items: Vec<Choice>,
    /// What the Default buttons put back.
    pub default_capture_types: String,
    pub default_excluded_sites: String,
    pub connections: Vec<u32>,
    pub server_connections: Vec<u32>,
    pub tools: Vec<ToolRow>,
    /// What the last install said.
    pub tool_note: Option<super::Banner>,
    /// `None` until the daemon said how it stands.
    pub remote: Option<RemoteView>,
    /// `None` while they are read.
    pub browsers: Option<Vec<BrowserRowView>>,
    /// Which browsers read Chrome's registration.
    pub chrome_note: String,
    /// What the last browser change said.
    pub browser_note: Option<super::Banner>,
    /// The theme files that give no theme, and why.
    pub theme_notes: Vec<String>,
    /// A finished file's creation time can be set on this system.
    pub creation_time: bool,
    /// A portable copy, which does not start when the user signs in.
    pub portable: bool,
    pub debrid: DebridView,
}

/// Keys held while clicking a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct KeysDraft {
    pub on: bool,
    pub alt: bool,
    pub shift: bool,
    pub ctrl: bool,
    pub insert: bool,
    pub delete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct VideoTypeDraft {
    pub ext: String,
    pub on: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CategoryDraft {
    /// The name it had when the window opened; `None` for a new one.
    pub original: Option<String>,
    pub name: String,
    /// File extensions, separated by spaces.
    pub extensions: String,
    pub sites_on: bool,
    pub sites: String,
    pub folder: String,
    pub remember_folder: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ServerLimitDraft {
    pub site: String,
    pub max: u32,
}

/// One proxy: on for its protocol, its server, port and login.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProxyDraft {
    pub on: bool,
    pub host: String,
    pub port: String,
    pub user: String,
    /// Empty keeps the stored password.
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LoginDraft {
    pub site: String,
    pub user: String,
    /// Empty keeps the stored password.
    pub password: String,
    /// Had a password when the window opened.
    pub stored: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SoundDraft {
    pub on: bool,
    pub file: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ProxyChoice {
    None,
    System,
    Script,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum DuplicateChoice {
    Ask,
    NewName,
    Overwrite,
    Existing,
}

/// The settings the Options window edits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct OptionsDraft {
    // General.
    pub start_with_system: bool,
    pub tray_icon: bool,
    pub close_to_tray: bool,
    pub minimize_to_tray: bool,
    pub keep_awake: bool,
    pub check_updates: bool,
    pub clipboard_monitor: bool,
    pub clipboard_start: bool,
    pub capture_private: bool,
    /// `system`, `light`, `dark`, `highContrast`, or `custom:` and a name.
    pub theme: String,
    pub language: String,
    pub skip_key: KeysDraft,
    pub force_key: KeysDraft,
    pub keys_left_click: bool,
    pub force_skips_pages: bool,
    /// The context menu items shown in Chrome-like browsers and in Firefox.
    pub menu_chromium: Vec<String>,
    pub menu_firefox: Vec<String>,
    pub video_button: bool,
    pub video_mini: bool,
    pub video_types: Vec<VideoTypeDraft>,
    pub video_auto_capture: bool,
    pub video_protected_note: bool,
    // File types.
    pub capture_types: String,
    pub capture_excluded_sites: String,
    pub capture_excluded_urls: Vec<String>,
    pub offer_exceptions: bool,
    // Save to.
    pub download_dir: String,
    pub general_remember_folder: bool,
    pub categories: Vec<CategoryDraft>,
    pub server_file_date: bool,
    pub temp_dir: String,
    // Downloads.
    pub ask_where_to_save: bool,
    pub add_only: bool,
    pub done_dialog: bool,
    pub start_while_asking: bool,
    pub later_panel: bool,
    pub list_queue_panel: bool,
    pub resume_ignores_date: bool,
    pub if_duplicate: DuplicateChoice,
    pub user_agent: String,
    pub progress_on_start: bool,
    pub progress_speed_tab: bool,
    pub progress_done_tab: bool,
    pub progress_hide_buttons: bool,
    pub progress_start_details: bool,
    pub scan_after_download: bool,
    pub scanner_custom: bool,
    pub scanner_command: String,
    // Connection.
    pub connections: u32,
    pub server_limits: Vec<ServerLimitDraft>,
    pub quota_enabled: bool,
    pub quota_mb: u32,
    pub quota_hours: u32,
    pub quota_warn: bool,
    // Proxy / Socks.
    pub proxy_mode: ProxyChoice,
    pub browser_proxy: bool,
    pub pac_url: String,
    /// HTTP, HTTPS, FTP and SOCKS, in that order.
    pub proxies: Vec<ProxyDraft>,
    pub socks4: bool,
    pub passive_ftp: bool,
    // Sites Logins.
    pub logins: Vec<LoginDraft>,
    // Sounds: download complete, failed, queue started, queue done.
    pub sounds: Vec<SoundDraft>,
    /// What UDM adds to IDM's pages, on pages of its own.
    pub extras: ExtrasDraft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum NotifyChoice {
    Off,
    Toast,
    Dialog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum FileExistsChoice {
    Rename,
    Overwrite,
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum StartTabChoice {
    Status,
    SpeedLimit,
    WhenDone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum SelectionChoice {
    Off,
    Everywhere,
    Sites,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum UnpackChoice {
    /// A folder named after the package, beside the archives.
    Subfolder,
    /// The folder the archives are in.
    Beside,
    /// A folder named after the package, in `unpack_folder`.
    Folder,
}

/// The settings UDM has beyond IDM's pages: speed limits, how downloads
/// behave, the browser's buttons, archives, tools and remote control.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ExtrasDraft {
    // Speed.
    pub max_concurrent: u32,
    pub limit_on: bool,
    pub limit_kb: u32,
    pub limit_on_startup: bool,
    pub per_file_on: bool,
    pub per_file_kb: u32,
    pub limit_while_active: bool,
    pub idle_minutes: u32,
    // Behavior.
    pub notify: NotifyChoice,
    pub file_exists: FileExistsChoice,
    /// File types started without asking, separated by spaces or commas.
    pub auto_start_types: String,
    pub resume_at_startup: bool,
    pub server_file_date_created: bool,
    pub progress_start_tab: StartTabChoice,
    pub confirm_remove: bool,
    pub remove_deletes_files: bool,
    /// The interface size, in percent.
    pub text_size: u32,
    // Browser.
    pub capture_min_size_kb: u32,
    pub selection_button: SelectionChoice,
    pub selection_sites: String,
    pub video_excluded_sites: String,
    pub video_min_size_kb: u32,
    pub video_auto_types: String,
    // Proxy, advanced.
    pub socks_https: bool,
    pub socks_ftp: bool,
    pub remote_dns: bool,
    pub proxy_bypass: String,
    pub browser_proxy_after_errors: bool,
    // Archives.
    pub unpack_auto: bool,
    pub unpack_to: UnpackChoice,
    pub unpack_folder: String,
    pub unpack_recycle: bool,
    pub seven_zip_path: String,
    // Tools.
    pub ffmpeg_path: String,
    pub ytdlp_auto_update: bool,
    // Remote control.
    pub remote_enabled: bool,
    pub remote_port: u32,
    /// Listen on every network interface.
    pub remote_all: bool,
    /// The addresses to listen on otherwise, separated by spaces or commas.
    pub remote_addresses: String,
    // Debrid account (experimental).
    pub debrid_on: bool,
    pub debrid_service: Option<DebridChoice>,
    /// A new API key to store; empty keeps the stored one. It comes empty:
    /// the stored key never crosses to the webview.
    pub debrid_key: String,
    /// Remove the stored key.
    pub debrid_forget: bool,
}

/// A debrid service Options offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum DebridChoice {
    RealDebrid,
    AllDebrid,
    Premiumize,
}

/// A debrid service as Options lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DebridServiceRow {
    pub service: DebridChoice,
    /// Its own name, untranslated.
    pub name: String,
    /// Where its users find their API key.
    pub key_page: String,
}

/// Options' Debrid page beyond the settings.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DebridView {
    pub services: Vec<DebridServiceRow>,
    /// A key is stored; the key itself is never sent.
    pub key_stored: bool,
    /// Check account is waiting for the service.
    pub checking: bool,
    /// What the last check said.
    pub check: Option<super::Banner>,
}

/// A tool UDM can install.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ToolKey {
    YtDlp,
    Ffmpeg,
    GalleryDl,
}

/// A tool's row in Options > Tools: where it is, and installing it.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ToolRow {
    pub tool: ToolKey,
    pub status: String,
    /// Install or Update, with its name.
    pub action: String,
    /// Where the install comes from, or the package manager's command.
    pub hint: String,
    pub installable: bool,
    pub busy: bool,
}

/// A paired phone.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRow {
    pub id: Id,
    pub name: String,
    /// When it was paired and last seen.
    pub detail: String,
}

/// A pairing code being shown, with its QR code.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PairingView {
    pub link: String,
    pub code: String,
    /// It was used or ran out.
    pub expired: bool,
    /// The QR code's modules row by row, true for dark, `side` a row.
    pub modules: Vec<bool>,
    pub side: u32,
    /// Where to type the code instead.
    pub type_note: String,
}

/// How remote control stands.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RemoteView {
    pub status: String,
    /// The status is why it does not listen.
    pub error: bool,
    pub fingerprint: Option<String>,
    pub devices: Vec<DeviceRow>,
    pub pairing: Option<PairingView>,
    /// It listens, so a phone can pair.
    pub can_pair: bool,
}

/// A browser UDM can be registered with.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BrowserRowView {
    pub index: u32,
    pub name: String,
    /// For an added browser: its program and whose registration it reads.
    pub detail: String,
    pub registered: bool,
    /// The user added it, so it can be removed.
    pub added: bool,
    pub can_restart: bool,
}

/// What Get System found: the four proxies and the addresses without one.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SystemProxyView {
    pub proxies: Vec<ProxyDraft>,
    pub bypass: String,
}

/// What a Browse button picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum PickKind {
    Folder,
    Sound,
    Program,
}
