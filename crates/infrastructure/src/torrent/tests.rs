//! The client against a local swarm: a seeding librqbit session on
//! 127.0.0.1, with the DHT, trackers, UPnP and local peer discovery all off,
//! so nothing leaves this computer. The client under test finds the seeder
//! only through the magnet link's `x.pe` peer.

use super::*;
use std::sync::mpsc::{channel, Receiver};

use sha1::{Digest, Sha1};
use udm_domain::settings::{Folders, Settings};

use crate::engine::{DownloadRequest, Engine};

const PIECE: usize = 16 * 1024;

/// A folder of its own under the temporary folder, empty.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("udm_torrent_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `len` bytes that differ from file to file.
fn data(seed: u8, len: usize) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
}

/// The files of the test torrent, by path inside its folder.
fn files() -> Vec<(Vec<&'static str>, Vec<u8>)> {
    vec![
        (vec!["a.bin"], data(1, 5 * PIECE + 100)),
        (vec!["b.bin"], data(2, 3 * PIECE)),
        (vec!["sub", "c.bin"], data(3, 2 * PIECE + 7)),
    ]
}

fn bstr(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(s.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(s);
}

/// A `.torrent` file (no trackers) for `files` in a folder named `name`,
/// and the files written there under `dir`.
fn make_torrent(dir: &Path, name: &str) -> Vec<u8> {
    let mut all = Vec::new();
    let mut list = b"l".to_vec();
    for (path, bytes) in files() {
        let file = path.iter().fold(dir.join(name), |p, part| p.join(part));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, &bytes).unwrap();
        list.extend_from_slice(format!("d6:lengthi{}e4:pathl", bytes.len()).as_bytes());
        for part in &path {
            bstr(&mut list, part.as_bytes());
        }
        list.extend_from_slice(b"ee");
        all.extend_from_slice(&bytes);
    }
    list.push(b'e');
    let mut pieces = Vec::new();
    for chunk in all.chunks(PIECE) {
        pieces.extend_from_slice(&Sha1::digest(chunk));
    }
    let mut info = b"d5:files".to_vec();
    info.extend_from_slice(&list);
    bstr(&mut info, b"name");
    bstr(&mut info, name.as_bytes());
    bstr(&mut info, b"piece length");
    info.extend_from_slice(format!("i{PIECE}e").as_bytes());
    bstr(&mut info, b"pieces");
    bstr(&mut info, &pieces);
    info.push(b'e');
    metainfo::build(&info, &[])
}

/// A librqbit session seeding the test torrent from `dir`, on 127.0.0.1
/// only. Returns it, the torrent's magnet link (with the seeder as its
/// peer) and the torrent file.
fn seeder(name: &str) -> (Arc<Session>, String, Vec<u8>, PathBuf) {
    let dir = scratch(name);
    let bytes = make_torrent(&dir, "Album");
    let runtime = engine::http::runtime().unwrap();
    let session = runtime
        .block_on(Session::new_with_opts(
            dir.clone(),
            SessionOptions {
                dht: None,
                disable_trackers: true,
                disable_local_service_discovery: true,
                listen: Some(ListenerOptions {
                    mode: ListenerMode::TcpOnly,
                    listen_addr: "127.0.0.1:0".parse().unwrap(),
                    ipv4_only: true,
                    ..ListenerOptions::default()
                }),
                ipv4_only: true,
                ..SessionOptions::default()
            },
        ))
        .unwrap();
    let options = AddTorrentOptions {
        overwrite: true,
        output_folder: Some(dir.join("Album").to_string_lossy().into_owned()),
        ..AddTorrentOptions::default()
    };
    let handle = runtime.block_on(add(&session, bytes.clone(), options)).unwrap();
    let waited = Instant::now();
    while !handle.stats().finished {
        assert!(waited.elapsed() < Duration::from_secs(30), "the seeder checks its files");
        std::thread::sleep(Duration::from_millis(50));
    }
    let port = session.listen_addr().unwrap().port();
    let meta = Metainfo::read(&bytes).unwrap();
    let magnet = format!("magnet:?xt=urn:btih:{}&dn=Album&x.pe=127.0.0.1:{port}", meta.info.info_hash);
    (session, magnet, bytes, dir)
}

