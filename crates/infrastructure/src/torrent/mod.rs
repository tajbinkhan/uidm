//! Torrents and magnet links, through librqbit: one BitTorrent session on
//! the engine's shared tokio runtime (`engine::http::runtime`), built for
//! the network setting and the torrent settings in force and built again
//! when either changes.
//!
//! A torrent downloads on an engine thread like any download
//! ([`TorrentClient::run`], which the engine calls for a magnet link), so it
//! counts as a running download, pauses, resumes and is stopped by the kill
//! switch there. Once it finishes it seeds in the session, outside the
//! engine, until the scheduler's pass stops it at its limits
//! (`ApplySeedLimits`); [`TorrentDesk`] is the `Torrents` port for that and
//! the rest.
//!
//! What UDM keeps: each torrent's `.torrent` file in `paths::torrents_dir()`
//! (written by UDM, named by info hash), and in its `session` folder
//! librqbit's resume data (which pieces are here), so a torrent resumes
//! after a restart without checking all its data again. The torrents the
//! session itself remembers are dropped at each start: the list decides what
//! runs, not the library.
//!
//! The network binding (`docs/network.md`): with an adapter chosen, the
//! session binds every socket it makes (the listener, outgoing peer
//! connections, the DHT's and the UDP trackers' sockets, the HTTP trackers'
//! client, LSD and UPnP) to that adapter by name, which librqbit does on
//! Linux and macOS; on Windows it cannot, so torrents refuse to run while an
//! adapter is chosen there. UPnP and LSD are off while bound in any case.
//! With DNS over HTTPS chosen, no tracker or DHT bootstrap name goes to the
//! system's resolver: UDP trackers and the DHT's bootstrap nodes are looked
//! up through UDM's resolver and handed to librqbit as addresses, and HTTP
//! trackers with a name (whose lookups librqbit's own client would make)
//! are left out.

pub mod metainfo;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use librqbit::dht::{DhtPersistenceConfig, Id20};
use librqbit::limits::LimitsConfig;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ConnectionOptions, DhtSessionConfig, ListenerMode,
    ListenerOptions, ManagedTorrent, Session, SessionOptions, SessionPersistenceConfig, TorrentStatsState,
};
use udm_domain::bytes::ByteCount;
use udm_domain::download::Download;
use udm_domain::id::DownloadId;
use udm_domain::ports::{Seeding, Torrents, TorrentsError};
use udm_domain::settings::TorrentSettings;
use udm_domain::sync::LockExt;
use udm_domain::torrent::{
    FileSelection, InfoHash, MagnetLink, PeerInfo, PieceMap, TorrentError, TorrentInfo, TorrentPhase, TorrentStats,
    TrackerInfo, TrackerState, MAX_LISTED_PEERS, MAX_METAINFO_BYTES,
};

use crate::engine::{self, EngineError, Progress, Saved};
use crate::route::Route;
use metainfo::Metainfo;

/// How long asking peers for a magnet link's metadata may take before the
/// save-as dialog gives up.
pub const METADATA_WAIT: Duration = Duration::from_secs(3 * 60);
/// How long a download of a magnet link waits for its metadata before it
/// fails (and a queue may try it again).
const RUN_METADATA_WAIT: Duration = Duration::from_secs(10 * 60);
/// How often a running torrent's progress is looked at.
const TICK: Duration = Duration::from_millis(500);
/// How often its stats go to the window.
const STATS_EVERY: Duration = Duration::from_secs(1);
/// How often the seeding torrents are looked at.
const SEED_EVERY: Duration = Duration::from_secs(2);
/// Peers from a magnet link tried first, at most.
const MAX_INITIAL_PEERS: usize = 50;

/// What a download of a torrent says when it cannot be bound on this system.
pub const WINDOWS_BOUND: &str = "Torrents cannot be bound to a network adapter on Windows, so they do not run while \
     one is chosen. Choose Any network in Options > Network to download torrents.";

/// Where the client's events go: the engine's progress sink.
pub type Sink = Arc<dyn Fn(Progress) + Send + Sync>;
/// How connections go out now.
pub type RouteSource = Arc<dyn Fn() -> Route + Send + Sync>;
/// The torrent settings now.
pub type SettingsSource = Arc<dyn Fn() -> TorrentSettings + Send + Sync>;

/// What tests change: where the session listens, and the DHT's bootstrap
/// nodes.
#[derive(Debug, Clone)]
pub struct Tuning {
    pub listen_ip: IpAddr,
    /// `None` uses librqbit's (by name, through the system's resolver,
    /// unless DNS over HTTPS is chosen).
    pub dht_bootstrap: Option<Vec<String>>,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning { listen_ip: IpAddr::V6(Ipv6Addr::UNSPECIFIED), dht_bootstrap: None }
    }
}

/// The settings a session is built for: a change of any rebuilds it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionKey {
    adapter: Option<String>,
    doh: bool,
    port: Option<u16>,
    dht: bool,
    upnp: bool,
    local_peers: bool,
    peers: Option<u32>,
}

impl SessionKey {
    fn of(route: &Route, s: &TorrentSettings) -> Self {
        SessionKey {
            adapter: route.binding.as_ref().map(|b| b.choice.id.as_str().to_string()),
            doh: route.dns.is_some(),
            port: s.listen_port(),
            dht: s.dht,
            upnp: s.upnp,
            local_peers: s.local_peers,
            peers: s.peers(),
        }
    }
}

/// The session in use.
struct Live {
    session: Arc<Session>,
    key: SessionKey,
    generation: u64,
    /// Set by the adapter watcher when the adapter the session is bound to
    /// goes.
    down: Arc<AtomicBool>,
    _watch: Option<crate::route::Watch>,
}

