//! Reading and writing the user's settings in `settings.json`, in UDM's
//! settings folder (`%APPDATA%\UDM` on Windows; see
//! `paths::config_dir`), and the settings rules that need the filesystem.
//! The settings themselves are `udm_domain::settings`.
//!
//! A missing file, or a missing field in an older file, means the default
//! for that value, so new options can be added without breaking existing
//! files.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use udm_domain::settings::*;

/// Read settings from `path`. A missing file gives the defaults; an
/// unreadable or invalid one is an error, so the caller can say so instead
/// of silently dropping the user's choices.
pub fn load(path: &Path) -> io::Result<Settings> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(e) => return Err(e),
    };
    // Older files hold proxy passwords; serde's message could quote one.
    let invalid = |e: serde_json::Error| crate::logging::json_error(&e);
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).map_err(invalid)?;
    let split = split_proxy_login(&mut value);
    split_browser_menu(&mut value);
    add_hds_type(&mut value);
    add_default_exclusions(&mut value);
    let mut settings: Settings = serde_json::from_value(value).map_err(invalid)?;
    settings.proxy_logins_split = split;
    validate(&settings).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(settings)
}

/// Write settings to `path` through a temp file and a rename, so a crash
/// never leaves a half-written file.
pub fn save(settings: &Settings, path: &Path) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(settings).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write_atomic(path, &bytes)
}

/// Why settings fail their checks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckError {
    #[error(transparent)]
    Invalid(#[from] udm_domain::settings::InvalidSettings),
    #[error("{0} is a file, not a folder: {1}")]
    FolderIsFile(SettingsField, String),
    #[error("ffmpeg must be the full path of ffmpeg.exe: {0}")]
    FfmpegIsFolder(String),
}

impl From<CheckError> for udm_domain::ports::SettingsError {
    fn from(e: CheckError) -> Self {
        match e {
            CheckError::Invalid(e) => udm_domain::ports::SettingsError::Invalid(e),
            other => udm_domain::ports::SettingsError::Refused(other.to_string()),
        }
    }
}

/// The checks of [`Settings::validate`], then those that look at the disk:
/// a folder setting must not name a file, and ffmpeg must not be a folder.
pub fn validate(settings: &Settings) -> Result<(), CheckError> {
    settings.validate()?;
    let folders = settings.download_dir.iter().map(|dir| (SettingsField::DownloadFolder, dir)).chain(
        settings.categories.iter().filter_map(|c| {
            c.folder.as_ref().map(|dir| (SettingsField::CategoryFolder(c.name.trim().to_string()), dir))
        }),
    );
    let unpack =
        (settings.unpack_to == UnpackTo::Folder).then_some((SettingsField::UnpackFolder, &settings.unpack_folder));
    let folders = folders.chain(unpack);
    for (what, dir) in folders {
        let path = Path::new(dir);
        if path.exists() && !path.is_dir() {
            return Err(CheckError::FolderIsFile(what, dir.clone()));
        }
    }
    let ffmpeg = settings.ffmpeg_path.trim();
    if !ffmpeg.is_empty() && Path::new(ffmpeg).is_dir() {
        return Err(CheckError::FfmpegIsFolder(ffmpeg.to_string()));
    }
    Ok(())
}

/// ffmpeg: the one set in Options if it exists, otherwise the one UDM
/// installed (`paths::tools_dir`), otherwise the first on `PATH`.
pub fn ffmpeg(settings: &Settings) -> Option<PathBuf> {
    let set = settings.ffmpeg_path.trim();
    if !set.is_empty() {
        return Some(PathBuf::from(set)).filter(|p| p.is_file());
    }
    // The one UDM installed (Options > Tools), then the PATH's.
    let own = crate::paths::tools_dir().map(|d| d.join(crate::platform::exe_name("ffmpeg")));
    let path = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default();
    own.into_iter()
        .chain(path.into_iter().map(|dir| dir.join(crate::platform::exe_name("ffmpeg"))))
        .find(|p| p.is_file())
}

