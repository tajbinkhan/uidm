//! UIDM daemon: the long-lived process that owns the download engine.
//!
//! Clients (a host per browser connection, and the desktop window) connect over
//! loopback TCP and speak the same length-prefixed JSON frames as the browser.
//! Each connection starts with a mutual handshake over the token this daemon
//! wrote at start (see `uidm::ipc`); until the client passes it, it gets no
//! events and its requests are not read.
//!
//! Replies to a request (`hello`, `pong`, `ack`, `downloads`, `added_many`,
//! `media_info`, the site grabber's `grab_progress` and `grab_done`, `error`)
//! go to the client that asked. Download events (`added`, `started`, `progress`, `done`,
//! `failed`, `canceled`, `removed`) go to every connected client. The daemon
//! also applies each event to a `DownloadTable`, so a client that connects late
//! can ask for the whole list.
//!
//! Because downloads live here rather than in the host, they keep running when
//! the browser closes. The daemon exits on its own once it has had no clients
//! and no downloads for `IDLE_EXIT`.
//!
//! The list survives restarts: every change is written to the history file
//! (debounced, and again on exit), and the requests that may still be retried
//! go to a DPAPI-encrypted file next to it (see `uidm::history`). Settings are
//! loaded at start and changed with `set_settings` (see `uidm::settings`).
//!
//! With the save-as dialog on, a new download is probed first and waits as
//! `confirming` until a window answers with `confirm`. If no window is open
//! the daemon starts one; if none connects within `CONFIRM_FALLBACK`, the
//! download starts with the proposed name and folder.
//!
//! When a download finishes or fails, the daemon scans it for viruses if
//! asked, plays the chosen sound, and shows a Windows notification, whose
//! buttons it handles itself (open, show in folder, retry). In dialog mode an
//! open window shows a dialog instead.
//!
//! Queues sit above the engine. Each holds downloads waiting in order; while a
//! queue is started, a scheduler thread starts its waiting downloads up to the
//! queue's own limit (the engine's limit still caps the total), starts and
//! stops queues on their schedules, and when a queue's run is done does what
//! it asks: exit, or sleep, hibernate or shut down after a countdown the
//! window can cancel. Stopping a queue puts its running downloads back to
//! waiting in it, progress kept.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::net::{TcpListener, TcpStream};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use uidm::app::{progress_to_outgoing, to_request, MAX_MESSAGE_BYTES};
use uidm::engine::DownloadRequest;
use uidm::net::{self, Login, NetConfig};
use uidm::settings::{ProxyMode, SocksVersion};
use uidm::engine::{self, Engine, Progress};
use uidm::history::{self, History};
use uidm::ipc::{self, Handshake, AUTH_TIMEOUT, DAEMON_ADDR, DAEMON_LABEL, HOST_LABEL};
use uidm::protocol::{
    read_message, write_frame, Answer, Download, DownloadEntry, DownloadLogin, Incoming, OnDone, Outgoing, Properties,
    Status,
};
use uidm::settings::{self, Duplicate, FileExists, Folders, Settings, SPEED_LIMIT_RANGE_KBPS};
use uidm::notify::{self, Sound, Toast};
use uidm::power;
use uidm::queue::{self, Queue, WhenDone, COUNTDOWN_SECS, MAIN_QUEUE};
use uidm::protocol::Scan;
use uidm::grab::{self, GrabRequest};
use uidm::registry;
use uidm::scan;
use uidm::settings::Notify;
use uidm::quota::Usage;
use uidm::state::DownloadTable;

/// Exit after this long with no clients and no queued or running downloads.
const IDLE_EXIT: Duration = Duration::from_secs(60);
const IDLE_POLL: Duration = Duration::from_secs(5);
/// Collect changes for this long before writing the history.
const HISTORY_DEBOUNCE: Duration = Duration::from_secs(1);
/// How long a download waits for a window to show its save-as dialog before
/// it starts with the proposed name and folder.
const CONFIRM_FALLBACK: Duration = Duration::from_secs(20);
/// How long answering a save-as dialog waits for the download it started
/// early to stop, before starting over without its bytes.
const EARLY_STOP_WAIT: Duration = Duration::from_secs(15);
/// The folder early downloads are written in, inside the temporary folder.
const EARLY_FOLDER: &str = "UIDM early";
/// A failure is announced after this long, so a download the browser
/// extension takes back (and removes) right away is not announced at all.
const FAILED_GRACE: Duration = Duration::from_secs(1);
/// Notifications kept alive so their buttons keep working.
const KEEP_TOASTS: usize = 32;
/// The icon Windows shows on UIDM's notifications, written next to the
/// history so the registry can point at it.
const ICON_PNG: &[u8] = include_bytes!("../../extension/icons/128.png");
/// The `client` a desktop window names in its `hello`.
const WINDOW_CLIENT: &str = "window";
/// How often the scheduler looks at the clock when nothing wakes it.
const SCHEDULER_POLL: Duration = Duration::from_secs(5);
/// How the site grabber names itself to servers.
const GRABBER_AGENT: &str = "Mozilla/5.0 (compatible; UIDM site grabber; obeys robots.txt)";
/// Set to make sleep, hibernate and shut down only log what they would do
/// (for testing the countdown without losing the machine).
const POWER_DRY_RUN_ENV: &str = "UIDM_POWER_DRY_RUN";

static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

/// Everything the engine's progress callback needs, so it can exist before the
/// engine does.
struct Hub {
    /// Connected clients by id, each with a channel drained by its writer.
    clients: Mutex<HashMap<u64, Sender<Vec<u8>>>>,
    /// Every download this daemon has seen, kept current from its events.
    table: Mutex<DownloadTable>,
    /// The original request for each download that may still be retried. It
    /// holds the browser's cookies, so it is dropped once a download is done or
    /// removed, and saved only encrypted.
    requests: Mutex<HashMap<u64, Download>>,
    /// Wakes the history writer after every change.
    dirty: Sender<()>,
    /// Clients that said they are a desktop window.
    windows: Mutex<HashSet<u64>>,
    /// Finished and failed downloads, for the notification thread.
    notes: Sender<Note>,
    /// The queues, the main queue first. Lock after `table`, never before.
    queues: Mutex<Vec<Queue>>,
    /// Downloads being stopped to wait in their queue again: their
    /// `canceled` event becomes `waiting`.
    held: Mutex<HashSet<u64>>,
    /// Wakes the scheduler when a download ends or a queue changes.
    wake: Sender<()>,
    /// Bytes received lately, for the download quota.
    quota: Mutex<Usage>,
    /// Rows that are conversions, not downloads: their progress is not
    /// received bytes.
    uncounted: Mutex<HashSet<u64>>,
    /// Downloads started while their save-as dialog is open, by the engine
    /// id they run under, which clients never see.
    early: Mutex<HashMap<u64, Early>>,
}

/// A download started while its save-as dialog is open, into a folder of
/// its own, until the dialog is answered.
struct Early {
    /// The download in the list it is for.
    real: u64,
    /// Its folder.
    dir: PathBuf,
    /// The file it writes, once started.
    file: Option<PathBuf>,
    /// How it ended: finished (with its file), or stopped or failed.
    end: Option<Option<PathBuf>>,
    /// Nobody waits for it: once it ends it goes, with its folder.
    discard: bool,
}

/// A page the browser rendered for a crawl: its HTML and its address then.
type Rendered = Option<(String, String)>;

/// Work for the notification thread.
enum Note {
    Finished(u64),
    Failed(u64),
    /// A finished download's virus scan is done.
    Scanned(u64),
    /// A download stopped to ask about its taken file name.
    NameTaken(u64),
}

impl Hub {
    /// Apply `msg` to the table and send it to every client. The table lock is
    /// held across the send, so the order clients see matches the table's, and
    /// a `downloads` snapshot (taken under the same lock) is never stale.
    fn publish(&self, msg: &Outgoing) {
        let mut table = self.table.lock().unwrap();
        if let Outgoing::Canceled { id, .. } = msg {
            if self.held.lock().unwrap().remove(id) {
                if let Some(mut item) = table.get(*id).cloned() {
                    item.status = Status::Waiting;
                    let waiting = Outgoing::Updated { item };
                    table.apply(&waiting);
                    let _ = self.dirty.send(());
                    let _ = self.wake.send(());
                    self.broadcast(&waiting);
                    return;
                }
            }
        }
        match msg {
            // Bytes a resumed download already had do not count again.
            Outgoing::Started { id, .. } => {
                let done = table.get(*id).map_or(0, |e| e.done);
                self.quota.lock().unwrap().started(*id, done);
            }
            Outgoing::Progress { id, done, .. } if !self.uncounted.lock().unwrap().contains(id) => {
                self.quota.lock().unwrap().progress(*id, *done, uidm::quota::now_minute());
            }
            Outgoing::Done { id, .. } | Outgoing::Failed { id, .. } | Outgoing::Canceled { id, .. } => {
                self.quota.lock().unwrap().ended(*id);
            }
            _ => {}
        }
        table.apply(msg);
        if let Outgoing::Done { id, .. } | Outgoing::Failed { id, .. } = msg {
            // It ended before the stop reached it; nothing to hold.
            self.held.lock().unwrap().remove(id);
        }
        // A queue holds what is still to do: a finished download leaves it.
        if let Outgoing::Done { id, .. } = msg {
            if self.leave_queues(*id) {
                self.publish_queues();
            }
        }
        if matches!(
            msg,
            Outgoing::Done { .. } | Outgoing::Failed { .. } | Outgoing::Canceled { .. } | Outgoing::NameTaken { .. }
        ) {
            let _ = self.wake.send(());
        }
        match msg {
            Outgoing::Done { id, .. } => {
                self.requests.lock().unwrap().remove(id);
                let _ = self.notes.send(Note::Finished(*id));
            }
            Outgoing::Failed { id, .. } => {
                let _ = self.notes.send(Note::Failed(*id));
            }
            Outgoing::NameTaken { id, .. } => {
                let _ = self.notes.send(Note::NameTaken(*id));
            }
            _ => {}
        }
        let _ = self.dirty.send(());
        self.broadcast(msg);
        // The list shows a new download's page, description and login name
        // from its request.
        if let Outgoing::Added { id, .. } = msg {
            drop(table);
            let request = self.requests.lock().unwrap().get(id).cloned();
            if let Some(d) = request {
                let _ = self.update(*id, |e| {
                    let before = e.clone();
                    e.referrer = e.referrer.take().or(d.referrer);
                    e.description = e.description.take().or(d.description);
                    e.category = e.category.take().or(d.category);
                    e.login_user = e.login_user.take().or(d.login.map(|l| l.username));
                    e.speed_limit = e.speed_limit.or(d.speed_limit);
                    e.from_browser = e.from_browser || d.user_agent.is_some();
                    if *e == before {
                        return Err("nothing new".into());
                    }
                    Ok(())
                });
            }
        }
    }

    /// Change download `id` with `f` and announce the new entry, all under the
    /// table lock, so the check `f` makes cannot race another change. `f`
    /// returns an error to leave the entry alone.
    fn update(
        &self,
        id: u64,
        f: impl FnOnce(&mut DownloadEntry) -> Result<(), String>,
    ) -> Result<DownloadEntry, String> {
        let mut table = self.table.lock().unwrap();
        let mut item = table
            .get(id)
            .cloned()
            .ok_or_else(|| format!("no download with id {id}"))?;
        f(&mut item)?;
        let msg = Outgoing::Updated { item: item.clone() };
        table.apply(&msg);
        let _ = self.dirty.send(());
        self.broadcast(&msg);
        Ok(item)
    }

    /// Whether a client other than a window is connected: the browser
    /// extension, through its host.
    /// Keep the events of an early download (one started while its save-as
    /// dialog is open) from the clients: its progress shows on the download
    /// waiting in the dialog instead. Returns whether `p` was one.
    fn early_event(&self, p: &Progress) -> bool {
        let id = match p {
            Progress::Started { id, .. }
            | Progress::Advanced { id, .. }
            | Progress::Finished { id, .. }
            | Progress::Failed { id, .. }
            | Progress::Canceled { id, .. }
            | Progress::NameTaken { id, .. } => *id,
        };
        let mut early = self.early.lock().unwrap();
        let Some(e) = early.get_mut(&id) else {
            return false;
        };
        let real = e.real;
        let mut shown = None;
        match p {
            Progress::Started { path, partial, total, .. } => {
                e.file = Some(partial.clone().unwrap_or_else(|| path.clone()));
                shown = Some((0, *total));
            }
            Progress::Advanced { done, total, .. } => shown = Some((*done, *total)),
            Progress::Finished { path, .. } => e.end = Some(Some(path.clone())),
            Progress::Failed { .. } | Progress::Canceled { .. } | Progress::NameTaken { .. } => e.end = Some(None),
        }
        if e.discard && e.end.is_some() {
            if let Some(gone) = early.remove(&id) {
                let _ = std::fs::remove_dir_all(gone.dir);
            }
            return true;
        }
        drop(early);
        if let Some((done, total)) = shown {
            let _ = self.update(real, |e| {
                if e.status != Status::Confirming {
                    return Err("answered".into());
                }
                e.done = done;
                e.total = total.or(e.total);
                Ok(())
            });
        }
        true
    }

    /// The engine id of download `real`'s early download, if it has one.
    fn early_of(&self, real: u64) -> Option<u64> {
        self.early.lock().unwrap().iter().find(|(_, e)| e.real == real).map(|(id, _)| *id)
    }

    fn extension_connected(&self) -> bool {
        let windows = self.windows.lock().unwrap().len();
        self.clients.lock().unwrap().len() > windows
    }

    fn window_open(&self) -> bool {
        !self.windows.lock().unwrap().is_empty()
    }

    /// Send every queue to every client, and save them.
    fn publish_queues(&self) {
        let items = self.queues.lock().unwrap().clone();
        let _ = self.dirty.send(());
        self.broadcast(&Outgoing::Queues { items });
    }

    /// Change queue `id` with `f`, then announce the queues.
    fn update_queue<T>(&self, id: u64, f: impl FnOnce(&mut Queue) -> Result<T, String>) -> Result<T, String> {
        let result = {
            let mut queues = self.queues.lock().unwrap();
            let q = queues
                .iter_mut()
                .find(|q| q.id == id)
                .ok_or_else(|| format!("no queue with id {id}"))?;
            f(q)?
        };
        self.publish_queues();
        let _ = self.wake.send(());
        Ok(result)
    }

    /// Take download `id` out of whatever queue holds it.
    fn leave_queues(&self, id: u64) -> bool {
        let mut changed = false;
        for q in self.queues.lock().unwrap().iter_mut() {
            let before = q.order.len();
            q.order.retain(|x| *x != id);
            changed |= q.order.len() != before;
        }
        changed
    }

    /// The queue holding download `id`.
    fn queue_of(&self, id: u64) -> Option<u64> {
        self.queues
            .lock()
            .unwrap()
            .iter()
            .find(|q| q.order.contains(&id))
            .map(|q| q.id)
    }

    /// Send `msg` to every client.
    fn broadcast(&self, msg: &Outgoing) {
        let Ok(frame) = serde_json::to_vec(msg) else {
            return;
        };
        // Dropping a client whose writer is gone keeps the map tidy.
        self.clients
            .lock()
            .unwrap()
            .retain(|_, tx| tx.send(frame.clone()).is_ok());
    }
}

/// Where the daemon keeps its files. A path is `None` when its environment
/// variable is missing; that data then lives in memory only.
struct Paths {
    settings: Option<PathBuf>,
    history: Option<PathBuf>,
    requests: Option<PathBuf>,
    quota: Option<PathBuf>,
}

impl Paths {
    fn from_env() -> Self {
        let warn = |what: &str, r: io::Result<PathBuf>| {
            r.map_err(|e| eprintln!("[uidm-daemon] no {what} file: {e}")).ok()
        };
        Paths {
            settings: warn("settings", settings::settings_path()),
            history: warn("history", history::history_path()),
            requests: warn("saved requests", history::requests_path()),
            quota: warn("quota", uidm::quota::quota_path()),
        }
    }
}

struct Daemon {
    engine: Engine,
    hub: Arc<Hub>,
    settings: Mutex<Settings>,
    paths: Paths,
    /// Keeps the history writer and the exit flush from writing at once.
    save_lock: Mutex<()>,
    /// Downloads stopped by `pause_all`, for `resume_all`.
    paused: Mutex<HashSet<u64>>,
    /// Recent notifications; dropping one can stop its buttons working.
    toasts: Mutex<VecDeque<windows::UI::Notifications::ToastNotification>>,
    /// Queues stopped by `pause_all`, for `resume_all`.
    paused_queues: Mutex<HashSet<u64>>,
    /// What each started queue has started in its current run.
    runs: Mutex<HashMap<u64, HashSet<u64>>>,
    /// Retries each download got from its queue in the current run.
    attempts: Mutex<HashMap<u64, u32>>,
    /// The countdown before a power action, while one runs.
    countdown: Mutex<Option<Countdown>>,
    /// Keeps two scheduler passes from running at once.
    tick_lock: Mutex<()>,
    /// The download waiting for a new link from the browser, if any.
    refresh_waiting: Mutex<Option<u64>>,
    /// Each client's running site crawl, by its stop flag.
    grabs: Mutex<HashMap<u64, Arc<AtomicBool>>>,
    /// Links from a page, held for the window that is being opened.
    picked_links: Mutex<Option<Outgoing>>,
    /// What the quota stopped (queues and downloads), while it holds them.
    quota_paused: Mutex<Option<(Vec<u64>, HashSet<u64>)>>,
    /// The nearly-reached warning was sent for the current window.
    quota_warned: AtomicBool,
    /// Crawls waiting for the browser's cookies, by request.
    cookie_waits: Mutex<HashMap<u64, Sender<Option<String>>>>,
    /// Crawls waiting for a page the browser renders: its HTML and address.
    render_waits: Mutex<HashMap<u64, Sender<Rendered>>>,
    /// Grabber projects whose scheduled run is going on, by name.
    grab_runs: Mutex<HashSet<String>>,
    /// Running conversions, by their row's id, with a flag to stop them.
    conversions: Mutex<HashMap<u64, Arc<AtomicBool>>>,
    /// When the projects' schedules were last looked at.
    grab_checked: Mutex<Option<Instant>>,
}

/// A countdown before sleeping, hibernating or shutting down.
struct Countdown {
    action: WhenDone,
    queue: String,
    /// The download's file name, when one download asked for it.
    file: Option<String>,
    force_close: bool,
    deadline: Instant,
    canceled: Arc<AtomicBool>,
}

