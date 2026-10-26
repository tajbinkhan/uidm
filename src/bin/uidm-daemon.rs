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

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::net::{TcpListener, TcpStream};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
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
        table.apply(msg);
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
}

impl Daemon {
    /// Load settings, history and saved requests, and start the engine with
    /// them. Returns the receivers the history writer and the notification
    /// thread wait on.
    fn new(paths: Paths) -> (Self, Receiver<()>, Receiver<Note>) {
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
        table.apply(&Outgoing::Downloads {
            items: history.items,
        });
        requests.retain(|id, _| table.get(*id).is_some_and(|e| e.status.is_retryable()));

        let (dirty, dirty_rx) = channel();
        let (notes, notes_rx) = channel();
        let hub = Arc::new(Hub {
            clients: Mutex::new(HashMap::new()),
            table: Mutex::new(table),
            requests: Mutex::new(requests),
            dirty,
            windows: Mutex::new(HashSet::new()),
            notes,
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
        };
        (daemon, dirty_rx, notes_rx)
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
        if self.settings.lock().unwrap().ask_where_to_save {
            self.ask(download)
        } else {
            self.start(download, None)
        }
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
            let found = match engine::inspect(&download.url, download.filename.as_deref(), &headers) {
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
                if let Err(e) = daemon.confirm(id, None, None, true) {
                    eprintln!("[uidm-daemon] could not start #{id}: {e}");
                }
            }
        });
        (id, url)
    }

    /// Answer the save-as dialog of a `confirming` download, or start a
    /// `waiting` one. Missing names and folders keep the proposed ones.
    fn confirm(
        &self,
        id: u64,
        filename: Option<String>,
        folder: Option<String>,
        start: bool,
    ) -> Result<(), String> {
        let download = self
            .hub
            .requests
            .lock()
            .unwrap()
            .get(&id)
            .cloned();
        let mut chosen: Option<(String, PathBuf)> = None;
        let item = self.hub.update(id, |e| {
            if !matches!(e.status, Status::Confirming | Status::Waiting) {
                return Err(format!("download {id} is not waiting to be started"));
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
            e.status = if start { Status::Queued } else { Status::Waiting };
            chosen = Some((name, dir));
            Ok(())
        })?;
        let Some((name, dir)) = chosen else {
            return Ok(());
        };
        if start {
            let download = download.unwrap_or_else(|| Download {
                url: item.url.clone(),
                ..Download::default()
            });
            let mut req = to_request(download, &self.settings.lock().unwrap());
            req.filename = Some(name);
            req.folders = Folders::fixed(dir);
            self.engine.enqueue_as(id, req);
        }
        Ok(())
    }

    /// Cancel a download, whether it waits for its dialog or is in the engine.
    fn cancel(&self, id: u64) {
        if self.status(id) == Some(Status::Confirming) {
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
        self.hub.publish(&Outgoing::Removed { id });
        // Only a download that started has a file of its own to resume. One
        // that never got past its save-as dialog only has a proposed path,
        // which may be somebody else's file.
        let started = entry.connections > 0;
        let reuse_path = entry.path.filter(|_| started).map(PathBuf::from);
        Ok(self.start(download, reuse_path))
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
        self.hub.publish(&Outgoing::Removed { id });
        Ok(())
    }

    /// Send the whole list to one client, under the table lock so no event can
    /// slip between the snapshot and the client's next frame.
    fn send_list(&self, reply: &Sender<Vec<u8>>) {
        let table = self.hub.table.lock().unwrap();
        let msg = Outgoing::Downloads {
            items: table.snapshot(),
        };
        if let Ok(frame) = serde_json::to_vec(&msg) {
            let _ = reply.send(frame);
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

    let (daemon, dirty, notes) = Daemon::new(Paths::from_env());
    let daemon = Arc::new(daemon);
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
            }
            send(&Outgoing::Hello {
                name: "uidm-daemon".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            })
        }
        Incoming::Ping => send(&Outgoing::Pong),
        Incoming::Add(download) => {
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
        } => match daemon.confirm(id, filename, folder, start) {
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
                && daemon.engine.active_count() == 0;
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