/// Give a settings file from before the default exclusion list those
/// addresses, once, after the ones the user added.
fn add_default_exclusions(value: &mut serde_json::Value) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    if map.contains_key("capture_excluded_urls_defaults") {
        return;
    }
    let mut list: Vec<String> = match map.get("capture_excluded_urls") {
        Some(serde_json::Value::Array(old)) => old.iter().filter_map(|u| u.as_str().map(String::from)).collect(),
        _ => Vec::new(),
    };
    add_missing_exclusions(&mut list);
    map.insert("capture_excluded_urls".into(), list.into());
    map.insert("capture_excluded_urls_defaults".into(), true.into());
}

/// Give a settings file from before HDS support `f4m` among its video types,
/// once: those files list the types the video button offers, so without
/// this an upgraded UDM never offers HDS streams.
fn add_hds_type(value: &mut serde_json::Value) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    if map.contains_key("video_types_hds") {
        return;
    }
    if let Some(serde_json::Value::Array(list)) = map.get_mut("video_types") {
        if !list.iter().any(|t| t.as_str().is_some_and(|t| t.eq_ignore_ascii_case("f4m"))) {
            list.push("f4m".into());
        }
    }
    map.insert("video_types_hds".into(), true.into());
}

/// Give a settings file from before each proxy had its own login an HTTPS
/// and a SOCKS login: the shared one, which they used. Returns whether it
/// did.
fn split_proxy_login(value: &mut serde_json::Value) -> bool {
    let Some(map) = value.as_object_mut() else {
        return false;
    };
    if map.contains_key("proxy_https_username") {
        return false;
    }
    let user = match map.get("proxy_username").and_then(|u| u.as_str()) {
        Some(u) if !u.trim().is_empty() => u.to_string(),
        _ => return false,
    };
    for key in ["proxy_https_username", "proxy_socks_username"] {
        map.insert(key.into(), user.clone().into());
    }
    true
}

/// Give a settings file from before Chromium browsers and Firefox had their
/// own context menu lists both lists, filled from its one list.
fn split_browser_menu(value: &mut serde_json::Value) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    let Some(old) = map.remove("browser_menu_hidden") else {
        return;
    };
    for key in ["browser_menu_hidden_chromium", "browser_menu_hidden_firefox"] {
        map.entry(key).or_insert_with(|| old.clone());
    }
}

/// `settings.json` in UDM's settings folder (`%APPDATA%\UDM` on Windows).
pub fn settings_path() -> io::Result<PathBuf> {
    Ok(crate::paths::config_dir()?.join("settings.json"))
}

