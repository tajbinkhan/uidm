//! The daemon's download list, kept requests and queues, and the files
//! they are saved in.
//!
//! `DownloadStore` is the adapter for the `QueueStore`, `DownloadList` and
//! `PackageStore` ports, and with `Intake` for `DownloadIntake`. It applies each download
//! event to its `DownloadTable`, and hands the event to a `StoreEvents`
//! listener the daemon wires; who hears an event, and how it travels to
//! them, is the listener's business.
//!
//! Every change wakes the receiver `DownloadStore::open` returns, so the
//! daemon can write the files (`save`) a moment later. The files are the
//! history (`history.json`, with the queues and packages), the saved requests (sealed
//! with DPAPI, see `history`) and the quota's usage, in the same formats as
//! before this module.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use udm_domain::address::Link;
use udm_domain::bytes::ByteCount;
use udm_domain::id::{DownloadId, QueueId};
use udm_domain::sync::LockExt;

use tracing::warn;
use udm_application::daemon::Event;
use udm_domain::download::{Download, DownloadEntry, Status};
use udm_domain::package::Package;
use udm_domain::ports::{DownloadIntake, DownloadList, PackageStore, QueueStore};
use udm_domain::queue::{Queue, QueueError};

use crate::engine::{self, Engine};
use crate::history::{self, History};
use crate::protocol::Outgoing;
use crate::quota;
use crate::state::DownloadTable;
use udm_domain::quota::Usage;

/// How a download ended, for the listener's notifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Finished(DownloadId),
    Failed(DownloadId),
    /// It stopped to ask about its taken file name.
    NameTaken(DownloadId),
}

/// Hears what the store changed. The store calls it with its table locked,
/// so the order of `announce` calls is the order of the changes.
pub trait StoreEvents: Send + Sync {
    /// The list or the queues changed as `event` says.
    fn announce(&self, event: &Outgoing);

    /// A download finished, failed, or stopped on a taken name.
    fn ended(&self, how: Ended);

    /// The scheduler should look at the queues again: a download ended or
    /// a queue changed.
    fn wake(&self);
}

/// Where the store's files are. A missing path is not saved.
#[derive(Debug, Clone, Default)]
pub struct StoreFiles {
    pub history: Option<PathBuf>,
    pub requests: Option<PathBuf>,
    pub quota: Option<PathBuf>,
}

/// The download list, the requests kept for retries and the queues.
pub struct DownloadStore {
    /// Every download the daemon has seen, kept current from its events.
    table: Mutex<DownloadTable>,
    /// The original request for each download that may still be retried.
    /// It holds the browser's cookies, so it is dropped once a download is
    /// done or removed, and saved only encrypted.
    requests: Mutex<HashMap<DownloadId, Download>>,
    /// The queues, the main queue first. Lock after `table`, never before.
    queues: Mutex<Vec<Queue>>,
    /// The packages, oldest first. Lock after `table`, never before.
    packages: Mutex<Vec<Package>>,
    /// Downloads being stopped to wait in their queue again: their
    /// `canceled` event becomes `waiting`.
    held: Mutex<HashSet<DownloadId>>,
    /// Bytes received lately, for the download quota.
    quota: Mutex<Usage>,
    /// Rows that are conversions, not downloads: their progress is not
    /// received bytes.
    uncounted: Mutex<HashSet<DownloadId>>,
    /// Wakes the history writer after every change.
    dirty: Sender<()>,
    /// Keeps two saves from writing at once.
    save_lock: Mutex<()>,
    files: StoreFiles,
    events: Arc<dyn StoreEvents>,
}

/// Lock `m`. A thread that panicked while holding one of the store's locks
/// left plain data behind (a table, a map, a list), which is still the best
/// the daemon has, so a poisoned lock is used as it is.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock_or_recover()
}

