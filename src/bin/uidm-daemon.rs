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

use std::collections::HashMap;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use uidm::app::{progress_to_outgoing, to_request, MAX_MESSAGE_BYTES};
use uidm::engine::{Engine, Progress};
use uidm::history::{self, History};
use uidm::ipc::{self, Handshake, AUTH_TIMEOUT, DAEMON_ADDR, DAEMON_LABEL, HOST_LABEL};
use uidm::protocol::{read_message, write_frame, Download, Incoming, Outgoing, Status};
use uidm::settings::{self, Settings};
use uidm::state::DownloadTable;

/// Exit after this long with no clients and no queued or running downloads.
const IDLE_EXIT: Duration = Duration::from_secs(60);
const IDLE_POLL: Duration = Duration::from_secs(5);
/// Collect changes for this long before writing the history.
const HISTORY_DEBOUNCE: Duration = Duration::from_secs(1);

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
}

impl Hub {
    /// Apply `msg` to the table and send it to every client. The table lock is
    /// held across the send, so the order clients see matches the table's, and
    /// a `downloads` snapshot (taken under the same lock) is never stale.
    fn publish(&self, msg: &Outgoing) {
        let mut table = self.table.lock().unwrap();
        table.apply(msg);
        if let Outgoing::Done { id, .. } = msg {
            self.requests.lock().unwrap().remove(id);
        }
        let _ = self.dirty.send(());
        self.broadcast(msg);
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
}

impl Daemon {
    /// Load settings, history and saved requests, and start the engine with
    /// them. Returns the receiver the history writer waits on.
    fn new(paths: Paths) -> (Self, Receiver<()>) {
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
        let hub = Arc::new(Hub {
            clients: Mutex::new(HashMap::new()),
            table: Mutex::new(table),
            requests: Mutex::new(requests),
            dirty,
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
        };
        (daemon, dirty_rx)
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
        Ok(self.start(download, entry.path.map(PathBuf::from)))
    }

    /// Validate, save and apply new settings, then tell every client. Running
    /// downloads keep going; new ones use the new values.
    fn set_settings(&self, new: Settings) -> Result<(), String> {
        new.validate()?;
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

    let (daemon, dirty) = Daemon::new(Paths::from_env());
    let daemon = Arc::new(daemon);
    spawn_history_writer(Arc::clone(&daemon), dirty);
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
fn handle_client(mut stream: TcpStream, daemon: &Daemon, token: &str) {
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
            Ok(Some(raw)) => dispatch(&raw, daemon, &tx),
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

/// Handle one request frame, sending any direct reply to `reply`.
fn dispatch(raw: &[u8], daemon: &Daemon, reply: &Sender<Vec<u8>>) {
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
        Incoming::Hello { .. } => send(&Outgoing::Hello {
            name: "uidm-daemon".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }),
        Incoming::Ping => send(&Outgoing::Pong),
        Incoming::Add(download) => {
            let (id, url) = daemon.start(download, None);
            eprintln!("[uidm-daemon] add #{id}: {url}");
            send(&Outgoing::Ack {
                id,
                url,
                status: "accepted".to_string(),
            });
        }
        Incoming::Cancel { id } => {
            eprintln!("[uidm-daemon] cancel #{id}");
            daemon.engine.cancel(id);
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
        Incoming::Remove { id } => {
            if let Err(message) = daemon.remove(id) {
                send(&Outgoing::Error { message });
            }
        }
        Incoming::List => daemon.send_list(reply),
        Incoming::GetSettings => send(&Outgoing::Settings {
            settings: daemon.settings.lock().unwrap().clone(),
        }),
        Incoming::SetSettings { settings } => match daemon.set_settings(settings) {
            Ok(()) => eprintln!("[uidm-daemon] settings changed"),
            Err(message) => send(&Outgoing::Error { message }),
        },
    }
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