/// The current user's Downloads folder, or the temp dir if it can't be found.
/// A portable copy downloads into its own `Downloads` folder, on its drive,
/// and a server into the one it was given (`paths::chosen_downloads_dir`).
pub fn default_downloads_dir() -> PathBuf {
    if let Some(dir) = crate::paths::chosen_downloads_dir().or_else(crate::paths::portable_downloads_dir) {
        return dir;
    }
    if let Some(home) = crate::paths::home_dir() {
        let dir = home.join("Downloads");
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

/// The `SettingsChecks` adapter: `validate` and `ffmpeg` above, and
/// `unpack::seven_zip`.
pub struct DiskChecks;

impl udm_application::window::SettingsChecks for DiskChecks {
    fn validate(&self, settings: &Settings) -> Result<(), udm_domain::ports::SettingsError> {
        Ok(validate(settings)?)
    }

    fn ffmpeg(&self, settings: &Settings) -> Option<PathBuf> {
        ffmpeg(settings)
    }

    fn seven_zip(&self, settings: &Settings) -> Option<PathBuf> {
        crate::unpack::seven_zip(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use udm_domain::id::QueueId;

    /// A Windows full path as a full path on this system: `C:\a\b` stays
    /// on Windows and is `/a/b` elsewhere.
    fn full(windows: &str) -> String {
        if cfg!(windows) {
            windows.to_string()
        } else {
            windows[2..].replace('\\', "/")
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("udm_settings_{}_{name}", std::process::id()))
    }

    #[test]
    fn missing_file_gives_defaults() {
        let s = load(&temp_path("missing.json")).unwrap();
        assert_eq!(s, Settings::default());
        assert_eq!(s.connections, DEFAULT_CONNECTIONS);
        assert_eq!(s.max_concurrent, DEFAULT_MAX_CONCURRENT);
        assert_eq!(s.speed_limit_bytes(), None);
    }

    #[test]
    fn save_load_roundtrip() {
        let path = temp_path("roundtrip.json");
        let s = Settings {
            download_dir: Some(full("C:\\Downloads\\UDM")),
            connections: 12,
            max_concurrent: 5,
            speed_limit_enabled: true,
            speed_limit_kbps: 500,
            speed_limit_on_startup: true,
            speed_limit_per_file_kbps: Some(200),
            speed_limit_only_while_active: true,
            speed_limit_idle_minutes: 15,
            resume_at_startup: true,
            file_exists: FileExists::Overwrite,
            if_duplicate: Duplicate::Existing,
            ask_where_to_save: false,
            auto_start_types: vec!["exe".into()],
            start_while_asking: true,
            add_only: true,
            general_remember_folder: true,
            general_last_folder: Some(full("D:\\Misc")),
            later_queue: Some(QueueId::new(4)),
            categories: vec![Category::new("Books", &["epub"])],
            general_extensions: vec!["pdf".into()],
            capture_types: vec!["zip".into()],
            capture_min_size_kb: 100,
            capture_excluded_sites: vec!["*.example.com".into()],
            close_to_tray: false,
            minimize_to_tray: false,
            tray_icon: false,
            categories_panel: false,
            start_with_windows: true,
            keep_awake: false,
            notify: Notify::Dialog,
            sound_on_done: true,
            sound_on_failed: true,
            sound_on_queue_started: true,
            sound_on_queue_done: true,
            sound_done_file: full("C:\\Windows\\Media\\tada.wav"),
            sound_failed_file: String::new(),
            sound_queue_started_file: full("C:\\Windows\\Media\\ding.wav"),
            sound_queue_file: full("C:\\Windows\\Media\\chimes.wav"),
            scan_after_download: true,
            scanner: Scanner::Custom,
            scanner_command: full("C:\\scan.exe") + " {path}",
            proxy_mode: ProxyMode::Manual,
            proxy_http: "proxy:3128".into(),
            proxy_https: String::new(),
            proxy_ftp: "ftpproxy:21".into(),
            proxy_socks5: String::new(),
            proxy_socks_https: true,
            proxy_socks_ftp: false,
            proxy_socks_version: SocksVersion::V4,
            proxy_remote_dns: false,
            proxy_bypass: "<local> *.corp.net".into(),
            proxy_pac_url: "http://wpad.corp.net/proxy.pac".into(),
            ftp_active: true,
            proxy_username: "me".into(),
            proxy_password: None,
            proxy_https_username: "tls".into(),
            proxy_https_password: None,
            proxy_ftp_username: "ftp".into(),
            proxy_ftp_password: None,
            proxy_socks_username: "sock".into(),
            proxy_socks_password: None,
            proxy_logins_split: false,
            use_browser_proxy: true,
            browser_proxy_after_errors: true,
            site_logins: vec![SiteLogin {
                site: "files.example.com".into(),
                username: "ann".into(),
                password: None,
                plain: false,
            }],
            clipboard_monitor: true,
            clipboard_start: true,
            video_button: false,
            video_button_excluded_sites: vec!["video.example.com".into()],
            video_types: vec!["mp4".into(), "m3u8".into()],
            video_types_hds: true,
            video_min_size_kb: 512,
            video_view: PanelView::Mini,
            video_protected_note: false,
            video_auto_capture: true,
            video_auto_types: vec!["mp4".into(), "flv".into()],
            browser_menu_hidden_chromium: vec!["all_links".into()],
            browser_menu_hidden_firefox: vec!["recent_videos".into()],
            added_browsers: vec![AddedBrowser { name: "Thorium".into(), program: full("C:\\Thorium\\thorium.exe") }],
            ffmpeg_path: full("C:\\ffmpeg\\bin\\ffmpeg.exe"),
            seven_zip_path: full("C:\\Program Files\\7-Zip\\7z.exe"),
            unpack_auto: true,
            unpack_to: UnpackTo::Folder,
            unpack_folder: full("C:\\Unpacked"),
            unpack_recycle: true,
            ytdlp_auto_update: true,
            confirm_remove: false,
            remove_deletes_files: true,
            server_file_date: false,
            server_file_date_created: true,
            resume_ignores_date: true,
            manual_user_agent: USER_AGENT_PRESETS[0].1.into(),
            temp_dir: Some(full("D:\\UDM temp")),
            capture_skip_key: CaptureKey::SHIFT,
            capture_force_key: CaptureKey { insert: true, ..CaptureKey::CTRL },
            capture_keys_left_click: true,
            capture_force_skips_pages: false,
            capture_private: true,
            capture_excluded_urls: vec!["https://example.com/files/*".into()],
            capture_excluded_urls_defaults: true,
            offer_exceptions: false,
            selection_button: SelectionButton::Sites,
            selection_button_sites: vec!["*.example.org".into()],
            server_limits: vec![ServerLimit { site: "*.mirror.net".into(), max: 2 }],
            quota_enabled: true,
            quota_mb: 500,
            quota_hours: 6,
            quota_warn: false,
            check_updates: false,
            last_update_check: 1_700_000_000,
            drop_target: true,
            progress_on_start: true,
            progress_speed_tab: false,
            progress_done_tab: false,
            progress_hide_buttons: false,
            progress_start_tab: ProgressTab::WhenDone,
            progress_start_details: true,
            list_queue_panel: false,
            schedule_note_hidden: true,
            grab_add_on_close: true,
            grab_save_unique_silently: true,
            tips_on_startup: false,
            next_tip: 3,
            theme: Theme::Dark,
            text_size: 125,
            language: "de".into(),
            remote_enabled: true,
            remote_port: 20443,
            remote_addresses: vec!["192.168.1.20".into()],
            debrid: udm_domain::debrid::DebridSettings {
                enabled: true,
                service: Some(udm_domain::debrid::DebridService::Premiumize),
                key_service: Some(udm_domain::debrid::DebridService::Premiumize),
            },
            network_adapter: Some(udm_domain::network::AdapterChoice {
                id: udm_domain::network::AdapterId::parse("wg0").unwrap(),
                name: "Work VPN".into(),
                subnets: Vec::new(),
            }),
            dns: udm_domain::network::DnsChoice::Custom(
                udm_domain::network::DohUrl::parse("https://doh.example/dns-query").unwrap(),
            ),
        };
        save(&s, &path).unwrap();
        assert_eq!(load(&path).unwrap(), s);
        assert_eq!(s.speed_limit_bytes(), Some(500 * 1024));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn files_from_before_hds_get_f4m_once() {
        let path = temp_path("hds.json");
        fs::write(&path, br#"{"video_types":["mp4","m3u8"]}"#).unwrap();
        let s = load(&path).unwrap();
        assert_eq!(s.video_types, ["mp4", "m3u8", "f4m"]);
        assert!(s.video_types_hds);
        // Turned off after that, it stays off.
        let off = Settings { video_types: vec!["mp4".into()], ..s };
        save(&off, &path).unwrap();
        assert_eq!(load(&path).unwrap().video_types, ["mp4"]);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn files_from_before_default_exclusions_get_them_once() {
        let path = temp_path("exclusions.json");
        fs::write(&path, br#"{"capture_excluded_urls":["https://dl.net/keep/*","WEB.WHATSAPP.COM/res/*.mp3"]}"#)
            .unwrap();
        let s = load(&path).unwrap();
        assert!(s.capture_excluded_urls_defaults);
        assert_eq!(s.capture_excluded_urls[0], "https://dl.net/keep/*");
        assert_eq!(s.capture_excluded_urls[1], "WEB.WHATSAPP.COM/res/*.mp3");
        assert_eq!(s.capture_excluded_urls.len(), DEFAULT_CAPTURE_EXCLUDED_URLS.len() + 1);
        // Removed after that, they stay removed.
        let cleared = Settings { capture_excluded_urls: vec!["https://dl.net/keep/*".into()], ..s };
        save(&cleared, &path).unwrap();
        assert_eq!(load(&path).unwrap().capture_excluded_urls, ["https://dl.net/keep/*"]);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn invalid_file_is_an_error() {
        let path = temp_path("invalid.json");
        fs::write(&path, b"{ not json").unwrap();
        assert!(load(&path).is_err());
        fs::write(&path, br#"{"connections":99}"#).unwrap();
        assert!(load(&path).is_err());
        fs::remove_file(&path).ok();
    }

    #[test]
    fn validation_of_folder() {
        let relative = Settings { download_dir: Some("Downloads".into()), ..Settings::default() };
        assert!(relative.validate().is_err());

        let empty = Settings { download_dir: Some("  ".into()), ..Settings::default() };
        assert!(empty.validate().is_err());

        let file = temp_path("not-a-dir.txt");
        fs::write(&file, b"x").unwrap();
        let s = Settings { download_dir: Some(file.display().to_string()), ..Settings::default() };
        assert!(validate(&s).is_err());
        fs::remove_file(&file).ok();

        let dir = Settings { download_dir: Some(std::env::temp_dir().display().to_string()), ..Settings::default() };
        assert!(dir.validate().is_ok());
    }

    #[test]
    fn category_folders() {
        let base = std::env::temp_dir().join("udm-dl");
        let mut s = Settings { download_dir: Some(base.display().to_string()), ..Settings::default() };
        let custom = std::env::temp_dir().join("udm-music");
        s.categories[2].folder = Some(custom.display().to_string());
        let f = s.folders(&default_downloads_dir());
        assert_eq!(f.for_name("a.ZIP"), base.join("Compressed"));
        assert_eq!(f.for_name("song.mp3"), custom);
        assert_eq!(f.for_name("noext"), base);
        assert_eq!(f.for_name("photo.jpg"), base);

        let fixed = Folders::fixed(base.join("picked"));
        assert_eq!(fixed.for_name("a.zip"), base.join("picked"));
    }

    #[test]
    fn a_category_can_be_limited_to_some_sites() {
        let base = std::env::temp_dir().join("udm-dl");
        let mut s = Settings { download_dir: Some(base.display().to_string()), ..Settings::default() };
        let mut work = Category::new("Work", &["pdf"]);
        work.sites = vec!["*.corp.example".into()];
        s.categories.insert(0, work);
        assert_eq!(s.category_name("a.pdf", "https://files.corp.example/a.pdf"), "Work");
        assert_eq!(s.category_name("a.pdf", "https://elsewhere.net/a.pdf"), "Documents");
        assert_eq!(
            s.folders_for("https://corp.example/x", &default_downloads_dir()).for_name("a.pdf"),
            base.join("Work")
        );
        assert_eq!(
            s.folders_for("https://elsewhere.net/x", &default_downloads_dir()).for_name("a.pdf"),
            base.join("Documents")
        );
    }

    #[test]
    fn a_category_remembers_the_last_folder_chosen() {
        let mut s = Settings::default();
        let chosen = std::env::temp_dir().join("udm-chosen");
        assert!(!s.remember_folder("Compressed", &chosen), "off by default");
        s.categories[0].remember_folder = true;
        assert!(s.remember_folder("compressed", &chosen));
        assert_eq!(s.folders(&default_downloads_dir()).for_name("b.zip"), chosen);
        assert_eq!(s.category_folder("Compressed", &default_downloads_dir()), chosen);
        assert!(!s.remember_folder("Compressed", &chosen), "unchanged");
    }

    #[test]
    fn each_category_has_a_folder() {
        let mut s = Settings::default();
        let base = std::env::temp_dir().join("udm-base");
        s.download_dir = Some(base.display().to_string());
        s.categories[1].folder = Some("D:\\Docs".into());
        assert_eq!(s.category_folder("Music", &default_downloads_dir()), base.join("Music"));
        assert_eq!(s.category_folder("documents", &default_downloads_dir()), PathBuf::from("D:\\Docs"));
        assert_eq!(s.category_folder(GENERAL, &default_downloads_dir()), base);
        assert_eq!(s.category_folder("No such", &default_downloads_dir()), base);
    }

    #[test]
    fn general_can_remember_its_folder_too() {
        let mut s = Settings::default();
        let base = std::env::temp_dir().join("udm-base");
        s.download_dir = Some(base.display().to_string());
        let chosen = std::env::temp_dir().join("udm-general");
        assert!(!s.remember_folder(GENERAL, &chosen), "off by default");
        assert!(s.set_remember_folder(GENERAL, true));
        assert!(s.remembers_folder(GENERAL));
        assert!(s.remember_folder(GENERAL, &chosen));
        assert_eq!(s.folders(&default_downloads_dir()).for_name("notes.xyz"), chosen);
        assert_eq!(s.category_folder(GENERAL, &default_downloads_dir()), chosen);
        // The other categories keep their folders inside the download folder.
        assert_eq!(s.folders(&default_downloads_dir()).for_name("a.zip"), base.join("Compressed"));
        assert!(s.set_remember_folder(GENERAL, false));
        assert_eq!(s.folders(&default_downloads_dir()).for_name("notes.xyz"), base);
    }

    #[test]
    fn an_old_shared_proxy_login_goes_to_https_and_socks() {
        let path = temp_path("old-proxy.json");
        fs::write(&path, br#"{"proxy_username":"me","proxy_http":"p:1"}"#).unwrap();
        let s = load(&path).unwrap();
        assert!(s.proxy_logins_split);
        assert_eq!((s.proxy_https_username.as_str(), s.proxy_socks_username.as_str()), ("me", "me"));
        assert_eq!(s.proxy_ftp_username, "", "the FTP proxy never had a login");
        // Saved again, the file has its own logins and is left alone.
        save(&s, &path).unwrap();
        let again = load(&path).unwrap();
        assert!(!again.proxy_logins_split);
        fs::write(&path, br#"{"proxy_http":"p:1"}"#).unwrap();
        assert!(!load(&path).unwrap().proxy_logins_split, "no login to split");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn one_context_menu_list_becomes_one_per_browser_family() {
        let path = std::env::temp_dir().join(format!("udm-menu-{}.json", std::process::id()));
        fs::write(&path, br#"{"browser_menu_hidden":["all_links","last_video"]}"#).unwrap();
        let s = load(&path).unwrap();
        assert_eq!(s.browser_menu_hidden_chromium, ["all_links", "last_video"]);
        assert_eq!(s.browser_menu_hidden_firefox, ["all_links", "last_video"]);
        // A newer file keeps its own lists.
        fs::write(&path, br#"{"browser_menu_hidden_firefox":["download"]}"#).unwrap();
        let s = load(&path).unwrap();
        assert!(s.browser_menu_hidden_chromium.is_empty());
        assert_eq!(s.browser_menu_hidden_firefox, ["download"]);
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains(r#""browser_menu_hidden""#), "{json}");
        fs::remove_file(&path).ok();
        let bad = Settings { browser_menu_hidden_firefox: vec!["nope".into()], ..Settings::default() };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn general_has_types_of_its_own() {
        let mut s = Settings { download_dir: Some(full("D:\\dl")), ..Settings::default() };
        assert_eq!(s.category_name("a.pdf", ""), "Documents");
        s.general_extensions = vec!["pdf".into(), "iso".into()];
        assert_eq!(s.category_name("a.pdf", ""), GENERAL, "General's list wins over a category's");
        assert_eq!(s.category_name("a.iso", ""), GENERAL);
        assert_eq!(s.category_name("a.zip", ""), "Compressed");
        let folders = s.folders(&default_downloads_dir());
        assert_eq!(folders.for_name("a.pdf"), PathBuf::from(full("D:\\dl")));
        assert_eq!(folders.for_name("a.zip"), PathBuf::from(full("D:\\dl")).join("Compressed"));
        assert!(s.validate().is_ok());
        s.general_extensions = vec![".bad".into()];
        assert!(s.validate().is_err());
    }
}
