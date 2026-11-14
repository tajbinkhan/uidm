//! The Options window's draft: made from the settings, and turned back into
//! settings when the window is saved. Settings the window does not show
//! keep their values. Plain functions, tested without Tauri.

use udm_application::daemon::Command;
use udm_domain::debrid::{DebridKey, DebridService};
use udm_domain::id::QueueId;
use udm_domain::network::{AdapterChoice, AdapterId, DnsChoice, DohUrl};
use udm_domain::settings::{
    default_capture_types, parse_extensions, CaptureKey, Category, Duplicate, InvalidSettings, Notify, PanelView,
    ProxyMode, Scanner, ServerLimit, Settings, SiteLogin, SocksVersion, Theme, BROWSER_MENU_ITEMS, CONNECTIONS_RANGE,
    GENERAL, SERVER_CONNECTIONS_RANGE,
};
use udm_domain::settings::{TorrentSettings, MAX_TORRENT_PEERS};
use udm_domain::theme::ThemeName;
use udm_domain::torrent::{Ratio, SeedLimits, DEFAULT_SEED_MINUTES, DEFAULT_SEED_RATIO};

use crate::contract::SystemProxyView;
use crate::contract::{AdapterDraft, DnsDraft, DnsRow, NetworkView};
use crate::contract::{
    CategoryDraft, Choice, DebridChoice, DebridServiceRow, DebridView, DuplicateChoice, ExtrasDraft, FileExistsChoice,
    KeysDraft, LoginDraft, NotifyChoice, OptionsDraft, OptionsView, ProxyChoice, ProxyDraft, SelectionChoice,
    ServerLimitDraft, SoundDraft, StartTabChoice, UnpackChoice, VideoTypeDraft,
};
use crate::i18n::{Language, LANGUAGES};
use udm_domain::settings::{FileExists, ProgressTab, SelectionButton, UnpackTo};

/// Why the window's settings cannot be saved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OptionsError {
    #[error(transparent)]
    Invalid(#[from] InvalidSettings),
    #[error("Type a name for each category.")]
    CategoryName,
    #[error("There are two categories named \"{0}\".")]
    CategoryTwice(String),
    #[error("\"{0}\" is not a theme.")]
    Theme(String),
    #[error("Type a port from 1 to 65535.")]
    Port,
    #[error("Choose the debrid service the account is with.")]
    DebridService,
    /// The custom DNS over HTTPS address, or the adapter, is not one.
    #[error(transparent)]
    Network(#[from] udm_domain::network::NetworkError),
    #[error("Type a number of peers from 1 to 1000.")]
    TorrentPeers,
    #[error("Type an upload limit from 1 KB/s up.")]
    UploadLimit,
    /// The seeding ratio is not one.
    #[error(transparent)]
    Torrent(#[from] udm_domain::torrent::TorrentError),
}

/// The debrid service a choice names.
pub fn debrid_service(c: DebridChoice) -> DebridService {
    match c {
        DebridChoice::RealDebrid => DebridService::RealDebrid,
        DebridChoice::AllDebrid => DebridService::AllDebrid,
        DebridChoice::Premiumize => DebridService::Premiumize,
    }
}

fn debrid_choice(s: DebridService) -> DebridChoice {
    match s {
        DebridService::RealDebrid => DebridChoice::RealDebrid,
        DebridService::AllDebrid => DebridChoice::AllDebrid,
        DebridService::Premiumize => DebridChoice::Premiumize,
    }
}

/// What OK sends the daemon for the debrid key, after the settings: a key
/// typed is stored for the chosen service (`settings` refuses one without
/// a service), the remove box removes the stored key, and otherwise the
/// key is left alone.
pub fn debrid_key(d: &OptionsDraft) -> Option<Command> {
    let e = &d.extras;
    match (DebridKey::new(&e.debrid_key), e.debrid_service) {
        (Some(key), Some(service)) => Some(Command::SetDebridKey { service: debrid_service(service), key }),
        _ if e.debrid_forget => Some(Command::RemoveDebridKey),
        _ => None,
    }
}

/// The debrid services Options lists.
fn debrid_view(s: &Settings) -> DebridView {
    DebridView {
        services: DebridService::ALL
            .into_iter()
            .map(|service| DebridServiceRow {
                service: debrid_choice(service),
                name: service.name().to_string(),
                key_page: service.key_page().to_string(),
            })
            .collect(),
        key_service: s.debrid.key_service.map(debrid_choice),
        checking: false,
        check: None,
    }
}

fn keys(k: CaptureKey) -> KeysDraft {
    KeysDraft { on: !k.is_none(), alt: k.alt, shift: k.shift, ctrl: k.ctrl, insert: k.insert, delete: k.delete }
}

fn key_of(d: KeysDraft) -> CaptureKey {
    if !d.on {
        return CaptureKey::NONE;
    }
    CaptureKey { alt: d.alt, shift: d.shift, ctrl: d.ctrl, insert: d.insert, delete: d.delete }
}

/// `host:port` as its host and port; a port only when it is digits.
fn split_address(text: &str) -> (String, String) {
    let text = text.trim();
    match text.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) && !host.ends_with(':') => {
            (host.to_string(), port.to_string())
        }
        _ => (text.to_string(), String::new()),
    }
}

fn join_address(host: &str, port: &str) -> String {
    let (host, port) = (host.trim(), port.trim());
    match (host.is_empty(), port.is_empty()) {
        (true, _) => String::new(),
        (false, true) => host.to_string(),
        (false, false) => format!("{host}:{port}"),
    }
}

fn words(list: &[String]) -> String {
    list.join(" ")
}

