//! UIDM native-messaging host and download launcher.
//!
//! Default mode speaks Chrome's native-messaging protocol over stdio: each
//! message is a 4-byte little-endian length prefix followed by that many bytes
//! of UTF-8 JSON. The host reads requests from stdin and writes replies and
//! download progress to stdout using the same framing.
//!
//! stdout carries protocol frames only; a dedicated writer thread owns it so
//! replies and background progress events never interleave. Human-readable logs
//! go to stderr, which the browser captures.
//!
//! A CLI test mode runs a download directly without a browser:
//!   uidm-host download <url> [output_dir]

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

mod engine;
mod protocol;

use engine::{DownloadRequest, Engine, Progress};
use protocol::{read_message, write_message, Download, Incoming, Outgoing};

/// Chrome caps a host message at 1 MiB each way; we allow a bit more on read so
/// an oversized frame fails cleanly instead of parsing a truncated body.
const MAX_MESSAGE_BYTES: u32 = 4 * 1024 * 1024;
/// Parallel connections per download unless a request overrides it.
const DEFAULT_CONNECTIONS: usize = 8;
/// How many downloads run at once; the rest queue.
const MAX_CONCURRENT_DOWNLOADS: usize = 3;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("download") {
        std::process::exit(run_cli(&args));
    }
    if let Err(e) = run_host() {
        eprintln!("[uidm-host] fatal: {e}");
        std::process::exit(1);
    }
}

/// Native-messaging mode: read frames from stdin, dispatch, reply on stdout.
fn run_host() -> io::Result<()> {
    // One writer thread owns stdout. Replies from the read loop and progress
    // from engine threads both flow through this channel, so frames stay whole.
    let (tx, rx) = std::sync::mpsc::channel::<Outgoing>();
    let writer = thread::spawn(move || {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        while let Ok(msg) = rx.recv() {
            if write_message(&mut out, &msg).is_err() || out.flush().is_err() {
                break;
            }
        }
    });

    // The engine reports progress by converting events to protocol messages and
    // sending them on the same channel.
    let progress_tx = tx.clone();
    let engine = Engine::new(
        Arc::new(move |p: Progress| {
            let _ = progress_tx.send(progress_to_outgoing(p));
        }),
        MAX_CONCURRENT_DOWNLOADS,
    );

    let stdin = io::stdin();
    let mut reader = stdin.lock();
    eprintln!("[uidm-host] started, waiting for messages");

    loop {
        match read_message(&mut reader, MAX_MESSAGE_BYTES)? {
            None => {
                eprintln!("[uidm-host] stdin closed, exiting");
                break;
            }
            Some(raw) => {
                if let Some(reply) = handle(&raw, &engine) {
                    if tx.send(reply).is_err() {
                        break;
                    }
                }
            }
        }
    }

    // On EOF the browser is gone. Dropping the engine closes the job channel so
    // its workers exit, and dropping the senders lets the process wind down.
    // We do not join here: a worker blocked on a live download would otherwise
    // hold the process open with nothing left to deliver to.
    drop(engine);
    drop(tx);
    let _ = writer; // detach the writer thread; process exit reclaims it.
    Ok(())
}

/// Turn one raw JSON frame into an optional reply. A frame we cannot parse gets
/// an error reply rather than killing the connection. A `cancel` has no direct
/// reply; its `Canceled` event arrives from the engine.
fn handle(raw: &[u8], engine: &Engine) -> Option<Outgoing> {
    let incoming: Incoming = match serde_json::from_slice(raw) {
        Ok(msg) => msg,
        Err(e) => {
            eprintln!("[uidm-host] bad message: {e}");
            return Some(Outgoing::Error {
                message: format!("invalid message: {e}"),
            });
        }
    };

    match incoming {
        Incoming::Hello { version } => {
            eprintln!("[uidm-host] hello from extension (version {version:?})");
            Some(Outgoing::Hello {
                name: env!("CARGO_PKG_NAME").to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            })
        }
        Incoming::Ping => Some(Outgoing::Pong),
        Incoming::Add(download) => {
            let url = download.url.clone();
            let id = engine.enqueue(to_request(download));
            eprintln!("[uidm-host] add #{id}: {url}");
            // Progress, Done or Failed follow asynchronously over the channel.
            Some(Outgoing::Ack {
                id,
                url,
                status: "accepted".to_string(),
            })
        }
        Incoming::Cancel { id } => {
            eprintln!("[uidm-host] cancel #{id}");
            engine.cancel(id);
            None
        }
    }
}