/// Torrent settings that reach no one: no DHT, UPnP or local discovery.
fn quiet() -> TorrentSettings {
    TorrentSettings { dht: false, upnp: false, local_peers: false, ..TorrentSettings::default() }
}

/// A client under `dir`, listening on 127.0.0.1, sending its events to the
/// returned receiver.
fn client(dir: &Path) -> (Arc<TorrentClient>, Receiver<Progress>, Sink) {
    let tuning = Tuning { listen_ip: IpAddr::from([127, 0, 0, 1]), dht_bootstrap: Some(Vec::new()) };
    let client = Arc::new(TorrentClient::tuned(dir.to_path_buf(), tuning));
    let (tx, rx) = channel();
    let tx = Mutex::new(tx);
    let sink: Sink = Arc::new(move |p| {
        let _ = tx.lock_or_recover().send(p);
    });
    client.connect(Arc::clone(&sink), Arc::new(Route::default), Arc::new(quiet));
    (client, rx, sink)
}

/// An engine downloading through `client`.
fn engine(client: &Arc<TorrentClient>, sink: Sink) -> Engine {
    let engine = Engine::new(sink, 2);
    engine.torrents_with(Arc::clone(client));
    engine
}

fn request(magnet: &str, into: &Path) -> DownloadRequest {
    let download = Download { url: magnet.into(), ..Download::default() };
    let mut req = crate::app::to_request(download, &Settings::default());
    req.folders = Folders::fixed(into.to_path_buf());
    req
}

/// Events until `id` ends; the last is how it ended.
fn until_end(rx: &Receiver<Progress>, id: DownloadId) -> Vec<Progress> {
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let p = rx.recv_timeout(left).expect("the torrent ends within a minute");
        let ends = match &p {
            Progress::Finished { id: i, .. } | Progress::Failed { id: i, .. } | Progress::Canceled { id: i, .. } => {
                *i == id
            }
            _ => false,
        };
        seen.push(p);
        if ends {
            return seen;
        }
    }
}

fn same_files(root: &Path, only: Option<&[usize]>) {
    for (i, (path, bytes)) in files().into_iter().enumerate() {
        let file = path.iter().fold(root.to_path_buf(), |p, part| p.join(part));
        let wanted = only.is_none_or(|o| o.contains(&i));
        let got = std::fs::read(&file).unwrap_or_default();
        if wanted {
            assert!(got == bytes, "{} is whole", file.display());
        } else {
            assert!(got != bytes, "{} was not chosen", file.display());
        }
    }
}

#[test]
fn a_magnet_link_downloads_its_files_and_then_seeds() {
    let (_seeder, magnet, _, _) = seeder("seed_all");
    let dir = scratch("leech_all");
    let (client, rx, sink) = client(&dir.join("state"));
    let engine = engine(&client, sink);
    let id = engine.enqueue(request(&magnet, &dir.join("dl")));
    let events = until_end(&rx, id);
    let root = match events.last() {
        Some(Progress::Finished { path, .. }) => path.clone(),
        other => panic!("{:?}", other.map(std::mem::discriminant)),
    };
    assert_eq!(root, dir.join("dl").join("Album"), "a torrent of many files gets its own folder");
    same_files(&root, None);
    let info = events.iter().find_map(|p| match p {
        Progress::TorrentInfo { info, .. } => Some(info.clone()),
        _ => None,
    });
    let info = info.expect("the window hears what the torrent holds");
    assert_eq!(info.files.len(), 3);
    assert_eq!(info.files.as_slice()[2].path, ["sub", "c.bin"]);
    let started_total = events.iter().find_map(|p| match p {
        Progress::Started { total, .. } => *total,
        _ => None,
    });
    assert_eq!(started_total, Some(info.files.selected_size(None)));
    assert!(events.iter().any(|p| matches!(p, Progress::TorrentStats { stats, .. } if stats.pieces.pieces > 0)));
    // It seeds now, outside the engine.
    let seeding = client.seeding();
    assert_eq!(seeding.iter().map(|s| s.info_hash).collect::<Vec<_>>(), [info.info_hash]);
    assert_eq!(engine.active_count(), 0, "seeding takes no download slot");
    client.stop_seeding(info.info_hash);
    assert!(client.seeding().is_empty());
    let stopped = rx
        .try_iter()
        .any(|p| matches!(p, Progress::TorrentStats { stats, .. } if stats.phase == TorrentPhase::Complete));
    assert!(stopped, "its row hears it no longer seeds");
}