impl Daemon {
    /// Load settings, history and saved requests, and start the engine with
    /// them. Returns the receivers the history writer and the notification
    /// thread wait on.
    fn new(paths: Paths) -> (Self, Receiver<()>, Receiver<Note>, Receiver<()>) {
        let settings = match &paths.settings {
            Some(path) => Settings::load(path).unwrap_or_else(|e| {
                eprintln!("[uidm-daemon] ignoring {}: {e}", path.display());
                Settings::default()
            }),
            None => Settings::default(),
        };
        let history = match &paths.history {
            Some(path) => History::load(path).unwrap_or_else(|e| {
                // Keep the unreadable file for inspection instead of
                // overwriting it with an empty list.
                let bad = path.with_extension("json.bad");
                eprintln!(
                    "[uidm-daemon] could not read {}: {e}; moved to {}",
                    path.display(),
                    bad.display()
                );
                let _ = std::fs::rename(path, &bad);
                History::default()
            }),
            None => History::default(),
        };
        let mut requests = match &paths.requests {
            Some(path) => history::load_requests(path).unwrap_or_else(|e| {
                eprintln!("[uidm-daemon] could not read saved requests: {e}");
                HashMap::new()
            }),
            None => HashMap::new(),
        };

        let mut table = DownloadTable::new();
        let mut queues = history.queues;
        if queues.is_empty() {
            queues.push(Queue::main());
        }
        table.apply(&Outgoing::Downloads {
            items: history.items,
        });
        requests.retain(|id, _| table.get(*id).is_some_and(|e| e.status.is_retryable()));

        let (dirty, dirty_rx) = channel();
        let (notes, notes_rx) = channel();
        let (wake, wake_rx) = channel();
        let hub = Arc::new(Hub {
            clients: Mutex::new(HashMap::new()),
            table: Mutex::new(table),
            requests: Mutex::new(requests),
            dirty,
            windows: Mutex::new(HashSet::new()),
            notes,
            queues: Mutex::new(queues),
            held: Mutex::new(HashSet::new()),
            quota: Mutex::new(paths.quota.as_deref().map(Usage::load).unwrap_or_default()),
            uncounted: Mutex::new(HashSet::new()),
            early: Mutex::new(HashMap::new()),
            wake,
        });
        let events = Arc::clone(&hub);
        let engine = Engine::new(
            Arc::new(move |p: Progress| {
                if !events.early_event(&p) {
                    events.publish(&progress_to_outgoing(p));
                }
            }),
            settings.max_concurrent,
        );
        engine.set_speed_limit(settings.speed_limit_bytes());
        let max_id = hub.table.lock().unwrap().max_id();
        engine.set_next_id(history.next_id.max(max_id + 1));

        let daemon = Daemon {
            engine,
            hub,
            settings: Mutex::new(settings),
            paths,
            save_lock: Mutex::new(()),
            paused: Mutex::new(HashSet::new()),
            toasts: Mutex::new(VecDeque::new()),
            paused_queues: Mutex::new(HashSet::new()),
            runs: Mutex::new(HashMap::new()),
            attempts: Mutex::new(HashMap::new()),
            countdown: Mutex::new(None),
            tick_lock: Mutex::new(()),
            refresh_waiting: Mutex::new(None),
            grabs: Mutex::new(HashMap::new()),
            picked_links: Mutex::new(None),
            quota_paused: Mutex::new(None),
            quota_warned: AtomicBool::new(false),
            cookie_waits: Mutex::new(HashMap::new()),
            render_waits: Mutex::new(HashMap::new()),
            grab_runs: Mutex::new(HashSet::new()),
            conversions: Mutex::new(HashMap::new()),
            grab_checked: Mutex::new(None),
        };
        (daemon, dirty_rx, notes_rx, wake_rx)
    }