impl DownloadStore {
    /// Load the store from `files`, telling `events` of changes. Returns it,
    /// the receiver every change wakes, and the next download id the history
    /// asks for (the engine's next id must be at least that).
    pub fn open(files: StoreFiles, events: Arc<dyn StoreEvents>) -> (Self, Receiver<()>, DownloadId) {
        let history = match &files.history {
            Some(path) => History::load(path).unwrap_or_else(|e| {
                // Keep the unreadable file for inspection instead of
                // overwriting it with an empty list.
                let bad = path.with_extension("json.bad");
                warn!(path = %path.display(), moved_to = %bad.display(), error = %e, "could not read the download list");
                let _ = std::fs::rename(path, &bad);
                History::default()
            }),
            None => History::default(),
        };
        let mut requests = match &files.requests {
            Some(path) => history::load_requests(path).unwrap_or_else(|e| {
                warn!(error = %e, "could not read saved requests");
                HashMap::new()
            }),
            None => HashMap::new(),
        };
        let mut table = DownloadTable::new();
        let mut queues = history.queues;
        if queues.is_empty() {
            queues.push(Queue::main());
        }
        table.apply(&Outgoing::Downloads { items: history.items });
        requests.retain(|id, _| table.get(*id).is_some_and(|e| e.status.is_retryable()));
        let next_id = history.next_id.max(table.max_id().next());
        let usage = files.quota.as_deref().map(quota::load).unwrap_or_default();
        let (dirty, dirty_rx) = channel();
        let store = DownloadStore {
            table: Mutex::new(table),
            requests: Mutex::new(requests),
            queues: Mutex::new(queues),
            packages: Mutex::new(history.packages),
            held: Mutex::new(HashSet::new()),
            quota: Mutex::new(usage),
            uncounted: Mutex::new(HashSet::new()),
            dirty,
            save_lock: Mutex::new(()),
            files,
            events,
        };
        (store, dirty_rx, next_id)
    }