#[test]
fn only_the_chosen_files_are_downloaded() {
    let (_seeder, magnet, _, _) = seeder("seed_some");
    let dir = scratch("leech_some");
    let (client, rx, sink) = client(&dir.join("state"));
    let engine = engine(&client, sink);
    let mut req = request(&magnet, &dir.join("dl"));
    req.torrent_files = Some(FileSelection::parse(&[0, 2], 3).unwrap());
    let id = engine.enqueue(req);
    let events = until_end(&rx, id);
    assert!(matches!(events.last(), Some(Progress::Finished { .. })));
    same_files(&dir.join("dl").join("Album"), Some(&[0, 2]));
    let total = events.iter().find_map(|p| match p {
        Progress::Started { total, .. } => *total,
        _ => None,
    });
    assert_eq!(total, Some(ByteCount::new((5 * PIECE + 100 + 2 * PIECE + 7) as u64)));
}

#[test]
fn a_paused_torrent_resumes_and_survives_a_restart() {
    let (_seeder, magnet, _, _) = seeder("seed_pause");
    let dir = scratch("leech_pause");
    let state = dir.join("state");
    let (client, rx, sink) = client(&state);
    // Slow, so it can be stopped partway.
    client.set_limits(Some(32 * 1024), None);
    let engine = engine(&client, sink);
    let id = engine.enqueue(request(&magnet, &dir.join("dl")));
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("progress") {
            Progress::Advanced { done, .. } if done.get() > 0 => break,
            Progress::Failed { error, .. } => panic!("{error}"),
            _ => {}
        }
    }
    engine.cancel(id);
    assert!(matches!(until_end(&rx, id).last(), Some(Progress::Canceled { .. })));
    // Started again, by the same client.
    client.set_limits(None, None);
    engine.enqueue_as(id, request(&magnet, &dir.join("dl")));
    assert!(matches!(until_end(&rx, id).last(), Some(Progress::Finished { .. })));
    same_files(&dir.join("dl").join("Album"), None);
    drop(engine);
    drop(client);

    // A new client on the same folders (UDM started again): the torrent's
    // file and resume data are kept, so a new download of it needs no
    // metadata from peers and finds its data there.
    let (again, rx, sink) = self::client(&state);
    let engine = self::engine(&again, sink);
    let mut req = request(&magnet.replace("&x.pe=", "&x.pe.none="), &dir.join("dl"));
    req.reuse_path = Some(dir.join("dl").join("Album"));
    let id = engine.enqueue(req);
    let events = until_end(&rx, id);
    assert!(matches!(events.last(), Some(Progress::Finished { .. })), "complete from its files, with no peer");
}