    /// Start a `waiting` (or `confirming`) download now. A download that ran
    /// before resumes its own file; one that never ran goes to the file chosen
    /// for it, if any.
    fn dispatch(&self, id: u64) -> Result<(), String> {
        let download = self.hub.requests.lock().unwrap().get(&id).cloned();
        let mut plan = (None, false);
        let item = self.hub.update(id, |e| {
            if !matches!(e.status, Status::Waiting | Status::Confirming) {
                return Err(format!("download {id} is not waiting to be started"));
            }
            plan = (e.path.clone(), e.connections > 0);
            e.status = Status::Queued;
            Ok(())
        })?;
        let download = download.unwrap_or_else(|| Download {
            url: item.url.clone(),
            ..Download::default()
        });
        let mut req = self.request_for(download);
        req.speed_limit = item.speed_limit;
        req.reuse_partial = item.partial.as_deref().map(PathBuf::from);
        match plan {
            (Some(path), true) => req.reuse_path = Some(PathBuf::from(path)),
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

    /// Put download `id` in queue `queue` at `position` (or at the end). A
    /// download that is not running waits there.
    fn move_to_queue(&self, id: u64, queue: u64, position: Option<usize>) -> Result<(), String> {
        let status = self.status(id).ok_or_else(|| format!("no download with id {id}"))?;
        if matches!(status, Status::Done | Status::Confirming) {
            return Err("Only a download that has not finished can wait in a queue.".to_string());
        }
        if !self.hub.queues.lock().unwrap().iter().any(|q| q.id == queue) {
            return Err(format!("no queue with id {queue}"));
        }
        // Joining a queue (not just moving within it) makes a stopped download
        // wait to run again.
        let reorder = position.is_some() && self.hub.queue_of(id) == Some(queue);
        if status.is_retryable() && status != Status::Waiting && !reorder {
            self.hub.update(id, |e| {
                e.status = Status::Waiting;
                Ok(())
            })?;
        }
        self.hub.leave_queues(id);
        self.hub.update_queue(queue, |q| {
            let at = position.unwrap_or(q.order.len()).min(q.order.len());
            q.order.insert(at, id);
            Ok(())
        })
    }

    /// Create a queue (id 0) or change one. Its downloads and running state
    /// are kept; only what the user edits changes.
    fn save_queue(&self, edited: Queue) -> Result<u64, String> {
        edited.validate()?;
        let name = edited.name.trim().to_string();
        let id = {
            let mut queues = self.hub.queues.lock().unwrap();
            if queues
                .iter()
                .any(|q| q.id != edited.id && q.name.eq_ignore_ascii_case(&name))
            {
                return Err(format!("There is already a queue named \"{name}\"."));
            }
            let id = if edited.id == 0 {
                let id = queues.iter().map(|q| q.id).max().unwrap_or(MAIN_QUEUE) + 1;
                queues.push(Queue::new(id, &name));
                id
            } else {
                edited.id
            };
            let q = queues
                .iter_mut()
                .find(|q| q.id == id)
                .ok_or_else(|| format!("no queue with id {id}"))?;
            if q.schedule != edited.schedule && !q.running {
                // A new schedule may start today even if the old one ran. A
                // run in progress keeps its day, so its stop time still holds.
                q.last_scheduled_day = None;
                q.last_scheduled_start = None;
            }
            q.name = name;
            q.max_concurrent = edited.max_concurrent;
            q.schedule = edited.schedule;
            q.when_done = edited.when_done;
            q.retries = edited.retries;
            q.open_when_done = edited.open_when_done.map(|f| f.trim().to_string()).filter(|f| !f.is_empty());
            q.start_on_launch = edited.start_on_launch;
            id
        };
        self.hub.publish_queues();
        let _ = self.hub.wake.send(());
        Ok(id)
    }

    /// Delete a queue. Its downloads move to the main queue, which cannot be
    /// deleted; the running ones keep running.
    fn delete_queue(&self, id: u64) -> Result<(), String> {
        if id == MAIN_QUEUE {
            return Err("The main queue cannot be deleted.".to_string());
        }
        {
            let mut queues = self.hub.queues.lock().unwrap();
            let pos = queues
                .iter()
                .position(|q| q.id == id)
                .ok_or_else(|| format!("no queue with id {id}"))?;
            let gone = queues.remove(pos);
            if let Some(main) = queues.iter_mut().find(|q| q.id == MAIN_QUEUE) {
                main.order.extend(gone.order);
            }
        }
        self.runs.lock().unwrap().remove(&id);
        self.hub.publish_queues();
        Ok(())
    }

    /// Start a queue: its waiting downloads run in order.
    fn start_queue(&self, id: u64, scheduled_day: Option<i64>) -> Result<(), String> {
        self.hub.update_queue(id, |q| {
            q.running = true;
            q.started_by_schedule = scheduled_day.is_some();
            if scheduled_day.is_some() {
                q.last_scheduled_day = scheduled_day;
                q.last_scheduled_start = Some(queue::local_now().total());
            }
            Ok(())
        })?;
        self.runs.lock().unwrap().insert(id, HashSet::new());
        Ok(())
    }

    /// Stop a queue. Its running downloads stop and wait in it again, with
    /// their progress kept.
    fn stop_queue(&self, id: u64) -> Result<(), String> {
        let order = self.hub.update_queue(id, |q| {
            q.running = false;
            q.started_by_schedule = false;
            q.when_done_once = None;
            Ok(q.order.clone())
        })?;
        self.runs.lock().unwrap().remove(&id);
        let running: Vec<u64> = {
            let table = self.hub.table.lock().unwrap();
            order
                .into_iter()
                .filter(|i| {
                    table
                        .get(*i)
                        .is_some_and(|e| matches!(e.status, Status::Queued | Status::Downloading))
                })
                .collect()
        };
        for i in running {
            self.hub.held.lock().unwrap().insert(i);
            self.engine.cancel(i);
        }
        Ok(())
    }

    /// One pass of the scheduler: start and stop queues on their schedules,
    /// start waiting downloads in started queues, retry failed ones, and end
    /// the runs that are done.
    fn tick(&self) {
        let _guard = self.tick_lock.lock().unwrap();
        self.check_quota();
        let now = queue::local_now();

        let queues = self.hub.queues.lock().unwrap().clone();
        for q in &queues {
            let Some(schedule) = &q.schedule else {
                continue;
            };
            if let Some(day) = schedule.due(now, q.last_scheduled_day, q.last_scheduled_start) {
                if !q.running {
                    eprintln!("[uidm-daemon] schedule starts queue {}", q.name);
                    let _ = self.start_queue(q.id, Some(day));
                }
            }
            // At the end of its time a scheduled run stops: always when it
            // has a stop time, and without one only if it never started
            // anything (otherwise it runs until it is done).
            let started_any = !self.runs.lock().unwrap().get(&q.id).is_none_or(|r| r.is_empty());
            if q.running && q.started_by_schedule && (schedule.stop.is_some() || !started_any) {
                let ended = q
                    .last_scheduled_day
                    .and_then(|day| schedule.end_of_run(day))
                    .is_some_and(|end| now >= end);
                if ended {
                    eprintln!("[uidm-daemon] schedule stops queue {}", q.name);
                    let _ = self.stop_queue(q.id);
                }
            }
        }

        let queues = self.hub.queues.lock().unwrap().clone();
        for q in queues.iter().filter(|q| q.running) {
            let statuses: Vec<(u64, Status)> = {
                let table = self.hub.table.lock().unwrap();
                q.order
                    .iter()
                    .filter_map(|id| table.get(*id).map(|e| (*id, e.status)))
                    .collect()
            };
            let started = self.runs.lock().unwrap().entry(q.id).or_default().clone();

            // Failed in this run and retries left: wait again.
            for (id, status) in &statuses {
                if *status == Status::Failed && started.contains(id) {
                    let mut attempts = self.attempts.lock().unwrap();
                    let n = attempts.entry(*id).or_insert(0);
                    if *n < q.retries {
                        *n += 1;
                        eprintln!("[uidm-daemon] queue {} retries #{id} ({}/{})", q.name, n, q.retries);
                        drop(attempts);
                        let _ = self.hub.update(*id, |e| {
                            e.status = Status::Waiting;
                            Ok(())
                        });
                    }
                }
            }
            let statuses: Vec<(u64, Status)> = {
                let table = self.hub.table.lock().unwrap();
                q.order
                    .iter()
                    .filter_map(|id| table.get(*id).map(|e| (*id, e.status)))
                    .collect()
            };

            let mut active = statuses
                .iter()
                .filter(|(_, s)| matches!(s, Status::Queued | Status::Downloading))
                .count();
            for (id, status) in &statuses {
                if active >= q.max_concurrent {
                    break;
                }
                if *status == Status::Waiting && self.dispatch(*id).is_ok() {
                    self.runs.lock().unwrap().entry(q.id).or_default().insert(*id);
                    active += 1;
                }
            }

            // A download asking about a taken file name is still to do.
            let busy = active > 0
                || statuses
                    .iter()
                    .any(|(_, s)| matches!(s, Status::Waiting | Status::Confirming));
            // A scheduled run that has started nothing yet stays open until
            // its stop time, for downloads added meanwhile.
            let started_any = !self.runs.lock().unwrap().get(&q.id).is_none_or(|r| r.is_empty());
            if !busy && (started_any || !q.started_by_schedule) {
                self.finish_run(q.id);
            }
        }
    }

    /// A started queue has nothing left to do: stop it and, if it started
    /// anything, do what it asks when done.
    fn finish_run(&self, id: u64) {
        let started = self.runs.lock().unwrap().remove(&id).unwrap_or_default();
        {
            let mut attempts = self.attempts.lock().unwrap();
            for d in &started {
                attempts.remove(d);
            }
        }
        let Ok((name, action, open, force_close)) = self.hub.update_queue(id, |q| {
            q.running = false;
            q.started_by_schedule = false;
            let action = q.when_done_once.take().unwrap_or(q.when_done);
            Ok((q.name.clone(), action, q.open_when_done.clone(), q.force_close))
        }) else {
            return;
        };
        eprintln!("[uidm-daemon] queue {name} is done");
        if !started.is_empty() {
            if let Some(file) = open {
                open_after_run(&file);
            }
            self.when_done(action, &name, None, force_close);
        }
    }

    /// Do what a finished queue, or with `file` one finished download, asks.
    fn when_done(&self, action: WhenDone, queue: &str, file: Option<&str>, force_close: bool) {
        match action {
            WhenDone::Nothing => {}
            WhenDone::Exit => {
                eprintln!("[uidm-daemon] {} done: exiting", file.unwrap_or(queue));
                self.hub.broadcast(&Outgoing::Quit);
                self.save_history();
                // Other downloads may still run; the idle monitor ends the
                // daemon once they are done and every window has closed.
            }
            _ => self.start_countdown(action, queue, file, force_close),
        }
    }

    /// Run what download `id` asked to happen once it finished, then clear
    /// it so downloading it again does not repeat it.
    fn after_download(&self, id: u64) {
        let mut then = OnDone::default();
        let cleared = self.hub.update(id, |e| {
            if e.status != Status::Done {
                return Err("not finished".into());
            }
            then = e.on_done.take().ok_or_else(|| "nothing to do".to_string())?;
            Ok(())
        });
        if let Ok(e) = cleared {
            let name = e.path.as_deref().and_then(|p| Path::new(p).file_name()).map(|n| n.to_string_lossy().into_owned());
            self.when_done(then.then, "", Some(name.as_deref().unwrap_or(&e.url)), then.force_close);
        }
    }

    /// Set or clear what download `id` does once it finishes.
    fn set_on_done(&self, id: u64, on_done: Option<OnDone>) -> Result<(), String> {
        self.hub.update(id, |e| {
            if e.status == Status::Done {
                return Err("This download has already finished.".into());
            }
            e.on_done = on_done.filter(|o| *o != OnDone::default());
            Ok(())
        })?;
        Ok(())
    }

    fn start_countdown(&self, action: WhenDone, queue: &str, file: Option<&str>, force_close: bool) {
        let canceled = Arc::new(AtomicBool::new(false));
        let deadline = Instant::now() + Duration::from_secs(COUNTDOWN_SECS as u64);
        if let Some(old) = self.countdown.lock().unwrap().replace(Countdown {
            action,
            queue: queue.to_string(),
            file: file.map(str::to_string),
            force_close,
            deadline,
            canceled: Arc::clone(&canceled),
        }) {
            old.canceled.store(true, Ordering::Relaxed);
        }
        eprintln!("[uidm-daemon] {} done: {} in {COUNTDOWN_SECS}s", file.unwrap_or(queue), action.label());
        self.hub.broadcast(&Outgoing::Countdown {
            action,
            seconds: COUNTDOWN_SECS,
            queue: queue.to_string(),
            file: file.map(str::to_string),
        });
        self.show_window();
        // Save now: the machine may go away when the countdown ends.
        self.save_history();
        thread::spawn(move || {
            while Instant::now() < deadline {
                if canceled.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(Duration::from_millis(250));
            }
            if canceled.load(Ordering::Relaxed) {
                return;
            }
            power_off(action, force_close);
        });
    }

    /// End the countdown early and carry out its action now.
    fn countdown_now(&self) {
        let Some(c) = self.countdown.lock().unwrap().take() else {
            return;
        };
        c.canceled.store(true, Ordering::Relaxed);
        self.hub.broadcast(&Outgoing::CountdownCanceled);
        power_off(c.action, c.force_close);
    }

    fn cancel_countdown(&self) {
        if let Some(c) = self.countdown.lock().unwrap().take() {
            c.canceled.store(true, Ordering::Relaxed);
            eprintln!("[uidm-daemon] countdown canceled");
            self.hub.broadcast(&Outgoing::CountdownCanceled);
        }
    }

    /// The countdown still running, as the message a window joining now needs.
    fn countdown_message(&self) -> Option<Outgoing> {
        let mut slot = self.countdown.lock().unwrap();
        let c = slot.as_ref()?;
        if c.canceled.load(Ordering::Relaxed) || Instant::now() >= c.deadline {
            *slot = None;
            return None;
        }
        Some(Outgoing::Countdown {
            action: c.action,
            seconds: c.deadline.saturating_duration_since(Instant::now()).as_secs() as u32,
            queue: c.queue.clone(),
            file: c.file.clone(),
        })
    }

    /// Whether the daemon must stay up for a queue: one is started, one has a
    /// schedule ahead, or a countdown runs.
    fn queues_need_daemon(&self) -> bool {
        let now = queue::local_now();
        let projects = || {
            let Ok(dir) = grab::projects_dir() else {
                return false;
            };
            !self.grab_runs.lock().unwrap().is_empty()
                || grab::recent_projects(&dir).iter().any(|name| {
                    grab::Project::load(&dir, name).ok().and_then(|p| p.schedule).is_some_and(|s| {
                        s.next_start(now).is_some() || s.open_run(now).is_some()
                    })
                })
        };
        self.countdown_message().is_some()
            || projects()
            || self.hub.queues.lock().unwrap().iter().any(|q| {
                q.running
                    || q.schedule.as_ref().is_some_and(|s| {
                        s.next_start(now).is_some() || s.open_run(now).is_some()
                    })
            })
    }

    fn entry(&self, id: u64) -> Option<DownloadEntry> {
        self.hub.table.lock().unwrap().get(id).cloned()
    }

    /// Bring the window forward: ask an open one to show itself, or start one.
    fn show_window(&self) {
        if self.hub.window_open() {
            self.hub.broadcast(&Outgoing::Show);
        } else if let Err(e) = open_window() {
            eprintln!("[uidm-daemon] could not open the window: {e}");
        }
    }

    /// Stop every queued or running download, remembering them for
    /// `resume_all`.
    fn pause_all(&self) {
        let (queues, ids) = self.stop_running();
        self.paused_queues.lock().unwrap().extend(queues);
        self.paused.lock().unwrap().extend(ids);
    }

    /// Stop every started queue and every queued or running download,
    /// keeping their progress. Returns what was stopped.
    fn stop_running(&self) -> (Vec<u64>, Vec<u64>) {
        let running: Vec<u64> = self.hub.queues.lock().unwrap().iter().filter(|q| q.running).map(|q| q.id).collect();
        for q in &running {
            let _ = self.stop_queue(*q);
        }
        let ids: Vec<u64> = self
            .hub
            .table
            .lock()
            .unwrap()
            .entries()
            .filter(|e| matches!(e.status, Status::Queued | Status::Downloading))
            .map(|e| e.id)
            .collect();
        for id in &ids {
            self.engine.cancel(*id);
        }
        (running, ids)
    }

    /// The quota's state for a window that just connected, if downloads are
    /// paused for it or it is nearly reached.
    fn quota_message(&self) -> Option<Outgoing> {
        let (limit, window) = self.settings.lock().unwrap().quota()?;
        let now = uidm::quota::now_minute();
        let mut usage = self.hub.quota.lock().unwrap();
        let used = usage.used(now, window);
        let paused = self.quota_paused.lock().unwrap().is_some();
        (paused || used >= limit / 10 * 9).then(|| Outgoing::Quota {
            used,
            limit,
            paused,
            resumes_at: paused.then(|| usage.frees_at(now, window, limit) * 60),
        })
    }

    /// Pause everything when the download quota is reached, warn shortly
    /// before, and resume what it stopped once the window allows. Runs with
    /// the scheduler.
    fn check_quota(&self) {
        let quota = self.settings.lock().unwrap().quota();
        let now = uidm::quota::now_minute();
        let (used, limit, window) = match quota {
            Some((limit, window)) => (self.hub.quota.lock().unwrap().used(now, window), limit, window),
            None => (0, u64::MAX, 0),
        };
        let paused = self.quota_paused.lock().unwrap().is_some();
        let msg = |paused: bool, resumes_at: Option<u64>| Outgoing::Quota {
            used,
            limit,
            paused,
            resumes_at,
        };
        if paused && used < limit {
            let (queues, ids) = self.quota_paused.lock().unwrap().take().unwrap_or_default();
            eprintln!("[uidm-daemon] quota allows downloads again");
            self.quota_warned.store(false, Ordering::Relaxed);
            for q in queues {
                let _ = self.start_queue(q, None);
            }
            for id in ids.into_iter().filter(|id| self.status(*id) == Some(Status::Canceled)) {
                if let Err(e) = self.retry(id) {
                    eprintln!("[uidm-daemon] could not resume #{id}: {e}");
                }
            }
            self.hub.broadcast(&msg(false, None));
        } else if used >= limit {
            // Also stops what started since.
            let (queues, ids) = self.stop_running();
            let first = !paused;
            {
                let mut held = self.quota_paused.lock().unwrap();
                let (q, i) = held.get_or_insert_with(Default::default);
                q.extend(queues);
                i.extend(ids);
            }
            if first {
                let at = self.hub.quota.lock().unwrap().frees_at(now, window, limit) * 60;
                eprintln!("[uidm-daemon] quota reached: {used} of {limit} bytes");
                self.hub.broadcast(&msg(true, Some(at)));
            }
        } else if used >= limit / 10 * 9 && !self.quota_warned.swap(true, Ordering::Relaxed) {
            self.hub.broadcast(&msg(false, None));
        }
    }

    /// Start again what `pause_all` stopped, and every interrupted download.
    fn resume_all(&self) {
        let queues: HashSet<u64> = std::mem::take(&mut *self.paused_queues.lock().unwrap());
        for q in queues {
            let _ = self.start_queue(q, None);
        }
        let paused: HashSet<u64> = std::mem::take(&mut *self.paused.lock().unwrap());
        let ids: Vec<u64> = self
            .hub
            .table
            .lock()
            .unwrap()
            .entries()
            .filter(|e| {
                e.status == Status::Interrupted || (e.status == Status::Canceled && paused.contains(&e.id))
            })
            .map(|e| e.id)
            .collect();
        for id in ids {
            if let Err(e) = self.retry(id) {
                eprintln!("[uidm-daemon] could not resume #{id}: {e}");
            }
        }
    }

    /// Handle one note: scan a finished download first when asked, then play
    /// the sound and notify.
    fn on_note(self: &Arc<Self>, note: Note) {
        let settings = self.settings.lock().unwrap().clone();
        match note {
            Note::Finished(id) if settings.scan_after_download => {
                let scanning = self.hub.update(id, |e| {
                    if e.status != Status::Done || e.path.is_none() {
                        return Err("not a finished file".into());
                    }
                    e.scan = Some(Scan::Scanning);
                    Ok(())
                });
                let Ok(entry) = scanning else {
                    return;
                };
                let daemon = Arc::clone(self);
                thread::spawn(move || {
                    let path = PathBuf::from(entry.path.unwrap_or_default());
                    let result = scan::scan(settings.scanner, &settings.scanner_command, &path);
                    eprintln!("[uidm-daemon] scan #{id}: {result:?}");
                    let _ = daemon.hub.update(id, |e| {
                        e.scan = Some(result);
                        Ok(())
                    });
                    let _ = daemon.hub.notes.send(Note::Scanned(id));
                });
            }
            Note::Finished(id) | Note::Scanned(id) => {
                self.notify(id, false, &settings);
                self.after_download(id);
            }
            Note::Failed(id) => {
                thread::sleep(FAILED_GRACE);
                if self.entry(id).is_some_and(|e| e.status == Status::Failed) {
                    self.notify(id, true, &settings);
                }
            }
            // Without a window to ask, a taken name gets a new one.
            Note::NameTaken(id) => self.await_answer(id, Some(FileExists::Rename)),
        }
    }

    /// Wait for a window to answer download `id`'s save-as dialog, opening
    /// one if none is open. If none connects within `CONFIRM_FALLBACK`, the
    /// download starts as proposed: `fallback` decides what happens if its
    /// name is taken, `None` leaving that to the setting.
    fn await_answer(self: &Arc<Self>, id: u64, fallback: Option<FileExists>) {
        if !self.hub.window_open() {
            if let Err(e) = open_window() {
                eprintln!("[uidm-daemon] could not open the window: {e}");
            }
        }
        let daemon = Arc::clone(self);
        thread::spawn(move || {
            thread::sleep(CONFIRM_FALLBACK);
            if daemon.status(id) == Some(Status::Confirming) && !daemon.hub.window_open() {
                eprintln!("[uidm-daemon] no window to confirm #{id}; starting it as proposed");
                // A "download later" asked only about a duplicate keeps waiting.
                let (start, queue) = daemon
                    .hub
                    .requests
                    .lock()
                    .unwrap()
                    .get(&id)
                    .map_or((true, None), |d| (d.start != Some(false), d.queue));
                let answer = Answer {
                    start,
                    queue,
                    file_exists: fallback,
                    ..Answer::start(id)
                };
                if let Err(e) = daemon.confirm(answer) {
                    eprintln!("[uidm-daemon] could not start #{id}: {e}");
                }
            }
        });
    }

    fn notify(self: &Arc<Self>, id: u64, failed: bool, settings: &Settings) {
        let Some(entry) = self.entry(id) else {
            return;
        };
        if failed && settings.sound_on_failed {
            notify::play(Sound::Failed);
        }
        if !failed && settings.sound_on_done {
            notify::play(Sound::Done);
        }
        // A download that asked for the completion dialog gets it, or a
        // notification in its place when no window is open.
        let dialog = !failed && entry.on_done.is_some_and(|o| o.dialog);
        let toast = match settings.notify {
            _ if dialog => !self.hub.window_open(),
            Notify::Off => false,
            Notify::Toast => true,
            // An open window shows its own dialog.
            Notify::Dialog => !self.hub.window_open(),
        };
        if !toast {
            return;
        }
        let daemon = Arc::clone(self);
        match notify::show(&toast_for(&entry, failed), move |arg| daemon.on_toast(&arg)) {
            Ok(t) => {
                let mut toasts = self.toasts.lock().unwrap();
                toasts.push_back(t);
                while toasts.len() > KEEP_TOASTS {
                    toasts.pop_front();
                }
            }
            Err(e) => eprintln!("[uidm-daemon] could not show a notification: {e}"),
        }
    }

    /// Act on a click on a notification or one of its buttons.
    fn on_toast(&self, arg: &str) {
        let (action, id) = match arg.split_once(':') {
            Some((a, id)) => (a, id.parse::<u64>().ok()),
            None => (arg, None),
        };
        let entry = id.and_then(|id| self.entry(id));
        let path = entry.as_ref().and_then(|e| e.path.clone());
        match (action, path) {
            // A file with a threat is opened only from the window, which asks.
            ("open", Some(_)) if matches!(entry.as_ref().and_then(|e| e.scan.as_ref()), Some(Scan::Threat { .. })) => {
                self.show_window()
            }
            ("open", Some(path)) => {
                let _ = Command::new("explorer").arg(path).spawn();
            }
            ("show", Some(path)) => {
                // Explorer parses /select itself; Windows paths hold no quotes.
                let _ = Command::new("explorer").raw_arg(format!("/select,\"{path}\"")).spawn();
            }
            ("retry", _) => {
                if let Some(id) = id {
                    if let Err(e) = self.retry(id) {
                        eprintln!("[uidm-daemon] retry from a notification: {e}");
                    }
                }
            }
            _ => self.show_window(),
        }
    }

    /// Take a new download: ask where to save it when the settings say so
    /// (unless its type starts without asking), otherwise queue it at once,
    /// or only add it to the Download later queue when the settings say
    /// that. If its address is already in the list, the duplicate setting
    /// decides first, asking in the save-as dialog under `Ask` even when
    /// that dialog is off.
    fn add(self: &Arc<Self>, mut download: Download) -> (u64, String) {
        if let Some(old) = self.duplicate_of(&download) {
            let choice = download.duplicate.unwrap_or(self.settings.lock().unwrap().if_duplicate);
            eprintln!("[uidm-daemon] {} is already in the list as #{} ({choice:?})", download.url, old.id);
            if choice == Duplicate::Ask {
                return self.ask(download, Some(old.id));
            }
            if let Some(kept) = self.settle_duplicate(&mut download, &old, choice) {
                return kept;
            }
        }
        let settings = self.settings.lock().unwrap().clone();
        if download.start == Some(false) || (download.start.is_none() && settings.add_only) {
            return self.add_later(download);
        }
        let name = engine::Inspection::unprobed(&download.url, download.filename.as_deref()).filename;
        if download.ask.unwrap_or(settings.ask_where_to_save && !settings.starts_without_asking(&name)) {
            self.ask(download, None)
        } else {
            self.start(download, None)
        }
    }

    /// The newest download in the list with `download`'s address. Form posts
    /// and streams are never duplicates: one address can stand for different
    /// files there (another form, another quality).
    fn duplicate_of(&self, download: &Download) -> Option<DownloadEntry> {
        if uidm::app::post_body(download).is_some() || download.media.is_some() {
            return None;
        }
        let table = self.hub.table.lock().unwrap();
        table
            .entries()
            .filter(|e| uidm::app::same_address(&e.url, &download.url))
            .last()
            .cloned()
    }

    /// Apply `choice` to `download`, whose address download `old` already
    /// has. Returns the download to report when nothing new should be added;
    /// otherwise `download` is ready to add.
    fn settle_duplicate(
        self: &Arc<Self>,
        download: &mut Download,
        old: &DownloadEntry,
        choice: Duplicate,
    ) -> Option<(u64, String)> {
        match choice {
            Duplicate::Ask | Duplicate::NewName => {
                download.file_exists = Some(FileExists::Rename);
                None
            }
            Duplicate::Existing => self.use_existing(old),
            // A running download's file cannot be replaced under it.
            Duplicate::Overwrite if !old.status.is_terminal() => self.use_existing(old),
            Duplicate::Overwrite => {
                if let Some(path) = old.path.as_deref().map(Path::new) {
                    download.filename = path.file_name().map(|n| n.to_string_lossy().into_owned());
                    download.folder = path.parent().map(|d| d.display().to_string());
                }
                download.file_exists = Some(FileExists::Overwrite);
                if let Err(e) = self.remove(old.id, false) {
                    eprintln!("[uidm-daemon] could not remove #{}: {e}", old.id);
                }
                None
            }
        }
    }

    /// Point the user at download `old` instead of adding its address again:
    /// show a finished one's completion dialog again, resume a stopped one,
    /// and bring the window up for a running one. `None` when the finished
    /// file is gone, so the new download should be added after all.
    fn use_existing(self: &Arc<Self>, old: &DownloadEntry) -> Option<(u64, String)> {
        let kept = Some((old.id, old.url.clone()));
        match old.status {
            Status::Done if !old.path.as_deref().is_some_and(|p| Path::new(p).is_file()) => None,
            Status::Done => {
                if self.hub.window_open() {
                    self.hub.broadcast(&Outgoing::ShowDone { id: old.id });
                } else {
                    // Without a window, a notification stands in for the
                    // dialog, whatever the notification setting.
                    let mut settings = self.settings.lock().unwrap().clone();
                    settings.notify = Notify::Toast;
                    settings.sound_on_done = false;
                    self.notify(old.id, false, &settings);
                }
                kept
            }
            s if s.is_retryable() => match self.retry(old.id) {
                Ok(resumed) => Some(resumed),
                Err(e) => {
                    eprintln!("[uidm-daemon] could not resume #{}: {e}", old.id);
                    kept
                }
            },
            _ => {
                self.show_window();
                kept
            }
        }
    }

    /// Take download `id` off the list though it never ran: a duplicate the
    /// user chose not to add.
    fn drop_entry(&self, id: u64) {
        self.hub.requests.lock().unwrap().remove(&id);
        if self.hub.leave_queues(id) {
            self.hub.publish_queues();
        }
        self.hub.publish(&Outgoing::Removed { id });
    }

    /// Add `download` to a queue (the one it names, else the one Download
    /// later goes to, else the main one) without starting it or opening a
    /// connection.
    fn add_later(&self, download: Download) -> (u64, String) {
        let later = self.settings.lock().unwrap().later_queue;
        let queue = self.queue_or_main(download.queue.or(later));
        let (id, url) = self.stage(download);
        let _ = self.hub.update_queue(queue, |q| {
            q.order.push(id);
            Ok(())
        });
        (id, url)
    }

    /// `queue` if it exists, else the main queue.
    fn queue_or_main(&self, queue: Option<u64>) -> u64 {
        queue
            .filter(|q| self.hub.queues.lock().unwrap().iter().any(|x| x.id == *q))
            .unwrap_or(MAIN_QUEUE)
    }

    /// Announce `download` as waiting, not yet in any queue. A full-path
    /// `folder` fixes where it will be saved.
    fn stage(&self, download: Download) -> (u64, String) {
        let url = download.url.clone();
        let path = download
            .folder
            .as_deref()
            .map(PathBuf::from)
            .filter(|f| f.is_absolute())
            .map(|f| f.join(engine::Inspection::unprobed(&url, download.filename.as_deref()).filename));
        let id = self.engine.reserve_id();
        self.hub.requests.lock().unwrap().insert(id, download);
        self.hub.publish(&Outgoing::Added {
            id,
            url: url.clone(),
        });
        let _ = self.hub.update(id, |e| {
            e.status = Status::Waiting;
            if let Some(p) = &path {
                e.path = Some(p.display().to_string());
            }
            Ok(())
        });
        (id, url)
    }

    /// Add `items` to wait in `queue`, announcing the queue once rather than
    /// once per download. Items that are not web addresses are skipped, and
    /// so are those already in the list unless their own answer or the
    /// duplicate setting says to add them (with a new name, or in place of a
    /// stopped download). Returns
    /// how many were added and how many were skipped as duplicates.
    fn add_many(self: &Arc<Self>, items: Vec<Download>, queue: Option<u64>) -> (usize, usize) {
        let queue = self.queue_or_main(queue);
        let choice = self.settings.lock().unwrap().if_duplicate;
        let mut ids = Vec::new();
        let mut skipped = 0;
        for mut d in items {
            if !uidm::batch::is_download_url(&d.url) || check_body(&d).is_err() {
                continue;
            }
            if let Some(old) = self.duplicate_of(&d) {
                let choice = d.duplicate.unwrap_or(choice);
                let add = match choice {
                    Duplicate::NewName => true,
                    Duplicate::Overwrite => old.status.is_terminal(),
                    Duplicate::Ask | Duplicate::Existing => false,
                };
                if !add {
                    skipped += 1;
                    continue;
                }
                self.settle_duplicate(&mut d, &old, choice);
            }
            ids.push(self.stage(d).0);
        }
        let count = ids.len();
        let _ = self.hub.update_queue(queue, |q| {
            q.order.extend(ids);
            Ok(())
        });
        (count, skipped)
    }

    /// Read a stream's playlist on another thread and answer `reply` with
    /// what it offers.
    fn probe_media(&self, request: u64, download: Download, reply: Sender<Vec<u8>>) {
        let req = self.request_for(download);
        thread::spawn(move || {
            let info = engine::probe_media(&req.url, &req.headers, &req.net);
            if let Ok(frame) = serde_json::to_vec(&Outgoing::MediaInfo { request, info }) {
                let _ = reply.send(frame);
            }
        });
    }

    /// Crawl a site for `client` on another thread, sending it progress and
    /// then what was found.
    fn grab(self: &Arc<Self>, client: u64, req: GrabRequest, reply: Sender<Vec<u8>>) -> Result<(), String> {
        req.validate()?;
        let stop = Arc::new(AtomicBool::new(false));
        {
            let mut grabs = self.grabs.lock().unwrap();
            if grabs.contains_key(&client) {
                return Err("A site crawl is already running.".into());
            }
            grabs.insert(client, Arc::clone(&stop));
        }
        let daemon = Arc::clone(self);
        thread::spawn(move || {
            let send = |msg: &Outgoing| {
                if let Ok(frame) = serde_json::to_vec(msg) {
                    let _ = reply.send(frame);
                }
            };
            eprintln!("[uidm-daemon] client #{client} crawls {}", req.start);
            let out = daemon.run_crawl(&req, &stop, |pages, bytes, found| {
                send(&Outgoing::GrabProgress { pages, bytes, found })
            });
            daemon.grabs.lock().unwrap().remove(&client);
            eprintln!("[uidm-daemon] crawl for #{client} read {} pages, found {} files", out.pages, out.files.len());
            send(&Outgoing::GrabDone {
                files: out.files,
                pages: out.pages,
                stopped: out.stopped,
            });
        });
        Ok(())
    }

    /// Crawl for `req` on this thread, with the settings' proxy and logins,
    /// the browser's cookies when it asks, and short waits so `stop` is
    /// noticed quickly.
    fn run_crawl(&self, req: &GrabRequest, stop: &AtomicBool, progress: impl FnMut(u64, u64, usize)) -> grab::Outcome {
        let settings = self.settings.lock().unwrap().clone();
        let start = Download {
            url: req.start.clone(),
            ..Download::default()
        };
        let net = self.net_for(&start, &settings);
        let headers = [("User-Agent".to_string(), GRABBER_AGENT.to_string())];
        let cookies = if req.browser_cookies { self.browser_cookies(&req.start) } else { None };
        // The browser's cookies go to the start site's pages only.
        let page_headers = |url: &str| {
            let mut h = headers.to_vec();
            if let Some(c) = cookies.as_ref().filter(|_| grab::on_start_site(req, url)) {
                h.push(("Cookie".to_string(), c.clone()));
            }
            h
        };
        // Rendering needs the browser extension connected.
        let render = std::cell::Cell::new(req.render_scripts && self.hub.extension_connected());
        let fetch = |url: &str| {
            // A page as the browser shows it, when asked and the browser
            // can; otherwise as the server sends it.
            let rendered = (render.get() && !url.ends_with("/robots.txt"))
                .then(|| self.render_in_browser(url))
                .flatten();
            // A browser that does not answer once is not asked again.
            if render.get() && rendered.is_none() && !url.ends_with("/robots.txt") {
                eprintln!("[uidm-daemon] the browser did not render {url}; reading pages as sent");
                render.set(false);
            }
            if let Some((html, at)) = rendered {
                let mut body = html.into_bytes();
                body.truncate(grab::MAX_PAGE_BYTES as usize);
                let page = grab::Page {
                    url: at,
                    content_type: Some("text/html".into()),
                    body,
                };
                if let Some(m) = req.mirror.as_ref().filter(|m| m.pages) {
                    save_page(req, m, &page);
                }
                return Ok(page);
            }
            let page = engine::fetch(url, &page_headers(url), &net, grab::MAX_PAGE_BYTES)
                .map(|f| grab::Page {
                    url: f.url,
                    content_type: f.content_type,
                    body: f.body,
                })
                .map_err(|e| e.to_string())?;
            if let Some(m) = req.mirror.as_ref().filter(|m| m.pages) {
                save_page(req, m, &page);
            }
            Ok(page)
        };
        // Wait in short steps, so stopping is quick.
        let wait = |d: Duration| {
            let until = Instant::now() + d;
            while Instant::now() < until && !stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(100).min(until - Instant::now()));
            }
        };
        // A file's size, for the size filters and duplicates.
        let size = |url: &str| engine::inspect(url, None, &page_headers(url), &net).ok().and_then(|i| i.total);
        grab::crawl(req, fetch, size, wait, stop, progress)
    }

    /// Look for due grabber projects at most every half minute: the
    /// scheduler wakes on every download event, and this reads files.
    fn check_grab_schedules(self: &Arc<Self>) {
        {
            let mut last = self.grab_checked.lock().unwrap();
            if last.is_some_and(|t| t.elapsed() < Duration::from_secs(30)) {
                return;
            }
            *last = Some(Instant::now());
        }
        self.run_grab_schedules(queue::local_now());
    }

    /// Start the saved grabber projects whose scheduled run is due, each on
    /// its own thread. Runs with the scheduler.
    fn run_grab_schedules(self: &Arc<Self>, now: queue::Moment) {
        let Ok(dir) = grab::projects_dir() else {
            return;
        };
        for name in grab::recent_projects(&dir) {
            let Ok(mut project) = grab::Project::load(&dir, &name) else {
                continue;
            };
            let due = project
                .schedule
                .as_ref()
                .and_then(|s| s.due(now, project.last_scheduled_day, project.last_scheduled_start));
            let Some(day) = due else {
                continue;
            };
            if !self.grab_runs.lock().unwrap().insert(name.clone()) {
                continue;
            }
            // Saved first, so a restart does not start the same run again.
            project.last_scheduled_day = Some(day);
            project.last_scheduled_start = Some(now.total());
            if let Err(e) = project.save(&dir) {
                eprintln!("[uidm-daemon] could not save grabber project {name}: {e}");
            }
            let daemon = Arc::clone(self);
            let dir = dir.clone();
            thread::spawn(move || {
                daemon.run_scheduled_grab(&dir, project, day);
                daemon.grab_runs.lock().unwrap().remove(&name);
            });
        }
    }

