//! UIDM daemon: the long-lived process that owns the download engine.
//!
//! Clients (a host per browser connection, and the desktop window) connect over
//! loopback TCP and speak the same length-prefixed JSON frames as the browser.
//! Each connection starts with a mutual handshake over the token this daemon
//! wrote at start (see `uidm::ipc`); until the client passes it, it gets no
//! events and its requests are not read.
//!
//! Replies to a request (`hello`, `pong`, `ack`, `downloads`, `error`) go to the
//! client that asked. Download events (`added`, `started`, `progress`, `done`,
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
use uidm::engine::{self, Engine, Progress};
use uidm::history::{self, History};
use uidm::ipc::{self, Handshake, AUTH_TIMEOUT, DAEMON_ADDR, DAEMON_LABEL, HOST_LABEL};
use uidm::protocol::{read_message, write_frame, Download, DownloadEntry, Incoming, Outgoing, Status};
use uidm::settings::{self, Folders, Settings};
use uidm::notify::{self, Sound, Toast};
use uidm::power;
use uidm::queue::{self, Queue, WhenDone, COUNTDOWN_SECS, MAIN_QUEUE};
use uidm::protocol::Scan;
use uidm::registry;
use uidm::scan;
use uidm::settings::Notify;
use uidm::state::DownloadTable;

/// Exit after this long with no clients and no queued or running downloads.
const IDLE_EXIT: Duration = Duration::from_secs(60);
const IDLE_POLL: Duration = Duration::from_secs(5);
/// Collect changes for this long before writing the history.
const HISTORY_DEBOUNCE: Duration = Duration::from_secs(1);
/// How long a download waits for a window to show its save-as dialog before
/// it starts with the proposed name and folder.
const CONFIRM_FALLBACK: Duration = Duration::from_secs(20);
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
}

/// Work for the notification thread.
enum Note {
    Finished(u64),
    Failed(u64),
    /// A finished download's virus scan is done.
    Scanned(u64),
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
            Outgoing::Done { .. } | Outgoing::Failed { .. } | Outgoing::Canceled { .. }
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
            _ => {}
        }
        let _ = self.dirty.send(());
        self.broadcast(msg);
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
}

