//! UIDM daemon: the long-lived process that owns the download engine.
//!
//! Hosts (one per browser connection) connect over loopback TCP and speak the
//! same length-prefixed JSON frames as the browser. Each connection starts
//! with a mutual handshake over the token this daemon wrote at start (see
//! `uidm::ipc`); until the client passes it, it gets no events and its
//! requests are not read. Replies to a request
//! (`hello`, `pong`, `ack`, `error`) go to the client that asked. Download
//! events (`started`, `progress`, `done`, `failed`, `canceled`) are broadcast
//! to every connected client.
//!
//! Because downloads live here rather than in the host, they keep running when
//! the browser closes. The daemon exits on its own once it has had no clients
//! and no downloads for `IDLE_EXIT`.

use std::collections::HashMap;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use uidm::app::{progress_to_outgoing, to_request, MAX_CONCURRENT_DOWNLOADS, MAX_MESSAGE_BYTES};
use uidm::engine::{Engine, Progress};
use uidm::ipc::{self, Handshake, AUTH_TIMEOUT, DAEMON_ADDR, DAEMON_LABEL, HOST_LABEL};
use uidm::protocol::{read_message, write_frame, Incoming, Outgoing};

/// Exit after this long with no clients and no queued or running downloads.
const IDLE_EXIT: Duration = Duration::from_secs(60);
const IDLE_POLL: Duration = Duration::from_secs(5);

/// Connected clients by id, each with a channel drained by its writer thread.
type Clients = Arc<Mutex<HashMap<u64, Sender<Vec<u8>>>>>;

static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

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
            eprintln!("[uidm-daemon] {DAEMON_ADDR} unavailable ({e}); another daemon is likely running");
            return Ok(());
        }
    };
    eprintln!("[uidm-daemon] listening on {DAEMON_ADDR}");

    // Publish the token only after winning the bind. A daemon that lost the
    // bind must not overwrite the running daemon's token.
    let token = ipc::new_token()?;
    ipc::write_token(&token)?;
    let token: Arc<str> = token.into();

    let clients: Clients = Arc::new(Mutex::new(HashMap::new()));

    let engine_clients = Arc::clone(&clients);
    let engine = Arc::new(Engine::new(
        Arc::new(move |p: Progress| {
            if let Ok(frame) = serde_json::to_vec(&progress_to_outgoing(p)) {
                broadcast(&engine_clients, frame);
            }
        }),
        MAX_CONCURRENT_DOWNLOADS,
    ));

    spawn_idle_monitor(Arc::clone(&clients), Arc::clone(&engine));

    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let engine = Arc::clone(&engine);
                let clients = Arc::clone(&clients);
                let token = Arc::clone(&token);
                thread::spawn(move || handle_client(stream, engine, clients, &token));
            }
            Err(e) => eprintln!("[uidm-daemon] accept failed: {e}"),
        }
    }
    Ok(())
}

/// Send one encoded frame to every client, dropping any whose writer has gone.
fn broadcast(clients: &Clients, frame: Vec<u8>) {
    let mut map = clients.lock().unwrap();
    map.retain(|_, tx| tx.send(frame.clone()).is_ok());
}

/// Serve one client until it disconnects. The client is registered for
/// broadcasts only after it authenticates.
fn handle_client(mut stream: TcpStream, engine: Arc<Engine>, clients: Clients, token: &str) {
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
    clients.lock().unwrap().insert(id, tx.clone());

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
            Ok(Some(raw)) => {
                if let Some(reply) = dispatch(&raw, &engine) {
                    if let Ok(frame) = serde_json::to_vec(&reply) {
                        if tx.send(frame).is_err() {
                            break;
                        }
                    }
                }
            }
            Ok(None) => break, // clean disconnect
            Err(e) => {
                eprintln!("[uidm-daemon] client #{id} read error: {e}");
                break;
            }
        }
    }

    // Removing the map's sender and dropping ours leaves the writer's channel
    // with no senders, so it ends. Downloads this client started keep running.
    clients.lock().unwrap().remove(&id);
    drop(tx);
    let _ = writer.join();
    eprintln!("[uidm-daemon] client #{id} disconnected");
}

/// Run the daemon side of the handshake: answer the client's nonce with ours
/// and a proof that we hold the token, then check the client's proof and reply
/// `auth_ok`. Each read waits at most `AUTH_TIMEOUT`. Dropping the stream on
/// error closes the connection.
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

/// Handle one request frame. Returns the direct reply, if any.
fn dispatch(raw: &[u8], engine: &Engine) -> Option<Outgoing> {
    let incoming: Incoming = match serde_json::from_slice(raw) {
        Ok(msg) => msg,
        Err(e) => {
            return Some(Outgoing::Error {
                message: format!("invalid message: {e}"),
            })
        }
    };

    match incoming {
        Incoming::Hello { .. } => Some(Outgoing::Hello {
            name: "uidm-daemon".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }),
        Incoming::Ping => Some(Outgoing::Pong),
        Incoming::Add(download) => {
            let url = download.url.clone();
            let id = engine.enqueue(to_request(download));
            eprintln!("[uidm-daemon] add #{id}: {url}");
            Some(Outgoing::Ack {
                id,
                url,
                status: "accepted".to_string(),
            })
        }
        Incoming::Cancel { id } => {
            eprintln!("[uidm-daemon] cancel #{id}");
            engine.cancel(id);
            None
        }
    }
}

/// Exit the process once it has been idle (no clients, no downloads) for
/// `IDLE_EXIT`, so a daemon started by the browser does not linger forever.
fn spawn_idle_monitor(clients: Clients, engine: Arc<Engine>) {
    thread::spawn(move || {
        let mut idle_since: Option<Instant> = None;
        loop {
            thread::sleep(IDLE_POLL);
            let idle = clients.lock().unwrap().is_empty() && engine.active_count() == 0;
            match (idle, idle_since) {
                (true, None) => idle_since = Some(Instant::now()),
                (true, Some(t)) if t.elapsed() >= IDLE_EXIT => {
                    eprintln!("[uidm-daemon] idle for {}s, exiting", IDLE_EXIT.as_secs());
                    std::process::exit(0);
                }
                (false, _) => idle_since = None,
                _ => {}
            }
        }
    });
}