/// A finished torrent seeding in the session.
struct Seed {
    id: DownloadId,
    url: String,
    handle: Arc<ManagedTorrent>,
    /// Where its files are, and which files it has, to add it again to a
    /// new session.
    folder: PathBuf,
    files: Vec<usize>,
    trackers: Vec<TrackerInfo>,
    /// Its data was found complete (a torrent seeded after a restart is
    /// checked first).
    checked: bool,
}

/// A seeding torrent, as `watch_seeding` looks at it without the lock.
struct SeedView {
    id: DownloadId,
    url: String,
    handle: Arc<ManagedTorrent>,
    trackers: Vec<TrackerInfo>,
    checked: bool,
}

/// What a torrent uploaded: librqbit counts per run, UDM over all of them.
#[derive(Default, Clone, Copy)]
struct Uploads {
    base: u64,
    last: u64,
}

impl Uploads {
    /// The total, given librqbit's count for this run now.
    fn total(&mut self, now: u64) -> u64 {
        if now < self.last {
            self.base += self.last;
        }
        self.last = now;
        self.base + now
    }

    /// A new run starts counting from 0.
    fn new_run(&mut self) {
        self.base += self.last;
        self.last = 0;
    }
}

#[derive(Default)]
struct Inner {
    live: Option<Live>,
    seeding: HashMap<InfoHash, Seed>,
    uploads: HashMap<InfoHash, Uploads>,
}

/// The BitTorrent client: the session, the torrents' kept files, and the
/// seeding torrents.
pub struct TorrentClient {
    dir: PathBuf,
    tuning: Tuning,
    inner: Mutex<Inner>,
    sink: OnceLock<Sink>,
    route: OnceLock<RouteSource>,
    settings: OnceLock<SettingsSource>,
    /// Bytes per second down (the engine's total limit) and up (the
    /// settings').
    limits: Mutex<(Option<u64>, Option<u64>)>,
    generation: AtomicU64,
}

/// A failure of librqbit's, as text.
fn failed(e: impl std::fmt::Display) -> EngineError {
    EngineError::Torrent(format!("{e:#}"))
}

fn torrent_error(e: TorrentError) -> EngineError {
    EngineError::Torrent(e.to_string())
}

/// The least a torrent's speed may be limited to: librqbit asks its limiter
/// for a whole 16 KiB block at once, which a lower limit never grants.
const MIN_LIMIT: u64 = 16 * 1024;

/// A limit in bytes per second, as librqbit takes it: at least
/// [`MIN_LIMIT`].
fn limit(bytes_per_sec: Option<u64>) -> Option<NonZeroU32> {
    bytes_per_sec.filter(|b| *b > 0).and_then(|b| NonZeroU32::new(u32::try_from(b.max(MIN_LIMIT)).unwrap_or(u32::MAX)))
}

fn id20(hash: InfoHash) -> Id20 {
    Id20::new(hash.bytes())
}

impl TorrentClient {
    /// A client keeping its files in `dir` (`paths::torrents_dir()`).
    pub fn new(dir: PathBuf) -> Self {
        Self::tuned(dir, Tuning::default())
    }

    pub fn tuned(dir: PathBuf, tuning: Tuning) -> Self {
        TorrentClient {
            dir,
            tuning,
            inner: Mutex::default(),
            sink: OnceLock::new(),
            route: OnceLock::new(),
            settings: OnceLock::new(),
            limits: Mutex::new((None, None)),
            generation: AtomicU64::new(0),
        }
    }

    /// Send the client's events (seeding torrents' stats) to `sink`, take
    /// the network setting from `route` and the torrent settings from
    /// `settings`. Only the first call counts.
    pub fn connect(&self, sink: Sink, route: RouteSource, settings: SettingsSource) {
        let _ = self.sink.set(sink);
        let _ = self.route.set(route);
        let _ = self.settings.set(settings);
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock_or_recover()
    }

    fn report(&self, p: Progress) {
        if let Some(sink) = self.sink.get() {
            sink(p);
        }
    }

    fn settings(&self) -> TorrentSettings {
        self.settings.get().map(|s| s()).unwrap_or_default()
    }

    /// How connections go out now.
    pub fn route(&self) -> Route {
        self.route.get().map(|r| r()).unwrap_or_default()
    }

    fn session_dir(&self) -> PathBuf {
        self.dir.join("session")
    }

    fn kept_path(&self, hash: InfoHash) -> PathBuf {
        self.dir.join(format!("{}.torrent", hash.hex()))
    }

    /// The torrent's kept `.torrent` file, read.
    fn kept(&self, hash: InfoHash) -> Option<Metainfo> {
        let bytes = std::fs::read(self.kept_path(hash)).ok()?;
        Metainfo::read(&bytes).ok().filter(|m| m.info.info_hash == hash)
    }

