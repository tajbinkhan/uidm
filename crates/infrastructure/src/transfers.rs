//! `Transfers`, the adapter between the download engine and the rest of the
//! daemon: it owns the engine, turns kept requests into engine requests
//! with the settings' folders, proxy and logins, starts and stops downloads
//! (`DownloadControl`), runs the early downloads started while a save-as
//! dialog is open, checks finished downloads for a newer server copy, and
//! applies new settings (`SettingsStore`). `SyncChecker` runs a
//! synchronizing queue's checks (`SyncChecks`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use udm_domain::address::Link;
use udm_domain::bytes::ByteCount;
use udm_domain::filename::FileName;
use udm_domain::id::{DownloadId, QueueId};
use udm_domain::sync::LockExt;

use tracing::{info, warn};
use udm_domain::download::{Download, DownloadEntry, Status, Validator};
use udm_domain::ports::{
    ControlError, DownloadControl, DownloadList, LimitLift, SettingsError, SettingsStore, SyncChecks,
};

use crate::app::{progress_to_outgoing, to_request};
use crate::connect::{config_for, NetState};
use crate::engine::{self, DownloadRequest, Engine, Progress};
use crate::live_settings::LiveSettings;
use crate::net::{self, ProxyKind};
use crate::protocol::Outgoing;
use crate::settings;
use crate::store::{DownloadStore, EditError, StoreEvents};
use udm_domain::settings::{EngineLimits, FileExists, Folders, Settings};

/// How long answering a save-as dialog waits for the download it started
/// early to stop, before starting over without its bytes.
const EARLY_STOP_WAIT: Duration = Duration::from_secs(15);
/// The folder early downloads are written in, inside the temporary folder
/// when UDM's data folder cannot be had.
const EARLY_FOLDER: &str = "UDM early";

/// Where early downloads go: UDM's own data folder, which only this user
/// can write. The temporary folder is shared by every user on Linux, where
/// another user could read the files or plant a link under the folder.
fn early_root() -> PathBuf {
    crate::paths::data_dir().map(|d| d.join("early")).unwrap_or_else(|_| std::env::temp_dir().join(EARLY_FOLDER))
}

/// The saved file of an early download reported as stopped, if it in fact
/// finished: its file is there at the size the server gave, and neither a
/// partial file nor a resume file is left (a stopped download keeps both).
fn early_finished_anyway(early: &Early) -> Option<&Path> {
    let target = early.target.as_deref()?;
    let total = early.total?;
    let partial_left = early.file.as_deref().is_some_and(|f| f != target && f.exists());
    let finished = !partial_left
        && !engine::sidecar_path(target).exists()
        && early.file.as_deref().is_none_or(|f| !engine::sidecar_path(f).exists())
        && std::fs::metadata(target).is_ok_and(|m| m.is_file() && m.len() == total.get());
    finished.then_some(target)
}

/// Downloads started while their save-as dialog is open, by the engine id
/// they run under, which clients never see.
#[derive(Default)]
pub struct EarlyRuns {
    runs: Mutex<HashMap<DownloadId, Early>>,
}

/// A download started while its save-as dialog is open, into a folder of
/// its own, until the dialog is answered.
pub struct Early {
    /// The download in the list it is for.
    pub real: DownloadId,
    /// Its folder.
    pub dir: PathBuf,
    /// The file it writes, once started.
    pub file: Option<PathBuf>,
    /// The file it saves to when finished, and its size, once known.
    pub target: Option<PathBuf>,
    pub total: Option<ByteCount>,
    /// How it ended: finished (with its file), or stopped or failed.
    pub end: Option<Option<PathBuf>>,
    /// Nobody waits for it: once it ends it goes, with its folder.
    pub discard: bool,
}

impl EarlyRuns {
    /// The early downloads, locked. Each change inserts, removes or edits one
    /// entry, so a lock poisoned by a panicking thread is still read.
    pub fn runs(&self) -> MutexGuard<'_, HashMap<DownloadId, Early>> {
        self.runs.lock_or_recover()
    }

    /// Keep the events of an early download (one started while its save-as
    /// dialog is open) from the clients: its progress shows on the download
    /// waiting in the dialog, in `store`, instead. Returns whether `p` was
    /// one.
    fn event(&self, store: &DownloadStore, p: &Progress) -> bool {
        let id = match p {
            Progress::Started { id, .. }
            | Progress::Advanced { id, .. }
            | Progress::Finished { id, .. }
            | Progress::Failed { id, .. }
            | Progress::Canceled { id, .. }
            | Progress::NameTaken { id, .. }
            | Progress::TorrentInfo { id, .. }
            | Progress::TorrentStats { id, .. } => *id,
        };
        let mut early = self.runs();
        let Some(e) = early.get_mut(&id) else {
            return false;
        };
        let real = e.real;
        let mut shown = None;
        match p {
            Progress::Started { path, partial, total, .. } => {
                e.file = Some(partial.clone().unwrap_or_else(|| path.clone()));
                e.target = Some(path.clone());
                e.total = *total;
                shown = Some((ByteCount::ZERO, *total));
            }
            Progress::Advanced { done, total, .. } => {
                e.total = total.or(e.total);
                shown = Some((*done, *total));
            }
            Progress::Finished { path, .. } => e.end = Some(Some(path.clone())),
            Progress::Failed { .. } | Progress::Canceled { .. } | Progress::NameTaken { .. } => e.end = Some(None),
            // A torrent never starts early.
            Progress::TorrentInfo { .. } | Progress::TorrentStats { .. } => {}
        }
        if e.discard && e.end.is_some() {
            if let Some(gone) = early.remove(&id) {
                let _ = std::fs::remove_dir_all(gone.dir);
            }
            return true;
        }
        drop(early);
        if let Some((done, total)) = shown {
            let _ = store.update(real, |e| {
                if e.status != Status::Confirming {
                    return false;
                }
                e.done = done;
                e.total = total.or(e.total);
                true
            });
        }
        true
    }

    /// The engine id of download `real`'s early download, if it has one.
    pub fn early_of(&self, real: DownloadId) -> Option<DownloadId> {
        self.runs().iter().find(|(_, e)| e.real == real).map(|(id, _)| *id)
    }
}