    /// One scheduled run of a grabber project: crawl until done or until the
    /// schedule's stop time, keep what it found, and add new finds to the
    /// project's queue when it says to.
    fn run_scheduled_grab(self: &Arc<Self>, dir: &Path, mut project: grab::Project, day: i64) {
        eprintln!("[uidm-daemon] schedule runs grabber project {}", project.name);
        let end = project.schedule.as_ref().filter(|s| s.stop.is_some()).and_then(|s| s.end_of_run(day));
        let stop = AtomicBool::new(false);
        let out = self.run_crawl(&project.request, &stop, |_, _, _| {
            if end.is_some_and(|end| queue::local_now() >= end) {
                stop.store(true, Ordering::Relaxed);
            }
        });
        let new = project.record_run(out.files);
        eprintln!("[uidm-daemon] grabber project {} found {} new files", project.name, new.len());
        if project.download_new && !new.is_empty() {
            let items = new.iter().filter_map(|f| grab::download_for(&project.request, f)).collect();
            // Through the usual path, which skips addresses already in the
            // list.
            let (added, skipped) = self.add_many(items, Some(project.queue));
            eprintln!("[uidm-daemon] added {added}, skipped {skipped} already in the list");
        }
        if let Err(e) = project.save(dir) {
            eprintln!("[uidm-daemon] could not save grabber project {}: {e}", project.name);
        }
    }
    /// Convert finished download `id` on another thread. The new file is a
    /// row of its own: added and started now, with ffmpeg's progress, done
    /// with a note, or failed with ffmpeg's complaint.
    fn convert(self: &Arc<Self>, id: u64, to: uidm::media::convert::Target, delete_original: bool) -> Result<(), String> {
        let entry = self.entry(id).ok_or_else(|| format!("no download with id {id}"))?;
        let input = entry
            .path
            .as_deref()
            .map(PathBuf::from)
            .filter(|p| entry.status == Status::Done && p.is_file())
            .ok_or("Only a finished download whose file is still there can be converted.")?;
        let ffmpeg = self
            .settings
            .lock()
            .unwrap()
            .ffmpeg()
            .ok_or("Converting needs ffmpeg. Install it, or choose it in Options > General.")?;
        let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "converted".into());
        let ext = to.extension();
        let same = input.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext));
        let name = if same { format!("{stem} (converted).{ext}") } else { format!("{stem}.{ext}") };
        let dir = input.parent().map(Path::to_path_buf).unwrap_or_default();
        let output = (0..)
            .map(|n| dir.join(engine::numbered_name(&name, n)))
            .find(|p| !p.exists())
            .expect("some name is free");
        let size = std::fs::metadata(&input).map(|m| m.len()).unwrap_or(0);
        let new_id = self.engine.reserve_id();
        self.hub.uncounted.lock().unwrap().insert(new_id);
        let url = entry.url.clone();
        self.hub.publish(&Outgoing::Added { id: new_id, url: url.clone() });
        self.hub.publish(&Outgoing::Started {
            id: new_id,
            url: url.clone(),
            path: output.display().to_string(),
            total: Some(size),
            resumable: false,
            connections: 1,
            partial: None,
        });
        let stop = Arc::new(AtomicBool::new(false));
        self.conversions.lock().unwrap().insert(new_id, Arc::clone(&stop));
        eprintln!("[uidm-daemon] converting #{id} to {}", output.display());
        let daemon = Arc::clone(self);
        thread::spawn(move || {
            let mut last = Instant::now() - Duration::from_secs(1);
            let result = uidm::media::convert::convert(&ffmpeg, &input, &output, to, &stop, |f| {
                if last.elapsed() >= Duration::from_millis(500) {
                    last = Instant::now();
                    daemon.hub.publish(&Outgoing::Progress {
                        id: new_id,
                        url: url.clone(),
                        done: (f * size as f64) as u64,
                        total: Some(size),
                        parts: None,
                        segments: None,
                    });
                }
            });
            daemon.conversions.lock().unwrap().remove(&new_id);
            let from = input.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            match result {
                Ok(()) => {
                    if delete_original {
                        if let Err(e) = daemon.remove(id, true) {
                            eprintln!("[uidm-daemon] kept the original of #{id}: {e}");
                        }
                    }
                    daemon.hub.publish(&Outgoing::Done {
                        id: new_id,
                        url,
                        path: output.display().to_string(),
                        note: Some(format!("Converted from {from}")),
                    });
                }
                Err(e) if e == "canceled" => daemon.hub.publish(&Outgoing::Canceled { id: new_id, url }),
                Err(error) => daemon.hub.publish(&Outgoing::Failed { id: new_id, url, error }),
            }
        });
        Ok(())
    }

    /// Ask GitHub (at `url`) for the latest release, through the settings'
    /// proxy, and note when that happened.
    fn check_update(&self, url: &str) -> Outgoing {
        let settings = self.settings.lock().unwrap().clone();
        let start = Download {
            url: url.to_string(),
            ..Download::default()
        };
        let net = self.net_for(&start, &settings);
        let headers = [
            ("User-Agent".to_string(), format!("UIDM/{}", env!("CARGO_PKG_VERSION"))),
            ("Accept".to_string(), "application/vnd.github+json".to_string()),
        ];
        let found = engine::fetch(url, &headers, &net, 1024 * 1024)
            .map_err(|e| format!("Could not reach GitHub: {e}"))
            .and_then(|page| uidm::update::parse_release(&page.body));
        {
            let mut s = self.settings.lock().unwrap();
            s.last_update_check = uidm::state::unix_now();
            if let Some(path) = &self.paths.settings {
                let _ = s.save(path);
            }
        }
        match found {
            Ok(release) => Outgoing::Update {
                newer: uidm::update::is_newer(&release.version, env!("CARGO_PKG_VERSION")).then_some(release),
                error: None,
            },
            Err(e) => Outgoing::Update { newer: None, error: Some(e) },
        }
    }

    /// Once a week, when a window connects, look for a newer release on
    /// another thread and tell the windows if there is one.
    fn weekly_update_check(self: &Arc<Self>) {
        let (on, last) = {
            let s = self.settings.lock().unwrap();
            (s.check_updates, s.last_update_check)
        };
        if !on || uidm::state::unix_now().saturating_sub(last) < uidm::update::CHECK_EVERY_SECS {
            return;
        }
        // Noted at once, so two windows do not both check.
        self.settings.lock().unwrap().last_update_check = uidm::state::unix_now();
        let daemon = Arc::clone(self);
        thread::spawn(move || {
            if let msg @ Outgoing::Update { newer: Some(_), .. } = daemon.check_update(uidm::update::LATEST_RELEASE) {
                daemon.hub.broadcast(&msg);
            }
        });
    }

    /// List the files inside zip download `id` on another thread, reading
    /// the end of the archive with its own request (cookies and all), and
    /// answer `reply` with `zip_preview`.
    fn preview_zip(&self, id: u64, reply: Sender<Vec<u8>>) {
        let entry = self.entry(id);
        let download = self.hub.requests.lock().unwrap().get(&id).cloned();
        let answer = move |entries: Vec<uidm::zip::ZipEntry>, error: Option<String>| {
            if let Ok(frame) = serde_json::to_vec(&Outgoing::ZipPreview { id, entries, error }) {
                let _ = reply.send(frame);
            }
        };
        let Some(entry) = entry else {
            return answer(Vec::new(), Some(format!("no download with id {id}")));
        };
        let download = download.unwrap_or_else(|| Download {
            url: entry.url.clone(),
            ..Download::default()
        });
        let req = self.request_for(download);
        thread::spawn(move || {
            let total = match entry.total {
                Some(t) => Ok(t),
                None => engine::inspect(&req.url, None, &req.headers, &req.net)
                    .map_err(|e| e.to_string())
                    .and_then(|i| i.total.ok_or_else(|| "The server does not say how big the file is.".to_string())),
            };
            let listed = total.and_then(|total| {
                uidm::zip::list(total, |a, b| {
                    engine::fetch_range_bytes(&req.url, &req.headers, &req.net, a, b)
                        .map_err(|e| format!("The server would not send part of the file: {e}"))
                })
            });
            match listed {
                Ok(entries) => answer(entries, None),
                Err(e) => answer(Vec::new(), Some(e)),
            }
        });
    }

    /// Ask the browser extension to render `url` in a background tab and
    /// wait for its HTML and final address. `None` without an extension or
    /// an answer in time.
    fn render_in_browser(&self, url: &str) -> Option<(String, String)> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let request = NEXT.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = channel();
        self.render_waits.lock().unwrap().insert(request, tx);
        self.hub.broadcast(&Outgoing::RenderPage {
            request,
            url: url.to_string(),
        });
        // The extension gives up on a page after 30 seconds itself.
        let got = rx.recv_timeout(Duration::from_secs(40)).ok().flatten();
        self.render_waits.lock().unwrap().remove(&request);
        got
    }

    /// Ask the browser extension for its cookies for `url`, waiting a few
    /// seconds for the answer. `None` without an extension or cookies.
    fn browser_cookies(&self, url: &str) -> Option<String> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let request = NEXT.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = channel();
        self.cookie_waits.lock().unwrap().insert(request, tx);
        self.hub.broadcast(&Outgoing::CookiesWanted {
            request,
            url: url.to_string(),
        });
        let got = rx.recv_timeout(Duration::from_secs(5)).ok().flatten();
        self.cookie_waits.lock().unwrap().remove(&request);
        eprintln!("[uidm-daemon] browser cookies for the crawl: {}", if got.is_some() { "found" } else { "none" });
        got.filter(|c| !c.is_empty())
    }

    /// Stop `client`'s crawl, if it has one.
    fn stop_grab(&self, client: u64) {
        if let Some(stop) = self.grabs.lock().unwrap().get(&client) {
            stop.store(true, Ordering::Relaxed);
        }
    }

    /// Announce `download` as queued, probe it on another thread, then hold it
    /// as `confirming` with the proposed file until a window answers.
    /// `duplicate_of` is another download with the same address, for the
    /// dialog to ask about.
    fn ask(self: &Arc<Self>, download: Download, duplicate_of: Option<u64>) -> (u64, String) {
        let url = download.url.clone();
        let id = self.engine.reserve_id();
        self.hub.requests.lock().unwrap().insert(id, download.clone());
        self.hub.publish(&Outgoing::Added {
            id,
            url: url.clone(),
        });

        let daemon = Arc::clone(self);
        thread::spawn(move || {
            let req = daemon.request_for(download.clone());
            // Probing a POST download would send a GET (or repeat the POST),
            // so its dialog proposes a name from the request alone.
            let probed = if uidm::app::post_body(&download).is_some() {
                Ok(engine::Inspection::unprobed(&download.url, download.filename.as_deref()))
            } else {
                engine::inspect(&download.url, download.filename.as_deref(), &req.headers, &req.net)
            };
            let found = match probed {
                Ok(found) => found,
                Err(e) => {
                    // Same as a download that fails to start, so the browser
                    // can take it back.
                    daemon.hub.publish(&Outgoing::Failed {
                        id,
                        url: download.url,
                        error: e.to_string(),
                    });
                    return;
                }
            };
            let path = req.folders.for_name(&found.filename).join(&found.filename);
            // The other one may have been removed while this was probed.
            let duplicate_of = duplicate_of.filter(|o| daemon.status(*o).is_some());
            let settings = daemon.settings.lock().unwrap().clone();
            // A type that starts without asking, known only now from the
            // server's name for the file, skips the dialog after all.
            let auto = duplicate_of.is_none() && download.ask != Some(true) && settings.starts_without_asking(&found.filename);
            let asked = daemon.hub.update(id, |e| {
                if e.status != Status::Queued {
                    return Err("no longer queued".into());
                }
                e.status = if auto { Status::Waiting } else { Status::Confirming };
                e.path = Some(path.display().to_string());
                e.total = found.total;
                e.resumable = found.resumable;
                e.duplicate_of = duplicate_of;
                Ok(())
            });
            if asked.is_err() {
                return; // canceled while being probed
            }
            if auto {
                if let Err(e) = daemon.dispatch(id) {
                    eprintln!("[uidm-daemon] could not start #{id}: {e}");
                }
                return;
            }
            // Only a file the server sends in ranges can be stopped and
            // carried on elsewhere once the dialog is answered.
            let plain = uidm::app::post_body(&download).is_none() && download.media.is_none();
            if settings.start_while_asking && duplicate_of.is_none() && plain && found.resumable && found.total.is_some() {
                daemon.start_early(id, download.clone(), &found.filename);
            }
            daemon.await_answer(id, None);
        });
        (id, url)
    }

    /// Start download `id` while its save-as dialog is open, under an engine
    /// id of its own (whose events clients never see) and into a folder of
    /// its own. Answering the dialog keeps what it fetched: see
    /// `settle_early`.
    fn start_early(&self, id: u64, download: Download, filename: &str) {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(EARLY_FOLDER).join(format!("{}-{n}", std::process::id()));
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("[uidm-daemon] could not start #{id} early: {e}");
            return;
        }
        let mut req = self.request_for(download);
        req.folders = Folders::fixed(dir.clone());
        req.filename = Some(filename.to_string());
        req.file_exists = FileExists::Overwrite;
        req.temp_dir = None;
        req.server_date = false;
        let shadow = self.engine.reserve_id();
        self.hub.early.lock().unwrap().insert(
            shadow,
            Early {
                real: id,
                dir,
                file: None,
                end: None,
                discard: false,
            },
        );
        eprintln!("[uidm-daemon] #{id} starts early while its dialog is open");
        self.engine.enqueue_as(shadow, req);
    }

    /// Stop the early download `shadow` and wait for it, returning how it
    /// ended. `None` if it did not stop in time; its events
    /// stay hidden, and its folder goes when UIDM next starts.
    fn stop_early(&self, shadow: u64) -> Option<Early> {
        self.engine.cancel(shadow);
        let deadline = Instant::now() + EARLY_STOP_WAIT;
        loop {
            {
                let mut early = self.hub.early.lock().unwrap();
                if early.get(&shadow).is_some_and(|e| e.end.is_some()) {
                    return early.remove(&shadow);
                }
                if Instant::now() >= deadline {
                    if let Some(e) = early.get_mut(&shadow) {
                        e.real = 0;
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
    fn settle_early(&self, id: u64, shadow: u64) -> bool {
        let Some(early) = self.stop_early(shadow) else {
            return false;
        };
        let entry = self.entry(id);
        let path = entry.as_ref().and_then(|e| e.path.as_deref()).map(PathBuf::from);
        let overwrite = {
            let asked = self.hub.requests.lock().unwrap().get(&id).and_then(|d| d.file_exists);
            asked.unwrap_or(self.settings.lock().unwrap().file_exists) == FileExists::Overwrite
        };
        let mut kept = false;
        match (entry, path, early.end, early.file) {
            (Some(e), Some(path), Some(Some(finished)), _) => {
                let to = free_path(&path, overwrite);
                match move_file(&finished, &to) {
                    Ok(()) => {
                        eprintln!("[uidm-daemon] #{id} finished while its dialog was open");
                        self.hub.leave_queues(id);
                        self.hub.publish(&Outgoing::Done {
                            id,
                            url: e.url,
                            path: to.display().to_string(),
                            note: None,
                        });
                        let _ = std::fs::remove_dir_all(&early.dir);
                        return true;
                    }
                    Err(err) => eprintln!("[uidm-daemon] could not keep #{id}'s early download: {err}"),
                }
            }
            (Some(_), Some(path), _, Some(partial)) if partial.is_file() && engine::sidecar_path(&partial).is_file() => {
                let to = free_path(&path, overwrite);
                kept = self
                    .hub
                    .update(id, |e| {
                        e.path = Some(to.display().to_string());
                        e.partial = Some(partial.display().to_string());
                        e.connections = e.connections.max(1);
                        Ok(())
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
    fn discard_early(&self, id: u64) {
        let Some(shadow) = self.hub.early_of(id) else {
            return;
        };
        let mut early = self.hub.early.lock().unwrap();
        let ended = early.get(&shadow).is_some_and(|e| e.end.is_some());
        if ended {
            if let Some(gone) = early.remove(&shadow) {
                let _ = std::fs::remove_dir_all(gone.dir);
            }
            return;
        }
        if let Some(e) = early.get_mut(&shadow) {
            e.real = 0;
            e.discard = true;
        }
        drop(early);
        self.engine.cancel(shadow);
    }

    /// Answer the save-as dialog of a `confirming` download, or start a
    /// `waiting` one. Missing names and folders keep the proposed ones. With
    /// `start` false it waits in `queue` (the main queue by default).
    /// `file_exists`, when given, decides what happens if the name is taken,
    /// now and on later starts. For a download asked about as a duplicate,
    /// `duplicate` decides what happens to the other download first (a new
    /// name when missing). A category and description, when given, are kept
    /// with it. The dialog's "remember" boxes change the settings.
    fn confirm(self: &Arc<Self>, answer: Answer) -> Result<(), String> {
        let Answer {
            id,
            mut filename,
            mut folder,
            start,
            queue,
            mut file_exists,
            duplicate,
            category,
            description,
            remember_folder,
            remember_duplicate,
            remember_queue,
        } = answer;
        let category = category.map(|c| c.trim().to_string()).filter(|c| !c.is_empty());
        let description = description.map(|d| d.trim().to_string());
        let other = self.entry(id).and_then(|e| e.duplicate_of).and_then(|o| self.entry(o));
        if let Some(old) = other {
            let choice = duplicate.unwrap_or(Duplicate::NewName);
            if remember_duplicate {
                let mut settings = self.settings.lock().unwrap().clone();
                if settings.if_duplicate != choice {
                    settings.if_duplicate = choice;
                    if let Err(e) = self.set_settings(settings) {
                        eprintln!("[uidm-daemon] could not remember the duplicate choice: {e}");
                    }
                }
            }
            let mut download = self.hub.requests.lock().unwrap().get(&id).cloned().unwrap_or_default();
            if let Some((kept, _)) = self.settle_duplicate(&mut download, &old, choice) {
                eprintln!("[uidm-daemon] #{id} duplicates #{}; using #{kept} instead", old.id);
                self.drop_entry(id);
                return Ok(());
            }
            // Replacing the other download means writing over its file.
            if choice == Duplicate::Overwrite && old.path.is_some() {
                filename = download.filename.clone();
                folder = download.folder.clone();
            }
            file_exists = file_exists.or(download.file_exists);
        }
        let item = self.hub.update(id, |e| {
            if !matches!(e.status, Status::Confirming | Status::Waiting) {
                return Err(format!("download {id} is not waiting to be started"));
            }
            e.duplicate_of = None;
            if let Some(c) = &category {
                e.category = Some(c.clone());
            }
            if let Some(d) = &description {
                e.description = Some(d.clone()).filter(|d| !d.is_empty());
            }
            if filename.is_none() && folder.is_none() {
                // Keep whatever the download already has: a chosen file, a
                // partial file, or nothing yet.
                e.status = Status::Waiting;
                return Ok(());
            }
            let proposed = e.path.as_deref().map(Path::new);
            let name = filename
                .as_deref()
                .map(engine::clean_filename)
                .filter(|n| !n.is_empty())
                .or_else(|| {
                    proposed
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                })
                .ok_or("the download has no file name")?;
            let dir = match folder.as_deref().map(str::trim) {
                Some(f) if !f.is_empty() => PathBuf::from(f),
                _ => proposed
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
                    .ok_or("the download has no folder")?,
            };
            if !dir.is_absolute() {
                return Err(format!("the folder must be a full path: {}", dir.display()));
            }
            e.path = Some(dir.join(&name).display().to_string());
            e.status = Status::Waiting;
            Ok(())
        })?;
        let mut settings = self.settings.lock().unwrap().clone();
        let mut changed = false;
        if let Some(path) = item.path.as_deref().map(Path::new) {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let chosen = item.category.clone().unwrap_or_else(|| settings.category_name(&name, &item.url).to_string());
            if let Some(on) = remember_folder {
                changed |= settings.set_remember_folder(&chosen, on);
            }
            // A category that remembers its folder keeps the one chosen here.
            let picked = remember_folder == Some(true) || folder.as_deref().is_some_and(|f| !f.trim().is_empty());
            if let (true, Some(dir)) = (picked, path.parent()) {
                changed |= settings.remember_folder(&chosen, dir);
            }
        }
        if remember_queue && !start {
            let later = Some(queue.unwrap_or(MAIN_QUEUE));
            changed |= settings.later_queue != later;
            settings.later_queue = later;
        }
        if changed {
            if let Err(e) = self.set_settings(settings) {
                eprintln!("[uidm-daemon] could not remember the dialog's choices: {e}");
            }
        }
        if file_exists.is_some() || category.is_some() || description.is_some() {
            let mut requests = self.hub.requests.lock().unwrap();
            let request = requests.entry(id).or_insert_with(|| Download {
                url: item.url.clone(),
                ..Download::default()
            });
            if file_exists.is_some() {
                request.file_exists = file_exists;
            }
            if category.is_some() {
                request.category = category;
            }
            if description.is_some() {
                request.description = item.description.clone();
            }
        }
        // A download started early stops first, so its bytes can move to
        // the file chosen, which takes a moment.
        if let Some(shadow) = self.hub.early_of(id) {
            let daemon = Arc::clone(self);
            thread::spawn(move || {
                if daemon.settle_early(id, shadow) {
                    return;
                }
                if let Err(e) = daemon.start_or_wait(id, start, queue) {
                    eprintln!("[uidm-daemon] could not go on with #{id}: {e}");
                }
            });
            return Ok(());
        }
        self.start_or_wait(id, start, queue)
    }

    /// Start a `waiting` download now, or (with `start` false) have it wait
    /// in `queue`, the Download later queue, or the main one.
    fn start_or_wait(&self, id: u64, start: bool, queue: Option<u64>) -> Result<(), String> {
        if start {
            return self.dispatch(id);
        }
        if self.hub.queue_of(id).is_none() {
            let later = self.settings.lock().unwrap().later_queue;
            self.move_to_queue(id, self.queue_or_main(queue.or(later)), None)?;
        }
        Ok(())
    }

    /// Cancel a download, whether it waits for its dialog or is in the engine.
    fn cancel(&self, id: u64) {
        if let Some(stop) = self.conversions.lock().unwrap().get(&id) {
            stop.store(true, Ordering::Relaxed);
            return;
        }
        if matches!(self.status(id), Some(Status::Confirming | Status::Waiting)) {
            self.discard_early(id);
            let url = self.hub.table.lock().unwrap().get(id).map(|e| e.url.clone());
            if let Some(url) = url {
                self.hub.publish(&Outgoing::Canceled { id, url });
            }
            return;
        }
        self.engine.cancel(id);
    }

    /// Queue `download`, remember it for retries, and announce it.
    /// `reuse_path` is a partial file an earlier attempt left behind.
    fn start(&self, download: Download, reuse_path: Option<PathBuf>) -> (u64, String) {
        self.start_resuming(download, reuse_path, None)
    }

    /// `start`, with where the earlier attempt kept its partial data when
    /// that was a temporary folder.
    fn start_resuming(&self, download: Download, reuse_path: Option<PathBuf>, reuse_partial: Option<PathBuf>) -> (u64, String) {
        let url = download.url.clone();
        let mut req = self.request_for(download.clone());
        req.reuse_path = reuse_path;
        req.reuse_partial = reuse_partial;
        let id = self.engine.enqueue(req);
        self.hub.requests.lock().unwrap().insert(id, download);
        // A very fast download can finish before its request is stored; drop
        // the request (and its cookies) in that case too.
        if self.status(id) == Some(Status::Done) {
            self.hub.requests.lock().unwrap().remove(&id);
        }
        self.hub.publish(&Outgoing::Added {
            id,
            url: url.clone(),
        });
        (id, url)
    }

    fn status(&self, id: u64) -> Option<Status> {
        self.hub.table.lock().unwrap().get(id).map(|e| e.status)
    }

    /// Re-queue a failed, canceled or interrupted download from its stored
    /// request, or from its URL alone when the request was not kept. The
    /// engine resumes the partial file when the server still reports the same
    /// file.
    fn retry(&self, id: u64) -> Result<(u64, String), String> {
        let entry = self
            .hub
            .table
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| format!("no download with id {id}"))?;
        if !entry.status.is_retryable() {
            return Err(format!("download {id} is not failed, canceled or interrupted"));
        }
        let download = self
            .hub
            .requests
            .lock()
            .unwrap()
            .remove(&id)
            .unwrap_or_else(|| Download {
                url: entry.url.clone(),
                referrer: entry.referrer.clone(),
                ..Download::default()
            });
        // The new entry keeps the old one's description, category and speed
        // limit.
        let download = Download {
            description: entry.description.clone(),
            category: entry.category.clone(),
            speed_limit: entry.speed_limit,
            ..download
        };
        // A retried download keeps its place in its queue.
        let queue = self.hub.queue_of(id);
        self.hub.leave_queues(id);
        self.hub.publish(&Outgoing::Removed { id });
        // Only a download that started has a file of its own to resume. One
        // that never got past its save-as dialog only has a proposed path,
        // which may be somebody else's file.
        let started = entry.connections > 0;
        let reuse_path = entry.path.filter(|_| started).map(PathBuf::from);
        let reuse_partial = entry.partial.filter(|_| started).map(PathBuf::from);
        let (new_id, url) = self.start_resuming(download, reuse_path, reuse_partial);
        if let Some(q) = queue {
            let _ = self.move_to_queue(new_id, q, None);
        }
        Ok((new_id, url))
    }

    /// Set or clear download `id`'s own speed limit. It is kept on the
    /// entry (so in the history) and in the stored request, and a running
    /// download changes speed at once.
    fn set_download_limit(&self, id: u64, bytes_per_sec: Option<u64>) -> Result<(), String> {
        let limit = bytes_per_sec.filter(|b| *b > 0);
        let (lo, hi) = SPEED_LIMIT_RANGE_KBPS;
        if let Some(b) = limit.filter(|b| !(lo * 1024..=hi * 1024).contains(b)) {
            return Err(format!("{} KB/s is not a speed limit between {lo} and {hi} KB/s.", b / 1024));
        }
        self.hub.update(id, |e| {
            e.speed_limit = limit;
            Ok(())
        })?;
        if let Some(d) = self.hub.requests.lock().unwrap().get_mut(&id) {
            d.speed_limit = limit;
        }
        self.engine.set_download_limit(id, limit);
        Ok(())
    }

    /// Start download `id` again from the first byte, under the same id, so
    /// it keeps its place and description. It goes to the same name and
    /// folder; a file still there is handled by the file-exists setting, as
    /// anybody else's file would be.
    fn redownload(&self, id: u64) -> Result<(), String> {
        let again = |s: Status| matches!(s, Status::Done | Status::Failed | Status::Canceled | Status::Interrupted);
        let entry = self.entry(id).ok_or_else(|| format!("no download with id {id}"))?;
        if !again(entry.status) {
            return Err("Only a finished or stopped download can be downloaded again.".into());
        }
        let stored = self.hub.requests.lock().unwrap().get(&id).cloned();
        if stored.is_none() && entry.parts.is_some() {
            return Err("This stream's playlist is no longer known. Play it in the browser again to download it.".into());
        }
        let mut download = stored.unwrap_or_else(|| Download {
            url: entry.url.clone(),
            referrer: entry.referrer.clone(),
            ..Download::default()
        });
        download.description = entry.description.clone();
        download.category = entry.category.clone();
        download.speed_limit = entry.speed_limit;
        // Only a download that started has a file of its own; a proposed
        // path may be somebody else's.
        let wrote = entry.status == Status::Done || entry.connections > 0;
        if let Some(path) = entry.path.as_deref().filter(|_| wrote).map(Path::new) {
            download.filename = path.file_name().map(|n| n.to_string_lossy().into_owned());
            download.folder = path.parent().map(|d| d.display().to_string());
        }
        // No reuse_path, and no resume sidecar left: a partial file still
        // there is a taken name like any other, not something to resume.
        if entry.status != Status::Done {
            for p in [entry.path.as_deref(), entry.partial.as_deref()].into_iter().flatten() {
                let _ = std::fs::remove_file(engine::sidecar_path(Path::new(p)));
            }
        }
        let req = self.request_for(download.clone());
        self.hub.update(id, |e| {
            if !again(e.status) {
                return Err("The download started again in the meantime.".into());
            }
            e.status = Status::Queued;
            e.done = 0;
            e.total = None;
            e.parts = None;
            e.error = None;
            e.scan = None;
            e.note = None;
            e.resumable = false;
            e.connections = 0;
            e.partial = None;
            Ok(())
        })?;
        self.hub.requests.lock().unwrap().insert(id, download);
        eprintln!("[uidm-daemon] downloading #{id} again: {}", entry.url);
        self.engine.enqueue_as(id, req);
        Ok(())
    }

    /// Give download `id` a new link. Without `download`, wait for the next one
    /// the browser hands over. With it, check the new link against the
    /// partial file: on a mismatch (and no `restart`) return why, changing
    /// nothing. Otherwise store the new request: a waiting download keeps
    /// waiting with it, any other starts again, resuming its partial file
    /// when the link is for the same file.
    fn refresh_link(&self, id: u64, download: Option<Download>, restart: bool) -> Result<Option<String>, String> {
        let entry = self.entry(id).ok_or_else(|| format!("no download with id {id}"))?;
        if !entry.status.is_retryable() {
            return Err("Only a stopped download can get a new link.".to_string());
        }
        let Some(download) = download else {
            self.set_refresh_waiting(Some(id));
            return Ok(None);
        };
        if !uidm::batch::is_download_url(&download.url) {
            return Err(format!("{} is not an http, https or ftp link", download.url));
        }
        if self.refresh_waiting.lock().unwrap().is_some_and(|w| w == id) {
            self.set_refresh_waiting(None);
        }
        let started = entry.connections > 0;
        if started && !restart {
            if let Some(path) = entry.partial.as_deref().or(entry.path.as_deref()) {
                let req = self.request_for(download.clone());
                let found = engine::inspect(&download.url, None, &req.headers, &req.net)
                    .map_err(|e| format!("could not reach the new link: {e}"))?;
                let ignore_date = self.settings.lock().unwrap().resume_ignores_date;
                if let Err(reason) = engine::partial_matches(Path::new(path), &found, ignore_date) {
                    return Ok(Some(reason));
                }
            }
        }
        eprintln!("[uidm-daemon] new link for #{id}: {}", download.url);
        self.hub.requests.lock().unwrap().insert(id, download.clone());
        if entry.status == Status::Waiting {
            self.hub.update(id, |e| {
                e.url = download.url.clone();
                Ok(())
            })?;
            return Ok(None);
        }
        self.retry(id)?;
        Ok(None)
    }

    /// Change a stopped or finished download's details. The link, referrer
    /// and login go into the request a retry starts from; a finished
    /// download's file is renamed or moved on disk first.
    fn set_properties(&self, id: u64, p: Properties) -> Result<(), String> {
        let entry = self.entry(id).ok_or_else(|| format!("no download with id {id}"))?;
        let done = entry.status == Status::Done;
        let stopped = |s: Status| s == Status::Done || s.is_retryable();
        if !stopped(entry.status) {
            return Err("Stop the download before changing its properties.".into());
        }
        let trimmed = |s: Option<String>| s.map(|s| s.trim().to_string());
        let url = trimmed(p.url).filter(|u| *u != entry.url);
        if let Some(u) = url.as_deref().filter(|u| !uidm::batch::is_download_url(u)) {
            return Err(format!("{u} is not an http, https or ftp link"));
        }
        let referrer = trimmed(p.referrer);
        if let Some(r) = referrer.as_deref().filter(|r| !r.is_empty() && !uidm::batch::is_web_url(r)) {
            return Err(format!("{r} is not an http or https address"));
        }
        let description = trimmed(p.description);
        if p.login.is_some() && done {
            return Err("A finished download keeps no login. Download it again to use one.".into());
        }
        let moved = match (p.filename, p.folder) {
            (None, None) => None,
            _ if !done => return Err("Only a finished download's file can be renamed or moved.".into()),
            (filename, folder) => {
                let from = entry.path.as_deref().map(PathBuf::from).ok_or("The download has no file.")?;
                let name = filename
                    .as_deref()
                    .map(engine::clean_filename)
                    .filter(|n| !n.is_empty())
                    .or_else(|| from.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .ok_or("The download has no file name.")?;
                let dir = match folder.as_deref().map(str::trim) {
                    Some(f) if !f.is_empty() => PathBuf::from(f),
                    _ => from.parent().map(Path::to_path_buf).ok_or("The download has no folder.")?,
                };
                if !dir.is_absolute() {
                    return Err(format!("The folder must be a full path: {}", dir.display()));
                }
                let to = dir.join(name);
                (to != from).then_some((from, to))
            }
        };
        if let Some((from, to)) = &moved {
            move_file(from, to)?;
            eprintln!("[uidm-daemon] moved #{id} to {}", to.display());
        }
        let mut login_user = entry.login_user.clone();
        if !done {
            let mut requests = self.hub.requests.lock().unwrap();
            let req = requests.entry(id).or_insert_with(|| Download {
                url: entry.url.clone(),
                referrer: entry.referrer.clone(),
                ..Download::default()
            });
            if let Some(u) = &url {
                req.url = u.clone();
            }
            if let Some(r) = &referrer {
                req.referrer = Some(r.clone()).filter(|r| !r.is_empty());
            }
            if let Some(d) = &description {
                req.description = Some(d.clone()).filter(|d| !d.is_empty());
            }
            if let Some(l) = p.login {
                let username = l.username.trim().to_string();
                req.login = (!username.is_empty()).then(|| DownloadLogin {
                    password: l.password.or_else(|| req.login.as_ref().and_then(|o| o.password.clone())),
                    username,
                });
            }
            login_user = req.login.as_ref().map(|l| l.username.clone());
        }
        self.hub.update(id, |e| {
            if !stopped(e.status) {
                return Err("The download started while its properties were being saved.".into());
            }
            if let Some(u) = &url {
                e.url = u.clone();
            }
            if let Some(r) = &referrer {
                e.referrer = Some(r.clone()).filter(|r| !r.is_empty());
            }
            if let Some(d) = &description {
                e.description = Some(d.clone()).filter(|d| !d.is_empty());
            }
            if let Some((_, to)) = &moved {
                e.path = Some(to.display().to_string());
            }
            e.login_user = login_user;
            Ok(())
        })?;
        Ok(())
    }

    /// Change which download waits for a link from the browser, and tell
    /// every client.
    fn set_refresh_waiting(&self, id: Option<u64>) {
        *self.refresh_waiting.lock().unwrap() = id;
        self.hub.broadcast(&Outgoing::RefreshWaiting { id });
    }

    /// Validate, save and apply new settings, then tell every client. Running
    /// downloads keep going; new ones use the new values.
    /// The engine request for `download`, with the settings' folder,
    /// connections and file-name policy, and its proxy and logins.
    fn request_for(&self, download: Download) -> DownloadRequest {
        let settings = self.settings.lock().unwrap().clone();
        let net = self.net_for(&download, &settings);
        let folder = download.folder.as_deref().map(PathBuf::from).filter(|f| f.is_absolute());
        let mut req = to_request(download, &settings);
        req.net = Arc::new(net);
        if let Some(folder) = folder {
            req.folders = Folders::fixed(folder);
        }
        req
    }

    /// The proxy and logins for `download`. A download from the browser uses
    /// the browser's proxy when the settings say so.
    fn net_for(&self, download: &Download, s: &Settings) -> NetConfig {
        let browser = download
            .proxy
            .as_deref()
            .filter(|_| s.use_browser_proxy)
            .map(|p| net::ProxyRules {
                http: p.strip_prefix("http://").map(str::to_string),
                https: p.strip_prefix("http://").map(str::to_string),
                socks: p.starts_with("socks").then(|| p.to_string()),
                ..net::ProxyRules::default()
            });
        let proxy = browser.or_else(|| match s.proxy_mode {
            ProxyMode::None => None,
            ProxyMode::System => net::system_proxy(),
            ProxyMode::Script => Some(net::ProxyRules {
                pac: Some(uidm::pac::PacSource::Url(s.proxy_pac_url.trim().to_string())),
                ..net::ProxyRules::default()
            }),
            ProxyMode::Manual => {
                let addr = |a: &str| (!a.trim().is_empty()).then(|| a.trim().to_string());
                Some(net::ProxyRules {
                    http: addr(&s.proxy_http),
                    https: addr(&s.proxy_https),
                    socks: addr(&s.proxy_socks5).map(|a| {
                        let scheme = net::socks_scheme(s.proxy_socks_version == SocksVersion::V5, s.proxy_remote_dns);
                        format!("{scheme}://{a}")
                    }),
                    bypass: net::parse_bypass(&s.proxy_bypass),
                    username: s.proxy_username.trim().to_string(),
                    password: net::proxy_password_path()
                        .map(|p| net::load_proxy_password(&p))
                        .unwrap_or_default(),
                    pac: None,
                })
            }
        });
        let mut logins: Vec<Login> = s
            .site_logins
            .iter()
            .filter_map(|l| {
                Some(Login {
                    site: l.site.trim().to_string(),
                    username: l.username.clone(),
                    password: net::load_login_password(&l.site)?,
                })
            })
            .collect();
        // The download's own login takes the place of the site logins for
        // its host, even those for one of its folders.
        let host = url::Url::parse(&download.url).ok().and_then(|u| u.host_str().map(str::to_string));
        if let (Some(l), Some(host)) = (&download.login, host) {
            logins.retain(|s| !net::site_matches(net::split_login_pattern(&s.site).0, &host));
            logins.push(Login {
                site: host,
                username: l.username.clone(),
                password: l.password.clone().unwrap_or_default(),
            });
        }
        NetConfig {
            proxy,
            logins,
            ftp_active: s.ftp_active,
            ftp_proxy: (s.proxy_mode == ProxyMode::Manual && !s.proxy_ftp.trim().is_empty()).then(|| s.proxy_ftp.trim().to_string()),
        }
    }

    /// Store the passwords a settings change carries (proxy: DPAPI; site
    /// logins: Credential Manager), forget those of removed logins, and return
    /// the settings without them.
    fn store_secrets(&self, mut new: Settings) -> Result<Settings, String> {
        if let Some(pw) = new.proxy_password.take() {
            let path = net::proxy_password_path().map_err(|e| e.to_string())?;
            net::save_proxy_password(&path, &pw).map_err(|e| format!("could not save the proxy password: {e}"))?;
        }
        for l in &mut new.site_logins {
            if let Some(pw) = l.password.take() {
                net::save_login_password(&l.site, &l.username, &pw)
                    .map_err(|e| format!("could not save the password for {}: {e}", l.site))?;
            }
        }
        let old = self.settings.lock().unwrap().site_logins.clone();
        for gone in old.iter().filter(|o| !new.site_logins.iter().any(|n| n.site.eq_ignore_ascii_case(&o.site))) {
            if let Err(e) = net::delete_login_password(&gone.site) {
                eprintln!("[uidm-daemon] could not forget the password for {}: {e}", gone.site);
            }
        }
        Ok(new)
    }

    fn set_settings(&self, mut new: Settings) -> Result<(), String> {
        new.validate()?;
        // Only the daemon notes update checks; a window's copy may be older.
        new.last_update_check = new.last_update_check.max(self.settings.lock().unwrap().last_update_check);
        let new = self.store_secrets(new)?;
        let old_autostart = self.settings.lock().unwrap().start_with_windows;
        if new.start_with_windows != old_autostart {
            let gui = std::env::current_exe()
                .map_err(|e| e.to_string())?
                .with_file_name("uidm-gui.exe");
            registry::set_autostart(new.start_with_windows, &gui)
                .map_err(|e| format!("could not change Start with Windows: {e}"))?;
        }
        if let Some(path) = &self.paths.settings {
            new.save(path)
                .map_err(|e| format!("could not save settings to {}: {e}", path.display()))?;
        }
        self.engine.set_concurrency(new.max_concurrent);
        self.engine.set_speed_limit(new.speed_limit_bytes());
        *self.settings.lock().unwrap() = new.clone();
        self.hub.broadcast(&Outgoing::Settings { settings: new });
        Ok(())
    }

    /// Queue every interrupted download again (the "resume at startup"
    /// option).
    fn resume_interrupted(&self) {
        let ids: Vec<u64> = self
            .hub
            .table
            .lock()
            .unwrap()
            .entries()
            .filter(|e| e.status == Status::Interrupted)
            .map(|e| e.id)
            .collect();
        for id in ids {
            match self.retry(id) {
                Ok((new_id, url)) => eprintln!("[uidm-daemon] resuming #{id} as #{new_id}: {url}"),
                Err(e) => eprintln!("[uidm-daemon] could not resume #{id}: {e}"),
            }
        }
    }

    /// Write the history and the saved requests now.
    fn save_history(&self) {
        let _guard = self.save_lock.lock().unwrap();
        let history = History {
            next_id: self.engine.next_id(),
            items: self.hub.table.lock().unwrap().snapshot(),
            queues: self.hub.queues.lock().unwrap().clone(),
        };
        let requests = self.hub.requests.lock().unwrap().clone();
        if let Some(path) = &self.paths.quota {
            if let Err(e) = self.hub.quota.lock().unwrap().save(path) {
                eprintln!("[uidm-daemon] could not save {}: {e}", path.display());
            }
        }
        if let Some(path) = &self.paths.history {
            if let Err(e) = history.save(path) {
                eprintln!("[uidm-daemon] could not save {}: {e}", path.display());
            }
        }
        if let Some(path) = &self.paths.requests {
            if let Err(e) = history::save_requests(path, &requests) {
                eprintln!("[uidm-daemon] could not save {}: {e}", path.display());
            }
        }
    }

    /// Forget a download that is no longer running. Its files stay on disk
    /// unless `delete_files`, which sends them to the Recycle Bin.
    fn remove(&self, id: u64, delete_files: bool) -> Result<(), String> {
        let entry = match self.entry(id) {
            None => return Err(format!("no download with id {id}")),
            Some(e) if !e.status.is_terminal() => {
                return Err(format!("download {id} is still running; cancel it first"))
            }
            Some(e) => e,
        };
        // Only a download that started wrote anything. One that never got
        // past its save-as dialog only has a proposed path, which may be
        // somebody else's file.
        let wrote = entry.status == Status::Done || entry.connections > 0;
        let mut files = match entry.path.as_deref() {
            Some(p) if delete_files && wrote => engine::files_of(Path::new(p)),
            _ => Vec::new(),
        };
        if let Some(p) = entry.partial.as_deref().filter(|_| delete_files && wrote) {
            files.extend(engine::partial_files(Path::new(p)));
        }
        self.hub.requests.lock().unwrap().remove(&id);
        if self.refresh_waiting.lock().unwrap().is_some_and(|w| w == id) {
            self.set_refresh_waiting(None);
        }
        let in_queue = self.hub.leave_queues(id);
        self.hub.publish(&Outgoing::Removed { id });
        if in_queue {
            self.hub.publish_queues();
        }
        if !files.is_empty() {
            eprintln!("[uidm-daemon] recycling the files of #{id}");
            uidm::recycle::recycle(&files)
                .map_err(|e| format!("Removed {} from the list, but its files stay: {e}", entry.url))?;
        }
        Ok(())
    }

    /// Send the whole list to one client, under the table lock so no event can
    /// slip between the snapshot and the client's next frame.
    fn send_list(&self, reply: &Sender<Vec<u8>>) {
        let table = self.hub.table.lock().unwrap();
        let msg = Outgoing::Downloads {
            items: table.snapshot(),
        };
        let queues = Outgoing::Queues {
            items: self.hub.queues.lock().unwrap().clone(),
        };
        let refresh = Outgoing::RefreshWaiting {
            id: *self.refresh_waiting.lock().unwrap(),
        };
        for msg in [msg, queues, refresh] {
            if let Ok(frame) = serde_json::to_vec(&msg) {
                let _ = reply.send(frame);
            }
        }
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("[uidm-daemon] fatal: {e}");
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    // Binding is the single-instance lock.
    let listener = match TcpListener::bind(DAEMON_ADDR) {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "[uidm-daemon] {DAEMON_ADDR} unavailable ({e}); another daemon is likely running"
            );
            return Ok(());
        }
    };
    eprintln!("[uidm-daemon] listening on {DAEMON_ADDR}");

    // Publish the token only after winning the bind. A daemon that lost the
    // bind must not overwrite the running daemon's token.
    let token = ipc::new_token()?;
    ipc::write_token(&token)?;
    let token: Arc<str> = token.into();

    let (daemon, dirty, notes, wake) = Daemon::new(Paths::from_env());
    clean_early_folders(&daemon);
    let daemon = Arc::new(daemon);
    spawn_scheduler(Arc::clone(&daemon), wake);
    spawn_history_writer(Arc::clone(&daemon), dirty);
    spawn_notifier(Arc::clone(&daemon), notes);
    if let Err(e) = register_notifications() {
        eprintln!("[uidm-daemon] could not register for notifications: {e}");
    }
    // Queues set to start with UIDM.
    let launch: Vec<u64> = daemon.hub.queues.lock().unwrap().iter().filter(|q| q.start_on_launch).map(|q| q.id).collect();
    for id in launch {
        if let Err(e) = daemon.start_queue(id, None) {
            eprintln!("[uidm-daemon] could not start queue {id}: {e}");
        }
    }
    if daemon.settings.lock().unwrap().resume_at_startup {
        daemon.resume_interrupted();
    }
    spawn_idle_monitor(Arc::clone(&daemon));

    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let daemon = Arc::clone(&daemon);
                let token = Arc::clone(&token);
                thread::spawn(move || handle_client(stream, &daemon, &token));
            }
            Err(e) => eprintln!("[uidm-daemon] accept failed: {e}"),
        }
    }
    Ok(())
}

/// Serve one client until it disconnects. The client is registered for
/// broadcasts only after it authenticates.
fn handle_client(mut stream: TcpStream, daemon: &Arc<Daemon>, token: &str) {
    let id = NEXT_CLIENT.fetch_add(1, Ordering::Relaxed);
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "?".into());
    if let Err(e) = authenticate(&mut stream, token) {
        eprintln!("[uidm-daemon] client #{id} ({peer}) rejected: {e}");
        return;
    }
    eprintln!("[uidm-daemon] client #{id} connected ({peer})");

    let mut writer_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[uidm-daemon] client #{id}: {e}");
            return;
        }
    };
    let (tx, rx) = channel::<Vec<u8>>();
    daemon.hub.clients.lock().unwrap().insert(id, tx.clone());

    // One writer per client keeps its frames whole even when replies and
    // broadcasts race.
    let writer = thread::spawn(move || {
        while let Ok(frame) = rx.recv() {
            if write_frame(&mut writer_stream, &frame).is_err() {
                break;
            }
        }
    });

    let mut reader = stream;
    loop {
        match read_message(&mut reader, MAX_MESSAGE_BYTES) {
            Ok(Some(raw)) => dispatch(&raw, daemon, id, &tx),
            Ok(None) => break, // clean disconnect
            Err(e) => {
                eprintln!("[uidm-daemon] client #{id} read error: {e}");
                break;
            }
        }
    }

    // Removing the map's sender and dropping ours leaves the writer's channel
    // with no senders, so it drains what is queued and ends. Downloads this
    // client started keep running.
    daemon.hub.clients.lock().unwrap().remove(&id);
    daemon.hub.windows.lock().unwrap().remove(&id);
    daemon.stop_grab(id);
    drop(tx);
    let _ = writer.join();
    eprintln!("[uidm-daemon] client #{id} disconnected");
}

/// Run the daemon side of the handshake described in `uidm::ipc`.
fn authenticate(stream: &mut TcpStream, token: &str) -> io::Result<()> {
    stream.set_read_timeout(Some(AUTH_TIMEOUT))?;

    let client_nonce = match ipc::recv_handshake(stream)? {
        Handshake::AuthHello { client_nonce } => client_nonce,
        _ => return Err(denied("first frame was not auth_hello")),
    };
    let server_nonce = ipc::new_nonce()?;
    let proof = ipc::proof(token, DAEMON_LABEL, &client_nonce, &server_nonce);
    ipc::send_handshake(
        stream,
        &Handshake::AuthChallenge {
            server_nonce: server_nonce.clone(),
            proof,
        },
    )?;

    let expected = ipc::proof(token, HOST_LABEL, &client_nonce, &server_nonce);
    match ipc::recv_handshake(stream)? {
        Handshake::Auth { proof } if ipc::ct_eq(proof.as_bytes(), expected.as_bytes()) => {}
        _ => return Err(denied("missing or wrong auth proof")),
    }
    ipc::send_handshake(stream, &Handshake::AuthOk)?;

    stream.set_read_timeout(None)
}

fn denied(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, why.to_string())
}

/// Handle one request frame from client `client`, sending any direct reply to
/// `reply`.
fn dispatch(raw: &[u8], daemon: &Arc<Daemon>, client: u64, reply: &Sender<Vec<u8>>) {
    let send = |msg: &Outgoing| {
        if let Ok(frame) = serde_json::to_vec(msg) {
            let _ = reply.send(frame);
        }
    };
    let incoming: Incoming = match serde_json::from_slice(raw) {
        Ok(msg) => msg,
        Err(e) => {
            send(&Outgoing::Error {
                message: format!("invalid message: {e}"),
            });
            return;
        }
    };

    match incoming {
        Incoming::Hello { client: kind, .. } => {
            if kind.as_deref() == Some(WINDOW_CLIENT) {
                daemon.hub.windows.lock().unwrap().insert(client);
                // A window that opens during a countdown shows it too.
                if let Some(countdown) = daemon.countdown_message() {
                    send(&countdown);
                }
                if let Some(quota) = daemon.quota_message() {
                    send(&quota);
                }
                daemon.weekly_update_check();
                // Links picked from a page while no window was open.
                if let Some(links) = daemon.picked_links.lock().unwrap().take() {
                    send(&links);
                }
            }
            send(&Outgoing::Hello {
                name: "uidm-daemon".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            })
        }
        Incoming::Ping => send(&Outgoing::Pong),
        Incoming::Add(download) => {
            if let Err(message) = check_body(&download) {
                send(&Outgoing::Error { message });
                return;
            }
            let (id, url) = daemon.add(download);
            eprintln!("[uidm-daemon] add #{id}: {url}");
            send(&Outgoing::Ack {
                id,
                url,
                status: "accepted".to_string(),
            });
        }
        Incoming::AddMany { items, queue } => {
            let (count, skipped) = daemon.add_many(items, queue);
            eprintln!("[uidm-daemon] added {count} downloads to wait, skipped {skipped} already in the list");
            send(&Outgoing::AddedMany { count, skipped });
        }
        Incoming::ProbeMedia { request, download } => {
            if let Err(message) = check_body(&download) {
                send(&Outgoing::Error { message });
                return;
            }
            daemon.probe_media(request, download, reply.clone());
        }
        Incoming::Grab(req) => {
            if let Err(message) = daemon.grab(client, req, reply.clone()) {
                send(&Outgoing::Error { message });
            }
        }
        Incoming::CancelGrab => daemon.stop_grab(client),
        Incoming::Cancel { id } => {
            eprintln!("[uidm-daemon] cancel #{id}");
            daemon.cancel(id);
        }
        Incoming::Confirm(answer) => {
            let (id, start) = (answer.id, answer.start);
            match daemon.confirm(answer) {
                Ok(()) => eprintln!("[uidm-daemon] confirm #{id} (start: {start})"),
                Err(message) => send(&Outgoing::Error { message }),
            }
        }
        Incoming::Retry { id } => match daemon.retry(id) {
            Ok((new_id, url)) => {
                eprintln!("[uidm-daemon] retry #{id} as #{new_id}");
                send(&Outgoing::Ack {
                    id: new_id,
                    url,
                    status: "accepted".to_string(),
                });
            }
            Err(message) => send(&Outgoing::Error { message }),
        },
        Incoming::Remove { id, delete_files } => {
            if let Err(message) = daemon.remove(id, delete_files) {
                send(&Outgoing::Error { message });
            }
        }
        Incoming::List => daemon.send_list(reply),
        Incoming::SaveQueue { queue } => match daemon.save_queue(queue) {
            Ok(id) => eprintln!("[uidm-daemon] queue {id} saved"),
            Err(message) => send(&Outgoing::Error { message }),
        },
        Incoming::DeleteQueue { id } => {
            if let Err(message) = daemon.delete_queue(id) {
                send(&Outgoing::Error { message });
            }
        }
        Incoming::StartQueue { id } => match daemon.start_queue(id, None) {
            Ok(()) => {
                eprintln!("[uidm-daemon] start queue {id}");
                let _ = daemon.hub.wake.send(());
            }
            Err(message) => send(&Outgoing::Error { message }),
        },
        Incoming::StopQueue { id } => match daemon.stop_queue(id) {
            Ok(()) => eprintln!("[uidm-daemon] stop queue {id}"),
            Err(message) => send(&Outgoing::Error { message }),
        },
        Incoming::SetQueueOnce { id, when_done } => {
            if let Err(message) = daemon.hub.update_queue(id, |q| {
                q.when_done_once = when_done;
                Ok(())
            }) {
                send(&Outgoing::Error { message });
            }
        }
        Incoming::MoveToQueue { id, queue, position } => {
            if let Err(message) = daemon.move_to_queue(id, queue, position) {
                send(&Outgoing::Error { message });
            }
        }
        Incoming::CancelCountdown => daemon.cancel_countdown(),
        Incoming::CountdownNow => daemon.countdown_now(),
        Incoming::RefreshLink { id, download, restart } => {
            let url = download.as_ref().map(|d| d.url.clone()).unwrap_or_default();
            if let Some(d) = &download {
                if let Err(message) = check_body(d) {
                    send(&Outgoing::Error { message });
                    return;
                }
            }
            match daemon.refresh_link(id, download, restart) {
                Ok(Some(reason)) => send(&Outgoing::RefreshMismatch { id, url, reason }),
                Ok(None) => {}
                Err(message) => send(&Outgoing::Error { message }),
            }
        }
        Incoming::CancelRefresh => daemon.set_refresh_waiting(None),
        Incoming::PickLinks { page, mut links, user_agent } => {
            links.retain(|l| uidm::batch::is_web_url(&l.url));
            links.truncate(uidm::batch::MAX_BATCH);
            eprintln!("[uidm-daemon] {} links from {page} to pick from", links.len());
            let msg = Outgoing::PickLinks { page, links, user_agent };
            if daemon.hub.window_open() {
                daemon.hub.broadcast(&msg);
                daemon.hub.broadcast(&Outgoing::Show);
            } else {
                *daemon.picked_links.lock().unwrap() = Some(msg);
                daemon.show_window();
            }
        }
        Incoming::PreviewZip { id } => daemon.preview_zip(id, reply.clone()),
        Incoming::Convert { id, to, delete_original } => {
            if let Err(message) = daemon.convert(id, to, delete_original) {
                send(&Outgoing::Error { message });
            }
        }
        Incoming::CheckUpdate => {
            let (daemon, reply) = (Arc::clone(daemon), reply.clone());
            thread::spawn(move || {
                let msg = daemon.check_update(uidm::update::LATEST_RELEASE);
                if let Ok(frame) = serde_json::to_vec(&msg) {
                    let _ = reply.send(frame);
                }
            });
        }
        Incoming::Rendered { request, html, url } => {
            if let Some(tx) = daemon.render_waits.lock().unwrap().remove(&request) {
                let _ = tx.send(html.zip(url));
            }
        }
        Incoming::Cookies { request, cookies } => {
            if let Some(tx) = daemon.cookie_waits.lock().unwrap().remove(&request) {
                let _ = tx.send(cookies);
            }
        }
        Incoming::SetSpeedLimit { id, bytes_per_sec } => {
            if let Err(message) = daemon.set_download_limit(id, bytes_per_sec) {
                send(&Outgoing::Error { message });
            }
        }
        Incoming::SetOnDone { id, on_done } => {
            if let Err(message) = daemon.set_on_done(id, on_done) {
                send(&Outgoing::Error { message });
            }
        }
        Incoming::Redownload { id } => {
            if let Err(message) = daemon.redownload(id) {
                send(&Outgoing::Error { message });
            }
        }
        Incoming::SetProperties { id, changes } => {
            if let Err(message) = daemon.set_properties(id, changes) {
                send(&Outgoing::Error { message });
            }
        }
        Incoming::PauseAll => {
            eprintln!("[uidm-daemon] pause all");
            daemon.pause_all();
        }
        Incoming::ResumeAll => {
            eprintln!("[uidm-daemon] resume all");
            daemon.resume_all();
        }
        Incoming::GetSettings => send(&Outgoing::Settings {
            settings: daemon.settings.lock().unwrap().clone(),
        }),
        Incoming::SetSettings { settings } => match daemon.set_settings(settings) {
            Ok(()) => eprintln!("[uidm-daemon] settings changed"),
            Err(message) => send(&Outgoing::Error { message }),
        },
    }
}

/// Run the queue scheduler: a pass whenever something wakes it (a download
/// ended, a queue changed) and every few seconds for the clock.
fn spawn_scheduler(daemon: Arc<Daemon>, wake: Receiver<()>) {
    thread::spawn(move || loop {
        match wake.recv_timeout(SCHEDULER_POLL) {
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                while wake.try_recv().is_ok() {}
                daemon.tick();
                daemon.check_grab_schedules();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
    });
}

/// Run notifications on their own thread, so a slow toast or a scan never
/// holds up downloads.
fn spawn_notifier(daemon: Arc<Daemon>, notes: Receiver<Note>) {
    thread::spawn(move || {
        while let Ok(note) = notes.recv() {
            daemon.on_note(note);
        }
    });
}

/// Give UIDM's notifications a name and icon. Unpackaged apps need this
/// registration before Windows shows their notifications.
fn register_notifications() -> io::Result<()> {
    let icon = history::history_path()?.with_file_name("uidm.png");
    if std::fs::read(&icon).ok().as_deref() != Some(ICON_PNG) {
        settings::write_atomic(&icon, ICON_PNG)?;
    }
    registry::register_app_id(&icon)
}

/// What a notification says about `e`.
fn toast_for(e: &DownloadEntry, failed: bool) -> Toast {
    let path = e.path.as_deref().map(Path::new);
    let name = path
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| e.url.clone());
    let id = e.id;
    if failed {
        return Toast {
            title: "Download failed".into(),
            lines: vec![name, e.error.clone().unwrap_or_default()],
            buttons: vec![("Retry".into(), format!("retry:{id}"))],
            launch: format!("window:{id}"),
        };
    }
    let folder = path
        .and_then(|p| p.parent())
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let size = e.total.map(human_bytes).unwrap_or_default();
    let detail = [size, folder].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" in ");
    let show = ("Show in folder".to_string(), format!("show:{id}"));
    match &e.scan {
        Some(Scan::Threat { .. }) => Toast {
            title: "Threat found in a download".into(),
            lines: vec![name, "The virus scan found a threat. Open UIDM to decide what to do.".into()],
            buttons: vec![show],
            launch: format!("window:{id}"),
        },
        scan => Toast {
            title: match scan {
                Some(Scan::Failed { .. }) => "Download complete (virus scan failed)".into(),
                _ => "Download complete".into(),
            },
            lines: vec![name, detail],
            buttons: vec![("Open".into(), format!("open:{id}")), show],
            launch: format!("window:{id}"),
        },
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Refuse a POST whose body is not valid base64, rather than send another
/// body than the form's.
fn check_body(d: &Download) -> Result<(), String> {
    use base64::Engine as _;
    match &d.body {
        Some(body) if base64::engine::general_purpose::STANDARD.decode(body).is_err() => {
            Err(format!("the form body of {} is not valid base64", d.url))
        }
        _ => Ok(()),
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
        .map(|n| dir.join(engine::numbered_name(&name, n)))
        .find(|p| !p.exists())
        .unwrap_or_else(|| path.to_path_buf())
}

/// Delete the folders of early downloads a previous run left behind, except
/// those holding the partial file of a download in the list.
fn clean_early_folders(daemon: &Daemon) {
    let Ok(dirs) = std::fs::read_dir(std::env::temp_dir().join(EARLY_FOLDER)) else {
        return;
    };
    let partials: Vec<PathBuf> = daemon
        .hub
        .table
        .lock()
        .unwrap()
        .entries()
        .filter_map(|e| e.partial.as_deref().map(PathBuf::from))
        .collect();
    for dir in dirs.flatten().map(|d| d.path()) {
        if !partials.iter().any(|p| p.starts_with(&dir)) {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// Move a finished file to `to`, which must not exist yet. Across drives
/// this copies the file, then deletes the original.
fn move_file(from: &Path, to: &Path) -> Result<(), String> {
    if !from.is_file() {
        return Err(format!("The file is no longer at {}.", from.display()));
    }
    if to.exists() {
        return Err(format!("{} already exists.", to.display()));
    }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).map_err(|e| format!("Could not move the file to {}: {e}", to.display()))?;
    std::fs::remove_file(from).map_err(|e| format!("Copied the file, but could not delete {}: {e}", from.display()))
}

/// Keep a page a crawl read under its mirror folder, with its links to
/// kept pages and files pointing at the local copies. Not robots.txt, and
/// not pages that are not HTML.
fn save_page(req: &GrabRequest, m: &grab::Mirror, page: &grab::Page) {
    let html = page.content_type.as_deref().is_none_or(|t| t.to_ascii_lowercase().contains("html"));
    let Ok(url) = url::Url::parse(&page.url) else {
        return;
    };
    if !html || url.path() == "/robots.txt" {
        return;
    }
    let root = Path::new(m.folder.trim());
    let here = grab::mirror_path(root, &url, true);
    if here.exists() && !m.overwrite {
        return;
    }
    let text = String::from_utf8_lossy(&page.body);
    let out = grab::rewrite_links(&text, &url, &here, root, |u| grab::kept_locally(req, u));
    let written = here
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| uidm::settings::write_atomic(&here, out.as_bytes()));
    if let Err(e) = written {
        eprintln!("[uidm-daemon] could not save {}: {e}", here.display());
    }
}

/// Sleep, hibernate or shut down now, unless a test asked for a dry run.
fn power_off(action: WhenDone, force_close: bool) {
    if std::env::var_os(POWER_DRY_RUN_ENV).is_some() {
        eprintln!("[uidm-daemon] dry run: would {}", action.label());
        return;
    }
    eprintln!("[uidm-daemon] {}", action.label());
    if let Err(e) = power::perform(action, force_close) {
        eprintln!("[uidm-daemon] could not {}: {e}", action.label());
    }
}

/// Open the program or file a queue names for the end of its run, as
/// Explorer would on a double-click.
fn open_after_run(file: &str) {
    eprintln!("[uidm-daemon] opening {file}");
    if let Err(e) = Command::new("explorer.exe").arg(file).spawn() {
        eprintln!("[uidm-daemon] could not open {file}: {e}");
    }
}

/// Start the desktop window from this daemon's folder.
fn open_window() -> io::Result<()> {
    let gui = std::env::current_exe()?.with_file_name("uidm-gui.exe");
    Command::new(gui)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Write the history shortly after it changes, batching bursts of events
/// (progress, a cleared list) into one write.
fn spawn_history_writer(daemon: Arc<Daemon>, dirty: Receiver<()>) {
    thread::spawn(move || {
        while dirty.recv().is_ok() {
            thread::sleep(HISTORY_DEBOUNCE);
            while dirty.try_recv().is_ok() {}
            daemon.save_history();
        }
    });
}

/// Exit the process once it has been idle (no clients, no downloads) for
/// `IDLE_EXIT`, so a daemon started by the browser does not linger forever.
fn spawn_idle_monitor(daemon: Arc<Daemon>) {
    thread::spawn(move || {
        let mut idle_since: Option<Instant> = None;
        loop {
            thread::sleep(IDLE_POLL);
            let idle = daemon.hub.clients.lock().unwrap().is_empty()
                && daemon.engine.active_count() == 0
                && !daemon.queues_need_daemon();
            match (idle, idle_since) {
                (true, None) => idle_since = Some(Instant::now()),
                (true, Some(t)) if t.elapsed() >= IDLE_EXIT => {
                    eprintln!("[uidm-daemon] idle for {}s, exiting", IDLE_EXIT.as_secs());
                    daemon.save_history();
                    std::process::exit(0);
                }
                (false, _) => idle_since = None,
                _ => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    const BODY: &[u8] = b"the new bytes";

    /// A server that answers every request with `BODY`, without ranges.
    fn serve() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                thread::spawn(move || {
                    let mut req = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut buf) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => req.extend_from_slice(&buf[..n]),
                        }
                    }
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                        BODY.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    if !req.starts_with(b"HEAD") {
                        let _ = stream.write_all(BODY);
                    }
                });
            }
        });
        format!("http://{addr}/f.bin")
    }

    /// A server that sends `body` in byte ranges, slowly, counting the bytes
    /// it sends in `sent`.
    fn serve_ranges(body: Arc<Vec<u8>>, sent: Arc<AtomicU64>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let (body, sent) = (Arc::clone(&body), Arc::clone(&sent));
                thread::spawn(move || {
                    let mut req = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut buf) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => req.extend_from_slice(&buf[..n]),
                        }
                    }
                    let text = String::from_utf8_lossy(&req).to_ascii_lowercase();
                    let len = body.len() as u64;
                    let range = text
                        .lines()
                        .find_map(|l| l.strip_prefix("range: bytes="))
                        .and_then(|r| r.trim().split_once('-'))
                        .and_then(|(a, b)| Some((a.parse::<u64>().ok()?, b.parse::<u64>().unwrap_or(len - 1).min(len - 1))));
                    let (status, start, end, extra) = match range {
                        Some((a, b)) => ("206 Partial Content", a, b, format!("Content-Range: bytes {a}-{b}/{len}\r\n")),
                        None => ("200 OK", 0, len - 1, String::new()),
                    };
                    let head = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nETag: \"e1\"\r\n{extra}Connection: close\r\n\r\n",
                        end + 1 - start
                    );
                    if stream.write_all(head.as_bytes()).is_err() || req.starts_with(b"HEAD") {
                        return;
                    }
                    for chunk in body[start as usize..=end as usize].chunks(4096) {
                        if stream.write_all(chunk).is_err() {
                            return;
                        }
                        sent.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                        thread::sleep(Duration::from_millis(15));
                    }
                });
            }
        });
        format!("http://{addr}/big.bin")
    }

    struct Setup {
        daemon: Arc<Daemon>,
        dir: PathBuf,
        /// What a pretend window receives.
        window: Receiver<Vec<u8>>,
    }

    /// A daemon with no files, saving to a fresh folder, with a pretend
    /// window connected so nothing opens on screen.
    fn setup(name: &str, choice: Duplicate) -> Setup {
        let dir = std::env::temp_dir().join(format!("uidm_dup_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (daemon, ..) = Daemon::new(Paths {
            settings: None,
            history: None,
            requests: None,
            quota: None,
        });
        {
            let mut s = daemon.settings.lock().unwrap();
            s.download_dir = Some(dir.display().to_string());
            s.ask_where_to_save = false;
            s.if_duplicate = choice;
        }
        let (tx, window) = channel();
        daemon.hub.clients.lock().unwrap().insert(1, tx);
        daemon.hub.windows.lock().unwrap().insert(1);
        Setup {
            daemon: Arc::new(daemon),
            dir,
            window,
        }
    }

    impl Setup {
        /// Put a finished download of `url` in the list, its file at `path`
        /// holding `old`.
        fn finished(&self, url: &str, path: &Path) -> u64 {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"old").unwrap();
            let id = self.daemon.engine.reserve_id();
            let url = url.to_string();
            self.daemon.hub.publish(&Outgoing::Added { id, url: url.clone() });
            self.daemon.hub.publish(&Outgoing::Done {
                id,
                url,
                path: path.display().to_string(),
                note: None,
            });
            id
        }

        fn failed(&self, url: &str) -> u64 {
            let id = self.daemon.engine.reserve_id();
            let url = url.to_string();
            self.daemon.hub.publish(&Outgoing::Added { id, url: url.clone() });
            self.daemon.hub.publish(&Outgoing::Failed {
                id,
                url,
                error: "gone".into(),
            });
            id
        }

        fn add(&self, url: &str) -> u64 {
            self.daemon
                .add(Download {
                    url: url.into(),
                    ..Download::default()
                })
                .0
        }

        fn len(&self) -> usize {
            self.daemon.hub.table.lock().unwrap().len()
        }

        /// Wait for download `id` to reach `status`, and return it.
        fn wait(&self, id: u64, status: Status) -> DownloadEntry {
            let until = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(e) = self.daemon.entry(id).filter(|e| e.status == status) {
                    return e;
                }
                assert!(Instant::now() < until, "#{id} never became {status:?}: {:?}", self.daemon.entry(id));
                thread::sleep(Duration::from_millis(20));
            }
        }

        fn window_got(&self, wanted: impl Fn(&Outgoing) -> bool) -> bool {
            self.window
                .try_iter()
                .filter_map(|f| serde_json::from_slice::<Outgoing>(&f).ok())
                .any(|m| wanted(&m))
        }
    }

    #[test]
    fn new_name_keeps_the_old_file() {
        let t = setup("new_name", Duplicate::NewName);
        let url = serve();
        let old = t.finished(&url, &t.dir.join("f.bin"));
        let new = t.add(&format!("{url}#again"));
        assert_ne!(new, old);
        let done = t.wait(new, Status::Done);
        assert_eq!(done.path.map(PathBuf::from), Some(t.dir.join("f (1).bin")));
        assert_eq!(std::fs::read(t.dir.join("f.bin")).unwrap(), b"old");
        assert!(t.daemon.entry(old).is_some(), "the old download stays in the list");
    }

    #[test]
    fn overwrite_replaces_the_old_download_and_its_file() {
        let t = setup("overwrite", Duplicate::Overwrite);
        let url = serve();
        // Saved somewhere its category would not pick now.
        let path = t.dir.join("elsewhere").join("f.bin");
        let old = t.finished(&url, &path);
        let new = t.add(&url);
        let done = t.wait(new, Status::Done);
        assert_eq!(done.path.map(PathBuf::from), Some(path.clone()));
        assert_eq!(std::fs::read(&path).unwrap(), BODY);
        assert!(t.daemon.entry(old).is_none(), "the old download left the list");
    }

    #[test]
    fn overwrite_keeps_a_running_download() {
        let t = setup("overwrite_running", Duplicate::Overwrite);
        let url = serve();
        let old = t.daemon.engine.reserve_id();
        t.daemon.hub.publish(&Outgoing::Added { id: old, url: url.clone() });
        assert_eq!(t.daemon.status(old), Some(Status::Queued));
        assert_eq!(t.add(&url), old);
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn existing_shows_a_finished_download_again() {
        let t = setup("existing_done", Duplicate::Existing);
        let url = serve();
        let old = t.finished(&url, &t.dir.join("f.bin"));
        assert_eq!(t.add(&url), old);
        assert_eq!(t.len(), 1, "nothing was added");
        assert!(t.window_got(|m| matches!(m, Outgoing::ShowDone { id } if *id == old)));
    }

    #[test]
    fn existing_adds_anyway_when_the_finished_file_is_gone() {
        let t = setup("existing_gone", Duplicate::Existing);
        let url = serve();
        let old = t.finished(&url, &t.dir.join("f.bin"));
        std::fs::remove_file(t.dir.join("f.bin")).unwrap();
        let new = t.add(&url);
        assert_ne!(new, old);
        t.wait(new, Status::Done);
    }

    #[test]
    fn existing_resumes_a_stopped_download() {
        let t = setup("existing_failed", Duplicate::Existing);
        let url = serve();
        let old = t.failed(&url);
        let resumed = t.add(&url);
        assert_ne!(resumed, old, "a retry gets a new id");
        t.wait(resumed, Status::Done);
        assert!(t.daemon.entry(old).is_none());
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn ask_holds_it_in_the_dialog_then_follows_the_answer() {
        let t = setup("ask", Duplicate::Ask);
        let url = serve();
        let old = t.finished(&url, &t.dir.join("f.bin"));
        let new = t.add(&url);
        let asked = t.wait(new, Status::Confirming);
        assert_eq!(asked.duplicate_of, Some(old));
        t.daemon
            .confirm(Answer {
                duplicate: Some(Duplicate::Existing),
                ..Answer::start(new)
            })
            .unwrap();
        assert!(t.daemon.entry(new).is_none(), "the new download was not added");
        assert!(t.window_got(|m| matches!(m, Outgoing::ShowDone { id } if *id == old)));

        // No answer about the duplicate means a new name.
        let again = t.add(&url);
        t.wait(again, Status::Confirming);
        t.daemon.confirm(Answer::start(again)).unwrap();
        let done = t.wait(again, Status::Done);
        assert_eq!(done.path.map(PathBuf::from), Some(t.dir.join("f (1).bin")));
        assert_eq!(done.duplicate_of, None);
    }

    #[test]
    fn ask_then_overwrite_writes_over_the_old_file() {
        let t = setup("ask_overwrite", Duplicate::Ask);
        let url = serve();
        let path = t.dir.join("elsewhere").join("f.bin");
        let old = t.finished(&url, &path);
        let new = t.add(&url);
        t.wait(new, Status::Confirming);
        t.daemon
            .confirm(Answer {
                filename: Some("ignored.bin".into()),
                duplicate: Some(Duplicate::Overwrite),
                ..Answer::start(new)
            })
            .unwrap();
        let done = t.wait(new, Status::Done);
        assert_eq!(done.path.map(PathBuf::from), Some(path.clone()));
        assert_eq!(std::fs::read(&path).unwrap(), BODY);
        assert!(t.daemon.entry(old).is_none());
    }

    #[test]
    fn add_many_skips_duplicates_unless_told_to_add_them() {
        let url = serve();
        let items = |urls: &[&str]| {
            urls.iter()
                .map(|u| Download {
                    url: u.to_string(),
                    ..Download::default()
                })
                .collect::<Vec<_>>()
        };
        let t = setup("many_ask", Duplicate::Ask);
        t.finished(&url, &t.dir.join("f.bin"));
        assert_eq!(t.daemon.add_many(items(&[&url, "https://example.com/other.bin"]), None), (1, 1));

        let t = setup("many_new_name", Duplicate::NewName);
        t.finished(&url, &t.dir.join("f.bin"));
        assert_eq!(t.daemon.add_many(items(&[&url]), None), (1, 0));

        // An item's own answer wins over the setting.
        let t = setup("many_answered", Duplicate::Ask);
        t.finished(&url, &t.dir.join("f.bin"));
        let mut answered = items(&[&url]);
        answered[0].duplicate = Some(Duplicate::NewName);
        assert_eq!(t.daemon.add_many(answered, None), (1, 0));
    }

    #[test]
    fn form_posts_are_never_duplicates() {
        let t = setup("post", Duplicate::Existing);
        let url = "https://example.com/form";
        t.finished(url, &t.dir.join("form"));
        let get = Download {
            url: url.into(),
            ..Download::default()
        };
        assert!(t.daemon.duplicate_of(&get).is_some());
        let post = Download {
            method: Some("POST".into()),
            ..get
        };
        assert!(t.daemon.duplicate_of(&post).is_none());
    }

    #[test]
    fn properties_rename_and_move_a_finished_file() {
        let t = setup("props_move", Duplicate::NewName);
        let id = t.finished("https://x/f.bin", &t.dir.join("f.bin"));
        let changes = Properties {
            filename: Some("renamed.bin".into()),
            folder: Some(t.dir.join("sub").display().to_string()),
            description: Some("  kept for later ".into()),
            ..Properties::default()
        };
        t.daemon.set_properties(id, changes).unwrap();
        let e = t.daemon.entry(id).unwrap();
        let moved = t.dir.join("sub").join("renamed.bin");
        assert_eq!(e.path.map(PathBuf::from), Some(moved.clone()));
        assert_eq!(std::fs::read(&moved).unwrap(), b"old");
        assert!(!t.dir.join("f.bin").exists());
        assert_eq!(e.description.as_deref(), Some("kept for later"));

        // Never over another file.
        std::fs::write(t.dir.join("taken.bin"), b"x").unwrap();
        let onto = Properties {
            filename: Some("taken.bin".into()),
            folder: Some(t.dir.display().to_string()),
            ..Properties::default()
        };
        assert!(t.daemon.set_properties(id, onto).is_err());
        assert!(moved.exists());
    }

    #[test]
    fn properties_change_what_a_retry_sends() {
        let t = setup("props_retry", Duplicate::NewName);
        let id = t.failed("https://x/old.bin");
        let changes = Properties {
            url: Some("https://y/new.bin".into()),
            referrer: Some("https://y/page".into()),
            login: Some(DownloadLogin {
                username: "me".into(),
                password: Some("pw".into()),
            }),
            ..Properties::default()
        };
        t.daemon.set_properties(id, changes).unwrap();
        let e = t.daemon.entry(id).unwrap();
        assert_eq!(e.url, "https://y/new.bin");
        assert_eq!(e.referrer.as_deref(), Some("https://y/page"));
        assert_eq!(e.login_user.as_deref(), Some("me"));
        let req = t.daemon.hub.requests.lock().unwrap().get(&id).cloned().unwrap();
        assert_eq!(req.url, "https://y/new.bin");
        assert_eq!(req.referrer.as_deref(), Some("https://y/page"));

        // A new user name without a password keeps the old password.
        let rename = Properties {
            login: Some(DownloadLogin {
                username: "other".into(),
                password: None,
            }),
            ..Properties::default()
        };
        t.daemon.set_properties(id, rename).unwrap();
        let req = t.daemon.hub.requests.lock().unwrap().get(&id).cloned().unwrap();
        let login = req.login.clone().unwrap();
        assert_eq!((login.username.as_str(), login.password.as_deref()), ("other", Some("pw")));
        let net = t.daemon.net_for(&req, &Settings::default());
        let found = net::find_login(&net.logins, "https://y/new.bin").unwrap();
        assert_eq!(found.username, "other");

        // An empty user name removes the login.
        let clear = Properties {
            login: Some(DownloadLogin::default()),
            ..Properties::default()
        };
        t.daemon.set_properties(id, clear).unwrap();
        assert_eq!(t.daemon.entry(id).unwrap().login_user, None);
        assert!(t.daemon.hub.requests.lock().unwrap()[&id].login.is_none());
    }

    #[test]
    fn properties_refuse_running_downloads_and_bad_links() {
        let t = setup("props_refuse", Duplicate::NewName);
        let running = t.daemon.engine.reserve_id();
        t.daemon.hub.publish(&Outgoing::Added { id: running, url: "https://x/a".into() });
        let describe = Properties {
            description: Some("x".into()),
            ..Properties::default()
        };
        assert!(t.daemon.set_properties(running, describe).is_err());
        let failed = t.failed("https://x/b");
        let bad = Properties {
            url: Some("file://x/b".into()),
            ..Properties::default()
        };
        assert!(t.daemon.set_properties(failed, bad).is_err());
        let rename = Properties {
            filename: Some("c".into()),
            ..Properties::default()
        };
        assert!(t.daemon.set_properties(failed, rename).is_err(), "only finished files move");
    }

    #[test]
    fn removing_with_files_recycles_only_what_the_download_wrote() {
        let t = setup("remove_files", Duplicate::NewName);
        let kept = t.dir.join("kept.bin");
        let id = t.finished("https://x/kept.bin", &kept);
        t.daemon.remove(id, false).unwrap();
        assert!(kept.exists(), "a plain remove keeps the file");

        let gone = t.dir.join("gone.bin");
        let id = t.finished("https://x/gone.bin", &gone);
        let sidecar = t.dir.join("gone.bin.uidmpart");
        std::fs::write(&sidecar, b"{}").unwrap();
        t.daemon.remove(id, true).unwrap();
        assert!(!gone.exists() && !sidecar.exists());
        assert!(t.daemon.entry(id).is_none());

        // A download that never started only proposed its path.
        let other = t.dir.join("someone_elses.bin");
        std::fs::write(&other, b"theirs").unwrap();
        let id = t.failed("https://x/someone_elses.bin");
        t.daemon.hub.update(id, |e| {
            e.path = Some(other.display().to_string());
            Ok(())
        }).unwrap();
        t.daemon.remove(id, true).unwrap();
        assert!(other.exists());
    }

    #[test]
    fn downloading_again_keeps_the_row_and_follows_the_file_setting() {
        let t = setup("again_rename", Duplicate::NewName);
        let url = serve();
        let path = t.dir.join("kept").join("f.bin");
        let id = t.finished(&url, &path);
        t.daemon
            .set_properties(
                id,
                Properties {
                    description: Some("monthly".into()),
                    ..Properties::default()
                },
            )
            .unwrap();
        t.daemon.redownload(id).unwrap();
        let done = t.wait(id, Status::Done);
        assert_eq!(done.description.as_deref(), Some("monthly"));
        assert_eq!(done.path.map(PathBuf::from), Some(t.dir.join("kept").join("f (1).bin")));
        assert_eq!(std::fs::read(&path).unwrap(), b"old", "the old file is a taken name");
        assert_eq!(t.len(), 1, "still one row");

        let t = setup("again_overwrite", Duplicate::NewName);
        t.daemon.settings.lock().unwrap().file_exists = FileExists::Overwrite;
        let path = t.dir.join("f.bin");
        let id = t.finished(&url, &path);
        t.daemon.redownload(id).unwrap();
        let done = t.wait(id, Status::Done);
        assert_eq!(done.path.map(PathBuf::from), Some(path.clone()));
        assert_eq!(std::fs::read(&path).unwrap(), BODY);
    }

    #[test]
    fn only_stopped_downloads_go_again() {
        let t = setup("again_refuse", Duplicate::NewName);
        let id = t.daemon.engine.reserve_id();
        t.daemon.hub.publish(&Outgoing::Added { id, url: "https://x/a".into() });
        assert!(t.daemon.redownload(id).is_err());
        assert!(t.daemon.redownload(999).is_err());
    }

    #[test]
    fn a_download_limit_is_kept_through_a_retry() {
        let t = setup("own_limit", Duplicate::NewName);
        let id = t.failed("https://x/slow.bin");
        t.daemon.set_download_limit(id, Some(64 * 1024)).unwrap();
        assert_eq!(t.daemon.entry(id).unwrap().speed_limit, Some(64 * 1024));
        assert!(t.daemon.set_download_limit(id, Some(10)).is_err(), "below the smallest limit");
        let (new_id, _) = t.daemon.retry(id).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while t.daemon.entry(new_id).and_then(|e| e.speed_limit).is_none() {
            assert!(Instant::now() < until, "the retry lost its limit");
            thread::sleep(Duration::from_millis(20));
        }
        t.daemon.cancel(new_id);
        t.daemon.set_download_limit(new_id, None).unwrap();
        assert_eq!(t.daemon.entry(new_id).unwrap().speed_limit, None);
    }

    #[test]
    fn a_download_does_what_it_asked_once_finished() {
        let t = setup("on_done", Duplicate::NewName);
        let id = t.failed("https://x/a.bin");
        let asked = OnDone {
            dialog: true,
            then: WhenDone::Nothing,
            force_close: false,
        };
        t.daemon.set_on_done(id, Some(asked)).unwrap();
        assert_eq!(t.daemon.entry(id).unwrap().on_done, Some(asked));
        t.daemon.after_download(id);
        assert_eq!(t.daemon.entry(id).unwrap().on_done, Some(asked), "a failed download keeps it for its retry");

        t.daemon
            .hub
            .update(id, |e| {
                e.status = Status::Done;
                Ok(())
            })
            .unwrap();
        assert!(t.daemon.set_on_done(id, None).is_err(), "too late once finished");
        t.daemon.after_download(id);
        assert_eq!(t.daemon.entry(id).unwrap().on_done, None, "it runs once");
    }

    #[test]
    fn choosing_nothing_clears_the_choice() {
        let t = setup("on_done_clear", Duplicate::NewName);
        let id = t.failed("https://x/a.bin");
        t.daemon
            .set_on_done(
                id,
                Some(OnDone {
                    dialog: false,
                    then: WhenDone::ShutDown,
                    force_close: true,
                }),
            )
            .unwrap();
        t.daemon.set_on_done(id, Some(OnDone::default())).unwrap();
        assert_eq!(t.daemon.entry(id).unwrap().on_done, None);
    }

    #[test]
    fn the_save_as_folder_is_remembered_for_its_category() {
        let t = setup("remember_folder", Duplicate::NewName);
        t.daemon.settings.lock().unwrap().categories[0].remember_folder = true;
        let (id, _) = t.daemon.add_later(Download {
            url: "https://x/a.zip".into(),
            ..Download::default()
        });
        let chosen = t.dir.join("chosen");
        t.daemon
            .confirm(Answer {
                filename: Some("a.zip".into()),
                folder: Some(chosen.display().to_string()),
                start: false,
                ..Answer::start(id)
            })
            .unwrap();
        let s = t.daemon.settings.lock().unwrap().clone();
        assert_eq!(s.categories[0].last_folder.as_deref(), Some(chosen.display().to_string().as_str()));
        assert_eq!(s.folders().for_name("b.zip"), chosen);
    }

    #[test]
    fn the_save_as_dialog_keeps_a_category_and_description() {
        let t = setup("category", Duplicate::NewName);
        t.daemon.settings.lock().unwrap().categories[2].remember_folder = true;
        let (id, _) = t.daemon.add_later(Download {
            url: "https://x/a.zip".into(),
            ..Download::default()
        });
        let chosen = t.dir.join("tunes");
        t.daemon
            .confirm(Answer {
                filename: Some("a.zip".into()),
                folder: Some(chosen.display().to_string()),
                start: false,
                category: Some(" Music ".into()),
                description: Some(" the album ".into()),
                ..Answer::start(id)
            })
            .unwrap();
        let e = t.daemon.entry(id).unwrap();
        assert_eq!(e.category.as_deref(), Some("Music"));
        assert_eq!(e.description.as_deref(), Some("the album"));
        // The chosen category, not the file's type, remembers the folder.
        let s = t.daemon.settings.lock().unwrap().clone();
        assert_eq!(s.categories[2].last_folder.as_deref(), Some(chosen.display().to_string().as_str()));
        assert_eq!(s.categories[0].last_folder, None);
        // A retry keeps both.
        let (again, _) = t.daemon.retry(id).unwrap();
        let e = t.daemon.entry(again).unwrap();
        assert_eq!(e.category.as_deref(), Some("Music"));
        assert_eq!(e.description.as_deref(), Some("the album"));
    }

    #[test]
    fn the_save_as_dialog_remembers_its_choices() {
        let t = setup("remember_choices", Duplicate::Ask);
        let queue = t.daemon.save_queue(Queue::new(0, "Night")).unwrap();

        // "Remember this path" for General, and "Don't ask me again" for the
        // queue of a download added for later.
        let (id, _) = t.daemon.add_later(Download {
            url: "https://x/notes.xyz".into(),
            ..Download::default()
        });
        let chosen = t.dir.join("misc");
        t.daemon
            .confirm(Answer {
                folder: Some(chosen.display().to_string()),
                filename: Some("notes.xyz".into()),
                start: false,
                queue: Some(queue),
                remember_folder: Some(true),
                remember_queue: true,
                ..Answer::start(id)
            })
            .unwrap();
        let s = t.daemon.settings.lock().unwrap().clone();
        assert!(s.general_remember_folder);
        assert_eq!(s.folders().for_name("other.xyz"), chosen);
        assert_eq!(s.later_queue, Some(queue));
        // Download later now goes to that queue without being told.
        let (later, _) = t.daemon.add_later(Download {
            url: "https://x/more.xyz".into(),
            ..Download::default()
        });
        assert_eq!(t.daemon.hub.queue_of(later), Some(queue));

        // "Remember my selection" for an address already in the list.
        let url = serve();
        let old = t.finished(&url, &t.dir.join("twice.bin"));
        let again = t.add(&url);
        assert_eq!(t.wait(again, Status::Confirming).duplicate_of, Some(old));
        t.daemon
            .confirm(Answer {
                duplicate: Some(Duplicate::NewName),
                remember_duplicate: true,
                ..Answer::start(again)
            })
            .unwrap();
        assert_eq!(t.daemon.settings.lock().unwrap().if_duplicate, Duplicate::NewName);
    }

    #[test]
    fn the_quota_pauses_downloads_and_lets_them_go_again() {
        let t = setup("quota", Duplicate::NewName);
        {
            let mut s = t.daemon.settings.lock().unwrap();
            s.quota_enabled = true;
            s.quota_mb = 1;
            s.quota_hours = 1;
        }
        let url = serve();
        let id = t.failed(&url);
        // 2 MB came in this minute.
        {
            let mut q = t.daemon.hub.quota.lock().unwrap();
            q.started(99, 0);
            q.progress(99, 2 << 20, uidm::quota::now_minute());
        }
        t.daemon.check_quota();
        assert!(t.window_got(|m| matches!(m, Outgoing::Quota { paused: true, resumes_at: Some(_), .. })));
        assert!(t.daemon.quota_paused.lock().unwrap().is_some());
        // A download started while paused is stopped on the next check.
        t.daemon.retry(id).unwrap();
        t.daemon.check_quota();
        let held = t.daemon.quota_paused.lock().unwrap().clone().unwrap();
        assert!(!held.1.is_empty(), "the new download is held too");

        *t.daemon.hub.quota.lock().unwrap() = Usage::default();
        t.daemon.check_quota();
        assert!(t.daemon.quota_paused.lock().unwrap().is_none());
        assert!(t.window_got(|m| matches!(m, Outgoing::Quota { paused: false, .. })));
    }

    #[test]
    fn a_crawl_asks_the_browser_for_its_cookies() {
        let t = setup("crawl_cookies", Duplicate::NewName);
        let d = Arc::clone(&t.daemon);
        let asking = thread::spawn(move || d.browser_cookies("https://site.net/"));
        let request = loop {
            let frame = t.window.recv_timeout(Duration::from_secs(5)).expect("no cookies_wanted");
            if let Ok(Outgoing::CookiesWanted { request, url }) = serde_json::from_slice(&frame) {
                assert_eq!(url, "https://site.net/");
                break request;
            }
        };
        let tx = t.daemon.cookie_waits.lock().unwrap().remove(&request).unwrap();
        tx.send(Some("session=abc".into())).unwrap();
        assert_eq!(asking.join().unwrap().as_deref(), Some("session=abc"));
    }

    #[test]
    fn a_scheduled_project_adds_only_new_finds() {
        let t = setup("grab_schedule", Duplicate::NewName);
        // A one-page site linking to one PDF.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let (status, body) = if head.starts_with("GET / ") {
                    ("200 OK", "<a href=\"/report.pdf\">r</a>")
                } else {
                    ("404 Not Found", "")
                };
                let _ = write!(
                    s,
                    "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        let request = GrabRequest {
            start: format!("http://{addr}/"),
            depth: 0,
            delay_ms: 200,
            ..GrabRequest::default()
        };
        let project: grab::Project = serde_json::from_value(serde_json::json!({
            "name": "nightly",
            "request": request,
            "download_new": true,
        }))
        .unwrap();
        let dir = t.dir.join("projects");
        t.daemon.run_scheduled_grab(&dir, project, 1);
        let saved = grab::Project::load(&dir, "nightly").unwrap();
        assert_eq!(saved.seen, [format!("http://{addr}/report.pdf")]);
        assert_eq!(t.len(), 1, "the new find was added");
        t.daemon.run_scheduled_grab(&dir, saved, 2);
        assert_eq!(t.len(), 1, "nothing new the second time");
    }

    #[test]
    fn crawled_pages_are_saved_with_local_links() {
        let dir = std::env::temp_dir().join(format!("uidm_save_page_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mirror = grab::Mirror {
            folder: dir.display().to_string(),
            pages: true,
            overwrite: false,
        };
        let req = GrabRequest {
            start: "https://s/".into(),
            mirror: Some(mirror.clone()),
            ..GrabRequest::default()
        };
        let page = grab::Page {
            url: "https://s/docs/".into(),
            content_type: Some("text/html".into()),
            body: br#"<a href="guide.pdf">g</a>"#.to_vec(),
        };
        save_page(&req, &mirror, &page);
        let saved = dir.join("s").join("docs").join("index.html");
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), r#"<a href="guide.pdf">g</a>"#);
        std::fs::write(&saved, "mine").unwrap();
        save_page(&req, &mirror, &page);
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), "mine", "skipped, not replaced");
        let robots = grab::Page {
            url: "https://s/robots.txt".into(),
            content_type: Some("text/plain".into()),
            body: b"User-agent: *".to_vec(),
        };
        save_page(&req, &mirror, &robots);
        assert!(!dir.join("s").join("robots.txt.html").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_crawl_can_have_the_browser_render_a_page() {
        let t = setup("render_page", Duplicate::NewName);
        assert!(!t.daemon.hub.extension_connected(), "only a window is connected");
        let d = Arc::clone(&t.daemon);
        let asking = thread::spawn(move || d.render_in_browser("https://site.net/app"));
        let request = loop {
            let frame = t.window.recv_timeout(Duration::from_secs(5)).expect("no render_page");
            if let Ok(Outgoing::RenderPage { request, .. }) = serde_json::from_slice(&frame) {
                break request;
            }
        };
        let tx = t.daemon.render_waits.lock().unwrap().remove(&request).unwrap();
        tx.send(Some(("<a href=x.pdf>".into(), "https://site.net/app/".into()))).unwrap();
        let (html, url) = asking.join().unwrap().unwrap();
        assert_eq!((html.as_str(), url.as_str()), ("<a href=x.pdf>", "https://site.net/app/"));
    }

    #[test]
    fn the_update_check_reports_only_newer_releases() {
        let t = setup("update_check", Duplicate::NewName);
        let serve_tag = |tag: &'static str| {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            thread::spawn(move || {
                for mut s in listener.incoming().flatten() {
                    let mut buf = [0u8; 2048];
                    let _ = s.read(&mut buf);
                    let body = format!(r#"{{"tag_name":"{tag}","html_url":"https://github.com/tajbinkhan/uidm/releases/tag/{tag}"}}"#);
                    let _ = write!(
                        s,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                }
            });
            format!("http://{addr}/releases/latest")
        };
        match t.daemon.check_update(&serve_tag("v99.0.0")) {
            Outgoing::Update { newer: Some(r), error: None } => assert_eq!(r.version, "99.0.0"),
            other => panic!("expected a newer release: {other:?}"),
        }
        assert!(matches!(t.daemon.check_update(&serve_tag("v0.0.1")), Outgoing::Update { newer: None, error: None }));
        assert!(matches!(t.daemon.check_update("http://127.0.0.1:1/none"), Outgoing::Update { error: Some(_), .. }));
        assert!(t.daemon.settings.lock().unwrap().last_update_check > 0, "the check was noted");
    }

    #[test]
    fn a_dropped_link_always_gets_the_save_as_dialog() {
        let t = setup("ask_flag", Duplicate::NewName);
        assert!(!t.daemon.settings.lock().unwrap().ask_where_to_save);
        let (id, _) = t.daemon.add(Download {
            url: serve(),
            ask: Some(true),
            ..Download::default()
        });
        t.wait(id, Status::Confirming);
    }

    #[test]
    fn some_types_start_without_the_dialog_and_add_only_waits() {
        let t = setup("auto_start", Duplicate::NewName);
        {
            let mut s = t.daemon.settings.lock().unwrap();
            s.ask_where_to_save = true;
            s.auto_start_types = vec!["bin".into()];
        }
        let id = t.add(&serve());
        assert_eq!(t.wait(id, Status::Done).path.map(PathBuf::from), Some(t.dir.join("f.bin")));

        t.daemon.settings.lock().unwrap().add_only = true;
        let id = t.add(&serve());
        assert_eq!(t.daemon.status(id), Some(Status::Waiting));
        assert_eq!(t.daemon.hub.queue_of(id), Some(MAIN_QUEUE));
    }

    /// A download that starts while its dialog is open, the dialog then
    /// answered before or after it finishes: either way the file ends up
    /// where the answer says, whole, and its bytes come over once.
    #[test]
    fn a_download_started_while_asking_keeps_its_bytes() {
        const LEN: usize = 1024 * 1024;
        for finish_first in [false, true] {
            let t = setup(if finish_first { "early_done" } else { "early" }, Duplicate::NewName);
            {
                let mut s = t.daemon.settings.lock().unwrap();
                s.ask_where_to_save = true;
                s.start_while_asking = true;
                s.connections = 2;
            }
            let body: Arc<Vec<u8>> = Arc::new((0..LEN).map(|i| (i % 251) as u8).collect());
            let sent = Arc::new(AtomicU64::new(0));
            let id = t.add(&serve_ranges(Arc::clone(&body), Arc::clone(&sent)));
            let asked = t.wait(id, Status::Confirming);
            let wanted = if finish_first { LEN as u64 } else { LEN as u64 / 4 };
            let until = Instant::now() + Duration::from_secs(30);
            while t.daemon.entry(id).unwrap().done < wanted {
                assert!(Instant::now() < until, "the early download never got going");
                thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(t.daemon.entry(id).unwrap().status, Status::Confirming, "still asking");
            assert!(!Path::new(asked.path.as_deref().unwrap()).exists(), "nothing written where it was proposed");

            let chosen = t.dir.join("chosen");
            t.daemon
                .confirm(Answer {
                    folder: Some(chosen.display().to_string()),
                    filename: Some("big.bin".into()),
                    ..Answer::start(id)
                })
                .unwrap();
            let done = t.wait(id, Status::Done);
            assert_eq!(done.path.map(PathBuf::from), Some(chosen.join("big.bin")));
            assert_eq!(std::fs::read(chosen.join("big.bin")).unwrap(), *body);
            // Two one-byte probes on top of the file itself.
            assert!(sent.load(Ordering::Relaxed) < LEN as u64 + 4096, "sent {}", sent.load(Ordering::Relaxed));
        }
    }

    #[test]
    fn canceling_the_dialog_drops_the_early_download() {
        let t = setup("early_cancel", Duplicate::NewName);
        {
            let mut s = t.daemon.settings.lock().unwrap();
            s.ask_where_to_save = true;
            s.start_while_asking = true;
        }
        let body: Arc<Vec<u8>> = Arc::new(vec![7u8; 1024 * 1024]);
        let id = t.add(&serve_ranges(body, Arc::new(AtomicU64::new(0))));
        t.wait(id, Status::Confirming);
        let until = Instant::now() + Duration::from_secs(30);
        while t.daemon.hub.early_of(id).is_none_or(|s| t.daemon.hub.early.lock().unwrap()[&s].file.is_none()) {
            assert!(Instant::now() < until, "the early download never started");
            thread::sleep(Duration::from_millis(20));
        }
        let shadow = t.daemon.hub.early_of(id).unwrap();
        let dir = t.daemon.hub.early.lock().unwrap()[&shadow].dir.clone();
        t.daemon.cancel(id);
        t.wait(id, Status::Canceled);
        let until = Instant::now() + Duration::from_secs(10);
        while dir.exists() || t.daemon.hub.early.lock().unwrap().contains_key(&shadow) {
            assert!(Instant::now() < until, "the early download was not dropped");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_finished_video_converts_into_a_row_of_its_own() {
        let Some(ffmpeg) = Settings::default().ffmpeg() else {
            eprintln!("no ffmpeg; skipped");
            return;
        };
        let t = setup("convert", Duplicate::NewName);
        let clip = t.dir.join("talk.mkv");
        let id = t.finished("https://x/talk.mkv", &clip);
        let made = Command::new(&ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=duration=1:size=160x120:rate=10"])
            .args(["-f", "lavfi", "-i", "sine=duration=1", "-c:v", "libx264", "-c:a", "aac", "-shortest"])
            .arg(&clip)
            .creation_flags(0x0800_0000)
            .status()
            .unwrap();
        assert!(made.success());
        t.daemon.convert(id, uidm::media::convert::Target::M4a, true).unwrap();
        let new = t.daemon.hub.table.lock().unwrap().max_id();
        let done = t.wait(new, Status::Done);
        let out = PathBuf::from(done.path.unwrap());
        assert_eq!(out, t.dir.join("talk.m4a"));
        assert!(std::fs::metadata(&out).unwrap().len() > 0);
        assert_eq!(done.note.as_deref(), Some("Converted from talk.mkv"));
        assert!(t.daemon.entry(id).is_none() && !clip.exists(), "the original went, as asked");
        assert!(t.daemon.convert(new + 100, uidm::media::convert::Target::Mp3, false).is_err());
    }
}