    /// Keep `bytes`, the torrent's `.torrent` file, written whole or not at
    /// all.
    fn keep(&self, meta: &Metainfo) -> Result<(), EngineError> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.kept_path(meta.info.info_hash);
        let temp = path.with_extension("torrent.part");
        std::fs::write(&temp, metainfo::build(&meta.raw_info, &meta.trackers))?;
        std::fs::rename(&temp, &path)?;
        Ok(())
    }

    /// Read `bytes` as a `.torrent` file and keep it.
    pub fn add_file(&self, bytes: &[u8]) -> Result<Metainfo, EngineError> {
        let meta = Metainfo::read(bytes).map_err(torrent_error)?;
        self.keep(&meta)?;
        Ok(meta)
    }

    /// Set the total download limit (the engine's) and upload limit, in
    /// bytes per second.
    pub fn set_limits(&self, down: Option<u64>, up: Option<u64>) {
        *self.limits.lock_or_recover() = (down, up);
        if let Some(live) = self.inner().live.as_ref() {
            live.session.ratelimits.set_download_bps(limit(down));
            live.session.ratelimits.set_upload_bps(limit(up));
        }
    }

    /// Set the download limit alone, keeping the upload limit.
    pub fn set_download_limit(&self, down: Option<u64>) {
        let up = self.limits.lock_or_recover().1;
        self.set_limits(down, up);
    }

    /// The session for `route` and the settings now, built when there is
    /// none or it was built for others (the seeding torrents move to the
    /// new one). Fails on Windows while an adapter is chosen.
    fn live(&self, route: &Route) -> Result<(Arc<Session>, u64), EngineError> {
        if cfg!(windows) && route.binding.is_some() {
            return Err(EngineError::Torrent(WINDOWS_BOUND.to_string()));
        }
        let settings = self.settings();
        let key = SessionKey::of(route, &settings);
        let mut inner = self.inner();
        if let Some(live) = inner.live.as_ref().filter(|l| l.key == key && !l.down.load(Ordering::SeqCst)) {
            return Ok((Arc::clone(&live.session), live.generation));
        }
        let runtime = engine::http::runtime()?;
        if let Some(old) = inner.live.take() {
            tracing::info!("the torrent session starts again for new settings");
            runtime.block_on(old.session.stop());
        }
        let session = runtime.block_on(self.build(route, &settings))?;
        let down = Arc::new(AtomicBool::new(false));
        let watch = match &route.binding {
            Some(binding) => Some(crate::route::Watch::start(binding, &down, &Arc::new(AtomicBool::new(false)))?),
            None => None,
        };
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        inner.live = Some(Live { session: Arc::clone(&session), key, generation, down, _watch: watch });
        // The seeding torrents go on in the new session.
        let seeds: Vec<(InfoHash, Seed)> = inner.seeding.drain().collect();
        for (hash, seed) in seeds {
            let Some(meta) = self.kept(hash) else { continue };
            inner.uploads.entry(hash).or_default().new_run();
            let (bytes, trackers) = prepared(&meta, route);
            let options = self.options(&seed.folder, seed.files.clone(), Vec::new(), None, &settings);
            match runtime.block_on(add(&session, bytes, options)) {
                Ok(handle) => {
                    inner.seeding.insert(hash, Seed { handle, trackers, ..seed });
                }
                Err(e) => {
                    tracing::warn!(download_id = seed.id.get(), error = %e, "a torrent stopped seeding");
                    self.report_phase(seed.id, &seed.url, TorrentPhase::Complete);
                }
            }
        }
        drop(inner);
        Ok((session, generation))
    }

    /// A new session for `route` and `settings`.
    async fn build(&self, route: &Route, settings: &TorrentSettings) -> Result<Arc<Session>, EngineError> {
        let session_dir = self.session_dir();
        std::fs::create_dir_all(&session_dir)?;
        // The list decides what runs: the session starts with no torrents.
        // Their resume data (`<hash>.bitv`) stays.
        let db = session_dir.join("session.json");
        std::fs::write(&db, br#"{"torrents":{}}"#)?;
        let bound = route.binding.is_some();
        let bootstrap = match &route.dns {
            Some(_) => Some(bootstrap_through(route)),
            None => self.tuning.dht_bootstrap.clone(),
        };
        let port = settings.listen_port();
        let (down, up) = *self.limits.lock_or_recover();
        // A system without IPv6 (Linux booted with ipv6.disable=1) listens
        // and connects over IPv4 alone.
        let listen_ip = match self.tuning.listen_ip {
            IpAddr::V6(_) if !ipv6_available() => IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
            ip => ip,
        };
        let options = SessionOptions {
            dht: settings.dht.then(|| DhtSessionConfig {
                bootstrap_addrs: bootstrap,
                port,
                persistence: Some(DhtPersistenceConfig {
                    dump_interval: None,
                    config_filename: Some(session_dir.join("dht.json")),
                }),
            }),
            bind_device_name: route.binding.as_ref().map(|b| b.choice.id.as_str().to_string()),
            fastresume: true,
            persistence: Some(SessionPersistenceConfig::Json { folder: Some(session_dir.clone()) }),
            listen: Some(ListenerOptions {
                mode: ListenerMode::TcpOnly,
                listen_addr: SocketAddr::new(listen_ip, port.unwrap_or(0)),
                enable_upnp_port_forwarding: settings.upnp && !bound,
                ipv4_only: listen_ip.is_ipv4(),
                ..ListenerOptions::default()
            }),
            connect: Some(ConnectionOptions { proxy_url: None, enable_tcp: true, peer_opts: None }),
            concurrent_init_limit: Some(2),
            runtime_worker_threads: Some(2),
            ratelimits: LimitsConfig { download_bps: limit(down), upload_bps: limit(up) },
            peer_limit: settings.peers().map(|p| p as usize),
            disable_local_service_discovery: !settings.local_peers || bound,
            ipv4_only: listen_ip.is_ipv4(),
            ..SessionOptions::default()
        };
        let session = Session::new_with_opts(self.dir.clone(), options).await.map_err(failed)?;
        tracing::info!(listen = ?session.listen_addr(), bound, doh = route.dns.is_some(), "torrent session ready");
        Ok(session)
    }

    /// The options to add a torrent with: into `folder`, only `files`.
    fn options(
        &self,
        folder: &Path,
        files: Vec<usize>,
        peers: Vec<SocketAddr>,
        own_limit: Option<u64>,
        settings: &TorrentSettings,
    ) -> AddTorrentOptions {
        AddTorrentOptions {
            only_files: Some(files),
            overwrite: true,
            output_folder: Some(folder.to_string_lossy().into_owned()),
            initial_peers: Some(peers),
            peer_limit: settings.peers().map(|p| p as usize),
            ratelimits: LimitsConfig { download_bps: limit(own_limit), upload_bps: None },
            ..AddTorrentOptions::default()
        }
    }

    /// The torrent's metadata: kept, else asked of its peers (from its
    /// magnet link) and kept. `Ok(None)` when `stop` says to stop first.
    fn metadata_for(
        &self,
        session: &Arc<Session>,
        magnet: &MagnetLink,
        route: &Route,
        wait: Duration,
        stop: &dyn Fn() -> bool,
    ) -> Result<Option<Metainfo>, EngineError> {
        if let Some(kept) = self.kept(magnet.info_hash) {
            return Ok(Some(kept));
        }
        let (trackers, _) = trackers_for(&magnet.trackers, route);
        let asked = MagnetLink { trackers, web_seeds: Vec::new(), ..magnet.clone() }.to_uri();
        let options = AddTorrentOptions {
            list_only: true,
            initial_peers: Some(initial_peers(magnet, route)),
            ..AddTorrentOptions::default()
        };
        let runtime = engine::http::runtime()?;
        let deadline = Instant::now() + wait;
        let found = runtime.block_on(async {
            let asking = session.add_torrent(AddTorrent::from_url(asked), Some(options));
            tokio::select! {
                found = asking => Some(found),
                () = wait_until(stop, deadline) => None,
            }
        });
        let response = match found {
            None if stop() => return Ok(None),
            None => {
                return Err(EngineError::Torrent(format!(
                    "No peer sent the torrent's metadata within {} minutes.",
                    wait.as_secs() / 60
                )))
            }
            Some(response) => response.map_err(failed)?,
        };
        let AddTorrentResponse::ListOnly(listed) = response else {
            return Err(EngineError::Torrent("the torrent was added instead of read".into()));
        };
        if listed.torrent_bytes.len() > MAX_METAINFO_BYTES {
            return Err(torrent_error(TorrentError::TooBig { max: MAX_METAINFO_BYTES }));
        }
        // Kept with the magnet link's own trackers, whatever was left out
        // now, so a later change of the setting can use them.
        let read = Metainfo::read(&listed.torrent_bytes).map_err(torrent_error)?;
        let meta = Metainfo::read(&metainfo::build(&read.raw_info, &magnet.trackers)).map_err(torrent_error)?;
        if meta.info.info_hash != magnet.info_hash {
            return Err(EngineError::Torrent("the peers sent another torrent's metadata".into()));
        }
        self.keep(&meta)?;
        Ok(Some(meta))
    }

    /// What the torrent at `magnet` holds, asking its peers through the
    /// session for the network setting now, for at most `wait`.
    pub fn metadata(&self, magnet: &MagnetLink, wait: Duration) -> Result<Metainfo, EngineError> {
        if let Some(kept) = self.kept(magnet.info_hash) {
            return Ok(kept);
        }
        let route = self.route();
        route.ready()?;
        let (session, _) = self.live(&route)?;
        self.metadata_for(&session, magnet, &route, wait, &|| false)?
            .ok_or_else(|| EngineError::Torrent("stopped".into()))
    }

    /// Download the torrent at `req`'s magnet link on this (an engine)
    /// thread until it finishes, then leave it seeding. Stops when `token`
    /// is set, as every download does, reporting through `report`.
    pub(crate) fn run(
        &self,
        id: DownloadId,
        token: &AtomicBool,
        req: &engine::DownloadRequest,
        report: &dyn Fn(Progress),
    ) -> Result<Saved, EngineError> {
        let magnet = MagnetLink::parse(&req.url).map_err(torrent_error)?;
        let hash = magnet.info_hash;
        let url = req.url.clone();
        let route = &req.net.route;
        let stopped = || token.load(Ordering::SeqCst);
        let mut told = false;
        loop {
            let (session, generation) = self.live(route)?;
            if self.kept(hash).is_none() {
                report(Progress::TorrentStats { id, url: url.clone(), stats: TorrentStats::default() });
            }
            let Some(meta) = self.metadata_for(&session, &magnet, route, RUN_METADATA_WAIT, &stopped)? else {
                return Ok(Saved { path: PathBuf::new(), note: None, complete: false });
            };
            if !told {
                report(Progress::TorrentInfo { id, url: url.clone(), info: meta.info.clone() });
                told = true;
            }
            let root = root_of(req, &meta.info);
            let folder =
                if meta.info.single_file() { root.parent().map(Path::to_path_buf) } else { Some(root.clone()) }
                    .ok_or_else(|| EngineError::Torrent("the torrent has no folder to go in".into()))?;
            engine::writable(&folder)?;
            let files = selection(req.torrent_files.as_ref(), &meta.info);
            let size = meta.info.files.selected_size(FileSelection::try_from(files.clone()).ok().as_ref());
            let (bytes, trackers) = prepared(&meta, route);
            let options =
                self.options(&folder, files.clone(), initial_peers(&magnet, route), req.speed_limit, &self.settings());
            let runtime = engine::http::runtime()?;
            {
                let mut inner = self.inner();
                inner.seeding.remove(&hash);
                let uploads = inner.uploads.entry(hash).or_default();
                uploads.new_run();
                if uploads.base == 0 {
                    uploads.base = req.torrent_uploaded.get();
                }
            }
            let handle = runtime.block_on(add(&session, bytes, options)).map_err(EngineError::Torrent)?;
            // Already in the session (seeding, or stopped before): it takes
            // the files chosen now, and goes on.
            let wanted: HashSet<usize> = files.iter().copied().collect();
            if handle.only_files().is_some_and(|had| had.iter().copied().collect::<HashSet<_>>() != wanted) {
                if let Err(e) = runtime.block_on(session.update_only_files(&handle, &wanted)) {
                    tracing::debug!(error = %format!("{e:#}"), "the torrent keeps its files until it is checked");
                }
            }
            if handle.is_paused() {
                runtime.block_on(session.unpause(&handle)).map_err(failed)?;
            }
            report(Progress::Started {
                id,
                url: url.clone(),
                path: root.clone(),
                total: Some(size),
                resumable: true,
                connections: 0,
                partial: None,
                validator: None,
                country: None,
            });
            let follow = Follow { id, url: &url, hash, handle: &handle, trackers: &trackers };
            match self.follow(&follow, generation, token, report) {
                Followed::Finished => {
                    let seed = Seed {
                        id,
                        url: url.clone(),
                        handle: Arc::clone(&handle),
                        folder,
                        files,
                        trackers,
                        checked: true,
                    };
                    self.inner().seeding.insert(hash, seed);
                    return Ok(Saved { path: root, note: None, complete: true });
                }
                Followed::Stopped => {
                    if let Err(e) = runtime.block_on(session.pause(&handle)) {
                        tracing::debug!(error = %format!("{e:#}"), "the torrent was not running");
                    }
                    return Ok(Saved { path: root, note: None, complete: false });
                }
                Followed::Moved => continue,
                Followed::Failed(error) => return Err(EngineError::Torrent(error)),
            }
        }
    }

    /// Watch a running torrent until it finishes, stops or fails, or the
    /// session is replaced.
    fn follow(&self, f: &Follow<'_>, generation: u64, token: &AtomicBool, report: &dyn Fn(Progress)) -> Followed {
        let mut last_done = u64::MAX;
        let mut last_stats: Option<Instant> = None;
        let mut peers = PeerSpeeds::default();
        loop {
            if token.load(Ordering::SeqCst) {
                return Followed::Stopped;
            }
            if self.generation.load(Ordering::SeqCst) != generation {
                return Followed::Moved;
            }
            let st = f.handle.stats();
            if let Some(error) = st.error.clone() {
                return Followed::Failed(error);
            }
            if st.progress_bytes != last_done {
                last_done = st.progress_bytes;
                report(Progress::Advanced {
                    id: f.id,
                    url: f.url.to_string(),
                    done: ByteCount::new(st.progress_bytes),
                    total: Some(ByteCount::new(st.total_bytes)),
                    parts: None,
                    segments: None,
                });
            }
            let finished = st.finished && matches!(st.state, TorrentStatsState::Live);
            if finished || last_stats.is_none_or(|t| t.elapsed() >= STATS_EVERY) {
                last_stats = Some(Instant::now());
                let stats = self.stats_of(f.hash, f.handle, &st, f.trackers, &mut peers);
                report(Progress::TorrentStats { id: f.id, url: f.url.to_string(), stats });
            }
            if finished {
                return Followed::Finished;
            }
            if matches!(st.state, TorrentStatsState::Paused) {
                // Paused without being stopped: the session went.
                return Followed::Moved;
            }
            sleep_unless(token, TICK);
        }
    }

    /// The window's view of a torrent.
    fn stats_of(
        &self,
        hash: InfoHash,
        handle: &Arc<ManagedTorrent>,
        st: &librqbit::TorrentStats,
        trackers: &[TrackerInfo],
        speeds: &mut PeerSpeeds,
    ) -> TorrentStats {
        let phase = match st.state {
            TorrentStatsState::Initializing { .. } => TorrentPhase::Checking,
            TorrentStatsState::Live if st.finished => TorrentPhase::Seeding,
            TorrentStatsState::Paused if st.finished => TorrentPhase::Complete,
            _ => TorrentPhase::Downloading,
        };
        let uploaded = self.inner().uploads.entry(hash).or_default().total(st.uploaded_bytes);
        let (down_speed, up_speed) =
            st.live.as_ref().map_or((0, 0), |l| (l.download_speed.as_bytes(), l.upload_speed.as_bytes()));
        let left = st.total_bytes.saturating_sub(st.progress_bytes);
        let eta_seconds = (down_speed > 0 && left > 0).then(|| left.div_ceil(down_speed));
        let (peers_connected, peers_known) =
            st.live.as_ref().map_or((0, 0), |l| (l.snapshot.peer_stats.live, l.snapshot.peer_stats.seen));
        let peers = handle.live().map(|live| {
            let snapshot = live.per_peer_stats_snapshot(Default::default());
            speeds.list(snapshot.peers.into_iter().map(|(address, p)| PeerCount {
                address,
                downloaded: p.counters.fetched_bytes,
                uploaded: p.counters.uploaded_bytes,
                client: p.client_name,
                transport: p.conn_kind.map(|k| k.to_string()),
            }))
        });
        let pieces = self.inner().live.as_ref().and_then(|live| {
            let api = librqbit::Api::new(Arc::clone(&live.session), None);
            let (have, count) = api.api_dump_haves(id20(hash).into()).ok()?;
            Some(PieceMap::of(count, |i| have.get(i as usize).is_some_and(|b| *b)))
        });
        TorrentStats {
            phase,
            done: ByteCount::new(st.progress_bytes),
            total: ByteCount::new(st.total_bytes),
            down_speed,
            up_speed,
            uploaded: ByteCount::new(uploaded),
            eta_seconds,
            peers_connected,
            peers_known,
            peers: peers.unwrap_or_default(),
            trackers: trackers.to_vec(),
            pieces: pieces.unwrap_or_default(),
            file_done: st.file_progress.iter().map(|b| ByteCount::new(*b)).collect(),
        }
    }

    fn report_phase(&self, id: DownloadId, url: &str, phase: TorrentPhase) {
        self.report(Progress::TorrentStats {
            id,
            url: url.to_string(),
            stats: TorrentStats { phase, ..TorrentStats::default() },
        });
    }

    /// Seed finished download `id` from its files at `path` (the torrent's
    /// folder, or its one file), with `files` of it.
    pub fn seed(
        &self,
        id: DownloadId,
        magnet: &MagnetLink,
        path: &Path,
        files: Option<&FileSelection>,
        uploaded: ByteCount,
    ) -> Result<(), EngineError> {
        let hash = magnet.info_hash;
        let meta = self.kept(hash).ok_or_else(|| EngineError::Torrent("its torrent file is gone".into()))?;
        let folder = if meta.info.single_file() { path.parent().map(Path::to_path_buf) } else { Some(path.into()) }
            .ok_or_else(|| EngineError::Torrent("it has no folder".into()))?;
        if !path.exists() {
            return Err(EngineError::Torrent(format!("{} is gone", path.display())));
        }
        let route = self.route();
        route.ready()?;
        let (session, _) = self.live(&route)?;
        let files = selection(files, &meta.info);
        let (bytes, trackers) = prepared(&meta, &route);
        let options = self.options(&folder, files.clone(), Vec::new(), None, &self.settings());
        let handle = engine::http::runtime()?.block_on(add(&session, bytes, options)).map_err(EngineError::Torrent)?;
        let mut inner = self.inner();
        let uploads = inner.uploads.entry(hash).or_default();
        *uploads = Uploads { base: uploaded.get(), last: 0 };
        let url = magnet.to_uri();
        inner.seeding.insert(hash, Seed { id, url, handle, folder, files, trackers, checked: false });
        drop(inner);
        Ok(())
    }

    /// Build the session again for the settings now, when there is one, so
    /// the seeding torrents follow a change of them at once (running ones
    /// move when they next look).
    pub fn refresh(&self) {
        if self.inner().live.is_none() {
            return;
        }
        let route = self.route();
        if let Err(e) = route.ready().map_err(EngineError::from).and_then(|()| self.live(&route).map(|_| ())) {
            tracing::warn!(error = %e, "the torrent session did not start again");
            self.watch_down();
        }
    }

    /// Stop the session and every seeding torrent: the network they go out
    /// by is gone, or cannot be used. Running torrents stop through their
    /// own engine runs.
    fn watch_down(&self) {
        let (seeds, live) = {
            let mut inner = self.inner();
            (inner.seeding.drain().collect::<Vec<_>>(), inner.live.take())
        };
        self.generation.fetch_add(1, Ordering::SeqCst);
        if let (Some(live), Ok(runtime)) = (live, engine::http::runtime()) {
            runtime.block_on(live.session.stop());
        }
        for (_, seed) in seeds {
            self.report_phase(seed.id, &seed.url, TorrentPhase::Complete);
        }
    }

    /// The torrents seeding now, with what each uploaded.
    pub fn seeding(&self) -> Vec<Seeding> {
        let mut inner = self.inner();
        let seeds: Vec<(InfoHash, u64)> = inner
            .seeding
            .iter()
            .filter(|(_, s)| s.checked)
            .map(|(h, s)| (*h, s.handle.stats().uploaded_bytes))
            .collect();
        seeds
            .into_iter()
            .map(|(hash, now)| Seeding {
                info_hash: hash,
                uploaded: ByteCount::new(inner.uploads.entry(hash).or_default().total(now)),
            })
            .collect()
    }

    /// Stop seeding the torrent.
    pub fn stop_seeding(&self, hash: InfoHash) {
        let (seed, session) = {
            let mut inner = self.inner();
            let seed = inner.seeding.remove(&hash);
            (seed, inner.live.as_ref().map(|l| Arc::clone(&l.session)))
        };
        let Some(seed) = seed else { return };
        if let (Some(session), Ok(runtime)) = (session, engine::http::runtime()) {
            if let Err(e) = runtime.block_on(session.pause(&seed.handle)) {
                tracing::debug!(error = %format!("{e:#}"), "the torrent was not seeding");
            }
        }
        self.report_phase(seed.id, &seed.url, TorrentPhase::Complete);
    }

    /// Download only `files` of the torrent, at once if it is in the
    /// session.
    pub fn select(&self, hash: InfoHash, files: &FileSelection) -> Result<(), EngineError> {
        let Some(session) = self.inner().live.as_ref().map(|l| Arc::clone(&l.session)) else {
            return Ok(());
        };
        let Some(handle) = session.get(id20(hash).into()) else {
            return Ok(());
        };
        let wanted: HashSet<usize> = files.indexes().collect();
        engine::http::runtime()?.block_on(session.update_only_files(&handle, &wanted)).map_err(failed)
    }

    /// Forget the torrent: out of the session, its kept file and resume
    /// data gone.
    pub fn forget(&self, hash: InfoHash) {
        let session = {
            let mut inner = self.inner();
            inner.seeding.remove(&hash);
            inner.uploads.remove(&hash);
            inner.live.as_ref().map(|l| Arc::clone(&l.session))
        };
        if let (Some(session), Ok(runtime)) = (session, engine::http::runtime()) {
            if session.get(id20(hash).into()).is_some() {
                if let Err(e) = runtime.block_on(session.delete(id20(hash).into(), false)) {
                    tracing::debug!(error = %format!("{e:#}"), "the torrent was not in the session");
                }
            }
        }
        let session_dir = self.session_dir();
        for path in [
            self.kept_path(hash),
            session_dir.join(format!("{}.bitv", hash.hex())),
            session_dir.join(format!("{}.torrent", hash.hex())),
        ] {
            let _ = std::fs::remove_file(path);
        }
    }

    /// One look at the seeding torrents: stop them all if their adapter
    /// went; one whose data turned out incomplete (seeded again after a
    /// restart) stops; the others report how they are doing.
    pub fn watch_seeding(&self) {
        let down = self.inner().live.as_ref().is_some_and(|l| l.down.load(Ordering::SeqCst));
        if down {
            tracing::warn!("the network adapter torrents are bound to is down; seeding stops");
            self.watch_down();
            return;
        }
        let seeds: Vec<(InfoHash, SeedView)> = self
            .inner()
            .seeding
            .iter()
            .map(|(h, s)| {
                let view = SeedView {
                    id: s.id,
                    url: s.url.clone(),
                    handle: Arc::clone(&s.handle),
                    trackers: s.trackers.clone(),
                    checked: s.checked,
                };
                (*h, view)
            })
            .collect();
        for (hash, SeedView { id, url, handle, trackers, checked }) in seeds {
            let st = handle.stats();
            let broken = st.error.is_some()
                || (!checked
                    && matches!(st.state, TorrentStatsState::Live | TorrentStatsState::Paused)
                    && !st.finished);
            if broken {
                tracing::warn!(download_id = id.get(), "its files are not all there; it does not seed");
                self.stop_seeding(hash);
                continue;
            }
            if matches!(st.state, TorrentStatsState::Initializing { .. }) {
                continue;
            }
            if !checked {
                if let Some(seed) = self.inner().seeding.get_mut(&hash) {
                    seed.checked = true;
                }
            }
            let stats = self.stats_of(hash, &handle, &st, &trackers, &mut PeerSpeeds::default());
            self.report(Progress::TorrentStats { id, url, stats });
        }
    }
}

