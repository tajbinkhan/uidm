//! Snapshots of every window, made by the real projections from a sample
//! list, for the frontend's tests and its preview page
//! (`frontend/test/fixtures/`). `UDM_WRITE_FIXTURES=1 cargo test -p
//! udm-ui fixtures` writes them; otherwise the test checks they are
//! current.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::json;
use udm_application::daemon::Event;
use udm_domain::bytes::ByteCount;
use udm_domain::download::{SegmentInfo, SegmentState, Status};
use udm_domain::id::{DownloadId, QueueId};
use udm_domain::queue::Queue;
use udm_domain::settings::{Notify, Settings, Theme};

use crate::i18n::resolve;
use crate::projection::{self, Ctx};
use crate::state::{apply, AppState, UiMessage};
use crate::testing::entry;
use crate::windows::Label;

const DAY: u64 = 86_400;
/// Where the sample downloads come from.
const SITE: &str = "https://downloads.example.com/files";
/// 2026-10-21, the first download's day.
const START: u64 = 1_792_540_800;

fn sample() -> AppState {
    let mut s = AppState::default();
    let at = Instant::now();
    let send = |s: &mut AppState, event: Event, at: Instant| {
        apply(s, UiMessage::Daemon { event: Box::new(event), at, unix: 0 })
    };
    let files: [(&str, u64, Status); 17] = [
        ("interview-notes.pdf", 70_686, Status::Done),
        ("video-converter-1.11.2-setup.exe", 25_323_520, Status::Done),
        ("lecture-recording-2026-10-22.mp3", 88_749_000, Status::Done),
        ("udm-windows.zip", 4_445_962, Status::Done),
        ("display-fonts.zip", 904_396, Status::Done),
        ("Open.Movie.2026.1080p.mkv", 8_740_000_000, Status::Done),
        ("UDM-Setup-0.1.0.exe", 5_725_224, Status::Done),
        ("remote-desktop-6.6-setup.exe", 3_638_558, Status::Done),
        ("launcher-0.6.0.exe", 44_774_195, Status::Done),
        ("project-files-v2.zip", 353_802_000, Status::Interrupted),
        ("disc-image-tool-12.6.exe", 46_153_400, Status::Done),
        ("office-suite-24.8-x64.iso", 4_080_000_000, Status::Downloading),
        ("office-suite-24.8-x86.iso", 4_080_000_000, Status::Done),
        ("linux-desktop-26.04-amd64.iso", 5_068_000_000, Status::Downloading),
        ("usb-writer-4.15.exe", 1_981_808, Status::Done),
        ("UDM-Setup-0.3.3.exe", 7_927_000, Status::Queued),
        ("summer-photos.zip", 41_943_040, Status::Confirming),
    ];
    let items = files
        .iter()
        .enumerate()
        .map(|(i, (name, total, status))| {
            // Part of the file for the ones under way or stopped.
            let done = match (i + 1, status) {
                (14, _) => total * 4712 / 10_000,
                (_, Status::Downloading) => total * 1628 / 10_000,
                (_, Status::Interrupted) => total * 3807 / 10_000,
                (_, Status::Queued) => 0,
                _ => *total,
            };
            let mut e = entry(i as u64 + 1, name, *status, *total, done);
            e.url = format!("{SITE}/{name}").into();
            e.last_try = Some(START + DAY * (i as u64 / 2) + 3600 * 14 + 60 * i as u64);
            // The servers' countries, for the Country column and the
            // download window.
            e.country = match i + 1 {
                2 | 12 => udm_domain::country::CountryCode::parse("US"),
                4 => udm_domain::country::CountryCode::parse("DE"),
                6 | 14 => udm_domain::country::CountryCode::parse("NL"),
                _ => None,
            };
            e.description = match i + 1 {
                3 => Some("MP3, 64 kbps, 3:05:42".into()),
                6 => Some("Video: 1920x1080, 2:11:05".into()),
                14 => Some("Desktop installer, 26.04".into()),
                _ => None,
            };
            e
        })
        .collect();
    send(&mut s, Event::Downloads { items }, at);
    send(&mut s, Event::Connected, at);
    // No tip at startup, so the window's tests see the window itself.
    let settings =
        Settings { notify: Notify::Dialog, theme: Theme::Dark, tips_on_startup: false, ..Settings::default() };
    send(&mut s, Event::Settings { settings }, at);
    let queue: Queue = serde_json::from_value(json!({
        "id": 1, "name": "Main queue", "max_concurrent": 2, "running": true, "order": [12, 14, 16]
    }))
    .unwrap();
    let sync: Queue =
        serde_json::from_value(json!({"id": 2, "name": "Synchronization queue", "max_concurrent": 1})).unwrap();
    send(&mut s, Event::Queues { items: vec![queue, sync] }, at);
    // Two running downloads: one 16.28% done at about 2.9 MB/s, the other
    // 47.12% done at about 5.6 MB/s, each on eight connections.
    let id = DownloadId::new(12);
    let running = [
        (12u64, "office-suite-24.8-x64.iso", 4_080_000_000u64, [1600u64, 1628], 130u64, 7u64),
        (14, "linux-desktop-26.04-amd64.iso", 5_068_000_000, [4668, 4712], 420, 12),
    ];
    for (n_id, name, total, steps, first, step) in running {
        for (k, done) in steps.iter().map(|p| total * p / 10_000).enumerate() {
            let part = total / 8;
            let segments = (0..8)
                .map(|n| SegmentInfo {
                    start: ByteCount::new(n * part),
                    end: ByteCount::new((n + 1) * part - 1),
                    done: ByteCount::new(part * (first + n * step) / 1000),
                    state: SegmentState::Receiving,
                })
                .collect();
            let event = Event::Progress {
                id: DownloadId::new(n_id),
                url: format!("{SITE}/{name}").into(),
                done: ByteCount::new(done),
                total: Some(ByteCount::new(total)),
                parts: None,
                segments: Some(segments),
            };
            send(&mut s, event, at + Duration::from_secs(k as u64 * 4));
        }
    }
    apply(&mut s, UiMessage::Select(vec![DownloadId::new(2)]));
    apply(&mut s, UiMessage::Sort(crate::contract::ColumnKey::LastTry));
    // Columns wide enough for the names, as a user leaves them.
    for (column, width) in [
        (crate::contract::ColumnKey::Name, 260.0),
        (crate::contract::ColumnKey::Size, 80.0),
        (crate::contract::ColumnKey::Status, 84.0),
        (crate::contract::ColumnKey::LastTry, 124.0),
        (crate::contract::ColumnKey::Description, 230.0),
    ] {
        apply(&mut s, UiMessage::Resize(column, width));
    }
    apply(&mut s, UiMessage::PanelWidth(160.0));
    apply(&mut s, UiMessage::ShowProgress(id));
    s.done.insert(DownloadId::new(17));
    let _ = QueueId::MAIN;
    // What Windows can do: ask for administrator rights, set a creation time.
    let windows = crate::state::SystemFacts { can_elevate: true, creation_time: true, portable: false };
    apply(&mut s, UiMessage::SystemRead(windows));
    s
}