/// A countdown before sleeping, hibernating or shutting down.
struct Countdown {
    action: WhenDone,
    queue: String,
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
            wake,
        });
        let events = Arc::clone(&hub);
        let engine = Engine::new(
            Arc::new(move |p: Progress| events.publish(&progress_to_outgoing(p))),
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
        let mut req = to_request(download, &self.settings.lock().unwrap());
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
            }
            q.name = name;
            q.max_concurrent = edited.max_concurrent;
            q.schedule = edited.schedule;
            q.when_done = edited.when_done;
            q.retries = edited.retries;
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
        let now = queue::local_now();

        let queues = self.hub.queues.lock().unwrap().clone();
        for q in &queues {
            let Some(schedule) = &q.schedule else {
                continue;
            };
            if let Some(day) = schedule.open_run(now) {
                if q.last_scheduled_day != Some(day) && !q.running {
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

            let busy = active > 0 || statuses.iter().any(|(_, s)| *s == Status::Waiting);
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
        let Ok((name, action)) = self.hub.update_queue(id, |q| {
            q.running = false;
            q.started_by_schedule = false;
            let action = q.when_done_once.take().unwrap_or(q.when_done);
            Ok((q.name.clone(), action))
        }) else {
            return;
        };
        eprintln!("[uidm-daemon] queue {name} is done");
        if !started.is_empty() {
            self.when_done(action, &name);
        }
    }

    /// Do what a finished queue asks.
    fn when_done(&self, action: WhenDone, queue: &str) {
        match action {
            WhenDone::Nothing => {}
            WhenDone::Exit => {
                eprintln!("[uidm-daemon] queue {queue} done: exiting");
                self.hub.broadcast(&Outgoing::Quit);
                self.save_history();
                // Other downloads may still run; the idle monitor ends the
                // daemon once they are done and every window has closed.
            }
            _ => self.start_countdown(action, queue),
        }
    }

    fn start_countdown(&self, action: WhenDone, queue: &str) {
        let canceled = Arc::new(AtomicBool::new(false));
        let deadline = Instant::now() + Duration::from_secs(COUNTDOWN_SECS as u64);
        if let Some(old) = self.countdown.lock().unwrap().replace(Countdown {
            action,
            queue: queue.to_string(),
            deadline,
            canceled: Arc::clone(&canceled),
        }) {
            old.canceled.store(true, Ordering::Relaxed);
        }
        eprintln!("[uidm-daemon] queue {queue} done: {} in {COUNTDOWN_SECS}s", action.label());
        self.hub.broadcast(&Outgoing::Countdown {
            action,
            seconds: COUNTDOWN_SECS,
            queue: queue.to_string(),
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
            if std::env::var_os(POWER_DRY_RUN_ENV).is_some() {
                eprintln!("[uidm-daemon] dry run: would {}", action.label());
                return;
            }
            eprintln!("[uidm-daemon] {}", action.label());
            if let Err(e) = power::perform(action) {
                eprintln!("[uidm-daemon] could not {}: {e}", action.label());
            }
        });
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
        })
    }

    /// Whether the daemon must stay up for a queue: one is started, one has a
    /// schedule ahead, or a countdown runs.
    fn queues_need_daemon(&self) -> bool {
        let now = queue::local_now();
        self.countdown_message().is_some()
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
        let running: Vec<u64> = self.hub.queues.lock().unwrap().iter().filter(|q| q.running).map(|q| q.id).collect();
        for q in &running {
            let _ = self.stop_queue(*q);
        }
        self.paused_queues.lock().unwrap().extend(running);
        let ids: Vec<u64> = self
            .hub
            .table
            .lock()
            .unwrap()
            .entries()
            .filter(|e| matches!(e.status, Status::Queued | Status::Downloading))
            .map(|e| e.id)
            .collect();
        self.paused.lock().unwrap().extend(ids.iter().copied());
        for id in ids {
            self.engine.cancel(id);
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
            Note::Finished(id) | Note::Scanned(id) => self.notify(id, false, &settings),
            Note::Failed(id) => {
                thread::sleep(FAILED_GRACE);
                if self.entry(id).is_some_and(|e| e.status == Status::Failed) {
                    self.notify(id, true, &settings);
                }
            }
        }
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
        let toast = match settings.notify {
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

    /// Take a new download: ask where to save it when the settings say so,
    /// otherwise queue it at once.
    fn add(self: &Arc<Self>, download: Download) -> (u64, String) {
        if download.start == Some(false) {
            return self.add_later(download);
        }
        if self.settings.lock().unwrap().ask_where_to_save {
            self.ask(download)
        } else {
            self.start(download, None)
        }
    }

    /// Add `download` to a queue (the main one unless it names another)
    /// without starting it or opening a connection.
    fn add_later(&self, download: Download) -> (u64, String) {
        let url = download.url.clone();
        let queue = download
            .queue
            .filter(|q| self.hub.queues.lock().unwrap().iter().any(|x| x.id == *q))
            .unwrap_or(MAIN_QUEUE);
        let id = self.engine.reserve_id();
        self.hub.requests.lock().unwrap().insert(id, download);
        self.hub.publish(&Outgoing::Added {
            id,
            url: url.clone(),
        });
        let _ = self.hub.update(id, |e| {
            e.status = Status::Waiting;
            Ok(())
        });
        let _ = self.hub.update_queue(queue, |q| {
            q.order.push(id);
            Ok(())
        });
        (id, url)
    }

    /// Announce `download` as queued, probe it on another thread, then hold it
    /// as `confirming` with the proposed file until a window answers.
    fn ask(self: &Arc<Self>, download: Download) -> (u64, String) {
        let url = download.url.clone();
        let id = self.engine.reserve_id();
        self.hub.requests.lock().unwrap().insert(id, download.clone());
        self.hub.publish(&Outgoing::Added {
            id,
            url: url.clone(),
        });

        let daemon = Arc::clone(self);
        thread::spawn(move || {
            let (headers, folders) = {
                let settings = daemon.settings.lock().unwrap();
                (to_request(download.clone(), &settings).headers, settings.folders())
            };
            // Probing a POST download would send a GET (or repeat the POST),
            // so its dialog proposes a name from the request alone.
            let probed = if uidm::app::post_body(&download).is_some() {
                Ok(engine::Inspection::unprobed(&download.url, download.filename.as_deref()))
            } else {
                engine::inspect(&download.url, download.filename.as_deref(), &headers)
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
            let path = folders.for_name(&found.filename).join(&found.filename);
            let asked = daemon.hub.update(id, |e| {
                if e.status != Status::Queued {
                    return Err("no longer queued".into());
                }
                e.status = Status::Confirming;
                e.path = Some(path.display().to_string());
                e.total = found.total;
                e.resumable = found.resumable;
                Ok(())
            });
            if asked.is_err() {
                return; // canceled while being probed
            }
            if !daemon.hub.window_open() {
                if let Err(e) = open_window() {
                    eprintln!("[uidm-daemon] could not open the window: {e}");
                }
            }
            thread::sleep(CONFIRM_FALLBACK);
            if daemon.status(id) == Some(Status::Confirming) && !daemon.hub.window_open() {
                eprintln!("[uidm-daemon] no window to confirm #{id}; starting it as proposed");
                if let Err(e) = daemon.confirm(id, None, None, true, None) {
                    eprintln!("[uidm-daemon] could not start #{id}: {e}");
                }
            }
        });
        (id, url)
    }

    /// Answer the save-as dialog of a `confirming` download, or start a
    /// `waiting` one. Missing names and folders keep the proposed ones. With
    /// `start` false it waits in `queue` (the main queue by default).
    fn confirm(
        &self,
        id: u64,
        filename: Option<String>,
        folder: Option<String>,
        start: bool,
        queue: Option<u64>,
    ) -> Result<(), String> {
        let item = self.hub.update(id, |e| {
            if !matches!(e.status, Status::Confirming | Status::Waiting) {
                return Err(format!("download {id} is not waiting to be started"));
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
        if start {
            return self.dispatch(id);
        }
        if self.hub.queue_of(id).is_none() {
            self.move_to_queue(item.id, queue.unwrap_or(MAIN_QUEUE), None)?;
        }
        Ok(())
    }

    /// Cancel a download, whether it waits for its dialog or is in the engine.
    fn cancel(&self, id: u64) {
        if matches!(self.status(id), Some(Status::Confirming | Status::Waiting)) {
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
        let url = download.url.clone();
        let mut req = to_request(download.clone(), &self.settings.lock().unwrap());
        req.reuse_path = reuse_path;
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
                ..Download::default()
            });
        // A retried download keeps its place in its queue.
        let queue = self.hub.queue_of(id);
        self.hub.leave_queues(id);
        self.hub.publish(&Outgoing::Removed { id });
        // Only a download that started has a file of its own to resume. One
        // that never got past its save-as dialog only has a proposed path,
        // which may be somebody else's file.
        let started = entry.connections > 0;
        let reuse_path = entry.path.filter(|_| started).map(PathBuf::from);
        let (new_id, url) = self.start(download, reuse_path);
        if let Some(q) = queue {
            let _ = self.move_to_queue(new_id, q, None);
        }
        Ok((new_id, url))
    }

    /// Validate, save and apply new settings, then tell every client. Running
    /// downloads keep going; new ones use the new values.
    fn set_settings(&self, new: Settings) -> Result<(), String> {
        new.validate()?;
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

    /// Forget a download that is no longer running. Its files stay on disk.
    fn remove(&self, id: u64) -> Result<(), String> {
        match self.status(id) {
            None => return Err(format!("no download with id {id}")),
            Some(s) if !s.is_terminal() => {
                return Err(format!("download {id} is still running; cancel it first"))
            }
            Some(_) => {}
        }
        self.hub.requests.lock().unwrap().remove(&id);
        let in_queue = self.hub.leave_queues(id);
        self.hub.publish(&Outgoing::Removed { id });
        if in_queue {
            self.hub.publish_queues();
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
        for msg in [msg, queues] {
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
    let daemon = Arc::new(daemon);
    spawn_scheduler(Arc::clone(&daemon), wake);
    spawn_history_writer(Arc::clone(&daemon), dirty);
    spawn_notifier(Arc::clone(&daemon), notes);
    if let Err(e) = register_notifications() {
        eprintln!("[uidm-daemon] could not register for notifications: {e}");
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
        Incoming::Cancel { id } => {
            eprintln!("[uidm-daemon] cancel #{id}");
            daemon.cancel(id);
        }
        Incoming::Confirm {
            id,
            filename,
            folder,
            start,
            queue,
        } => match daemon.confirm(id, filename, folder, start, queue) {
            Ok(()) => eprintln!("[uidm-daemon] confirm #{id} (start: {start})"),
            Err(message) => send(&Outgoing::Error { message }),
        },
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
        Incoming::Remove { id } => {
            if let Err(message) = daemon.remove(id) {
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