    /// The download table, locked. Lock it before `queues`, never after.
    pub fn table(&self) -> MutexGuard<'_, DownloadTable> {
        lock(&self.table)
    }

    /// The requests kept for retries, locked.
    pub fn requests(&self) -> MutexGuard<'_, HashMap<DownloadId, Download>> {
        lock(&self.requests)
    }

    /// The queues, locked. Changes made here are not announced; call
    /// `publish_queues` after.
    pub fn queues(&self) -> MutexGuard<'_, Vec<Queue>> {
        lock(&self.queues)
    }

    /// Bytes received lately, locked.
    pub fn quota(&self) -> MutexGuard<'_, Usage> {
        lock(&self.quota)
    }

    /// Make download `id`'s coming `canceled` event a `waiting` one: it is
    /// being stopped to wait in its queue again.
    pub fn hold(&self, id: DownloadId) {
        lock(&self.held).insert(id);
    }

    /// Whether download `id` is being stopped to wait again.
    pub fn is_held(&self, id: DownloadId) -> bool {
        lock(&self.held).contains(&id)
    }

    /// Do not count row `id`'s progress as received bytes: it is a
    /// conversion.
    pub fn uncount(&self, id: DownloadId) {
        lock(&self.uncounted).insert(id);
    }

    /// Apply `msg` to the table and announce it. The table lock is held
    /// across the announcement, so the order listeners see matches the
    /// table's, and a snapshot taken under the same lock is never stale.
    /// Apply `event` to the list and announce it, as `publish` does with a
    /// wire message.
    pub fn announce(&self, event: Event) {
        if let Some(msg) = crate::wire::outgoing(event) {
            self.publish(&msg);
        }
    }

    pub fn publish(&self, msg: &Outgoing) {
        let mut table = lock(&self.table);
        if let Outgoing::Canceled { id, .. } = msg {
            if lock(&self.held).remove(id) {
                if let Some(mut item) = table.get(*id).cloned() {
                    item.status = Status::Waiting;
                    // Stopping its queue stops it; starting the queue again
                    // resumes it, without its own limit unless remembered.
                    if !item.limit_remember {
                        item.speed_limit = None;
                    }
                    let waiting = Outgoing::Updated { item };
                    table.apply(&waiting);
                    let _ = self.dirty.send(());
                    self.events.wake();
                    self.events.announce(&waiting);
                    return;
                }
            }
        }
        match msg {
            // Bytes a resumed download already had do not count again.
            Outgoing::Started { id, .. } => {
                let done = table.get(*id).map_or(ByteCount::ZERO, |e| e.done);
                lock(&self.quota).started(*id, done);
            }
            Outgoing::Progress { id, done, .. } if !lock(&self.uncounted).contains(id) => {
                lock(&self.quota).progress(*id, *done, quota::now_minute());
            }
            Outgoing::Done { id, .. } | Outgoing::Failed { id, .. } | Outgoing::Canceled { id, .. } => {
                lock(&self.quota).ended(*id);
            }
            _ => {}
        }
        table.apply(msg);
        if let Outgoing::Done { id, .. } | Outgoing::Failed { id, .. } = msg {
            // It ended before the stop reached it; nothing to hold.
            lock(&self.held).remove(id);
        }
        // A one-time queue holds what is still to do: a finished download
        // leaves it. A synchronizing queue keeps it, to check next run.
        if let Outgoing::Done { id, .. } = msg {
            if self.leave_queues_where(*id, |q| !q.kind.is_sync()) {
                self.publish_queues();
            }
        }
        // A download that leaves the list leaves its package.
        if let Outgoing::Removed { id } = msg {
            if self.leave_packages(*id) {
                self.publish_packages();
            }
        }
        if matches!(
            msg,
            Outgoing::Done { .. } | Outgoing::Failed { .. } | Outgoing::Canceled { .. } | Outgoing::NameTaken { .. }
        ) {
            self.events.wake();
        }
        match msg {
            Outgoing::Done { id, url, path, .. } => {
                match table.get(*id).and_then(|e| e.torrent.as_ref()) {
                    // A torrent marks each file it wrote, with no address:
                    // a magnet link is none Windows takes.
                    Some(torrent) => {
                        for file in torrent.written_files(Path::new(path)) {
                            crate::platform::mark_from_internet_unnamed(&file);
                        }
                    }
                    None => {
                        let referrer = table.get(*id).and_then(|e| e.referrer.clone());
                        crate::platform::mark_from_internet(Path::new(path), url.as_str(), referrer.as_deref());
                    }
                }
                lock(&self.requests).remove(id);
                self.events.ended(Ended::Finished(*id));
            }
            Outgoing::Failed { id, .. } => self.events.ended(Ended::Failed(*id)),
            Outgoing::NameTaken { id, .. } => self.events.ended(Ended::NameTaken(*id)),
            _ => {}
        }
        let _ = self.dirty.send(());
        self.events.announce(msg);
        // The list shows a new download's page, description, checksum and
        // login name from its request.
        if let Outgoing::Added { id, .. } = msg {
            drop(table);
            let request = lock(&self.requests).get(id).cloned();
            if let Some(d) = request {
                let _ = self.update(*id, |e| {
                    let before = e.clone();
                    // A source that knows no page apart from the referrer
                    // (a list file, the batch dialog) found the link there.
                    e.page = e.page.take().or(d.page).or_else(|| d.referrer.clone());
                    e.referrer = e.referrer.take().or(d.referrer);
                    e.description = e.description.take().or(d.description);
                    e.checksum = e.checksum.take().or(d.checksum);
                    e.category = e.category.take().or(d.category);
                    e.login_user = e.login_user.take().or(d.login.map(|l| l.username));
                    e.speed_limit = e.speed_limit.or(d.speed_limit);
                    e.from_browser = e.from_browser || d.user_agent.is_some();
                    *e != before
                });
            }
        }
    }

    /// Change download `id` with `f` and announce the new entry, all under
    /// the table lock, so the check `f` makes cannot race another change.
    /// `f` returns false to leave the entry alone.
    pub fn update(
        &self,
        id: DownloadId,
        f: impl FnOnce(&mut DownloadEntry) -> bool,
    ) -> Result<DownloadEntry, EditError> {
        let mut table = lock(&self.table);
        let mut item = table.get(id).cloned().ok_or(EditError::NoDownload(id))?;
        if !f(&mut item) {
            return Err(EditError::Refused);
        }
        let msg = Outgoing::Updated { item: item.clone() };
        table.apply(&msg);
        let _ = self.dirty.send(());
        self.events.announce(&msg);
        Ok(item)
    }

    /// Announce every queue, and save them.
    /// Tell every client the packages changed, and save them.
    pub fn publish_packages(&self) {
        let items = lock(&self.packages).clone();
        let _ = self.dirty.send(());
        self.events.announce(&Outgoing::Packages { items });
    }

    /// Take download `id` out of its package, dropping a package left
    /// empty. Returns whether a package changed.
    fn leave_packages(&self, id: DownloadId) -> bool {
        let mut packages = lock(&self.packages);
        let before = packages.clone();
        for p in packages.iter_mut() {
            p.downloads.retain(|d| *d != id);
        }
        packages.retain(|p| !p.downloads.is_empty());
        *packages != before
    }

    pub fn publish_queues(&self) {
        let items = lock(&self.queues).clone();
        let _ = self.dirty.send(());
        self.events.announce(&Outgoing::Queues { items });
    }

    /// Change queue `id` with `f`, then announce the queues.
    pub fn update_queue<T>(&self, id: QueueId, f: impl FnOnce(&mut Queue) -> T) -> Result<T, QueueError> {
        let result = {
            let mut queues = lock(&self.queues);
            queues.iter_mut().find(|q| q.id == id).map(f).ok_or(QueueError::NoQueue(id))?
        };
        self.publish_queues();
        self.events.wake();
        Ok(result)
    }

    /// Take download `id` out of whatever queue holds it. Returns whether
    /// one did; the change is not announced.
    pub fn leave_queues(&self, id: DownloadId) -> bool {
        self.leave_queues_where(id, |_| true)
    }

    /// Take download `id` out of the queues `which` picks.
    fn leave_queues_where(&self, id: DownloadId, which: impl Fn(&Queue) -> bool) -> bool {
        let mut changed = false;
        for q in lock(&self.queues).iter_mut().filter(|q| which(q)) {
            let before = q.order.len();
            q.order.retain(|x| *x != id);
            changed |= q.order.len() != before;
        }
        changed
    }

    /// The whole list and the queues, taken together under the table lock,
    /// for a client that has just connected. `then` runs under the lock
    /// too, so no event can slip in before the client hears it.
    pub fn snapshot<T>(&self, then: impl FnOnce(Vec<DownloadEntry>, Vec<Queue>) -> T) -> T {
        let table = lock(&self.table);
        let queues = lock(&self.queues).clone();
        then(table.snapshot(), queues)
    }

    /// Write the history (with `next_id`, the engine's next download id),
    /// the saved requests and the quota's usage now. A file that cannot be
    /// written is logged and left as it was.
    pub fn save(&self, next_id: DownloadId) {
        let _guard = lock(&self.save_lock);
        let history = History {
            next_id,
            items: lock(&self.table).snapshot(),
            queues: lock(&self.queues).clone(),
            packages: lock(&self.packages).clone(),
        };
        let requests = lock(&self.requests).clone();
        if let Some(path) = &self.files.quota {
            if let Err(e) = quota::save(&lock(&self.quota), path) {
                warn!(path = %path.display(), error = %e, "could not save");
            }
        }
        if let Some(path) = &self.files.history {
            if let Err(e) = history.save(path) {
                warn!(path = %path.display(), error = %e, "could not save");
            }
        }
        if let Some(path) = &self.files.requests {
            if let Err(e) = history::save_requests(path, &requests) {
                warn!(path = %path.display(), error = %e, "could not save");
            }
        }
    }

    /// Add `download` to the list as `id`, waiting, in no queue yet. A
    /// full-path `folder` fixes where it will be saved. Returns its address.
    pub fn stage(&self, id: DownloadId, download: Download) -> Link {
        let url = download.url.clone();
        let path = download
            .folder
            .as_deref()
            .map(PathBuf::from)
            .filter(|f| f.is_absolute())
            .map(|f| f.join(engine::Inspection::unprobed(url.as_str(), download.filename.as_deref()).filename));
        // The size the browser's response gave, until the server is asked.
        let size = download.size;
        lock(&self.requests).insert(id, download);
        self.publish(&Outgoing::Added { id, url: url.clone() });
        let _ = self.update(id, |e| {
            e.status = Status::Waiting;
            if e.total.is_none() {
                e.total = size;
            }
            if let Some(p) = &path {
                e.path = Some(p.display().to_string());
            }
            true
        });
        url
    }
}

