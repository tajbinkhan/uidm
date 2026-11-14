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

/// A folder of its own under the temporary folder, empty, removed with
/// what is in it when the test is done with it.
struct Scratch(PathBuf);

impl std::ops::Deref for Scratch {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("udm_torrent_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
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
/// peer), the torrent file and the folder its files are in.
fn seeder(name: &str) -> (Arc<Session>, String, Vec<u8>, Scratch) {
    let dir = scratch(name);
    let bytes = make_torrent(&dir, "Album");
    let runtime = engine::http::runtime().unwrap();
    let session = runtime
        .block_on(Session::new_with_opts(
            dir.to_path_buf(),
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

/// The tuning tests use: 127.0.0.1, no DHT nodes to start from.
fn local() -> Tuning {
    Tuning { listen_ip: IpAddr::from([127, 0, 0, 1]), dht_bootstrap: Some(Vec::new()), ..Tuning::default() }
}

/// A client under `dir`, listening on 127.0.0.1, sending its events to the
/// returned receiver.
fn client(dir: &Path) -> (Arc<TorrentClient>, Receiver<Progress>, Sink) {
    client_with(dir, local(), Arc::new(Route::default), Arc::new(quiet))
}

fn client_with(
    dir: &Path,
    tuning: Tuning,
    route: RouteSource,
    settings: SettingsSource,
) -> (Arc<TorrentClient>, Receiver<Progress>, Sink) {
    let client = Arc::new(TorrentClient::tuned(dir.to_path_buf(), tuning));
    let (tx, rx) = channel();
    let tx = Mutex::new(tx);
    let sink: Sink = Arc::new(move |p| {
        let _ = tx.lock_or_recover().send(p);
    });
    client.connect(Arc::clone(&sink), route, settings);
    (client, rx, sink)
}

/// Look at the seeding torrents until `done` holds, for at most 30 s.
fn watch_until(client: &TorrentClient, what: &str, done: impl Fn() -> bool) {
    let waited = Instant::now();
    while !done() {
        client.watch_seeding();
        assert!(waited.elapsed() < Duration::from_secs(30), "{what}");
        std::thread::sleep(Duration::from_millis(100));
    }
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
fn checked_data_never_counts_as_downloaded() {
    // Every file is there already; one is chosen. While the torrent checks
    // its data, librqbit counts the whole torrent: none of that may reach the
    // list or the quota as downloaded bytes.
    let (_seeder, magnet, _, _) = seeder("seed_checked");
    let dir = scratch("leech_checked");
    make_torrent(&dir.join("dl"), "Album");
    let (client, rx, sink) = client(&dir.join("state"));
    let engine = engine(&client, sink);
    let mut req = request(&magnet, &dir.join("dl"));
    req.torrent_files = Some(FileSelection::parse(&[1], 3).unwrap());
    req.file_exists = FileExists::Overwrite;
    let id = engine.enqueue(req);
    let events = until_end(&rx, id);
    assert!(matches!(events.last(), Some(Progress::Finished { .. })));
    let chosen = (3 * PIECE) as u64;
    for p in &events {
        if let Progress::Advanced { done, total, .. } = p {
            assert_eq!(total.map(|t| t.get()), Some(chosen));
            assert!(done.get() <= chosen, "{done} of {chosen}");
        }
    }
}

#[test]
fn a_taken_name_follows_the_file_exists_setting() {
    let (_seeder, magnet, _, _) = seeder("seed_taken");
    let dir = scratch("leech_taken");
    std::fs::create_dir_all(dir.join("dl").join("Album")).unwrap();
    std::fs::write(dir.join("dl").join("Album").join("a.bin"), b"somebody else's").unwrap();
    let (client, rx, sink) = client(&dir.join("state"));
    let engine = engine(&client, sink);
    // Rename (the default): a numbered folder; the other file stays.
    let id = engine.enqueue(request(&magnet, &dir.join("dl")));
    match until_end(&rx, id).last() {
        Some(Progress::Finished { path, .. }) => assert_eq!(path, &dir.join("dl").join("Album (1)")),
        _ => panic!("finished"),
    }
    same_files(&dir.join("dl").join("Album (1)"), None);
    assert_eq!(std::fs::read(dir.join("dl").join("Album").join("a.bin")).unwrap(), b"somebody else's");
    // Ask: nothing is written, the dialog asks.
    let mut req = request(&magnet, &dir.join("dl"));
    req.file_exists = FileExists::Ask;
    let id = engine.enqueue(req);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("an answer") {
            Progress::NameTaken { id: i, path, .. } if i == id => {
                assert_eq!(path, dir.join("dl").join("Album"));
                break;
            }
            Progress::Finished { id: i, .. } | Progress::Failed { id: i, .. } if i == id => panic!("it asks"),
            _ => {}
        }
    }
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
    // Started again, as a resume does: into its own folder.
    let mut again = request(&magnet, &dir.join("dl"));
    again.reuse_path = Some(dir.join("dl").join("Album"));
    engine.enqueue_as(id, again);
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
    assert!(client.seeding().is_empty() && client.active(), "checked before it uploads");
    watch_until(&client, "it checks its files, then seeds", || !client.seeding().is_empty());
    assert_eq!(client.seeding()[0].uploaded, ByteCount::new(42), "what it uploaded before counts");
    assert!(rx.try_iter().any(|p| matches!(p, Progress::TorrentStats { id, stats, .. }
        if id == DownloadId::new(7) && stats.phase == TorrentPhase::Seeding)));

    client.forget(link.info_hash);
    assert!(client.seeding().is_empty());
    assert!(!client.kept_path(link.info_hash).exists(), "its torrent file goes");

    // Files that are not all there, or not at their size, do not seed: no
    // file is made or written for them.
    let broken = scratch("broken_again");
    std::fs::create_dir_all(broken.join("Album")).unwrap();
    std::fs::write(broken.join("Album").join("a.bin"), b"short").unwrap();
    client.add_file(&bytes).unwrap();
    assert!(client.seed(DownloadId::new(8), &link, &broken.join("Album"), None, ByteCount::ZERO).is_err());
    assert!(!broken.join("Album").join("b.bin").exists());
    assert_eq!(std::fs::read(broken.join("Album").join("a.bin")).unwrap(), b"short");
    assert!(client.seeding().is_empty() && !client.active());

    // Files changed inside (same sizes) are found by the check: it stops
    // rather than download.
    let changed = scratch("changed_again");
    make_torrent(&changed, "Album");
    std::fs::write(changed.join("Album").join("b.bin"), vec![0u8; 3 * PIECE]).unwrap();
    client.seed(DownloadId::new(9), &link, &changed.join("Album"), None, ByteCount::ZERO).unwrap();
    watch_until(&client, "it stops once checked", || !client.inner().seeding.contains_key(&link.info_hash));
    assert_eq!(std::fs::read(changed.join("Album").join("b.bin")).unwrap(), vec![0u8; 3 * PIECE], "nothing written");
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
    assert_eq!(used, ["udp://127.0.0.1:6969/announce", "udp://[::1]:6969"]);
    let states: Vec<TrackerState> = shown.iter().map(|t| t.state).collect();
    use TrackerState::{Skipped, Used};
    // A name the resolver cannot give is left out, and so is every HTTP
    // tracker: librqbit's client would follow a redirect to a name.
    assert_eq!(states, [Used, Skipped, Skipped, Skipped, Used]);
    // The system's resolver: every tracker as it is.
    assert_eq!(trackers_for(&trackers, &Route::default()).0, trackers);
}

#[cfg(windows)]
#[test]
fn torrents_refuse_to_run_bound_on_windows() {
    let (route, _) = crate::route::tests::bound_to_loopback();
    let dir = scratch("windows_bound");
    let (client, _rx, _) = client(&dir);
    assert!(matches!(client.live(&route), Err(EngineError::Torrent(text)) if text == WINDOWS_BOUND));
}

/// The device a socket is bound to (`SO_BINDTODEVICE`), for each of this
/// process's IPv4 sockets on `port`.
#[cfg(target_os = "linux")]
fn devices_on_port(port: u16) -> Vec<String> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc/self/fd").unwrap().flatten() {
        let Ok(fd) = entry.file_name().to_string_lossy().parse::<i32>() else { continue };
        // SAFETY: getsockname and getsockopt only write into the buffers
        // given, with their lengths; a descriptor that is no socket (or was
        // closed since) makes them fail, which is skipped.
        unsafe {
            let mut addr: libc::sockaddr_in = std::mem::zeroed();
            let mut len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
            if libc::getsockname(fd, (&raw mut addr).cast(), &mut len) != 0
                || i32::from(addr.sin_family) != libc::AF_INET
                || u16::from_be(addr.sin_port) != port
            {
                continue;
            }
            let mut name = [0u8; 32];
            let mut len = name.len() as libc::socklen_t;
            if libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_BINDTODEVICE, name.as_mut_ptr().cast(), &mut len) == 0 {
                let end = name.iter().position(|b| *b == 0).unwrap_or(len as usize);
                found.push(String::from_utf8_lossy(&name[..end]).into_owned());
            }
        }
    }
    found
}

#[cfg(target_os = "linux")]
#[test]
fn a_session_bound_to_an_adapter_binds_its_sockets_and_stops_with_it() {
    let (route, stand_in) = crate::route::tests::bound_to_loopback();
    let device = stand_in.adapter.lock().unwrap().id.as_str().to_string();
    let dir = scratch("bound");
    let (client, _rx, _) = client(&dir);
    let (session, generation) = client.live(&route).unwrap();
    let port = session.listen_addr().unwrap().port();
    let devices = devices_on_port(port);
    assert!(!devices.is_empty() && devices.iter().all(|d| *d == device), "the listener is bound: {devices:?}");
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

#[cfg(target_os = "linux")]
#[test]
fn a_torrent_waits_to_seed_until_its_adapter_is_up() {
    let (_seeder, magnet, bytes, seed_dir) = seeder("seed_wait");
    let (route, stand_in) = crate::route::tests::bound_to_loopback();
    stand_in.gone.store(true, Ordering::SeqCst);
    let dir = scratch("wait_for_adapter");
    let live = route.clone();
    let (client, _rx, _) = client_with(&dir, local(), Arc::new(move || live.clone()), Arc::new(quiet));
    let link = MagnetLink::parse(&magnet).unwrap();
    client.add_file(&bytes).unwrap();
    client.seed(DownloadId::new(3), &link, &seed_dir.join("Album"), None, ByteCount::ZERO).unwrap();
    client.watch_seeding();
    assert!(client.active() && client.seeding().is_empty() && client.inner().live.is_none(), "it waits");
    stand_in.gone.store(false, Ordering::SeqCst);
    watch_until(&client, "it seeds once the adapter is up", || !client.seeding().is_empty());
}

#[test]
fn dns_over_https_builds_a_session_without_asking_the_system() {
    let endpoint = udm_domain::network::DohEndpoint {
        url: "https://127.0.0.1:9/dns-query".into(),
        host: "127.0.0.1".into(),
        bootstrap: Vec::new(),
    };
    let doh = Route { binding: None, dns: Some(Arc::new(crate::doh::Doh::new(endpoint, None))) };
    let dir = scratch("doh_session");
    let with_dht = || TorrentSettings { dht: true, ..quiet() };
    // librqbit's own bootstrap names, looked up through the resolver (which
    // does not answer here): the DHT starts with none, and nothing panics.
    let tuning = Tuning { dht_bootstrap: None, ..local() };
    let route = doh.clone();
    let (client, _rx, _) = client_with(&dir, tuning, Arc::new(move || route.clone()), Arc::new(with_dht));
    let (session, _) = client.live(&doh).unwrap();
    assert!(session.listen_addr().is_some());
    assert!(client.metadata(&MagnetLink::new(InfoHash::from_bytes([3; 20])), Duration::from_secs(1)).is_err());
}

#[test]
fn a_change_of_settings_moves_seeding_to_a_new_session_and_an_idle_one_closes() {
    let (_seeder, magnet, bytes, seed_dir) = seeder("seed_refresh");
    let dir = scratch("refresh");
    let settings = Arc::new(Mutex::new(quiet()));
    let source = Arc::clone(&settings);
    let tuning = Tuning { idle_session: Duration::ZERO, ..local() };
    let (client, _rx, _) =
        client_with(&dir, tuning, Arc::new(Route::default), Arc::new(move || source.lock().unwrap().clone()));
    let link = MagnetLink::parse(&magnet).unwrap();
    client.add_file(&bytes).unwrap();
    client.seed(DownloadId::new(4), &link, &seed_dir.join("Album"), None, ByteCount::ZERO).unwrap();
    watch_until(&client, "it seeds", || !client.seeding().is_empty());
    let before = client.generation.load(Ordering::SeqCst);
    settings.lock().unwrap().max_peers = Some(10);
    client.refresh();
    let generation = client.generation.load(Ordering::SeqCst);
    assert!(client.listen_addr().is_some());
    assert!(generation > before, "a new session");
    watch_until(&client, "it seeds there, checked again", || !client.seeding().is_empty());
    assert!(client.listen_addr().is_some(), "a seeding torrent keeps the session up");
    client.stop_seeding(link.info_hash);
    client.watch_seeding();
    client.watch_seeding();
    assert_eq!(client.listen_addr(), None, "nothing needs it: it closes");
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
    let transfers_store = Arc::new(store);
    let transfers =
        Arc::new(crate::transfers::Transfers::new(Arc::clone(&transfers_store), events, live, Arc::default(), next_id));
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

    // The scheduler's seeding limits against the real client: a torrent
    // seeding (from its files) for longer than its own limit stops.
    use udm_application::torrents::ApplySeedLimits;
    use udm_domain::ports::DownloadList as _;
    let row_id = DownloadId::new(1);
    let mut row: udm_domain::download::DownloadEntry = serde_json::from_value(serde_json::json!({
        "id": 1, "url": magnet.to_uri(), "status": "done", "connections": 1, "done": 0, "resumable": true,
        "path": dir.join("source").join("Album"),
    }))
    .unwrap();
    row.set_torrent_info(&info);
    if let Some(t) = row.torrent.as_mut() {
        t.phase = TorrentPhase::Seeding;
        t.seeding_since = Some(1_000);
        t.seed_limits = Some(udm_domain::torrent::SeedLimits { ratio: None, minutes: Some(1) });
    }
    transfers_store.publish(&crate::protocol::Outgoing::Updated { item: row });
    client.seed(row_id, &magnet, &dir.join("source").join("Album"), None, ByteCount::ZERO).unwrap();
    watch_until(&client, "it seeds", || !client.seeding().is_empty());
    struct At(u64);
    impl udm_domain::ports::Timestamps for At {
        fn unix_now(&self) -> u64 {
            self.0
        }
    }
    let early = ApplySeedLimits::new(&*transfers_store, &desk, &*transfers, &At(1_030)).run();
    assert!(early.is_empty(), "half a minute: it seeds on");
    let stopped = ApplySeedLimits::new(&*transfers_store, &desk, &*transfers, &At(1_060)).run();
    assert_eq!(stopped, [row_id]);
    assert!(client.seeding().is_empty());
    let phase = transfers_store.entry(row_id).and_then(|e| e.torrent).map(|t| t.phase);
    assert_eq!(phase, Some(TorrentPhase::Complete));

    // A page that is no torrent file.
    let page = TestServer::start(b"<html>not a torrent</html>".to_vec());
    let refused = desk.fetch_file(&Download { url: page.url.clone().into(), ..Download::default() });
    assert!(matches!(refused, Err(TorrentsError::Torrent(TorrentError::BadTorrent(_)))));
}