/// The thread that calls `watch_seeding` every [`SEED_EVERY`], for the
/// client the daemon keeps.
pub fn watch_seeding(client: &Arc<TorrentClient>) {
    let weak = Arc::downgrade(client);
    let spawned = std::thread::Builder::new().name("udm-seeding".into()).spawn(move || loop {
        std::thread::sleep(SEED_EVERY);
        match weak.upgrade() {
            Some(client) => client.watch_seeding(),
            None => return,
        }
    });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "could not watch the seeding torrents");
    }
}

/// What following a running torrent came to.
enum Followed {
    Finished,
    Stopped,
    /// The session was replaced: add it to the new one.
    Moved,
    Failed(String),
}

/// A running torrent being followed.
struct Follow<'a> {
    id: DownloadId,
    url: &'a str,
    hash: InfoHash,
    handle: &'a Arc<ManagedTorrent>,
    trackers: &'a [TrackerInfo],
}

/// Each peer's bytes when last looked at, for its speed.
#[derive(Default)]
struct PeerSpeeds {
    last: HashMap<String, (u64, u64, Instant)>,
}

/// One peer's counts, as librqbit gives them.
struct PeerCount {
    address: String,
    downloaded: u64,
    uploaded: u64,
    client: Option<String>,
    transport: Option<String>,
}