#[test]
fn a_finished_torrent_seeds_again_after_a_restart_and_is_forgotten() {
    let (_seeder, magnet, bytes, seed_dir) = seeder("seed_again");
    let dir = scratch("leech_again");
    let (client, rx, _) = client(&dir.join("state"));
    let link = MagnetLink::parse(&magnet).unwrap();
    // The client has the torrent file, and the files are where a download
    // of it left them.
    client.add_file(&bytes).unwrap();
    client.seed(DownloadId::new(7), &link, &seed_dir.join("Album"), None, ByteCount::new(42)).unwrap();
    let waited = Instant::now();
    while client.seeding().is_empty() {
        client.watch_seeding();
        assert!(waited.elapsed() < Duration::from_secs(30), "it checks its files, then seeds");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(client.seeding()[0].uploaded, ByteCount::new(42), "what it uploaded before counts");
    assert!(rx.try_iter().any(|p| matches!(p, Progress::TorrentStats { id, stats, .. }
        if id == DownloadId::new(7) && stats.phase == TorrentPhase::Seeding)));

    client.forget(link.info_hash);
    assert!(client.seeding().is_empty());
    assert!(!client.kept_path(link.info_hash).exists(), "its torrent file goes");

    // Files that are not all there do not seed (nor download).
    let broken = scratch("broken_again");
    std::fs::create_dir_all(broken.join("Album")).unwrap();
    std::fs::write(broken.join("Album").join("a.bin"), b"short").unwrap();
    client.add_file(&bytes).unwrap();
    client.seed(DownloadId::new(8), &link, &broken.join("Album"), None, ByteCount::ZERO).unwrap();
    let waited = Instant::now();
    while client.inner().seeding.contains_key(&link.info_hash) {
        client.watch_seeding();
        assert!(waited.elapsed() < Duration::from_secs(30), "it stops once checked");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(client.seeding().is_empty());
}

#[test]
fn metadata_comes_from_the_peers_and_is_kept() {
    let (_seeder, magnet, _, _) = seeder("seed_meta");
    let dir = scratch("leech_meta");
    let (client, _rx, _) = client(&dir);
    let link = MagnetLink::parse(&magnet).unwrap();
    let meta = client.metadata(&link, Duration::from_secs(60)).unwrap();
    assert_eq!((meta.info.name.as_str(), meta.info.files.len()), ("Album", 3));
    assert!(client.kept_path(link.info_hash).exists());
    // Kept: no peer needed the second time.
    let bare = MagnetLink::new(link.info_hash);
    assert_eq!(client.metadata(&bare, Duration::from_secs(1)).unwrap().info, meta.info);
}

#[test]
fn magnet_links_with_no_way_to_peers_fail_in_time() {
    let dir = scratch("nowhere");
    let (client, _rx, _) = client(&dir);
    let link = MagnetLink::new(InfoHash::parse("c12fe1c06bba254a9dc9f519b335aa7c1367a88a").unwrap());
    assert!(client.metadata(&link, Duration::from_secs(2)).is_err());
}

#[test]
fn with_dns_over_https_no_tracker_name_goes_to_the_system() {
    let endpoint = udm_domain::network::DohEndpoint {
        url: "https://127.0.0.1:9/dns-query".into(),
        host: "127.0.0.1".into(),
        bootstrap: Vec::new(),
    };
    let doh = Route { binding: None, dns: Some(Arc::new(crate::doh::Doh::new(endpoint, None))) };
    let trackers: Vec<String> = [
        "udp://localhost:6969/announce",
        "udp://tracker.invalid:6969/announce",
        "https://tracker.invalid/announce",
        "http://10.0.0.1:8080/announce",
        "udp://[::1]:6969",
    ]
    .iter()
    .map(|t| t.to_string())
    .collect();
    let (used, shown) = trackers_for(&trackers, &doh);
    assert_eq!(used, ["udp://127.0.0.1:6969/announce", "http://10.0.0.1:8080/announce", "udp://[::1]:6969"]);
    let states: Vec<TrackerState> = shown.iter().map(|t| t.state).collect();
    use TrackerState::{Skipped, Used};
    assert_eq!(states, [Used, Skipped, Skipped, Used, Used], "a name the resolver cannot give is left out");
    // The system's resolver: every tracker as it is.
    assert_eq!(trackers_for(&trackers, &Route::default()).0, trackers);
}

#[cfg(windows)]
#[test]
fn torrents_refuse_to_run_bound_on_windows() {
    let (route, _) = crate::route::tests::bound_to_loopback();
    let (client, _rx, _) = client(&scratch("windows_bound"));
    assert!(matches!(client.live(&route), Err(EngineError::Torrent(text)) if text == WINDOWS_BOUND));
}

#[cfg(target_os = "linux")]
#[test]
fn a_session_bound_to_an_adapter_binds_its_sockets_and_stops_with_it() {
    let (route, stand_in) = crate::route::tests::bound_to_loopback();
    let (client, _rx, _) = client(&scratch("bound"));
    let (session, generation) = client.live(&route).unwrap();
    assert!(session.listen_addr().is_some());
    // The same settings keep the session.
    assert_eq!(client.live(&route).unwrap().1, generation);
    stand_in.gone.store(true, Ordering::SeqCst);
    let waited = Instant::now();
    while !client.inner().live.as_ref().is_some_and(|l| l.down.load(Ordering::SeqCst)) {
        assert!(waited.elapsed() < Duration::from_secs(5), "the watcher sees the adapter go");
        std::thread::sleep(Duration::from_millis(50));
    }
    client.watch_seeding();
    assert!(client.inner().live.is_none(), "the session stops");
    assert!(client.live(&route).is_err(), "and does not start while the adapter is down");
}

/// The daemon's side: `.torrent` files fetched from the web, and the
/// save-as dialog's proposal for a magnet link, through `Transfers`.
#[test]
fn torrent_files_are_fetched_and_magnet_links_proposed() {
    use crate::engine::testserver::TestServer;
    use crate::live_settings::LiveSettings;
    use crate::store::{DownloadStore, Ended, StoreEvents, StoreFiles};
    use udm_domain::ports::Probes;

    struct Quiet;
    impl StoreEvents for Quiet {
        fn announce(&self, _: &crate::protocol::Outgoing) {}
        fn ended(&self, _: Ended) {}
        fn wake(&self) {}
    }

    let dir = scratch("desk");
    let bytes = make_torrent(&dir.join("source"), "Album");
    let server = TestServer::start(bytes.clone());
    let events: Arc<dyn StoreEvents> = Arc::new(Quiet);
    let (store, _, next_id) = DownloadStore::open(StoreFiles::default(), Arc::clone(&events));
    let settings = Settings { download_dir: Some(dir.join("dl").display().to_string()), ..Settings::default() };
    let live = Arc::new(LiveSettings::new(settings, None));
    let transfers = Arc::new(crate::transfers::Transfers::new(Arc::new(store), events, live, Arc::default(), next_id));
    let (client, _rx, _) = client(&dir.join("state"));
    transfers.torrents_with(Arc::clone(&client));
    let desk = TorrentDesk { client: Arc::clone(&client), transfers: Arc::clone(&transfers) };

    let asked =
        Download { url: format!("{}/get?id=1", server.url).into(), cookies: Some("a=1".into()), ..Download::default() };
    let (magnet, info) = desk.fetch_file(&asked).unwrap();
    assert_eq!((info.name.as_str(), info.files.len()), ("Album", 3));
    assert_eq!(magnet.name.as_deref(), Some("Album"));
    assert!(client.kept_path(info.info_hash).exists(), "kept for the download");

    // The save-as dialog for its magnet link: the kept metadata, no peer.
    let probes = crate::probes::ServerProbes::new(Arc::clone(&transfers));
    let facts = probes.file(&Download { url: magnet.to_uri().into(), ..Download::default() }).unwrap();
    assert_eq!(facts.filename.as_str(), "Album");
    assert!(facts.path.ends_with("Album"));
    assert_eq!(facts.torrent.map(|t| t.files.len()), Some(3));
    assert_eq!(facts.total, Some(info.files.selected_size(None)));

    // A page that is no torrent file.
    let page = TestServer::start(b"<html>not a torrent</html>".to_vec());
    let refused = desk.fetch_file(&Download { url: page.url.clone().into(), ..Download::default() });
    assert!(matches!(refused, Err(TorrentsError::Torrent(TorrentError::BadTorrent(_)))));
}