fn fixtures() -> serde_json::Value {
    let mut s = sample();
    let ctx = Ctx {
        lang: resolve("en", &[]),
        offset: 0,
        today: "2026-11-12".into(),
        now: std::time::Instant::now(),
        unix: 1_700_000_000,
        downloads: std::path::PathBuf::from("/home/u/Downloads"),
        flags: true,
    };
    let dark = projection::look(&s);
    if let Some(settings) = s.settings.as_mut() {
        settings.theme = Theme::Light;
    }
    let light = projection::look(&s);
    // Options then shows the theme that follows the system's, which suits
    // both looks.
    if let Some(settings) = s.settings.as_mut() {
        settings.theme = Theme::System;
    }
    apply(&mut s, UiMessage::Select(vec![DownloadId::new(2), DownloadId::new(4)]));
    apply(&mut s, UiMessage::AskRemove);
    // The main window then shows one download picked, as it was.
    apply(&mut s, UiMessage::Select(vec![DownloadId::new(2)]));
    // What Options shows beyond the settings: the tools, a paired phone
    // and a pairing code, the browsers, and a debrid account just checked.
    if let Some(settings) = s.settings.as_mut() {
        settings.remote_enabled = true;
        settings.debrid = udm_domain::debrid::DebridSettings {
            enabled: true,
            service: Some(udm_domain::debrid::DebridService::RealDebrid),
            key_service: Some(udm_domain::debrid::DebridService::RealDebrid),
        };
        // Downloads go through a VPN's adapter, with names looked up at
        // Quad9.
        settings.network_adapter = Some(udm_domain::network::AdapterChoice {
            id: udm_domain::network::AdapterId::parse("wg0").unwrap(),
            name: "wg0".into(),
            subnets: Vec::new(),
        });
        settings.dns = udm_domain::network::DnsChoice::Quad9;
    }
    let adapter = |id: &str, addresses: &[&str]| udm_domain::network::NetworkAdapter {
        id: udm_domain::network::AdapterId::parse(id).unwrap(),
        name: id.into(),
        addresses: addresses.iter().map(|a| a.parse().unwrap()).collect(),
        subnets: Vec::new(),
        up: true,
    };
    let adapters = vec![adapter("eth0", &["192.168.1.5", "fe80::5"]), adapter("wg0", &["10.2.0.2"])];
    apply(&mut s, UiMessage::AdaptersRead(Ok(adapters)));
    let tool = |tool, path: Option<&str>, installed: Option<&str>| udm_domain::tools::ToolState {
        tool,
        path: path.map(Into::into),
        installed: installed.map(Into::into),
        installable: true,
        command: String::new(),
    };
    let tools = vec![
        tool(udm_domain::tools::Tool::YtDlp, Some("/home/u/.local/share/UDM/tools/yt-dlp"), Some("2026.11.01")),
        tool(udm_domain::tools::Tool::Ffmpeg, Some("/usr/bin/ffmpeg"), None),
        tool(udm_domain::tools::Tool::GalleryDl, None, None),
    ];
    let at = Instant::now();
    let event = Event::Tools { tools, done: None, error: None };
    apply(&mut s, UiMessage::Daemon { event: Box::new(event), at, unix: 0 });
    let code = udm_domain::pairing::PairingCode::from_random(&[7; udm_domain::pairing::RANDOM_BYTES]);
    let remote = udm_domain::pairing::RemoteState {
        listener: udm_domain::pairing::ListenerState {
            listening: vec!["0.0.0.0:8765".into()],
            page: Some("https://192.168.1.5:8765/".into()),
            fault: None,
            fingerprint: Some("AB:CD:EF:01:23:45:67:89".into()),
        },
        devices: vec![udm_domain::pairing::DeviceInfo {
            id: udm_domain::id::DeviceId::new(1),
            name: udm_domain::pairing::DeviceName::clean("Pixel 8"),
            paired_at: 1_699_900_000,
            last_seen: Some(1_699_990_000),
        }],
        pairing: Some(udm_domain::pairing::PairingOffer {
            link: "https://192.168.1.5:8765/#pair".into(),
            code,
            expires_at: 1_700_000_300,
        }),
    };
    apply(&mut s, UiMessage::Daemon { event: Box::new(Event::Remote { state: remote }), at, unix: 0 });
    let check = udm_domain::debrid::DebridCheck {
        service: udm_domain::debrid::DebridService::RealDebrid,
        outcome: Ok(udm_domain::debrid::DebridAccount {
            username: Some("ann".into()),
            premium: true,
            premium_until: Some(1_702_592_000),
        }),
    };
    apply(&mut s, UiMessage::Daemon { event: Box::new(Event::Debrid { check }), at, unix: 0 });
    let slot =
        |name: &str, place: &str| udm_application::window::BrowserSlot { name: name.into(), place: place.into() };
    let row = |name: &str, registered: bool, added: Option<&str>| crate::services::BrowserRow {
        browser: slot(if added.is_some() { "Chromium" } else { name }, name),
        name: name.into(),
        added: added.map(|p| udm_domain::settings::AddedBrowser { name: name.into(), program: p.into() }),
        known: added.is_none(),
        registered,
        can_restart: true,
    };
    let browsers = crate::services::BrowsersRead {
        rows: vec![
            row("Google Chrome", true, None),
            row("Firefox", false, None),
            row("Thorium", true, Some("/opt/thorium/thorium")),
        ],
        chrome_readers: vec!["Brave".into()],
    };
    apply(&mut s, UiMessage::BrowsersRead(Some(Box::new(browsers))));
    let view = |label: Label| serde_json::to_value(projection::view(&ctx, &s, &label)).unwrap();
    // The Download complete dialog is drawn from a finished download.
    let mut finished = s.table.get(DownloadId::new(17)).cloned().unwrap();
    finished.status = Status::Done;
    finished.done = finished.total.unwrap_or_default();
    let mut done_state = sample();
    apply(
        &mut done_state,
        UiMessage::Daemon { event: Box::new(Event::Updated { item: finished }), at: Instant::now(), unix: 0 },
    );
    let done = serde_json::to_value(projection::view(&ctx, &done_state, &Label::Done(DownloadId::new(17)))).unwrap();
    // The grabber after a crawl of a small site.
    let mut grab_state = sample();
    let page = "http://127.0.0.1:8765/index.html";
    let file = |name: &str, size: u64, text: &str| udm_domain::grab::GrabFile {
        url: format!("http://127.0.0.1:8765/files/{name}"),
        name: name.into(),
        page: page.into(),
        size: Some(ByteCount::new(size)),
        text: Some(text.into()),
    };
    grab_state.grab.library = Some(crate::grabber::Library { recent: vec!["docs-site".into()], ..Default::default() });
    grab_state.grab.started(udm_domain::grab::GrabRequest { start: page.into(), ..Default::default() });
    grab_state.grab.done(
        vec![
            file("manual.pdf", 2_400_000, "User manual"),
            file("setup.exe", 41_943_040, "Download"),
            file("notes.txt", 3_100, ""),
        ],
        4,
        9,
        None,
        None,
    );
    let grab_results = serde_json::to_value(projection::view(&ctx, &grab_state, &Label::Grabber)).unwrap();
    // Links from the clipboard, to tick.
    let mut links_state = sample();
    let text = "http://127.0.0.1:8765/files/report.pdf http://127.0.0.1:8765/files/photos.zip http://127.0.0.1:8765/files/talk.mp3";
    apply(
        &mut links_state,
        UiMessage::Links(Some(Box::new(crate::tools::LinkList {
            file: None,
            items: crate::tools::links_of(text),
            rejected: 0,
        }))),
    );
    let links = serde_json::to_value(projection::view(&ctx, &links_state, &Label::Links)).unwrap();
    // What the daemon asks of the window: a login, a countdown, a new
    // link, a package's password, and links, videos and a gallery to pick.
    let mut asks = sample();
    let failed = |id: u64| {
        let mut e = asks.table.get(DownloadId::new(id)).cloned().unwrap();
        e.status = Status::Failed;
        e.error = Some(udm_domain::download::needs_login_error("127.0.0.1", Some("Private files")));
        e
    };
    let (f3, f4) = (failed(3), failed(4));
    let at = Instant::now();
    let send = |s: &mut AppState, event: Event| apply(s, UiMessage::Daemon { event: Box::new(event), at, unix: 0 });
    send(&mut asks, Event::Updated { item: f3.clone() });
    send(&mut asks, Event::Updated { item: f4.clone() });
    send(&mut asks, Event::Failed { id: f3.id, url: f3.url.clone(), error: f3.error.clone().unwrap() });
    send(&mut asks, Event::Failed { id: f4.id, url: f4.url.clone(), error: f4.error.clone().unwrap() });
    send(
        &mut asks,
        Event::Countdown {
            action: udm_domain::queue::WhenDone::ShutDown,
            seconds: 60,
            queue: "Main queue".into(),
            file: None,
        },
    );
    apply(&mut asks, UiMessage::ShowRefresh(f3.id));
    let link = |url: &str, text: &str| udm_domain::download::PageLink {
        url: url.into(),
        text: text.into(),
        image: false,
        cookies: None,
    };
    send(
        &mut asks,
        Event::PickLinks {
            page: "http://127.0.0.1:8765/index.html".into(),
            links: vec![
                link("http://127.0.0.1:8765/files/manual.pdf", "User manual"),
                link("http://127.0.0.1:8765/files/setup.exe", "Download"),
                link("http://127.0.0.1:8765/files/photos.zip", "Photos"),
                // A web page, which the filters hide at first.
                link("http://127.0.0.1:8765/about.html", "About"),
            ],
            user_agent: None,
        },
    );
    let request = asks.asks.links.as_ref().map_or(0, |l| l.request);
    send(
        &mut asks,
        Event::LinkInfo { request, index: 1, total: Some(ByteCount::new(41_943_040)), content_type: None, error: None },
    );
    let entry = |n: u32| udm_domain::media::PlaylistEntry {
        url: format!("https://videos.example.com/watch?v={n}"),
        title: format!("Lesson {n}: getting started"),
        duration: Some(f64::from(300 + n * 61)),
    };
    let playlist = udm_domain::media::Playlist { title: "A course".into(), entries: (1..=4).map(entry).collect() };
    send(&mut asks, Event::PickVideos { page: "https://videos.example.com/list".into(), playlist, user_agent: None });
    send(&mut asks, Event::PickGallery { page: "https://pics.example.com/album".into(), user_agent: None });
    let request = asks.asks.gallery.as_ref().map_or(0, |g| g.request);
    let picture = |n: u32| udm_domain::gallery::Picture {
        url: format!("https://pics.example.com/{n}.jpg"),
        name: format!("{n}.jpg"),
        extension: "jpg".into(),
        width: Some(1920),
        height: Some(1080),
    };
    let gallery = udm_domain::gallery::Gallery { title: "Album".into(), pictures: (1..=3).map(picture).collect() };
    send(
        &mut asks,
        Event::Gallery { request, page: "https://pics.example.com/album".into(), gallery: Some(gallery), error: None },
    );
    let package: udm_domain::package::Package = serde_json::from_value(json!({
        "id": 1, "name": "photos", "downloads": [5], "unpack": {"state": "needs_password", "wrong": false}
    }))
    .unwrap();
    send(&mut asks, Event::Packages { items: vec![package] });
    apply(&mut asks, UiMessage::AskPassword(udm_domain::id::PackageId::new(1)));
    // A zip's files, and the offer to keep a download in the browser.
    apply(&mut asks, UiMessage::PreviewZip(DownloadId::new(4)));
    let entries = vec![
        udm_domain::download::ZipEntry { name: "docs/readme.txt".into(), size: ByteCount::new(2_048) },
        udm_domain::download::ZipEntry { name: "bin/tool.exe".into(), size: ByteCount::new(3_145_728) },
    ];
    send(&mut asks, Event::ZipPreview { id: DownloadId::new(4), entries, error: None });
    asks.files.exception = Some("https://cdn.example.com/files/song.mp3".into());
    let asked = |label: Label| serde_json::to_value(projection::view(&ctx, &asks, &label)).unwrap();
    let countdown = asked(Label::Countdown);
    let (login, refresh, password) = (asked(Label::Login), asked(Label::Refresh), asked(Label::Password));
    let (pick_links, pick_videos, gallery) = (asked(Label::PickLinks), asked(Label::PickVideos), asked(Label::Gallery));
    let (zip_preview, exception) = (asked(Label::ZipPreview), asked(Label::Exception));
    json!({
        "look": { "light": light, "dark": dark },
        "views": {
            "main": view(Label::Main),
            "add-url": view(Label::AddUrl),
            "save-as": view(Label::SaveAs(DownloadId::new(17))),
            "download": view(Label::Download(DownloadId::new(12))),
            "done": done,
            "remove": view(Label::Remove),
            "about": view(Label::About),
            "options": view(Label::Options),
            "scheduler": view(Label::Scheduler),
            "grabber": view(Label::Grabber),
            "grabber-results": grab_results,
            "batch": view(Label::Batch),
            "links": links,
            "properties": view(Label::Properties(DownloadId::new(2))),
            "drop-target": view(Label::DropTarget),
            "login": login,
            "countdown": countdown,
            "refresh": refresh,
            "password": password,
            "pick-links": pick_links,
            "pick-videos": pick_videos,
            "gallery": gallery,
            "zip-preview": zip_preview,
            "exception": exception,
        }
    })
}

// The fixtures hold the system's own defaults and paths (Windows' default
// virus scanner, folders joined with `\`), so they are written and checked
// on Unix; the frontend's tests read the committed file on every system.
#[test]
#[cfg_attr(windows, ignore = "the fixtures are written with Unix defaults and paths")]
fn fixtures_are_current() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../frontend/test/fixtures/views.json");
    let fresh = serde_json::to_string_pretty(&fixtures()).unwrap() + "\n";
    if std::env::var_os("UDM_WRITE_FIXTURES").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, fresh).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    let first = committed.lines().zip(fresh.lines()).position(|(a, b)| a != b);
    assert!(
        committed == fresh,
        "frontend/test/fixtures/views.json is out of date (first at line {}): run UDM_WRITE_FIXTURES=1 cargo test -p udm-ui fixtures",
        first.unwrap_or(committed.lines().count().min(fresh.lines().count())) + 1
    );
}