impl PackageStore for DownloadStore {
    fn edit_packages<T>(&self, f: impl FnOnce(&mut Vec<Package>) -> T) -> T {
        f(&mut lock(&self.packages))
    }

    fn packages_changed(&self) {
        self.publish_packages();
    }
}

impl QueueStore for DownloadStore {
    fn edit<T>(&self, f: impl FnOnce(&mut Vec<Queue>) -> T) -> T {
        f(&mut lock(&self.queues))
    }

    fn queues_changed(&self) {
        self.publish_queues();
        self.events.wake();
    }
}

impl DownloadList for DownloadStore {
    fn status(&self, id: DownloadId) -> Option<Status> {
        lock(&self.table).get(id).map(|e| e.status)
    }

    fn finished(&self) -> HashSet<DownloadId> {
        lock(&self.table).entries().filter(|e| e.status == Status::Done).map(|e| e.id).collect()
    }

    fn set_waiting(&self, id: DownloadId) -> bool {
        DownloadStore::update(self, id, |e| {
            e.status = Status::Waiting;
            true
        })
        .is_ok()
    }

    fn find_last(&self, keep: impl Fn(&DownloadEntry) -> bool) -> Option<DownloadEntry> {
        lock(&self.table).entries().filter(|e| keep(e)).last().cloned()
    }