impl PeerSpeeds {
    fn list(&mut self, peers: impl Iterator<Item = PeerCount>) -> Vec<PeerInfo> {
        let now = Instant::now();
        let mut list: Vec<PeerInfo> = peers
            .map(|p| {
                let (address, down, up) = (p.address, p.downloaded, p.uploaded);
                let (down_speed, up_speed) = match self.last.get(&address) {
                    Some((d0, u0, at)) => {
                        let secs = now.duration_since(*at).as_secs_f64().max(0.001);
                        (per_sec(down.saturating_sub(*d0), secs), per_sec(up.saturating_sub(*u0), secs))
                    }
                    None => (0, 0),
                };
                self.last.insert(address.clone(), (down, up, now));
                PeerInfo {
                    address,
                    client: p.client,
                    down_speed,
                    up_speed,
                    downloaded: ByteCount::new(down),
                    uploaded: ByteCount::new(up),
                    transport: p.transport,
                }
            })
            .collect();
        list.sort_by(|a, b| (b.down_speed + b.up_speed, &a.address).cmp(&(a.down_speed + a.up_speed, &b.address)));
        list.truncate(MAX_LISTED_PEERS);
        list
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn per_sec(bytes: u64, secs: f64) -> u64 {
    (bytes as f64 / secs) as u64
}

/// Whether the system has IPv6 at all.
fn ipv6_available() -> bool {
    socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::DGRAM, None).is_ok()
}

/// Wait `every`, waking early when `token` is set.
fn sleep_unless(token: &AtomicBool, every: Duration) {
    let until = Instant::now() + every;
    while Instant::now() < until && !token.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Resolves once `stop` says so or `deadline` passes.
async fn wait_until(stop: &dyn Fn() -> bool, deadline: Instant) {
    while !stop() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Add the torrent `bytes` to `session` with `options`; the torrent the
/// session has already, when it has it.
async fn add(
    session: &Arc<Session>,
    bytes: Vec<u8>,
    options: AddTorrentOptions,
) -> Result<Arc<ManagedTorrent>, String> {
    let response =
        session.add_torrent(AddTorrent::from_bytes(bytes), Some(options)).await.map_err(|e| format!("{e:#}"))?;
    response.into_handle().ok_or_else(|| "the torrent was only read".to_string())
}

/// Where download `req` of the torrent `info` goes: where it went before,
/// else its folder (for many files: named as chosen, else as the torrent)
/// or its one file, in the folder for its category.
fn root_of(req: &engine::DownloadRequest, info: &TorrentInfo) -> PathBuf {
    if let Some(path) = &req.reuse_path {
        return path.clone();
    }
    let name = if info.single_file() {
        info.files.as_slice().first().and_then(|f| f.path.first()).cloned().unwrap_or_else(|| info.name.clone())
    } else {
        req.filename
            .as_deref()
            .and_then(udm_domain::filename::sanitize_filename)
            .map(|n| n.into_string())
            .unwrap_or_else(|| info.name.clone())
    };
    req.folders.for_name(&name).join(name)
}

/// The files to download, by number: those chosen that the torrent has,
/// else every one but its padding.
fn selection(chosen: Option<&FileSelection>, info: &TorrentInfo) -> Vec<usize> {
    let all = || info.files.as_slice().iter().enumerate().filter(|(_, f)| !f.padding).map(|(i, _)| i).collect();
    match chosen.and_then(|c| c.within(info.files.len())) {
        Some(c) => c.indexes().collect(),
        None => all(),
    }
}

/// The trackers of `trackers` to announce to through `route`, as they go
/// (a UDP tracker's name looked up through DNS over HTTPS becomes its
/// address), and each one's state for the window. Without DNS over HTTPS
/// every tracker is used as it is.
fn trackers_for(trackers: &[String], route: &Route) -> (Vec<String>, Vec<TrackerInfo>) {
    let mut used = Vec::new();
    let mut shown = Vec::new();
    for t in trackers {
        let state = match tracker_through(t, route) {
            Some(url) => {
                used.push(url);
                TrackerState::Used
            }
            None => TrackerState::Skipped,
        };
        shown.push(TrackerInfo { url: t.clone(), state });
    }
    (used, shown)
}

fn tracker_through(tracker: &str, route: &Route) -> Option<String> {
    if route.dns.is_none() {
        return Some(tracker.to_string());
    }
    let mut url = url::Url::parse(tracker).ok()?;
    let host = url.host_str()?.trim_start_matches('[').trim_end_matches(']').to_string();
    if host.parse::<IpAddr>().is_ok() {
        return Some(tracker.to_string());
    }
    // librqbit looks an HTTP tracker's name up itself, with the system.
    if url.scheme() != "udp" {
        return None;
    }
    let port = url.port()?;
    let ip = route.lookup(&host, port).ok()?.into_iter().map(|a| a.ip()).min_by_key(IpAddr::is_ipv6)?;
    url.set_ip_host(ip).ok()?;
    Some(url.to_string())
}

/// librqbit's DHT bootstrap nodes, looked up through `route`'s DNS over
/// HTTPS; none that cannot be.
fn bootstrap_through(route: &Route) -> Vec<String> {
    librqbit::dht::DHT_BOOTSTRAP
        .iter()
        .filter_map(|node| node.rsplit_once(':'))
        .filter_map(|(host, port)| route.lookup(host, port.parse().ok()?).ok())
        .flatten()
        .map(|a| a.to_string())
        .collect()
}

/// The magnet link's peers to try first, looked up through `route`.
fn initial_peers(magnet: &MagnetLink, route: &Route) -> Vec<SocketAddr> {
    magnet
        .peers
        .iter()
        .filter_map(|p| p.rsplit_once(':'))
        .filter_map(|(host, port)| {
            route.lookup(host.trim_start_matches('[').trim_end_matches(']'), port.parse().ok()?).ok()
        })
        .flatten()
        .take(MAX_INITIAL_PEERS)
        .collect()
}

/// The torrent's file to add, with the trackers `route` lets it use, and
/// every tracker's state.
fn prepared(meta: &Metainfo, route: &Route) -> (Vec<u8>, Vec<TrackerInfo>) {
    let (used, shown) = trackers_for(&meta.trackers, route);
    (metainfo::build(&meta.raw_info, &used), shown)
}

/// The `Torrents` port: the client, with the engine's requests for fetching
/// `.torrent` files from the web.
pub struct TorrentDesk {
    pub client: Arc<TorrentClient>,
    pub transfers: Arc<crate::transfers::Transfers>,
}

fn port_error(e: EngineError) -> TorrentsError {
    TorrentsError::Failed(e.to_string())
}

impl Torrents for TorrentDesk {
    fn add_file(&self, metainfo: &[u8]) -> Result<(MagnetLink, TorrentInfo), TorrentsError> {
        let meta = Metainfo::read(metainfo)?;
        self.client.keep(&meta).map_err(port_error)?;
        Ok((meta.magnet(), meta.info))
    }

    fn fetch_file(&self, download: &Download) -> Result<(MagnetLink, TorrentInfo), TorrentsError> {
        let mut req = self.transfers.request_for(download.clone());
        let net = req.net.with_route(self.transfers.live_route());
        req.net = Arc::new(net);
        let limit = ByteCount::new(MAX_METAINFO_BYTES as u64 + 1);
        let fetched = engine::fetch(&req.url, &req.headers, &req.net, limit).map_err(port_error)?;
        if fetched.body.len() > MAX_METAINFO_BYTES {
            return Err(TorrentError::TooBig { max: MAX_METAINFO_BYTES }.into());
        }
        self.add_file(&fetched.body)
    }

    fn metadata(&self, magnet: &MagnetLink) -> Result<TorrentInfo, TorrentsError> {
        self.client.metadata(magnet, METADATA_WAIT).map(|m| m.info).map_err(port_error)
    }

    fn select(&self, info_hash: InfoHash, files: &FileSelection) -> Result<(), TorrentsError> {
        self.client.select(info_hash, files).map_err(port_error)
    }

    fn seeding(&self) -> Vec<Seeding> {
        self.client.seeding()
    }

    fn seed(
        &self,
        id: DownloadId,
        magnet: &MagnetLink,
        path: &Path,
        files: Option<&FileSelection>,
        uploaded: ByteCount,
    ) -> Result<(), TorrentsError> {
        self.client.seed(id, magnet, path, files, uploaded).map_err(port_error)
    }

    fn stop_seeding(&self, info_hash: InfoHash) {
        self.client.stop_seeding(info_hash);
    }

    fn forget(&self, info_hash: InfoHash) {
        self.client.forget(info_hash);
    }
}
