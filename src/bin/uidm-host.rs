//! UIDM native-messaging host: the process the browser launches.
//!
//! It is a thin relay. It connects to the daemon (starting it if needed), then
//! copies frames both ways without parsing them: browser stdin -> daemon, and
//! daemon -> browser stdout. Both sides use the same 4-byte little-endian
//! length prefix plus UTF-8 JSON, so a frame passes through unchanged.
//!
//! When the browser closes the port the host exits, but the daemon and its
//! downloads keep running.
//!
//! stdout carries protocol frames only; logs go to stderr, which the browser
//! captures.
//!
//! A CLI test mode runs a download in-process, without a browser or daemon:
//!   uidm-host download <url> [output_dir]

use std::io::{self, Write};
use std::net::Shutdown;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use uidm::app::MAX_MESSAGE_BYTES;

/// After the browser closes, how long to wait for the daemon to deliver
/// replies already in flight before giving up on them.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
use uidm::engine::{self, DownloadRequest, Progress};
use uidm::ipc;
use uidm::protocol::{read_message, write_frame};
use uidm::settings::{default_downloads_dir, FileExists, Folders, DEFAULT_CONNECTIONS};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("download") {
        std::process::exit(run_cli(&args));
    }
    if let Err(e) = run_relay() {
        eprintln!("[uidm-host] fatal: {e}");
        std::process::exit(1);
    }
}

/// Relay frames between the browser (stdio) and the daemon (loopback TCP).
fn run_relay() -> io::Result<()> {
    let daemon = ipc::connect_or_spawn()?;
    eprintln!("[uidm-host] connected to daemon at {}", ipc::DAEMON_ADDR);
    let mut to_daemon = daemon.try_clone()?;
    let mut from_daemon = daemon;

    // daemon -> browser
    let downstream = thread::spawn(move || {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        while let Ok(Some(frame)) = read_message(&mut from_daemon, MAX_MESSAGE_BYTES) {
            if write_frame(&mut out, &frame).is_err() || out.flush().is_err() {
                break;
            }
        }
    });

    // browser -> daemon
    let stdin = io::stdin();
    let mut input = stdin.lock();
    loop {
        match read_message(&mut input, MAX_MESSAGE_BYTES)? {
            Some(frame) => {
                if write_frame(&mut to_daemon, &frame).is_err() {
                    eprintln!("[uidm-host] daemon connection lost");
                    break;
                }
            }
            None => {
                eprintln!("[uidm-host] stdin closed, exiting");
                break;
            }
        }
    }

    // Close only our sending side. The daemon sees EOF, flushes any reply still
    // queued for this client, and closes its end, which ends the downstream
    // thread. If that takes longer than DRAIN_TIMEOUT, force the socket shut.
    // Either way the daemon's downloads are unaffected.
    let _ = to_daemon.shutdown(Shutdown::Write);
    let deadline = Instant::now() + DRAIN_TIMEOUT;
    while !downstream.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    let _ = to_daemon.shutdown(Shutdown::Both);
    let _ = downstream.join();
    Ok(())
}

/// CLI test mode: download a URL in-process and print progress to stderr.
/// Returns a process exit code.
fn run_cli(args: &[String]) -> i32 {
    let url = match args.get(2) {
        Some(u) => u.clone(),
        None => {
            eprintln!("usage: uidm-host download <url> [output_dir]");
            return 2;
        }
    };
    let output_dir = args.get(3).map(PathBuf::from).unwrap_or_else(default_downloads_dir);
    eprintln!("[uidm-host] downloading {url} -> {}", output_dir.display());

    let ok = Arc::new(AtomicBool::new(false));
    let ok_cb = Arc::clone(&ok);
    engine::run_blocking(
        DownloadRequest {
            url,
            folders: Folders::fixed(output_dir),
            filename: None,
            headers: Vec::new(),
            connections: DEFAULT_CONNECTIONS,
            reuse_path: None,
            file_exists: FileExists::Rename,
            post_body: None,
            net: Default::default(),
            media: None,
            ffmpeg: None,
            speed_limit: None,
            server_date: true,
            temp_dir: None,
            reuse_partial: None,
            host_limit: None,
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
        Progress::Advanced { done, total, .. } => {
            match total {
                Some(t) if t > 0 => eprint!("\r[progress] {done}/{t} ({}%)   ", done * 100 / t),
                _ => eprint!("\r[progress] {done} bytes   "),
            }
            let _ = io::stderr().flush();
        }
        Progress::Finished { path, .. } => {
            ok.store(true, Ordering::Relaxed);
            eprintln!("\n[done] saved to {}", path.display());
        }
        Progress::Failed { error, .. } => eprintln!("\n[failed] {error}"),
        Progress::Canceled { .. } => eprintln!("\n[canceled]"),
        Progress::NameTaken { path, .. } => eprintln!("\n[failed] {} already exists", path.display()),
    }
}