/// The engine and what drives it.
pub struct Transfers {
    engine: Arc<Engine>,
    store: Arc<DownloadStore>,
    settings: Arc<LiveSettings>,
    retries: Arc<NetState>,
    /// Reaches every client.
    events: Arc<dyn StoreEvents>,
    early: Arc<EarlyRuns>,
    /// The download waiting for a new link from the browser, if any.
    refresh_waiting: Mutex<Option<DownloadId>>,
    /// The speed limit is lifted while nobody uses the computer.
    limit_lifted: AtomicBool,
    /// Asked for the address to download from as each download starts (the
    /// debrid account's direct links); set once at wiring.
    resolver: OnceLock<engine::Resolver>,
    /// Where the engine's events go, for the torrent client's too.
    sink: crate::torrent::Sink,
    /// The BitTorrent client magnet links download through; set once at
    /// wiring.
    torrents: OnceLock<Arc<crate::torrent::TorrentClient>>,
}

impl Transfers {
    /// Start the engine with the settings' limits, numbering new downloads
    /// from `next_id`. Its events go to `store`, except those of early
    /// downloads, which show on the download waiting in its dialog.
    pub fn new(
        store: Arc<DownloadStore>,
        events: Arc<dyn StoreEvents>,
        settings: Arc<LiveSettings>,
        retries: Arc<NetState>,
        next_id: DownloadId,
    ) -> Self {
        let early = Arc::new(EarlyRuns::default());
        let (to, shadows) = (Arc::clone(&store), Arc::clone(&early));
        let s = settings.get();
        let sink: crate::torrent::Sink = Arc::new(move |p: Progress| {
            if !shadows.event(&to, &p) {
                to.publish(&progress_to_outgoing(p));
            }
        });
        let engine = Engine::new(Arc::clone(&sink), s.max_concurrent);
        set_limits(&engine, s.engine_limits(false));
        engine.set_next_id(next_id);
        // Each download goes out the way the settings say as it starts, not
        // as they said when it was queued.
        let (live, net) = (Arc::clone(&settings), Arc::clone(&retries));
        engine.route_from(Arc::new(move || net.network.route(&live.get())));
        Transfers {
            engine: Arc::new(engine),
            store,
            settings,
            retries,
            events,
            early,
            refresh_waiting: Mutex::new(None),
            limit_lifted: AtomicBool::new(false),
            resolver: OnceLock::new(),
            sink,
            torrents: OnceLock::new(),
        }
    }

    /// Download magnet links through `client`, which reports to the same
    /// clients, goes out the way the settings say and follows the speed
    /// limits. Only the first call counts.
    pub fn torrents_with(&self, client: Arc<crate::torrent::TorrentClient>) {
        if self.torrents.set(Arc::clone(&client)).is_err() {
            return;
        }
        let (live, net) = (Arc::clone(&self.settings), Arc::clone(&self.retries));
        let route: crate::torrent::RouteSource = Arc::new(move || net.network.route(&live.get()));
        let live = Arc::clone(&self.settings);
        client.connect(Arc::clone(&self.sink), route, Arc::new(move || live.get().torrents));
        self.engine.torrents_with(client);
        self.limits_to_torrents();
    }

    /// The BitTorrent client, once wired.
    pub fn torrents(&self) -> Option<&Arc<crate::torrent::TorrentClient>> {
        self.torrents.get()
    }

    /// Give the torrent client the engine's total limit and the torrent
    /// settings' upload limit.
    fn limits_to_torrents(&self) {
        if let Some(client) = self.torrents.get() {
            client.set_limits(self.engine.speed_limit(), self.settings.get().torrents.upload_limit());
        }
    }

    /// Ask `resolver` for the address each download starts from. Only the
    /// first call counts.
    pub fn resolve_with(&self, resolver: engine::Resolver) {
        let _ = self.resolver.set(resolver);
    }

    /// The engine request for `download`, with the address it downloads
    /// from now: what the resolver makes of its own, for looking at the
    /// file before it downloads.
    pub fn resolved_request(&self, download: Download) -> Result<DownloadRequest, engine::EngineError> {
        let mut req = self.request_for(download);
        req.resolve()?;
        Ok(req)
    }

    pub fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    /// The early downloads running now.
    pub fn early(&self) -> &EarlyRuns {
        &self.early
    }