    fn ids_where(&self, keep: impl Fn(DownloadId, Status) -> bool) -> Vec<DownloadId> {
        lock(&self.table).entries().filter(|e| keep(e.id, e.status)).map(|e| e.id).collect()
    }

    fn entry(&self, id: DownloadId) -> Option<DownloadEntry> {
        lock(&self.table).get(id).cloned()
    }

    fn take_request(&self, id: DownloadId) -> Option<Download> {
        lock(&self.requests).remove(&id)
    }

    fn drop_row(&self, id: DownloadId) {
        self.publish(&Outgoing::Removed { id });
    }

    fn update(&self, id: DownloadId, f: impl FnOnce(&mut DownloadEntry) -> bool) -> Option<bool> {
        match DownloadStore::update(self, id, f) {
            Ok(_) => Some(true),
            Err(EditError::Refused) => Some(false),
            Err(EditError::NoDownload(_)) => None,
        }
    }

    fn request(&self, id: DownloadId) -> Option<Download> {
        lock(&self.requests).get(&id).cloned()
    }

    fn keep_request(&self, id: DownloadId, download: Download) {
        lock(&self.requests).insert(id, download);
    }

    fn edit_request(&self, id: DownloadId, f: impl FnOnce(&mut Download)) {
        if let Some(d) = lock(&self.requests).get_mut(&id) {
            f(d);
        }
    }
}

/// Whether `d` can be sent: a POST whose body is not valid base64 is
/// refused, rather than send another body than the form's.
pub fn body_is_sendable(d: &Download) -> bool {
    use base64::Engine as _;
    match &d.body {
        Some(body) => base64::engine::general_purpose::STANDARD.decode(body).is_ok(),
        None => true,
    }
}