/// Build an engine request from an `add` message: pick the output directory and
/// fold the browser context (user agent, cookies, referrer) into headers.
fn to_request(d: Download) -> DownloadRequest {
    let mut headers: Vec<(String, String)> = d.headers.into_iter().collect();
    if let Some(ua) = d.user_agent {
        headers.push(("User-Agent".to_string(), ua));
    }
    if let Some(cookies) = d.cookies {
        headers.push(("Cookie".to_string(), cookies));
    }
    if let Some(referrer) = d.referrer {
        headers.push(("Referer".to_string(), referrer));
    }
    DownloadRequest {
        url: d.url,
        output_dir: downloads_dir(),
        filename: d.filename,
        headers,
        connections: DEFAULT_CONNECTIONS,
    }
}

fn progress_to_outgoing(p: Progress) -> Outgoing {
    match p {
        Progress::Started {
            id,
            url,
            total,
            resumable,
            connections,
        } => Outgoing::Started {
            id,
            url,
            total,
            resumable,
            connections,
        },
        Progress::Advanced { id, url, done, total } => Outgoing::Progress { id, url, done, total },
        Progress::Finished { id, url, path } => Outgoing::Done {
            id,
            url,
            path: path.display().to_string(),
        },
        Progress::Failed { id, url, error } => Outgoing::Failed { id, url, error },
        Progress::Canceled { id, url } => Outgoing::Canceled { id, url },
    }
}

/// The current user's Downloads folder, or the temp dir if it can't be found.
fn downloads_dir() -> PathBuf {
    if let Ok(profile) = std::env::var("USERPROFILE") {
        let dir = PathBuf::from(profile).join("Downloads");
        if dir.is_dir() {
            return dir;
        }
    }
    std::env::temp_dir()
}

/// CLI test mode: download a URL and print progress to stderr. Returns a
/// process exit code.
fn run_cli(args: &[String]) -> i32 {
    let url = match args.get(2) {
        Some(u) => u.clone(),
        None => {
            eprintln!("usage: uidm-host download <url> [output_dir]");
            return 2;
        }
    };
    let output_dir = args.get(3).map(PathBuf::from).unwrap_or_else(downloads_dir);
    eprintln!("[uidm-host] downloading {url} -> {}", output_dir.display());

    let ok = Arc::new(AtomicBool::new(false));
    let ok_cb = Arc::clone(&ok);
    engine::run_blocking(
        DownloadRequest {
            url,
            output_dir,
            filename: None,
            headers: Vec::new(),
            connections: DEFAULT_CONNECTIONS,
        },
        Arc::new(move |p: Progress| print_progress(p, &ok_cb)),
    );

    if ok.load(Ordering::Relaxed) {
        0
    } else {
        1
    }
}

fn print_progress(p: Progress, ok: &AtomicBool) {
    match p {
        Progress::Started {
            total,
            resumable,
            connections,
            ..
        } => {
            let size = total.map(|t| format!("{t} bytes")).unwrap_or_else(|| "unknown size".into());
            eprintln!("[started] {size}, resumable={resumable}, connections={connections}");
        }
        Progress::Advanced { done, total, .. } => match total {
            Some(t) if t > 0 => {
                eprint!("\r[progress] {done}/{t} ({}%)   ", done * 100 / t);
                let _ = io::stderr().flush();
            }
            _ => {
                eprint!("\r[progress] {done} bytes   ");
                let _ = io::stderr().flush();
            }
        },
        Progress::Finished { path, .. } => {
            ok.store(true, Ordering::Relaxed);
            eprintln!("\n[done] saved to {}", path.display());
        }
        Progress::Failed { error, .. } => {
            eprintln!("\n[failed] {error}");
        }
        Progress::Canceled { .. } => {
            eprintln!("\n[canceled]");
        }
    }
}