    /// The slot is only ever replaced whole, so a poisoned lock still holds
    /// a usable value.
    fn waiting(&self) -> MutexGuard<'_, Option<DownloadId>> {
        self.refresh_waiting.lock_or_recover()
    }

    /// The proxy and logins for `download` (see `connect::config_for`).
    pub fn net_for(&self, download: &Download, s: &Settings) -> net::NetConfig {
        config_for(download, s, &self.retries)
    }

    /// How connections go out now, under the current settings: for work
    /// that runs a while (a crawl) and takes it per request.
    pub fn live_route(&self) -> crate::route::Route {
        self.retries.network.route(&self.settings.get())
    }

    /// Note that `url` is retried through the browser's proxy. Returns false
    /// when it already was.
    pub fn note_browser_proxy_retry(&self, url: String) -> bool {
        self.retries.retries.insert(url)
    }

    /// Write the history and the saved requests now.
    pub fn save_history(&self) {
        self.store.save(self.engine.next_id());
    }

    /// Start a `waiting` (or `confirming`) download now. A download that ran
    /// before resumes its own file; one that never ran goes to the file chosen
    /// for it, if any.
    pub fn dispatch(&self, id: DownloadId) -> Result<(), TransferError> {
        let download = self.store.requests().get(&id).cloned();
        let mut plan = (None, false);
        let item = self.store.update(id, |e| {
            if !matches!(e.status, Status::Waiting | Status::Confirming) {
                return false;
            }
            plan = (e.path.clone(), e.connections > 0);
            e.status = Status::Queued;
            true
        });
        let item = match item {
            Ok(item) => item,
            Err(EditError::NoDownload(id)) => return Err(TransferError::NoDownload(id)),
            Err(EditError::Refused) => return Err(TransferError::NotWaiting(id)),
        };
        let download = download.unwrap_or_else(|| Download {
            url: item.url.clone(),
            referrer: item.referrer.clone(),
            page: item.page.clone(),
            ..Download::default()
        });
        let mut req = self.request_for(download);
        req.speed_limit = item.speed_limit;
        req.reuse_partial = item.partial.as_deref().map(PathBuf::from);
        if let Some(torrent) = &item.torrent {
            req.torrent_uploaded = torrent.uploaded;
            if req.torrent_files.is_none() {
                req.torrent_files = torrent.selection.clone();
            }
        }
        match plan {
            (Some(path), true) => {
                let path = PathBuf::from(path);
                // A finished file whose server copy changed (see
                // `check_for_change`): nothing of the new copy is here yet,
                // and the file has no resume sidecar. Download beside it and
                // swap it in at the end, so a failed or stopped download
                // leaves the old file whole.
                let replacing = item.done.is_zero()
                    && req.temp_dir.is_none()
                    && req.reuse_partial.is_none()
                    && path.is_file()
                    && !engine::sidecar_path(&path).exists();
                if replacing {
                    req.temp_dir = path.parent().map(Path::to_path_buf);
                }
                req.reuse_path = Some(path);
            }
            (Some(path), false) => {
                let path = PathBuf::from(path);
                if let (Some(name), Some(dir)) = (path.file_name(), path.parent()) {
                    req.filename = Some(name.to_string_lossy().into_owned());
                    req.folders = Folders::fixed(dir.to_path_buf());
                }
            }
            (None, _) => {}
        }
        self.engine.enqueue_as(id, req);
        Ok(())
    }

    /// Ask the server about finished download `id` and, when its copy
    /// changed since (by size, ETag or Last-Modified, see
    /// `Validator::changed`) or the file is gone, make it wait to be
    /// downloaded again in place: same row, same file, which `dispatch`
    /// replaces only once the new copy is complete. Returns whether it
    /// changed. A check that fails leaves the download and its file alone,
    /// with a note saying why.
    pub fn check_for_change(&self, id: DownloadId) -> Result<bool, TransferError> {
        let entry = self.entry(id).ok_or(TransferError::NoDownload(id))?;
        if entry.status != Status::Done {
            return Err(TransferError::NotFinished(id));
        }
        // A torrent's files are its pieces: they cannot change.
        if entry.torrent.is_some() {
            return Ok(false);
        }
        let found = self.probe_finished(&entry);
        let found = match found {
            Ok(found) => found,
            Err(e) => {
                warn!(download_id = id.get(), error = %e, "could not check for changes");
                let note = format!("Could not check the server for a newer copy: {e}");
                let _ = self.store.update(id, |e| {
                    if e.status != Status::Done {
                        return false;
                    }
                    e.note = Some(note);
                    true
                });
                return Err(e);
            }
        };
        let known = entry.validator.clone().unwrap_or(Validator { size: entry.total, ..Validator::default() });
        let gone = !entry.path.as_deref().is_some_and(|p| Path::new(p).is_file());
        let changed = gone || known.changed(&found);
        self.store
            .update(id, |e| {
                if e.status != Status::Done {
                    return false;
                }
                e.note = None;
                if changed {
                    // Its own file, to download over (see `dispatch`). The
                    // validator stays the old one until the new copy starts, so
                    // a download that fails is found changed again next time.
                    e.status = Status::Waiting;
                    e.done = ByteCount::ZERO;
                    e.parts = None;
                    e.error = None;
                    e.scan = None;
                    e.partial = None;
                    e.connections = e.connections.max(1);
                } else {
                    // Learn what the server says now, such as an ETag it did
                    // not send before.
                    e.validator = Some(found);
                }
                true
            })
            .map_err(|e| match e {
                EditError::NoDownload(id) => TransferError::NoDownload(id),
                EditError::Refused => TransferError::StartedAgain,
            })?;
        info!(download_id = id.get(), changed, url = %entry.url, "checked the server for changes");
        Ok(changed)
    }

    /// What the server says now about finished download `entry`'s file.
    fn probe_finished(&self, entry: &DownloadEntry) -> Result<Validator, TransferError> {
        if entry.path.is_none() {
            return Err(TransferError::NoFile);
        }
        let download = self.store.requests().get(&entry.id).cloned().unwrap_or_else(|| Download {
            url: entry.url.clone(),
            referrer: entry.referrer.clone(),
            page: entry.page.clone(),
            ..Download::default()
        });
        if entry.parts.is_some() || download.media.is_some() || download.body.is_some() {
            return Err(TransferError::NotCheckable);
        }
        // The file a debrid account unlocks the link into, as the download
        // got it, or the host's page would always look changed.
        let req = self.resolved_request(download).map_err(|e| TransferError::Probe(e.to_string()))?;
        let found =
            engine::inspect(&req.url, None, &req.headers, &req.net).map_err(|e| TransferError::Probe(e.to_string()))?;
        Ok(Validator { size: found.total, etag: found.etag, last_modified: found.last_modified })
    }

    /// Start download `id` while its save-as dialog is open, under an engine
    /// id of its own (whose events clients never see) and into a folder of
    /// its own. Answering the dialog keeps what it fetched: see
    /// `settle_early`.
    pub fn start_early(&self, id: DownloadId, download: Download, filename: &FileName) {
        // A torrent's files are chosen in the dialog: it starts once answered.
        if download.url.address().is_some_and(udm_domain::address::Address::is_torrent) {
            return;
        }
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let root = early_root();
        let dir = root.join(format!("{}-{n}", std::process::id()));
        // A new folder each time: one that is already there, or a link in
        // its place, makes this fail rather than be written through.
        if let Err(e) = std::fs::create_dir_all(&root).and_then(|()| std::fs::create_dir(&dir)) {
            warn!(download_id = id.get(), error = %e, "could not start early");
            return;
        }
        let mut req = self.request_for(download);
        req.folders = Folders::fixed(dir.clone());
        req.filename = Some(filename.to_string());
        req.file_exists = FileExists::Overwrite;
        req.temp_dir = None;
        // The server's date stays as the settings say: the engine sets it
        // only on a finished file, and moving that file into place keeps it.
        let shadow = self.engine.reserve_id();
        self.early
            .runs()
            .insert(shadow, Early { real: id, dir, file: None, target: None, total: None, end: None, discard: false });
        info!(download_id = id.get(), "starts early while its dialog is open");
        self.engine.enqueue_as(shadow, req);
    }

    /// Stop the early download `shadow` and wait for it, returning how it
    /// ended. `None` if it did not stop in time; its events
    /// stay hidden, and its folder goes when UDM next starts.
    fn stop_early(&self, shadow: DownloadId) -> Option<Early> {
        self.engine.cancel(shadow);
        let deadline = Instant::now() + EARLY_STOP_WAIT;
        loop {
            {
                let mut early = self.early.runs();
                if early.get(&shadow).is_some_and(|e| e.end.is_some()) {
                    return early.remove(&shadow);
                }
                if Instant::now() >= deadline {
                    if let Some(e) = early.get_mut(&shadow) {
                        e.real = DownloadId::NONE;
                        e.discard = true;
                    }
                    return None;
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Download `id`'s dialog was answered: stop its early download and put
    /// what it fetched at the file the answer chose (a free name there,
    /// unless the answer or setting overwrites). A finished file moves there
    /// and the download is done; a partial one becomes the download's own
    /// partial file, so starting it carries on from there. Returns whether
    /// it finished.
    pub fn settle_early(&self, id: DownloadId, shadow: DownloadId) -> bool {
        let Some(early) = self.stop_early(shadow) else {
            return false;
        };
        let entry = self.entry(id);
        let path = entry.as_ref().and_then(|e| e.path.as_deref()).map(PathBuf::from);
        let overwrite = {
            let asked = self.store.requests().get(&id).and_then(|d| d.file_exists);
            asked.unwrap_or(self.settings.lock().file_exists) == FileExists::Overwrite
        };
        let mut kept = false;
        // Stopping it can land after its last byte was written and its file
        // saved, which the engine still reports as stopped: a whole file
        // with no partial file or resume file left is kept as finished,
        // rather than fetched again.
        let end = match early.end {
            Some(None) => Some(early_finished_anyway(&early).map(Path::to_path_buf)),
            end => end,
        };
        match (entry, path, end, early.file) {
            (Some(e), Some(path), Some(Some(finished)), _) => {
                let to = free_path(&path, overwrite);
                match crate::files::move_file(&finished, &to) {
                    Ok(()) => {
                        info!(download_id = id.get(), "finished while its dialog was open");
                        self.store.leave_queues(id);
                        self.store.publish(&Outgoing::Done {
                            id,
                            url: e.url,
                            path: to.display().to_string(),
                            note: None,
                        });
                        let _ = std::fs::remove_dir_all(&early.dir);
                        return true;
                    }
                    Err(err) => warn!(download_id = id.get(), error = %err, "could not keep the early download"),
                }
            }
            (Some(_), Some(path), _, Some(partial))
                if partial.is_file() && engine::sidecar_path(&partial).is_file() =>
            {
                let to = free_path(&path, overwrite);
                kept = self
                    .store
                    .update(id, |e| {
                        e.path = Some(to.display().to_string());
                        e.partial = Some(partial.display().to_string());
                        e.connections = e.connections.max(1);
                        true
                    })
                    .is_ok();
            }
            _ => {}
        }
        if !kept {
            let _ = std::fs::remove_dir_all(&early.dir);
        }
        false
    }

    /// Stop download `id`'s early download, if it has one, and drop what it
    /// fetched once it has stopped.
    pub fn discard_early(&self, id: DownloadId) {
        let Some(shadow) = self.early.early_of(id) else {
            return;
        };
        let mut early = self.early.runs();
        let ended = early.get(&shadow).is_some_and(|e| e.end.is_some());
        if ended {
            if let Some(gone) = early.remove(&shadow) {
                let _ = std::fs::remove_dir_all(gone.dir);
            }
            return;
        }
        if let Some(e) = early.get_mut(&shadow) {
            e.real = DownloadId::NONE;
            e.discard = true;
        }
        drop(early);
        self.engine.cancel(shadow);
    }

    /// `start`, with where the earlier attempt kept its partial data when
    /// that was a temporary folder.
    pub fn start_resuming(
        &self,
        download: Download,
        reuse_path: Option<PathBuf>,
        reuse_partial: Option<PathBuf>,
    ) -> (DownloadId, Link) {
        let url = download.url.clone();
        let mut req = self.request_for(download.clone());
        req.reuse_path = reuse_path;
        req.reuse_partial = reuse_partial;
        let id = self.engine.enqueue(req);
        self.store.requests().insert(id, download);
        // A very fast download can finish before its request is stored; drop
        // the request (and its cookies) in that case too.
        if self.status(id) == Some(Status::Done) {
            self.store.requests().remove(&id);
        }
        self.store.publish(&Outgoing::Added { id, url: url.clone() });
        (id, url)
    }

    pub fn status(&self, id: DownloadId) -> Option<Status> {
        self.store.table().get(id).map(|e| e.status)
    }

    pub fn entry(&self, id: DownloadId) -> Option<DownloadEntry> {
        self.store.table().get(id).cloned()
    }

    /// The engine request for `download`, with the settings' folder,
    /// connections and file-name policy, and its proxy and logins. The
    /// engine takes its network adapter and resolver afresh as it starts.
    pub fn request_for(&self, download: Download) -> DownloadRequest {
        let settings = self.settings.lock().clone();
        let net = config_for(&download, &settings, &self.retries);
        let folder = download.folder.as_deref().map(PathBuf::from).filter(|f| f.is_absolute());
        let mut req = to_request(download, &settings);
        req.net = Arc::new(net);
        req.resolve = self.resolver.get().cloned();
        if let Some(folder) = folder {
            req.folders = Folders::fixed(folder);
        }
        req
    }

    /// Store the passwords a settings change carries (proxy: DPAPI; site
    /// logins: Credential Manager), forget those of removed logins, and return
    /// the settings without them.
    fn store_secrets(&self, mut new: Settings) -> Result<Settings, SaveError> {
        for (kind, password) in [
            (ProxyKind::Http, new.proxy_password.take()),
            (ProxyKind::Https, new.proxy_https_password.take()),
            (ProxyKind::Ftp, new.proxy_ftp_password.take()),
            (ProxyKind::Socks, new.proxy_socks_password.take()),
        ] {
            if let Some(pw) = password {
                let path = net::proxy_password_path(kind).map_err(SaveError::ProxyPassword)?;
                net::save_proxy_password(&path, &pw).map_err(SaveError::ProxyPassword)?;
            }
        }

        for l in &mut new.site_logins {
            if let Some(pw) = l.password.take() {
                net::save_login_password(&l.site, &l.username, &pw)
                    .map_err(|source| SaveError::LoginPassword { site: l.site.clone(), source })?;
            }
        }
        // Only the daemon knows which service a debrid key is stored for.
        new.debrid.key_service = crate::debrid::stored_service();
        let old = self.settings.lock().site_logins.clone();
        for gone in old.iter().filter(|o| !new.site_logins.iter().any(|n| n.site.eq_ignore_ascii_case(&o.site))) {
            if let Err(e) = net::delete_login_password(&gone.site) {
                warn!(site = %gone.site, error = %e, "could not forget the password");
            }
        }
        Ok(new)
    }

    /// Validate, save and apply new settings, then tell every client. Running
    /// downloads keep going; new ones use the new values. A change of the
    /// network adapter or the resolver starts running downloads again from
    /// where they are, so none goes on the old way.
    pub fn set_settings(&self, mut new: Settings) -> Result<(), SaveError> {
        settings::validate(&new)?;
        let old = self.settings.get();
        // Only the daemon notes update checks; a window's copy may be older.
        new.last_update_check = new.last_update_check.max(old.last_update_check);
        // The networks a numbered adapter (`utun3`) was on when chosen, so
        // another connection given its name later is not taken for it.
        if let Some(choice) = new.network_adapter.as_mut() {
            let adapters = self.retries.network.adapters().list().unwrap_or_default();
            choice.remember(old.network_adapter.as_ref(), &adapters);
        }
        let network_changed = new.network_adapter != old.network_adapter || new.dns != old.dns;
        let torrents_changed = new.torrents != old.torrents;
        let new = self.store_secrets(new)?;
        let old_autostart = self.settings.lock().start_with_windows;
        if new.start_with_windows != old_autostart {
            crate::autostart::set_autostart(new.start_with_windows).map_err(SaveError::Autostart)?;
        }
        if let Some(path) = self.settings.path() {
            settings::save(&new, path).map_err(|source| SaveError::File { path: path.to_path_buf(), source })?;
        }
        self.engine.set_concurrency(new.max_concurrent);
        let applied = {
            let mut current = self.settings.lock();
            // Settings that no longer lift the limit apply it again at once;
            // otherwise the scheduler decides on its next pass.
            let lifted = self.limit_lifted.load(Ordering::SeqCst) && new.lift_limit_after().is_some();
            let was = self.limit_lifted.swap(lifted, Ordering::SeqCst);
            set_limits(&self.engine, new.engine_limits(lifted));
            *current = new.clone();
            (was != lifted).then_some(lifted)
        };
        if network_changed {
            self.engine.restart_running();
        }
        self.limits_to_torrents();
        if network_changed || torrents_changed {
            // The seeding torrents move to a session for the new settings
            // now; running ones as they notice.
            if let Some(client) = self.torrents.get() {
                let client = Arc::clone(client);
                thread::spawn(move || client.refresh());
            }
        }
        self.events.announce(&Outgoing::Settings { settings: new });
        if let Some(lifted) = applied {
            self.events.announce(&Outgoing::LimitLifted { lifted });
        }
        Ok(())
    }

    /// Change which download waits for a link from the browser, and tell
    /// every client.
    pub fn set_refresh_waiting(&self, id: Option<DownloadId>) {
        *self.waiting() = id;
        self.events.announce(&Outgoing::RefreshWaiting { id });
    }
}

/// Give `engine` the total and per-file speed limits.
fn set_limits(engine: &Engine, limits: EngineLimits) {
    engine.set_speed_limit(limits.total);
    engine.set_per_file_limit(limits.per_file);
}

impl LimitLift for Transfers {
    fn lifted(&self) -> bool {
        self.limit_lifted.load(Ordering::SeqCst)
    }

    fn set_lifted(&self, lifted: bool) {
        {
            // Under the settings' lock, so a change of settings cannot slip
            // in between and leave the engine with the old limits.
            let s = self.settings.lock();
            if self.limit_lifted.swap(lifted, Ordering::SeqCst) == lifted {
                return;
            }
            set_limits(&self.engine, s.engine_limits(lifted));
        }
        self.limits_to_torrents();
        self.events.announce(&Outgoing::LimitLifted { lifted });
    }
}

impl SettingsStore for Transfers {
    fn settings(&self) -> Settings {
        self.settings.get()
    }

    fn save_settings(&self, settings: Settings) -> Result<(), SettingsError> {
        self.set_settings(settings).map_err(|e| match e {
            SaveError::Check(e) => e.into(),
            other => SettingsError::Refused(other.to_string()),
        })
    }
}

impl DownloadControl for Transfers {
    fn hold(&self, id: DownloadId) {
        self.store.hold(id);
        self.engine.cancel(id);
    }

    fn stop(&self, id: DownloadId) {
        self.engine.cancel(id);
    }

    fn set_limit(&self, id: DownloadId, bytes_per_sec: Option<u64>) {
        self.engine.set_download_limit(id, bytes_per_sec);
    }

    fn start_again(&self, id: DownloadId, download: Download) {
        let req = self.request_for(download);
        self.engine.enqueue_as(id, req);
    }

    fn dispatch(&self, id: DownloadId) -> Result<(), ControlError> {
        Transfers::dispatch(self, id).map_err(|e| ControlError(e.to_string()))
    }

    fn cancel_waiting(&self, id: DownloadId) {
        self.discard_early(id);
        let url = self.store.table().get(id).map(|e| e.url.clone());
        if let Some(url) = url {
            self.store.publish(&Outgoing::Canceled { id, url });
        }
    }

    fn wait_for_link(&self, id: Option<DownloadId>) {
        self.set_refresh_waiting(id);
    }

    fn waiting_for_link(&self) -> Option<DownloadId> {
        *self.waiting()
    }

    fn check_partial(
        &self,
        download: &Download,
        partial: &str,
    ) -> Result<Option<udm_domain::download::Mismatch>, ControlError> {
        let req = self.resolved_request(download.clone()).map_err(|e| ControlError(e.to_string()))?;
        let found = engine::inspect(&req.url, None, &req.headers, &req.net).map_err(|e| ControlError(e.to_string()))?;
        let ignore_date = self.settings.lock().resume_ignores_date;
        Ok(engine::partial_matches(Path::new(partial), &found, ignore_date).err())
    }

    fn forget(&self, id: DownloadId) {
        self.store.requests().remove(&id);
        if self.waiting().is_some_and(|w| w == id) {
            self.set_refresh_waiting(None);
        }
        self.store.drop_row(id);
    }

    fn restart(
        &self,
        download: Download,
        path: Option<PathBuf>,
        partial: Option<PathBuf>,
        remember_limit: bool,
    ) -> (DownloadId, Link) {
        let (id, url) = self.start_resuming(download, path, partial);
        if !remember_limit {
            let _ = self.store.update(id, |e| {
                e.limit_remember = false;
                true
            });
        }
        (id, url)
    }
}

/// Why a download could not be started or checked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransferError {
    #[error("no download with id {0}")]
    NoDownload(DownloadId),
    #[error("download {0} is not waiting to be started")]
    NotWaiting(DownloadId),
    #[error("download {0} has not finished")]
    NotFinished(DownloadId),
    #[error("The download started again in the meantime.")]
    StartedAgain,
    #[error("it has no file")]
    NoFile,
    #[error("a stream or a form's answer cannot be checked")]
    NotCheckable,
    /// What the engine said about asking the server.
    #[error("{0}")]
    Probe(String),
}

/// Why new settings were not saved.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error(transparent)]
    Check(#[from] settings::CheckError),
    #[error("could not save the proxy password: {0}")]
    ProxyPassword(#[source] std::io::Error),
    #[error("could not save the password for {site}: {source}")]
    LoginPassword { site: String, source: std::io::Error },
    #[error("could not change starting at sign-in: {0}")]
    Autostart(#[source] std::io::Error),
    #[error("could not save settings to {}: {source}", path.display())]
    File { path: PathBuf, source: std::io::Error },
}

/// Only single inserts and removals change the set, so a poisoned lock
/// still holds a usable one.
fn lock_set(set: &Mutex<HashSet<DownloadId>>) -> MutexGuard<'_, HashSet<DownloadId>> {
    set.lock_or_recover()
}

/// The checks a synchronizing queue runs, each on a thread of its own.
pub struct SyncChecker {
    transfers: Arc<Transfers>,
    /// Finished downloads being checked with the server.
    checking: Arc<Mutex<HashSet<DownloadId>>>,
}

impl SyncChecker {
    pub fn new(transfers: Arc<Transfers>) -> Self {
        SyncChecker { transfers, checking: Arc::default() }
    }
}

impl SyncChecks for SyncChecker {
    fn checking(&self) -> HashSet<DownloadId> {
        lock_set(&self.checking).clone()
    }

    fn check(&self, id: DownloadId) {
        lock_set(&self.checking).insert(id);
        let (transfers, checking) = (Arc::clone(&self.transfers), Arc::clone(&self.checking));
        thread::spawn(move || {
            let _ = transfers.check_for_change(id);
            lock_set(&checking).remove(&id);
            transfers.events.wake();
        });
    }
}

/// `path`, or when a file is there, the first free numbered name next to
/// it (`name (1).ext`, ...), unless `overwrite` says to remove that file.
fn free_path(path: &Path, overwrite: bool) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    if overwrite && std::fs::remove_file(path).is_ok() {
        return path.to_path_buf();
    }
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return path.to_path_buf();
    };
    let name = name.to_string_lossy();
    (1..)
        .map(|n| dir.join(udm_domain::filename::numbered_name(&name, n)))
        .find(|p| !p.exists())
        .unwrap_or_else(|| path.to_path_buf())
}

/// Delete the folders of early downloads a previous run left behind, except
/// those holding the partial file of a download in the list.
pub fn clean_early_folders(store: &DownloadStore) {
    let Ok(dirs) = std::fs::read_dir(early_root()) else {
        return;
    };
    let partials: Vec<PathBuf> =
        store.table().entries().filter_map(|e| e.partial.as_deref().map(PathBuf::from)).collect();
    for dir in dirs.flatten().map(|d| d.path()) {
        if !partials.iter().any(|p| p.starts_with(&dir)) {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// Starts a download that waited, or puts it in its queue, through the use
/// case: the download's id, whether to start it, and the queue.
pub type GoOn = Arc<dyn Fn(DownloadId, bool, Option<QueueId>) + Send + Sync>;

/// Early starts, each handed over on a thread of its own: stopping the
/// early copy and moving its bytes takes a moment.
pub struct EarlyHandOver {
    transfers: Arc<Transfers>,
    go_on: GoOn,
}

impl EarlyHandOver {
    pub fn new(transfers: Arc<Transfers>, go_on: GoOn) -> Self {
        EarlyHandOver { transfers, go_on }
    }
}

impl udm_domain::ports::EarlyStarts for EarlyHandOver {
    fn start(&self, id: DownloadId, download: Download, filename: &FileName) {
        self.transfers.start_early(id, download, filename);
    }

    fn hand_over(&self, id: DownloadId, start: bool, queue: Option<QueueId>) -> bool {
        let Some(shadow) = self.transfers.early.early_of(id) else {
            return false;
        };
        let (transfers, go_on) = (Arc::clone(&self.transfers), Arc::clone(&self.go_on));
        thread::spawn(move || {
            if !transfers.settle_early(id, shadow) {
                go_on(id, start, queue);
            }
        });
        true
    }
}

impl udm_domain::ports::TransferEngine for Transfers {
    fn reserve_id(&self) -> DownloadId {
        self.engine.reserve_id()
    }

    fn active_count(&self) -> usize {
        self.engine.active_count()
    }

    fn start(&self, download: Download) -> (DownloadId, Link) {
        self.start_resuming(download, None, None)
    }

    fn save_history(&self) {
        Transfers::save_history(self);
    }

    fn has_changed(&self, id: DownloadId) -> bool {
        matches!(self.check_for_change(id), Ok(true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::testserver::TestServer;
    use crate::store::{Ended, StoreFiles};

    #[derive(Default)]
    struct Heard(Mutex<Vec<Outgoing>>);

    impl StoreEvents for Heard {
        fn announce(&self, event: &Outgoing) {
            self.0.lock().unwrap().push(event.clone());
        }
        fn ended(&self, _: Ended) {}
        fn wake(&self) {}
    }

    struct World {
        dir: PathBuf,
        store: Arc<DownloadStore>,
        heard: Arc<Heard>,
        transfers: Arc<Transfers>,
    }

    fn world(name: &str) -> World {
        let dir = std::env::temp_dir().join(format!("udm_transfers_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let heard = Arc::new(Heard::default());
        let (store, _, next_id) = DownloadStore::open(StoreFiles::default(), Arc::clone(&heard) as _);
        let store = Arc::new(store);
        let settings = Settings { download_dir: Some(dir.display().to_string()), ..Settings::default() };
        let live = Arc::new(LiveSettings::new(settings, Some(dir.join("settings.json"))));
        let transfers =
            Arc::new(Transfers::new(Arc::clone(&store), Arc::clone(&heard) as _, live, Arc::default(), next_id));
        World { dir, store, heard, transfers }
    }

    fn wait_for(w: &World, id: u64, status: Status) -> DownloadEntry {
        for _ in 0..500 {
            if let Some(e) = w.transfers.entry(DownloadId::new(id)).filter(|e| e.status == status) {
                return e;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("download {id} never became {status:?}: {:?}", w.transfers.entry(DownloadId::new(id)));
    }

    #[test]
    fn a_download_runs_to_the_settings_folder_and_is_checked_for_changes() {
        let w = world("runs");
        let server = TestServer::start(engine::testserver::body(10_000));
        let (id, _) =
            w.transfers.start_resuming(Download { url: server.url.as_str().into(), ..Download::default() }, None, None);
        let done = wait_for(&w, id.get(), Status::Done);
        let path = PathBuf::from(done.path.unwrap());
        assert_eq!(path.parent(), Some(w.dir.as_path()));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 10_000);

        assert_eq!(w.transfers.check_for_change(id), Ok(false), "the server's copy is the same");
        std::fs::remove_file(&path).unwrap();
        assert_eq!(w.transfers.check_for_change(id), Ok(true), "the file is gone");
        assert_eq!(w.transfers.status(id), Some(Status::Waiting));

        // Waiting again in place, it downloads into the same file.
        DownloadControl::dispatch(&*w.transfers, id).unwrap();
        wait_for(&w, id.get(), Status::Done);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 10_000);
        let _ = std::fs::remove_dir_all(&w.dir);
    }

    #[test]
    fn new_settings_are_saved_and_announced() {
        let w = world("settings");
        let new = Settings { max_concurrent: 2, ..w.transfers.settings() };
        w.transfers.save_settings(new).unwrap();
        assert_eq!(settings::load(&w.dir.join("settings.json")).unwrap().max_concurrent, 2);
        assert!(w.heard.0.lock().unwrap().iter().any(|m| matches!(m, Outgoing::Settings { .. })));
        let bad = Settings { max_concurrent: 0, ..w.transfers.settings() };
        assert!(w.transfers.save_settings(bad).is_err());
        assert_eq!(w.transfers.settings().max_concurrent, 2, "a refused change leaves them alone");
        let _ = std::fs::remove_dir_all(&w.dir);
    }

    #[test]
    fn the_speed_limit_is_lifted_while_away_and_back_when_the_setting_goes() {
        let w = world("away");
        let s = Settings {
            speed_limit_enabled: true,
            speed_limit_kbps: 100,
            speed_limit_per_file_kbps: Some(50),
            speed_limit_only_while_active: true,
            ..w.transfers.settings()
        };
        w.transfers.save_settings(s.clone()).unwrap();
        let limits = || (w.transfers.engine().speed_limit(), w.transfers.engine().per_file_limit());
        let lifts = || w.heard.0.lock().unwrap().iter().filter(|m| matches!(m, Outgoing::LimitLifted { .. })).count();
        assert_eq!(limits(), (Some(100 * 1024), Some(50 * 1024)));

        w.transfers.set_lifted(true);
        assert!(w.transfers.lifted());
        assert_eq!(limits(), (None, None));
        w.transfers.set_lifted(true);
        assert_eq!(lifts(), 1, "only a change is announced");

        // New limits wait while it is lifted; turning the option off applies them.
        w.transfers.save_settings(Settings { speed_limit_kbps: 200, ..s.clone() }).unwrap();
        assert_eq!(limits(), (None, None));
        w.transfers
            .save_settings(Settings { speed_limit_kbps: 200, speed_limit_only_while_active: false, ..s })
            .unwrap();
        assert!(!w.transfers.lifted());
        assert_eq!(limits(), (Some(200 * 1024), Some(50 * 1024)));
        assert!(matches!(w.heard.0.lock().unwrap().last(), Some(Outgoing::LimitLifted { lifted: false })));
        let _ = std::fs::remove_dir_all(&w.dir);
    }

    #[test]
    fn forgetting_the_download_waiting_for_a_link_stops_the_wait() {
        let w = world("forget");
        w.store.publish(&Outgoing::Added { id: DownloadId::new(5), url: "http://x/a".into() });
        w.transfers.wait_for_link(Some(DownloadId::new(5)));
        assert_eq!(w.transfers.waiting_for_link(), Some(DownloadId::new(5)));
        w.transfers.forget(DownloadId::new(5));
        assert_eq!(w.transfers.waiting_for_link(), None);
        assert!(w.transfers.entry(DownloadId::new(5)).is_none());
        let said = w.heard.0.lock().unwrap().clone();
        assert!(said.iter().any(|m| matches!(m, Outgoing::RefreshWaiting { id: None })));
        let _ = std::fs::remove_dir_all(&w.dir);
    }

    /// The stop that answering the dialog sends can land after the early
    /// download's last byte but before the engine saw it through: it is
    /// reported stopped, with its resume file left. Its file is then kept as
    /// the download's partial file, which carries on into the file chosen,
    /// in a folder nothing has made yet.
    #[test]
    fn an_early_download_stopped_after_its_last_byte_finishes_in_a_new_folder() {
        const LEN: usize = 64 * 1024;
        let w = world("early_whole");
        let body = engine::testserver::body(LEN);
        let server = TestServer::start(body.clone());
        let id = w.transfers.engine().reserve_id();
        w.store.publish(&Outgoing::Added { id, url: server.url.as_str().into() });
        w.transfers.start_early(
            id,
            Download { url: server.url.as_str().into(), ..Download::default() },
            &FileName::parse("big.bin").unwrap(),
        );
        let shadow = w.transfers.early().early_of(id).unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        while w.transfers.early().runs()[&shadow].end.is_none() {
            assert!(Instant::now() < until, "the early download never ended");
            thread::sleep(Duration::from_millis(10));
        }
        let (file, dir) = {
            let mut runs = w.transfers.early().runs();
            let e = runs.get_mut(&shadow).unwrap();
            assert!(matches!(e.end, Some(Some(_))), "it finished: {:?}", e.end);
            e.end = Some(None);
            (e.file.clone().unwrap(), e.dir.clone())
        };
        engine::leave_whole_sidecar(&file, &server.url, LEN as u64).unwrap();

        let chosen = w.dir.join("chosen").join("big.bin");
        w.store
            .update(id, |e| {
                e.path = Some(chosen.display().to_string());
                e.status = Status::Waiting;
                true
            })
            .unwrap();
        assert!(!w.transfers.settle_early(id, shadow), "kept as its partial file");
        assert_eq!(w.transfers.entry(id).unwrap().partial, Some(file.display().to_string()));
        DownloadControl::dispatch(&*w.transfers, id).unwrap();
        let done = wait_for(&w, id.get(), Status::Done);
        assert_eq!(done.path, Some(chosen.display().to_string()));
        assert_eq!(std::fs::read(&chosen).unwrap(), body);
        assert!(!file.exists() && !engine::sidecar_path(&file).exists());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&w.dir);
    }

    #[test]
    fn an_early_download_stopped_after_saving_counts_as_finished() {
        let dir = std::env::temp_dir().join(format!("udm_early_late_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("big.bin");
        let partial = dir.join("big.bin.part");
        let early = |file: &Path| Early {
            real: DownloadId::new(1),
            dir: dir.clone(),
            file: Some(file.to_path_buf()),
            target: Some(target.clone()),
            total: Some(ByteCount::new(4)),
            end: Some(None),
            discard: false,
        };

        // Stopped half way: the partial file and its resume file are left.
        std::fs::write(&partial, b"ab").unwrap();
        std::fs::write(engine::sidecar_path(&partial), b"{}").unwrap();
        assert_eq!(early_finished_anyway(&early(&partial)), None);

        // Saved before the stop landed: only the whole file is left.
        std::fs::remove_file(&partial).unwrap();
        std::fs::remove_file(engine::sidecar_path(&partial)).unwrap();
        std::fs::write(&target, b"abcd").unwrap();
        assert_eq!(early_finished_anyway(&early(&partial)), Some(target.as_path()));

        // Written in place without resume data but short: not finished.
        std::fs::write(&target, b"abc").unwrap();
        assert_eq!(early_finished_anyway(&early(&target)), None);
        // In place with its resume file: stopped, not finished.
        std::fs::write(&target, b"abcd").unwrap();
        std::fs::write(engine::sidecar_path(&target), b"{}").unwrap();
        assert_eq!(early_finished_anyway(&early(&target)), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