/// Why [`DownloadStore::update`] left a download alone.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    #[error("no download with id {0}")]
    NoDownload(DownloadId),
    #[error("the change was refused")]
    Refused,
}

/// The adapter for `DownloadIntake`: new downloads go into `store` under ids
/// `engine` hands out.
pub struct Intake<'a> {
    pub store: &'a DownloadStore,
    pub engine: &'a Engine,
}

impl DownloadIntake for Intake<'_> {
    fn sendable(&self, download: &Download) -> bool {
        body_is_sendable(download)
    }

    fn stage(&self, download: Download) -> (DownloadId, Link) {
        let id = self.engine.reserve_id();
        (id, self.store.stage(id, download))
    }
}

impl udm_application::daemon::DownloadFeed for DownloadStore {
    fn announce(&self, event: Event) {
        DownloadStore::announce(self, event);
    }

    fn snapshot<T>(&self, then: impl FnOnce(Vec<DownloadEntry>, Vec<Queue>) -> T) -> T {
        DownloadStore::snapshot(self, then)
    }
}

/// The `DownloadIntake` adapter that owns its store and engine, for a
/// service that keeps it.
pub struct StoreIntake {
    pub store: Arc<DownloadStore>,
    pub engine: Arc<Engine>,
}

impl DownloadIntake for StoreIntake {
    fn sendable(&self, download: &Download) -> bool {
        Intake { store: &self.store, engine: &self.engine }.sendable(download)
    }