/// Sites separated by spaces, commas or lines, lowercase.
fn sites(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in text.split(|c: char| c == ',' || c == ';' || c.is_whitespace()) {
        let s = s.trim().to_ascii_lowercase();
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

fn theme_key(t: &Theme) -> String {
    match t {
        Theme::System => "system".into(),
        Theme::Light => "light".into(),
        Theme::Dark => "dark".into(),
        Theme::HighContrast => "highContrast".into(),
        Theme::Custom(name) => format!("custom:{}", name.as_str()),
    }
}

fn theme_of(key: &str) -> Result<Theme, OptionsError> {
    Ok(match key {
        "system" => Theme::System,
        "light" => Theme::Light,
        "dark" => Theme::Dark,
        "highContrast" => Theme::HighContrast,
        other => match other.strip_prefix("custom:").map(ThemeName::parse) {
            Some(Ok(name)) => Theme::Custom(name),
            _ => return Err(OptionsError::Theme(other.to_string())),
        },
    })
}

/// What Get System says when the system sets no proxy.
pub const NO_SYSTEM_PROXY: &str = if cfg!(windows) {
    "Windows has no proxy server turned on."
} else {
    "No proxy is set in http_proxy, https_proxy or all_proxy."
};

/// The system's proxies as Get System fills them in: HTTP, HTTPS, FTP and
/// SOCKS, each on when the system names one, and the addresses that go
/// without. `None` when the system sets no proxy.
pub fn system_proxy(found: Option<&udm_application::window::SystemProxy>) -> Option<SystemProxyView> {
    let p = found?;
    let draft = |address: Option<&String>| {
        let text = address.map(|a| a.split_once("://").map_or(a.as_str(), |(_, rest)| rest)).unwrap_or_default();
        let (host, port) = split_address(text);
        ProxyDraft { on: !host.is_empty(), host, port, user: String::new(), password: String::new() }
    };
    let proxies = vec![draft(p.http.as_ref()), draft(p.https.as_ref()), draft(p.ftp.as_ref()), draft(p.socks.as_ref())];
    if proxies.iter().all(|d| !d.on) {
        return None;
    }
    Some(SystemProxyView { proxies, bypass: p.bypass.join("; ") })
}

/// The draft the window starts from.
pub fn draft(s: &Settings) -> OptionsDraft {
    let proxy = |address: &str, user: &str| {
        let (host, port) = split_address(address);
        ProxyDraft { on: !host.is_empty(), host, port, user: user.to_string(), password: String::new() }
    };
    let shown = |hidden: &[String]| {
        BROWSER_MENU_ITEMS
            .iter()
            .filter(|(id, _)| !hidden.iter().any(|h| h == id))
            .map(|(id, _)| id.to_string())
            .collect()
    };
    let mut video_types: Vec<VideoTypeDraft> = udm_domain::settings::default_video_types()
        .into_iter()
        .map(|ext| VideoTypeDraft { on: s.video_types.contains(&ext), ext })
        .collect();
    for t in &s.video_types {
        if !video_types.iter().any(|v| &v.ext == t) {
            video_types.push(VideoTypeDraft { ext: t.clone(), on: true });
        }
    }
    OptionsDraft {
        start_with_system: s.start_with_windows,
        tray_icon: s.tray_icon,
        close_to_tray: s.close_to_tray,
        minimize_to_tray: s.minimize_to_tray,
        keep_awake: s.keep_awake,
        check_updates: s.check_updates,
        clipboard_monitor: s.clipboard_monitor,
        clipboard_start: s.clipboard_start,
        capture_private: s.capture_private,
        theme: theme_key(&s.theme),
        language: s.language.clone(),
        skip_key: keys(s.capture_skip_key),
        force_key: keys(s.capture_force_key),
        keys_left_click: s.capture_keys_left_click,
        force_skips_pages: s.capture_force_skips_pages,
        menu_chromium: shown(&s.browser_menu_hidden_chromium),
        menu_firefox: shown(&s.browser_menu_hidden_firefox),
        video_button: s.video_button,
        video_mini: s.video_view == PanelView::Mini,
        video_types,
        video_auto_capture: s.video_auto_capture,
        video_protected_note: s.video_protected_note,
        capture_types: words(&s.capture_types).to_uppercase(),
        capture_excluded_sites: words(&s.capture_excluded_sites),
        capture_excluded_urls: s.capture_excluded_urls.clone(),
        offer_exceptions: s.offer_exceptions,
        download_dir: s.download_dir.clone().unwrap_or_default(),
        general_remember_folder: s.general_remember_folder,
        categories: s
            .categories
            .iter()
            .filter(|c| c.name != GENERAL)
            .map(|c| CategoryDraft {
                original: Some(c.name.clone()),
                name: c.name.clone(),
                extensions: words(&c.extensions),
                sites_on: !c.sites.is_empty(),
                sites: words(&c.sites),
                folder: c.folder.clone().unwrap_or_default(),
                remember_folder: c.remember_folder,
            })
            .collect(),
        server_file_date: s.server_file_date,
        temp_dir: s.temp_dir.clone().unwrap_or_default(),
        ask_where_to_save: s.ask_where_to_save,
        add_only: s.add_only,
        done_dialog: s.notify == Notify::Dialog,
        start_while_asking: s.start_while_asking,
        later_panel: s.later_queue.is_none(),
        list_queue_panel: s.list_queue_panel,
        resume_ignores_date: s.resume_ignores_date,
        if_duplicate: match s.if_duplicate {
            Duplicate::Ask => DuplicateChoice::Ask,
            Duplicate::NewName => DuplicateChoice::NewName,
            Duplicate::Overwrite => DuplicateChoice::Overwrite,
            Duplicate::Existing => DuplicateChoice::Existing,
        },
        user_agent: s.manual_user_agent.clone(),
        progress_on_start: s.progress_on_start,
        progress_speed_tab: s.progress_speed_tab,
        progress_done_tab: s.progress_done_tab,
        progress_hide_buttons: s.progress_hide_buttons,
        progress_start_details: s.progress_start_details,
        scan_after_download: s.scan_after_download,
        scanner_custom: s.scanner == Scanner::Custom,
        scanner_command: s.scanner_command.clone(),
        connections: s.connections as u32,
        server_limits: s
            .server_limits
            .iter()
            .map(|l| ServerLimitDraft { site: l.site.clone(), max: l.max as u32 })
            .collect(),
        quota_enabled: s.quota_enabled,
        quota_mb: s.quota_mb.min(u64::from(u32::MAX)) as u32,
        quota_hours: s.quota_hours.min(u64::from(u32::MAX)) as u32,
        quota_warn: s.quota_warn,
        proxy_mode: match s.proxy_mode {
            ProxyMode::None => ProxyChoice::None,
            ProxyMode::System => ProxyChoice::System,
            ProxyMode::Script => ProxyChoice::Script,
            ProxyMode::Manual => ProxyChoice::Manual,
        },
        browser_proxy: s.use_browser_proxy,
        pac_url: s.proxy_pac_url.clone(),
        proxies: vec![
            proxy(&s.proxy_http, &s.proxy_username),
            proxy(&s.proxy_https, &s.proxy_https_username),
            proxy(&s.proxy_ftp, &s.proxy_ftp_username),
            proxy(&s.proxy_socks5, &s.proxy_socks_username),
        ],
        socks4: s.proxy_socks_version == SocksVersion::V4,
        passive_ftp: !s.ftp_active,
        logins: s
            .site_logins
            .iter()
            .map(|l| LoginDraft {
                site: l.site.clone(),
                user: l.username.clone(),
                password: String::new(),
                stored: true,
            })
            .collect(),
        sounds: vec![
            SoundDraft { on: s.sound_on_done, file: s.sound_done_file.clone() },
            SoundDraft { on: s.sound_on_failed, file: s.sound_failed_file.clone() },
            SoundDraft { on: s.sound_on_queue_started, file: s.sound_queue_started_file.clone() },
            SoundDraft { on: s.sound_on_queue_done, file: s.sound_queue_file.clone() },
        ],
        extras: extras(s),
    }
}

fn kb(n: u64) -> u32 {
    n.min(u64::from(u32::MAX)) as u32
}

fn extras(s: &Settings) -> ExtrasDraft {
    let t = &s.torrents;
    ExtrasDraft {
        max_concurrent: s.max_concurrent as u32,
        limit_on: s.speed_limit_enabled,
        limit_kb: kb(s.speed_limit_kbps),
        limit_on_startup: s.speed_limit_on_startup,
        per_file_on: s.speed_limit_per_file_kbps.is_some(),
        per_file_kb: kb(s.speed_limit_per_file_kbps.unwrap_or(s.speed_limit_kbps.max(1))),
        limit_while_active: s.speed_limit_only_while_active,
        idle_minutes: s.speed_limit_idle_minutes,
        notify: match s.notify {
            Notify::Off => NotifyChoice::Off,
            Notify::Toast => NotifyChoice::Toast,
            Notify::Dialog => NotifyChoice::Dialog,
        },
        file_exists: match s.file_exists {
            FileExists::Rename => FileExistsChoice::Rename,
            FileExists::Overwrite => FileExistsChoice::Overwrite,
            FileExists::Ask => FileExistsChoice::Ask,
        },
        auto_start_types: s.auto_start_types.join(", "),
        resume_at_startup: s.resume_at_startup,
        server_file_date_created: s.server_file_date_created,
        progress_start_tab: match s.progress_start_tab {
            ProgressTab::Status => StartTabChoice::Status,
            ProgressTab::SpeedLimit => StartTabChoice::SpeedLimit,
            ProgressTab::WhenDone => StartTabChoice::WhenDone,
        },
        confirm_remove: s.confirm_remove,
        remove_deletes_files: s.remove_deletes_files,
        text_size: s.text_size,
        capture_min_size_kb: kb(s.capture_min_size_kb),
        selection_button: match s.selection_button {
            SelectionButton::Off => SelectionChoice::Off,
            SelectionButton::Everywhere => SelectionChoice::Everywhere,
            SelectionButton::Sites => SelectionChoice::Sites,
        },
        selection_sites: s.selection_button_sites.join("\n"),
        video_excluded_sites: s.video_button_excluded_sites.join("\n"),
        video_min_size_kb: kb(s.video_min_size_kb),
        video_auto_types: s.video_auto_types.join(", "),
        socks_https: s.proxy_socks_https,
        socks_ftp: s.proxy_socks_ftp,
        remote_dns: s.proxy_remote_dns,
        proxy_bypass: s.proxy_bypass.clone(),
        browser_proxy_after_errors: s.browser_proxy_after_errors,
        unpack_auto: s.unpack_auto,
        unpack_to: match s.unpack_to {
            UnpackTo::Subfolder => UnpackChoice::Subfolder,
            UnpackTo::Beside => UnpackChoice::Beside,
            UnpackTo::Folder => UnpackChoice::Folder,
        },
        unpack_folder: s.unpack_folder.clone(),
        unpack_recycle: s.unpack_recycle,
        seven_zip_path: s.seven_zip_path.clone(),
        ffmpeg_path: s.ffmpeg_path.clone(),
        ytdlp_auto_update: s.ytdlp_auto_update,
        remote_enabled: s.remote_enabled,
        remote_port: u32::from(s.remote_port),
        remote_all: s.remote_addresses.is_empty(),
        remote_addresses: s.remote_addresses.join(", "),
        debrid_on: s.debrid.enabled,
        debrid_service: s.debrid.service.map(debrid_choice),
        debrid_key: String::new(),
        debrid_forget: false,
        adapter: s.network_adapter.as_ref().map(|a| AdapterDraft {
            id: a.id.to_string(),
            name: a.name.clone(),
            renew: false,
        }),
        dns: dns_draft(&s.dns),
        dns_url: match &s.dns {
            DnsChoice::Custom(url) => url.as_str().to_string(),
            _ => String::new(),
        },
        torrent_port_fixed: t.listen_port().is_some(),
        torrent_port: u32::from(t.listen_port().unwrap_or(DEFAULT_TORRENT_PORT)),
        torrent_dht: t.dht,
        torrent_upnp: t.upnp,
        torrent_local_peers: t.local_peers,
        torrent_peers_on: t.max_peers.is_some(),
        torrent_peers: t.peers().unwrap_or(DEFAULT_TORRENT_PEERS),
        torrent_upload_on: t.upload_limit().is_some(),
        torrent_upload_kb: t.upload_limit_kbps.filter(|k| *k > 0).map_or(100, |k| u32::try_from(k).unwrap_or(u32::MAX)),
        seed_ratio_on: t.seed_limits.ratio.is_some(),
        seed_ratio: t.seed_limits.ratio.unwrap_or(DEFAULT_SEED_RATIO).get(),
        seed_minutes_on: t.seed_limits.minutes.is_some(),
        seed_minutes: t.seed_limits.minutes.unwrap_or(DEFAULT_SEED_MINUTES),
        catch_torrent_files: t.catch_torrent_files,
    }
}

/// The port Options offers when the torrents' port is first fixed: the
/// one BitTorrent clients have long used.
const DEFAULT_TORRENT_PORT: u16 = 6881;
/// The peers per torrent Options offers when the limit is first set:
/// librqbit's own.
const DEFAULT_TORRENT_PEERS: u32 = 128;

/// The torrent settings the extras give, checked.
fn torrent_settings(e: &ExtrasDraft) -> Result<TorrentSettings, OptionsError> {
    let port = if e.torrent_port_fixed {
        Some(u16::try_from(e.torrent_port).ok().filter(|p| *p > 0).ok_or(OptionsError::Port)?)
    } else {
        None
    };
    if e.torrent_peers_on && !(1..=MAX_TORRENT_PEERS).contains(&e.torrent_peers) {
        return Err(OptionsError::TorrentPeers);
    }
    if e.torrent_upload_on && e.torrent_upload_kb == 0 {
        return Err(OptionsError::UploadLimit);
    }
    let ratio = if e.seed_ratio_on { Some(Ratio::new(e.seed_ratio)?) } else { None };
    Ok(TorrentSettings {
        port,
        dht: e.torrent_dht,
        upnp: e.torrent_upnp,
        local_peers: e.torrent_local_peers,
        max_peers: e.torrent_peers_on.then_some(e.torrent_peers),
        upload_limit_kbps: e.torrent_upload_on.then_some(u64::from(e.torrent_upload_kb)),
        seed_limits: SeedLimits { ratio, minutes: e.seed_minutes_on.then_some(e.seed_minutes) },
        catch_torrent_files: e.catch_torrent_files,
    })
}

fn dns_draft(d: &DnsChoice) -> DnsDraft {
    match d {
        DnsChoice::System => DnsDraft::System,
        DnsChoice::Cloudflare => DnsDraft::Cloudflare,
        DnsChoice::Google => DnsDraft::Google,
        DnsChoice::Quad9 => DnsDraft::Quad9,
        DnsChoice::Custom(_) => DnsDraft::Custom,
    }
}

/// The resolvers Options offers: the system's, the presets by their own
/// names, and one the user names.
fn network_view(lang: &'static Language) -> NetworkView {
    let mut resolvers =
        vec![DnsRow { value: DnsDraft::System, label: lang.tr("The system's (not encrypted)").to_string() }];
    resolvers.extend(DnsChoice::PRESETS.iter().map(|d| DnsRow { value: dns_draft(d), label: d.name().to_string() }));
    resolvers.push(DnsRow { value: DnsDraft::Custom, label: lang.tr("Another DNS over HTTPS server").to_string() });
    NetworkView { adapters: None, adapters_error: None, resolvers }
}

/// The extras' values on `s`.
fn apply_extras(e: &ExtrasDraft, done_dialog: bool, current: &Settings, s: &mut Settings) -> Result<(), OptionsError> {
    s.max_concurrent = e.max_concurrent as usize;
    s.speed_limit_enabled = e.limit_on;
    s.speed_limit_kbps = u64::from(e.limit_kb);
    s.speed_limit_on_startup = e.limit_on_startup;
    s.speed_limit_per_file_kbps = e.per_file_on.then_some(u64::from(e.per_file_kb));
    s.speed_limit_only_while_active = e.limit_while_active;
    s.speed_limit_idle_minutes = e.idle_minutes;
    // Downloads' "Show download complete dialog" and the notification
    // choice set one setting: the one the user changed wins.
    let notify = match e.notify {
        NotifyChoice::Off => Notify::Off,
        NotifyChoice::Toast => Notify::Toast,
        NotifyChoice::Dialog => Notify::Dialog,
    };
    s.notify = if notify != current.notify {
        notify
    } else {
        match (done_dialog, current.notify) {
            (true, _) => Notify::Dialog,
            (false, Notify::Dialog) => Notify::Toast,
            (false, other) => other,
        }
    };
    s.file_exists = match e.file_exists {
        FileExistsChoice::Rename => FileExists::Rename,
        FileExistsChoice::Overwrite => FileExists::Overwrite,
        FileExistsChoice::Ask => FileExists::Ask,
    };
    s.auto_start_types = parse_extensions(&e.auto_start_types);
    s.resume_at_startup = e.resume_at_startup;
    s.server_file_date_created = e.server_file_date_created;
    s.progress_start_tab = match e.progress_start_tab {
        StartTabChoice::Status => ProgressTab::Status,
        StartTabChoice::SpeedLimit => ProgressTab::SpeedLimit,
        StartTabChoice::WhenDone => ProgressTab::WhenDone,
    };
    s.confirm_remove = e.confirm_remove;
    s.remove_deletes_files = e.remove_deletes_files;
    s.text_size = e.text_size;
    s.capture_min_size_kb = u64::from(e.capture_min_size_kb);
    s.selection_button = match e.selection_button {
        SelectionChoice::Off => SelectionButton::Off,
        SelectionChoice::Everywhere => SelectionButton::Everywhere,
        SelectionChoice::Sites => SelectionButton::Sites,
    };
    s.selection_button_sites = sites(&e.selection_sites);
    s.video_button_excluded_sites = sites(&e.video_excluded_sites);
    s.video_min_size_kb = u64::from(e.video_min_size_kb);
    s.video_auto_types = parse_extensions(&e.video_auto_types);
    s.proxy_socks_https = e.socks_https;
    s.proxy_socks_ftp = e.socks_ftp;
    s.proxy_remote_dns = e.remote_dns;
    s.proxy_bypass = e.proxy_bypass.trim().to_string();
    s.browser_proxy_after_errors = e.browser_proxy_after_errors;
    s.unpack_auto = e.unpack_auto;
    s.unpack_to = match e.unpack_to {
        UnpackChoice::Subfolder => UnpackTo::Subfolder,
        UnpackChoice::Beside => UnpackTo::Beside,
        UnpackChoice::Folder => UnpackTo::Folder,
    };
    s.unpack_folder = e.unpack_folder.trim().to_string();
    s.unpack_recycle = e.unpack_recycle;
    s.seven_zip_path = e.seven_zip_path.trim().to_string();
    s.ffmpeg_path = e.ffmpeg_path.trim().to_string();
    s.ytdlp_auto_update = e.ytdlp_auto_update;
    s.remote_enabled = e.remote_enabled;
    s.remote_port = u16::try_from(e.remote_port).map_err(|_| OptionsError::Port)?;
    s.remote_addresses = if e.remote_all {
        Vec::new()
    } else {
        e.remote_addresses
            .split([',', ' ', ';', '\n'])
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(str::to_string)
            .collect()
    };
    // Whether a key is stored stays the daemon's to say.
    s.debrid.enabled = e.debrid_on;
    s.debrid.service = e.debrid_service.map(debrid_service);
    let typed = DebridKey::new(&e.debrid_key).is_some();
    if (s.debrid.enabled || typed) && s.debrid.service.is_none() {
        return Err(OptionsError::DebridService);
    }
    s.network_adapter = match &e.adapter {
        None => None,
        // The daemon keeps the networks a numbered adapter was on.
        Some(a) => {
            Some(AdapterChoice { id: AdapterId::parse(&a.id)?, name: a.name.trim().to_string(), subnets: Vec::new() })
        }
    };
    s.dns = match e.dns {
        DnsDraft::System => DnsChoice::System,
        DnsDraft::Cloudflare => DnsChoice::Cloudflare,
        DnsDraft::Google => DnsChoice::Google,
        DnsDraft::Quad9 => DnsChoice::Quad9,
        DnsDraft::Custom => DnsChoice::Custom(DohUrl::parse(&e.dns_url)?),
    };
    s.torrents = torrent_settings(e)?;
    Ok(())
}

/// `s` with the adapter the user chose again (`AdapterDraft::renew`) taken
/// as it is connected now, among `adapters`: the networks it is on are the
/// ones to keep from now on.
pub fn renew_adapter(d: &OptionsDraft, s: &mut Settings, adapters: &[udm_domain::network::NetworkAdapter]) {
    let Some(drafted) = d.extras.adapter.as_ref().filter(|a| a.renew) else {
        return;
    };
    let Some(choice) = s.network_adapter.as_mut().filter(|c| c.id.as_str() == drafted.id.trim()) else {
        return;
    };
    if let Some(adapter) = adapters.iter().find(|a| a.id == choice.id) {
        choice.subnets = AdapterChoice::of(adapter).subnets;
    }
}

/// The Options window's view.
pub fn view(lang: &'static Language, s: &Settings) -> OptionsView {
    let choice = |value: &str, label: &str| Choice { value: value.to_string(), label: lang.tr(label).to_string() };
    let mut themes = vec![
        choice("system", "System"),
        choice("light", "Light"),
        choice("dark", "Dark"),
        choice("highContrast", "High contrast"),
    ];
    if let Theme::Custom(name) = &s.theme {
        themes.push(Choice { value: theme_key(&s.theme), label: name.as_str().to_string() });
    }
    let mut languages = vec![choice("", "System default")];
    languages.extend(LANGUAGES.iter().map(|l| Choice { value: l.code.to_string(), label: l.name.to_string() }));
    let span = |(lo, hi): (usize, usize)| {
        [1u32, 2, 4, 8, 16, 24, 32].into_iter().filter(move |n| (lo..=hi).contains(&(*n as usize))).collect()
    };
    OptionsView {
        draft: draft(s),
        themes,
        languages,
        menu_items: BROWSER_MENU_ITEMS.iter().map(|(id, label)| choice(id, label)).collect(),
        default_capture_types: words(&default_capture_types()).to_uppercase(),
        default_excluded_sites: words(&Settings::default().capture_excluded_sites),
        connections: span(CONNECTIONS_RANGE),
        server_connections: span(SERVER_CONNECTIONS_RANGE),
        tools: Vec::new(),
        tool_note: None,
        remote: None,
        browsers: None,
        chrome_note: String::new(),
        browser_note: None,
        theme_notes: Vec::new(),
        creation_time: false,
        portable: false,
        debrid: debrid_view(s),
        network: network_view(lang),
    }
}

/// `current` with the draft's values, checked as the daemon checks them.
pub fn settings(d: &OptionsDraft, current: &Settings) -> Result<Settings, OptionsError> {
    let optional = |text: &str| Some(text.trim().to_string()).filter(|t| !t.is_empty());
    let mut s = current.clone();
    s.start_with_windows = d.start_with_system;
    s.tray_icon = d.tray_icon;
    s.close_to_tray = d.close_to_tray;
    s.minimize_to_tray = d.minimize_to_tray;
    s.keep_awake = d.keep_awake;
    s.check_updates = d.check_updates;
    s.clipboard_monitor = d.clipboard_monitor;
    s.clipboard_start = d.clipboard_start;
    s.capture_private = d.capture_private;
    s.theme = theme_of(&d.theme)?;
    s.language = d.language.trim().to_string();
    s.capture_skip_key = key_of(d.skip_key);
    s.capture_force_key = key_of(d.force_key);
    s.capture_keys_left_click = d.keys_left_click;
    s.capture_force_skips_pages = d.force_skips_pages;
    let hidden = |shown: &[String]| {
        BROWSER_MENU_ITEMS
            .iter()
            .filter(|(id, _)| !shown.iter().any(|s| s == id))
            .map(|(id, _)| id.to_string())
            .collect()
    };
    s.browser_menu_hidden_chromium = hidden(&d.menu_chromium);
    s.browser_menu_hidden_firefox = hidden(&d.menu_firefox);
    s.video_button = d.video_button;
    s.video_view = if d.video_mini { PanelView::Mini } else { PanelView::Full };
    s.video_types = d.video_types.iter().filter(|v| v.on).map(|v| v.ext.trim().to_ascii_lowercase()).collect();
    s.video_auto_capture = d.video_auto_capture;
    s.video_protected_note = d.video_protected_note;
    s.capture_types = parse_extensions(&d.capture_types);
    s.capture_excluded_sites = sites(&d.capture_excluded_sites);
    s.capture_excluded_urls =
        d.capture_excluded_urls.iter().map(|u| u.trim().to_string()).filter(|u| !u.is_empty()).collect();
    s.offer_exceptions = d.offer_exceptions;
    s.download_dir = optional(&d.download_dir);
    s.general_remember_folder = d.general_remember_folder;
    if !d.general_remember_folder {
        s.general_last_folder = None;
    }
    // A General entry, which the window does not list, stays as it is.
    let mut categories: Vec<Category> = current.categories.iter().filter(|c| c.name == GENERAL).cloned().collect();
    for c in &d.categories {
        let name = c.name.trim().to_string();
        if name.is_empty() {
            return Err(OptionsError::CategoryName);
        }
        if categories.iter().any(|k| k.name.eq_ignore_ascii_case(&name)) || name.eq_ignore_ascii_case(GENERAL) {
            return Err(OptionsError::CategoryTwice(name));
        }
        let before = c.original.as_deref().and_then(|o| current.category(o));
        categories.push(Category {
            name,
            extensions: parse_extensions(&c.extensions),
            folder: optional(&c.folder),
            sites: if c.sites_on { sites(&c.sites) } else { Vec::new() },
            remember_folder: c.remember_folder,
            last_folder: before.and_then(|b| b.last_folder.clone()).filter(|_| c.remember_folder),
        });
    }
    s.categories = categories;
    s.server_file_date = d.server_file_date;
    s.temp_dir = optional(&d.temp_dir);
    s.ask_where_to_save = d.ask_where_to_save;
    s.add_only = d.add_only;
    s.start_while_asking = d.start_while_asking;
    s.later_queue = match (d.later_panel, current.later_queue) {
        (true, _) => None,
        (false, Some(q)) => Some(q),
        (false, None) => Some(QueueId::MAIN),
    };
    s.list_queue_panel = d.list_queue_panel;
    s.resume_ignores_date = d.resume_ignores_date;
    s.if_duplicate = match d.if_duplicate {
        DuplicateChoice::Ask => Duplicate::Ask,
        DuplicateChoice::NewName => Duplicate::NewName,
        DuplicateChoice::Overwrite => Duplicate::Overwrite,
        DuplicateChoice::Existing => Duplicate::Existing,
    };
    s.manual_user_agent = d.user_agent.trim().to_string();
    s.progress_on_start = d.progress_on_start;
    s.progress_speed_tab = d.progress_speed_tab;
    s.progress_done_tab = d.progress_done_tab;
    s.progress_hide_buttons = d.progress_hide_buttons;
    s.progress_start_details = d.progress_start_details;
    s.scan_after_download = d.scan_after_download;
    s.scanner = if d.scanner_custom { Scanner::Custom } else { Scanner::Defender };
    s.scanner_command = d.scanner_command.trim().to_string();
    s.connections = d.connections as usize;
    s.server_limits = d
        .server_limits
        .iter()
        .map(|l| ServerLimit { site: l.site.trim().to_ascii_lowercase(), max: l.max as usize })
        .collect();
    s.quota_enabled = d.quota_enabled;
    s.quota_mb = u64::from(d.quota_mb);
    s.quota_hours = u64::from(d.quota_hours);
    s.quota_warn = d.quota_warn;
    s.proxy_mode = match d.proxy_mode {
        ProxyChoice::None => ProxyMode::None,
        ProxyChoice::System => ProxyMode::System,
        ProxyChoice::Script => ProxyMode::Script,
        ProxyChoice::Manual => ProxyMode::Manual,
    };
    s.use_browser_proxy = d.browser_proxy;
    s.proxy_pac_url = d.pac_url.trim().to_string();
    // The HTTP proxy's server stands in for HTTPS and FTP when they are on
    // without one of their own, as the tab's protocol boxes mean.
    let empty = ProxyDraft {
        on: false,
        host: String::new(),
        port: String::new(),
        user: String::new(),
        password: String::new(),
    };
    let entry = |i: usize| d.proxies.get(i).unwrap_or(&empty);
    let http = entry(0);
    let filled = |p: &ProxyDraft| if p.on && p.host.trim().is_empty() { http.clone() } else { p.clone() };
    let address = |p: &ProxyDraft| if p.on { join_address(&p.host, &p.port) } else { String::new() };
    let password = |p: &ProxyDraft| Some(p.password.clone()).filter(|pw| !pw.is_empty());
    let (https, ftp, socks) = (filled(entry(1)), filled(entry(2)), entry(3).clone());
    s.proxy_http = address(http);
    s.proxy_username = http.user.trim().to_string();
    s.proxy_password = password(http);
    s.proxy_https = address(&https);
    s.proxy_https_username = https.user.trim().to_string();
    s.proxy_https_password = password(&https);
    s.proxy_ftp = address(&ftp);
    s.proxy_ftp_username = ftp.user.trim().to_string();
    s.proxy_ftp_password = password(&ftp);
    s.proxy_socks5 = address(&socks);
    s.proxy_socks_username = socks.user.trim().to_string();
    s.proxy_socks_password = password(&socks);
    s.proxy_socks_version = if d.socks4 { SocksVersion::V4 } else { SocksVersion::V5 };
    s.ftp_active = !d.passive_ftp;
    s.site_logins = d
        .logins
        .iter()
        .filter(|l| !l.site.trim().is_empty())
        .map(|l| {
            let plain = current
                .site_logins
                .iter()
                .find(|o| o.site.eq_ignore_ascii_case(l.site.trim()))
                .is_some_and(|o| o.plain);
            SiteLogin {
                site: l.site.trim().to_ascii_lowercase(),
                username: l.user.trim().to_string(),
                password: Some(l.password.clone()).filter(|p| !p.is_empty()),
                plain,
            }
        })
        .collect();
    let sound = |i: usize| d.sounds.get(i).cloned().unwrap_or(SoundDraft { on: false, file: String::new() });
    let (done, failed, started, finished) = (sound(0), sound(1), sound(2), sound(3));
    s.sound_on_done = done.on;
    s.sound_done_file = done.file.trim().to_string();
    s.sound_on_failed = failed.on;
    s.sound_failed_file = failed.file.trim().to_string();
    s.sound_on_queue_started = started.on;
    s.sound_queue_started_file = started.file.trim().to_string();
    s.sound_on_queue_done = finished.on;
    s.sound_queue_file = finished.file.trim().to_string();
    apply_extras(&d.extras, d.done_dialog, current, &mut s)?;
    s.validate()?;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::resolve;

    #[test]
    fn an_unchanged_draft_keeps_the_settings() {
        let s = Settings::default();
        assert_eq!(settings(&draft(&s), &s).unwrap(), s);
    }

    #[test]
    fn changes_reach_the_settings() {
        let s = Settings::default();
        let mut d = draft(&s);
        d.theme = "dark".into();
        d.connections = 4;
        d.done_dialog = true;
        d.capture_types = "ZIP  .Rar 7z".into();
        d.proxy_mode = ProxyChoice::Manual;
        d.proxies[0] = ProxyDraft {
            on: true,
            host: "proxy".into(),
            port: "3128".into(),
            user: "me".into(),
            password: "pw".into(),
        };
        d.proxies[1].on = true;
        let out = settings(&d, &s).unwrap();
        assert_eq!(out.theme, Theme::Dark);
        assert_eq!(out.connections, 4);
        assert_eq!(out.notify, Notify::Dialog);
        assert_eq!(out.capture_types, ["zip", "rar", "7z"]);
        assert_eq!((out.proxy_http.as_str(), out.proxy_https.as_str()), ("proxy:3128", "proxy:3128"));
        assert_eq!(out.proxy_password.as_deref(), Some("pw"));
    }

    #[test]
    fn the_extras_reach_the_settings() {
        let s = Settings::default();
        let mut d = draft(&s);
        d.extras.max_concurrent = 5;
        d.extras.per_file_on = true;
        d.extras.per_file_kb = 200;
        d.extras.notify = NotifyChoice::Off;
        d.extras.auto_start_types = "EXE, msi".into();
        d.extras.unpack_to = UnpackChoice::Folder;
        let folder = if cfg!(windows) { r"C:\u" } else { "/tmp/u" };
        d.extras.unpack_folder = format!(" {folder} ");
        d.extras.remote_all = false;
        d.extras.remote_addresses = "192.168.1.5, 10.0.0.2".into();
        let out = settings(&d, &s).unwrap();
        assert_eq!(out.max_concurrent, 5);
        assert_eq!(out.speed_limit_per_file_kbps, Some(200));
        assert_eq!(out.notify, Notify::Off);
        assert_eq!(out.auto_start_types, ["exe", "msi"]);
        assert_eq!((out.unpack_to, out.unpack_folder.as_str()), (UnpackTo::Folder, folder));
        assert_eq!(out.remote_addresses, ["192.168.1.5", "10.0.0.2"]);
        d.extras.remote_port = 70_000;
        assert_eq!(settings(&d, &s), Err(OptionsError::Port));
    }

    #[test]
    fn the_torrent_settings_reach_the_settings_and_come_back() {
        let s = Settings::default();
        let mut d = draft(&s);
        let e = &d.extras;
        assert!(!e.torrent_port_fixed && e.torrent_dht && e.torrent_upnp && e.catch_torrent_files);
        assert_eq!((e.seed_ratio, e.seed_minutes), (1.0, 60));
        assert_eq!(settings(&d, &s).unwrap(), s, "nothing changed");
        d.extras.torrent_port_fixed = true;
        d.extras.torrent_port = 51413;
        d.extras.torrent_dht = false;
        d.extras.torrent_peers_on = true;
        d.extras.torrent_peers = 50;
        d.extras.torrent_upload_on = true;
        d.extras.torrent_upload_kb = 300;
        d.extras.seed_ratio = 2.5;
        d.extras.seed_minutes_on = false;
        d.extras.catch_torrent_files = false;
        let out = settings(&d, &s).unwrap();
        let t = &out.torrents;
        assert_eq!((t.port, t.dht, t.max_peers, t.upload_limit_kbps), (Some(51413), false, Some(50), Some(300)));
        assert_eq!(t.seed_limits, udm_domain::torrent::SeedLimits { ratio: Ratio::new(2.5).ok(), minutes: None });
        assert!(!t.catch_torrent_files);
        assert_eq!(settings(&draft(&out), &out).unwrap(), out, "what was saved comes back as it was");
        for (change, error) in [
            ((|d: &mut OptionsDraft| d.extras.torrent_port = 70_000) as fn(&mut OptionsDraft), OptionsError::Port),
            (|d| d.extras.torrent_peers = 1001, OptionsError::TorrentPeers),
            (|d| d.extras.torrent_upload_kb = 0, OptionsError::UploadLimit),
        ] {
            let mut bad = d.clone();
            change(&mut bad);
            assert_eq!(settings(&bad, &s), Err(error));
        }
        d.extras.seed_ratio = 101.0;
        assert!(matches!(settings(&d, &s), Err(OptionsError::Torrent(udm_domain::torrent::TorrentError::BadRatio(_)))));
    }

    #[test]
    fn the_network_adapter_and_dns_reach_the_settings() {
        let s = Settings::default();
        let mut d = draft(&s);
        assert_eq!((d.extras.adapter.clone(), d.extras.dns), (None, DnsDraft::System));
        d.extras.adapter = Some(AdapterDraft { id: " wg0 ".into(), name: "Work VPN".into(), renew: false });
        d.extras.dns = DnsDraft::Custom;
        d.extras.dns_url = "http://doh.example/q".into();
        assert_eq!(settings(&d, &s), Err(OptionsError::Network(udm_domain::network::NetworkError::DohAddress)));
        d.extras.dns_url = "https://doh.example/q".into();
        let out = settings(&d, &s).unwrap();
        assert_eq!(out.network_adapter.as_ref().map(|a| (a.id.as_str(), a.name.as_str())), Some(("wg0", "Work VPN")));
        assert_eq!(out.dns, DnsChoice::Custom(DohUrl::parse("https://doh.example/q").unwrap()));
        // What was saved comes back as it was.
        let back = draft(&out);
        assert_eq!(back.extras.dns, DnsDraft::Custom);
        assert_eq!(back.extras.dns_url, "https://doh.example/q");
        assert_eq!(settings(&back, &out).unwrap(), out);
        d.extras.adapter = Some(AdapterDraft { id: "  ".into(), name: String::new(), renew: false });
        assert!(settings(&d, &s).is_err(), "no adapter without an id");
        d.extras.adapter = None;
        d.extras.dns = DnsDraft::Quad9;
        let out = settings(&d, &s).unwrap();
        assert_eq!((out.network_adapter, out.dns), (None, DnsChoice::Quad9));
        let view = view(resolve("en", &[]), &s);
        let labels: Vec<&str> = view.network.resolvers.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels[0], "The system's (not encrypted)");
        assert_eq!(labels.len(), 5);
    }

    #[test]
    fn an_adapter_chosen_again_takes_its_networks_now() {
        use udm_domain::network::{NetworkAdapter, Subnet};
        let ip = |a: &str| a.parse().unwrap();
        let old = AdapterChoice {
            id: AdapterId::parse("utun3").unwrap(),
            name: "utun3".into(),
            subnets: vec![Subnet::new(ip("10.8.0.0"), 24).unwrap()],
        };
        let current = Settings { network_adapter: Some(old), ..Settings::default() };
        let now = NetworkAdapter {
            id: AdapterId::parse("utun3").unwrap(),
            name: "utun3".into(),
            addresses: vec![ip("172.20.1.2")],
            subnets: vec![Subnet::new(ip("172.20.1.2"), 24).unwrap()],
            up: true,
        };
        let mut d = draft(&current);
        let mut s = settings(&d, &current).unwrap();
        renew_adapter(&d, &mut s, std::slice::from_ref(&now));
        assert!(s.network_adapter.as_ref().unwrap().subnets.is_empty(), "the daemon keeps the old ones");
        d.extras.adapter.as_mut().unwrap().renew = true;
        let mut s = settings(&d, &current).unwrap();
        renew_adapter(&d, &mut s, &[now]);
        assert_eq!(s.network_adapter.unwrap().subnets, vec![Subnet::new(ip("172.20.1.0"), 24).unwrap()]);
    }

    #[test]
    fn the_debrid_account_reaches_the_settings_but_its_key_goes_apart() {
        let s = Settings::default();
        let mut d = draft(&s);
        assert!(debrid_key(&d).is_none(), "an empty key keeps the stored one");
        d.extras.debrid_key = "  KEY  ".into();
        assert_eq!(settings(&d, &s), Err(OptionsError::DebridService), "a key needs its service");
        d.extras.debrid_key.clear();
        d.extras.debrid_on = true;
        assert_eq!(settings(&d, &s), Err(OptionsError::DebridService));
        d.extras.debrid_service = Some(DebridChoice::AllDebrid);
        d.extras.debrid_key = "  KEY  ".into();
        let out = settings(&d, &s).unwrap();
        assert!(out.debrid.enabled && out.debrid.key_service.is_none(), "only the daemon says a key is stored");
        assert_eq!(out.debrid.service, Some(DebridService::AllDebrid));
        assert!(matches!(
            debrid_key(&d),
            Some(Command::SetDebridKey { service: DebridService::AllDebrid, key }) if key.expose() == "KEY"
        ));
        assert!(!serde_json::to_string(&out).unwrap().contains("KEY"));
        d.extras.debrid_key.clear();
        d.extras.debrid_forget = true;
        assert!(matches!(debrid_key(&d), Some(Command::RemoveDebridKey)), "removing is its own command");
        // A stored key shows the service it is for, never its text.
        let mut stored = Settings::default();
        stored.debrid.service = Some(DebridService::RealDebrid);
        stored.debrid.key_service = Some(DebridService::RealDebrid);
        let v = view(resolve("en", &[]), &stored);
        assert_eq!(v.debrid.key_service, Some(DebridChoice::RealDebrid));
        assert!(v.draft.extras.debrid_key.is_empty());
        assert_eq!(v.debrid.services.len(), 3);
        assert_eq!(settings(&draft(&stored), &stored).unwrap(), stored);
    }

    #[test]
    fn the_done_dialog_and_the_notification_agree() {
        let s = Settings::default();
        let mut d = draft(&s);
        d.done_dialog = true;
        assert_eq!(settings(&d, &s).unwrap().notify, Notify::Dialog);
        let s = Settings { notify: Notify::Dialog, ..Settings::default() };
        let mut d = draft(&s);
        d.extras.notify = NotifyChoice::Off;
        assert_eq!(settings(&d, &s).unwrap().notify, Notify::Off);
    }

    #[test]
    fn categories_keep_their_last_folder_when_renamed() {
        let mut s = Settings::default();
        s.categories[0].remember_folder = true;
        s.categories[0].last_folder = Some("/tmp/x".into());
        let mut d = draft(&s);
        d.categories[0].name = "Archives".into();
        let out = settings(&d, &s).unwrap();
        assert_eq!(out.categories[0].name, "Archives");
        assert_eq!(out.categories[0].last_folder.as_deref(), Some("/tmp/x"));
        d.categories[1].name = "archives".into();
        assert_eq!(settings(&d, &s), Err(OptionsError::CategoryTwice("archives".into())));
    }

    #[test]
    fn the_settings_checks_apply() {
        let s = Settings::default();
        let mut d = draft(&s);
        d.proxy_mode = ProxyChoice::Script;
        d.pac_url = "not a url".into();
        assert_eq!(settings(&d, &s), Err(OptionsError::Invalid(InvalidSettings::ScriptNotWeb)));
    }

    #[test]
    fn get_system_fills_the_proxies() {
        let found = udm_application::window::SystemProxy {
            http: Some("proxy.lan:3128".into()),
            https: None,
            socks: Some("socks5://s.lan:1080".into()),
            ftp: None,
            bypass: vec!["<local>".into(), "*.lan".into()],
        };
        let v = system_proxy(Some(&found)).unwrap();
        assert_eq!((v.proxies[0].host.as_str(), v.proxies[0].port.as_str()), ("proxy.lan", "3128"));
        assert!(!v.proxies[1].on);
        assert_eq!(v.proxies[3].host, "s.lan");
        assert_eq!(v.bypass, "<local>; *.lan");
        assert!(system_proxy(None).is_none());
    }

    #[test]
    fn addresses_split_into_host_and_port() {
        assert_eq!(split_address("proxy.lan:8080"), ("proxy.lan".into(), "8080".into()));
        assert_eq!(split_address("proxy.lan"), ("proxy.lan".into(), String::new()));
        assert_eq!(join_address(" h ", ""), "h");
    }

    #[test]
    fn the_view_lists_choices() {
        let v = view(resolve("en", &[]), &Settings::default());
        assert_eq!(v.themes.len(), 4);
        assert_eq!(v.languages[0].value, "");
        assert!(v.connections.contains(&8) && !v.connections.contains(&32));
    }
}