    fn stage(&self, download: Download) -> (DownloadId, Link) {
        Intake { store: &self.store, engine: &self.engine }.stage(download)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use udm_domain::queue::QueueKind;

    /// What a store told its listener.
    #[derive(Default)]
    struct Heard {
        announced: Mutex<Vec<Outgoing>>,
        ended: Mutex<Vec<Ended>>,
        wakes: Mutex<usize>,
    }

    struct Listener(Arc<Heard>);

    impl StoreEvents for Listener {
        fn announce(&self, event: &Outgoing) {
            self.0.announced.lock().unwrap().push(event.clone());
        }
        fn ended(&self, how: Ended) {
            self.0.ended.lock().unwrap().push(how);
        }
        fn wake(&self) {
            *self.0.wakes.lock().unwrap() += 1;
        }
    }

    fn store(files: StoreFiles) -> (DownloadStore, Arc<Heard>, Receiver<()>) {
        let heard = Arc::new(Heard::default());
        let (store, dirty, _) = DownloadStore::open(files, Arc::new(Listener(Arc::clone(&heard))));
        (store, heard, dirty)
    }

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("udm_store_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn done(id: u64, path: &Path) -> Outgoing {
        Outgoing::Done {
            id: DownloadId::new(id),
            url: "http://x/f".into(),
            path: path.display().to_string(),
            note: None,
        }
    }

    #[test]
    fn a_new_store_has_the_main_queue() {
        let (store, ..) = store(StoreFiles::default());
        assert_eq!(store.queues().len(), 1);
        assert_eq!(store.queues()[0].id, udm_domain::queue::MAIN_QUEUE);
    }

    #[test]
    fn events_reach_the_table_and_the_listener_and_mark_it_dirty() {
        let (store, heard, dirty) = store(StoreFiles::default());
        store.publish(&Outgoing::Added { id: DownloadId::new(3), url: "http://x/f".into() });
        assert!(store.entry(DownloadId::new(3)).is_some());
        assert!(matches!(heard.announced.lock().unwrap()[0], Outgoing::Added { id, .. } if id == DownloadId::new(3)));
        assert!(dirty.try_recv().is_ok());
    }

    #[test]
    fn a_held_download_waits_again_instead_of_stopping() {
        let (store, heard, _) = store(StoreFiles::default());
        store.publish(&Outgoing::Added { id: DownloadId::new(1), url: "http://x/f".into() });
        store.hold(DownloadId::new(1));
        store.publish(&Outgoing::Canceled { id: DownloadId::new(1), url: "http://x/f".into() });
        assert_eq!(DownloadList::status(&store, DownloadId::new(1)), Some(Status::Waiting));
        assert!(!store.is_held(DownloadId::new(1)));
        assert!(matches!(heard.announced.lock().unwrap().last(), Some(Outgoing::Updated { .. })));
    }

    #[test]
    fn a_finished_download_leaves_one_time_queues_only() {
        let dir = folder("leave");
        let (store, heard, _) = store(StoreFiles::default());
        {
            let mut queues = store.queues();
            queues[0].order = vec![DownloadId::new(1), DownloadId::new(2)];
            let mut sync = Queue::new(QueueId::new(7), "sync");
            sync.kind = QueueKind::Synchronize;
            sync.order = vec![DownloadId::new(1)];
            queues.push(sync);
        }
        store.publish(&Outgoing::Added { id: DownloadId::new(1), url: "http://x/f".into() });
        store.publish(&done(1, &dir.join("f")));
        let queues = store.queues().clone();
        assert_eq!(queues[0].order, vec![DownloadId::new(2)]);
        assert_eq!(queues[1].order, vec![DownloadId::new(1)]);
        assert_eq!(*heard.ended.lock().unwrap(), vec![Ended::Finished(DownloadId::new(1))]);
        assert!(*heard.wakes.lock().unwrap() > 0);
    }

    #[test]
    fn a_finished_download_drops_its_request() {
        let dir = folder("request");
        let (store, ..) = store(StoreFiles::default());
        let url = store.stage(DownloadId::new(4), Download { url: "http://x/f".into(), ..Download::default() });
        assert_eq!(url, "http://x/f");
        assert_eq!(DownloadList::status(&store, DownloadId::new(4)), Some(Status::Waiting));
        assert!(store.request(DownloadId::new(4)).is_some());
        store.publish(&done(4, &dir.join("f")));
        assert!(store.request(DownloadId::new(4)).is_none());
    }

    #[test]
    fn updating_a_missing_download_names_it() {
        let (store, ..) = store(StoreFiles::default());
        assert_eq!(
            DownloadStore::update(&store, DownloadId::new(9), |_| true).unwrap_err(),
            EditError::NoDownload(DownloadId::new(9))
        );
        assert_eq!(DownloadList::update(&store, DownloadId::new(9), |_| true), None);
    }

    #[test]
    fn the_list_queues_and_next_id_survive_a_save() {
        let dir = folder("save");
        let files =
            StoreFiles { history: Some(dir.join("history.json")), requests: None, quota: Some(dir.join("quota.json")) };
        let (first, ..) = store(files.clone());
        first.publish(&Outgoing::Added { id: DownloadId::new(5), url: "http://x/f".into() });
        first.queues().push(Queue::new(QueueId::new(8), "night"));
        first.save(DownloadId::new(12));
        let (second, _, dirty) = store(files.clone());
        assert!(second.entry(DownloadId::new(5)).is_some());
        assert_eq!(second.queues().len(), 2);
        assert!(dirty.try_recv().is_err());
        let (_, _, next_id) = DownloadStore::open(files, Arc::new(Listener(Arc::default())));
        assert_eq!(next_id, DownloadId::new(12));
    }

    #[test]
    fn an_unreadable_history_is_moved_aside() {
        let dir = folder("bad");
        let path = dir.join("history.json");
        std::fs::write(&path, b"not json").unwrap();
        let (store, ..) = store(StoreFiles { history: Some(path.clone()), ..StoreFiles::default() });
        assert!(store.table().is_empty());
        assert!(path.with_extension("json.bad").exists());
    }

    #[test]
    fn a_form_body_must_be_base64() {
        let mut d = Download { url: "http://x/f".into(), body: Some("not base64!".into()), ..Download::default() };
        assert!(!body_is_sendable(&d));
        d.body = Some("aGk=".into());
        assert!(body_is_sendable(&d));
    }
}
