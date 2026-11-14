//! UDM native-messaging host: the process the browser launches.
//!
//! It is a thin relay. It connects to the daemon (starting it if needed), then
//! copies frames both ways without parsing them: browser stdin -> daemon, and
//! daemon -> browser stdout. Both sides use the same 4-byte little-endian
//! length prefix plus UTF-8 JSON, so a frame passes through unchanged.
//!
//! When the browser closes the port the host exits, but the daemon and its
//! downloads keep running.
//!
//! stdout carries protocol frames only; logs go to `udm-host.log` in the
//! data folder (see `udm_infrastructure::logging`).
//!
//! A CLI test mode runs a download in-process, without a browser or daemon:
//!   udm-host download <url> [output_dir]
//!
//! Other programs send links to UDM with
//!   udm-host send <file.json | ->
//! which reads one link, or a list of links for the link picker, as JSON
//! (see `udm_infrastructure::cli::parse_sent`) and hands it to the daemon, starting it if
//! needed. It goes through the same authenticated connection as the relay.
//!
//! Safari's app extension (`installer/macos/safari`) runs
//!   udm-host safari
//! a relay that does not start the daemon: it runs inside Safari's sandbox,
//! and a daemon it started would be sandboxed too. Without a running daemon
//! it exits with the reason on stderr, which the app extension passes on.

// Tests may use todo!, unimplemented! and unreachable!, which clippy.toml's
// test exemptions do not cover (AGENTS.md section 9).
#![cfg_attr(test, allow(clippy::todo, clippy::unimplemented, clippy::unreachable))]

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use udm_infrastructure::app::MAX_MESSAGE_BYTES;

/// After the browser closes, how long to wait for the daemon to deliver
/// replies already in flight before giving up on them.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
use udm_domain::id::DownloadId;
use udm_domain::settings::{FileExists, Folders, DEFAULT_CONNECTIONS};
use udm_infrastructure::cli::Sent;
use udm_infrastructure::engine::{self, DownloadRequest, Progress};
use udm_infrastructure::ipc;
use udm_infrastructure::logging;
use udm_infrastructure::protocol::{read_message, write_frame, Incoming, Outgoing};
use udm_infrastructure::settings::default_downloads_dir;

/// The requests UDM's browser extension sends. The host passes only these
/// on: anything else from the browser, which an extension that is not
/// UDM's could send, never reaches the daemon.
const BROWSER_OPS: &[&str] = &[
    "hello",
    "list",
    "get_settings",
    "set_settings",
    "add",
    "confirm",
    "cancel",
    "retry",
    "remove",
    "refresh_link",
    "cancel_refresh",
    "probe_media",
    "pick_links",
    "pick_videos",
    "pick_gallery",
    "rendered",
    "cookies",
];

/// A frame from the browser as it may go to the daemon, or `None` to drop
/// it. A `hello` loses any `client`, so a browser connection is never taken
/// for the desktop window, which may change every setting.
fn from_browser(frame: &[u8]) -> Option<Vec<u8>> {
    let mut msg: serde_json::Value = serde_json::from_slice(frame).ok()?;
    let op = msg.get("op")?.as_str()?;
    if !BROWSER_OPS.contains(&op) {
        return None;
    }
    if op == "hello" {
        msg.as_object_mut()?.remove("client");
    }
    serde_json::to_vec(&msg).ok()
}

fn main() {
    // Before the log, which would make UDM's data folder first.
    let moved = udm_infrastructure::from_old_names::move_from_old_names();
    logging::init(logging::Program::Host);
    // A panic leaves a report in the data folder (`crash`).
    udm_infrastructure::crash::install(logging::Program::Host);
    moved.log();
    let args: Vec<String> = std::env::args().collect();
    // The test and `send` modes print to stdout and stderr on purpose: that
    // is their output, for the person or program that ran them.
    let spawn = match mode(&args) {
        Mode::Download => std::process::exit(run_cli(&args)),
        Mode::Send => std::process::exit(run_send(&args)),
        Mode::Relay { spawn } => spawn,
    };
    if let Err(e) = run_relay(spawn) {
        tracing::error!(error = format!("{e:#}"), "fatal");
        std::process::exit(1);
    }
}

/// What the host was started to do.
#[derive(Debug, PartialEq, Eq)]
enum Mode {
    /// Relay a browser's frames, starting the daemon when `spawn` and it is
    /// not running.
    Relay { spawn: bool },
    /// `udm-host download <url> [output_dir]`.
    Download,
    /// `udm-host send <file | ->`.
    Send,
}

/// The mode the command line `args` (the program first) asks for. Chrome
/// and Edge pass the extension's origin and Firefox the host manifest's
/// path: anything but a mode's name is a browser's relay.
fn mode(args: &[String]) -> Mode {
    match args.get(1).map(String::as_str) {
        Some("download") => Mode::Download,
        Some("send") => Mode::Send,
        Some("safari") => Mode::Relay { spawn: false },
        _ => Mode::Relay { spawn: true },
    }
}

/// Relay frames between the browser (stdio) and the daemon (loopback TCP),
/// starting the daemon first when `spawn` and it is not running.
fn run_relay(spawn: bool) -> anyhow::Result<()> {
    use anyhow::Context as _;
    let daemon =
        if spawn { ipc::connect_or_spawn() } else { ipc::connect_running() }.context("could not reach the daemon")?;
    tracing::info!(addr = %ipc::daemon_addr(), "connected to the daemon");
    let mut to_daemon = daemon.try_clone().context("could not share the daemon connection")?;
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
        match read_message(&mut input, MAX_MESSAGE_BYTES).context("could not read from the browser")? {
            Some(frame) => {
                let Some(frame) = from_browser(&frame) else {
                    tracing::warn!("dropped a request the extension does not send");
                    continue;
                };
                if write_frame(&mut to_daemon, &frame).is_err() {
                    tracing::warn!("daemon connection lost");
                    break;
                }
            }
            None => {
                tracing::info!("stdin closed, exiting");
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

/// How long `send` waits for the daemon's answer once connected.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// `udm-host send <file | ->`: hand the JSON in the file (or on stdin) to
/// the daemon. One link is added as the extension adds one, and its id goes
/// to stdout; a list opens the link picker. Returns a process exit code: 0
/// when the daemon took it, 1 when it refused or could not be reached, 2 for
/// bad input.
fn run_send(args: &[String]) -> i32 {
    let source = match args.get(2) {
        Some(s) if args.len() == 3 => s,
        _ => {
            eprintln!("usage: udm-host send <file.json | ->");
            return 2;
        }
    };
    let json = match read_input(source) {
        Ok(json) => json,
        Err(e) => {
            eprintln!("[udm-host] could not read {source}: {e}");
            return 2;
        }
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let sent = match udm_infrastructure::cli::parse_sent(&json, &cwd) {
        Ok(sent) => sent,
        Err(e) => {
            eprintln!("[udm-host] {e}");
            return 2;
        }
    };
    match send(sent) {
        Ok(Some(id)) => {
            println!("{id}");
            0
        }
        Ok(None) => 0,
        Err(e) => {
            eprintln!("[udm-host] {e}");
            1
        }
    }
}

/// The text of `source`, a file or `-` for stdin, up to the size of one
/// message.
fn read_input(source: &str) -> io::Result<String> {
    let limit = u64::from(MAX_MESSAGE_BYTES);
    let mut text = String::new();
    if source == "-" {
        io::stdin().lock().take(limit + 1).read_to_string(&mut text)?;
    } else {
        std::fs::File::open(source)?.take(limit + 1).read_to_string(&mut text)?;
    }
    if text.len() as u64 > limit {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("larger than {limit} bytes")));
    }
    // A file saved by Windows PowerShell 5 or Notepad may start with a BOM.
    Ok(text.trim_start_matches('\u{feff}').to_string())
}

/// Give `sent` to the daemon and wait for its answer: the new download's id
/// for one link, `None` once a list has reached the picker.
fn send(sent: Sent) -> io::Result<Option<DownloadId>> {
    let mut daemon = ipc::connect_or_spawn()?;
    daemon.set_read_timeout(Some(SEND_TIMEOUT))?;
    let hello =
        Incoming::Hello { version: Some(env!("CARGO_PKG_VERSION").to_string()), client: Some("program".into()) };
    let one = matches!(sent, Sent::One(_));
    let request = match sent {
        Sent::One(download) => Incoming::Add(download),
        Sent::Many { referer, links } => Incoming::PickLinks { page: referer, links, user_agent: None },
    };
    // `pick_links` has no answer, so a ping after it shows the daemon has
    // handled it: it reads a client's requests in order.
    let mut messages = vec![hello, request];
    if !one {
        messages.push(Incoming::Ping);
    }
    for msg in &messages {
        write_frame(&mut daemon, &serde_json::to_vec(msg)?)?;
    }
    loop {
        let raw = read_message(&mut daemon, MAX_MESSAGE_BYTES)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "the daemon closed the connection"))?;
        // Other clients' news comes too; only the answers matter here.
        match serde_json::from_slice::<Outgoing>(&raw) {
            Ok(Outgoing::Ack { id, .. }) if one => return Ok(Some(id)),
            Ok(Outgoing::Pong) if !one => return Ok(None),
            Ok(Outgoing::Rejected { message, .. } | Outgoing::Error { message, .. }) => {
                return Err(io::Error::other(message))
            }
            _ => {}
        }
    }
}

/// CLI test mode: download a URL in-process and print progress to stderr.
/// Returns a process exit code.
fn run_cli(args: &[String]) -> i32 {
    let url = match args.get(2) {
        Some(u) => u.clone(),
        None => {
            eprintln!("usage: udm-host download <url> [output_dir]");
            return 2;
        }
    };
    let output_dir = args.get(3).map(PathBuf::from).unwrap_or_else(default_downloads_dir);
    eprintln!("[udm-host] downloading {url} -> {}", output_dir.display());

    // No proxy or login, as ever in this test mode, but the network adapter
    // and DNS the settings choose, so it never goes out another way.
    let settings =
        match udm_infrastructure::settings::settings_path().and_then(|p| udm_infrastructure::settings::load(&p)) {
            Ok(settings) => settings,
            Err(e) => {
                eprintln!("[udm-host] the settings could not be read: {e}");
                return 1;
            }
        };
    let route = udm_infrastructure::connect::Network::default().route(&settings);
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
            net: Arc::new(udm_infrastructure::net::NetConfig { route, ..Default::default() }),
            media: None,
            ffmpeg: None,
            speed_limit: None,
            server_date: true,
            server_date_created: false,
            ignore_date: false,
            temp_dir: None,
            reuse_partial: None,
            host_limit: None,
            elevate: false,
            resolve: None,
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
        Progress::Started { total, resumable, connections, .. } => {
            let size = total.map(|t| format!("{t} bytes")).unwrap_or_else(|| "unknown size".into());
            eprintln!("[started] {size}, resumable={resumable}, connections={connections}");
        }
        Progress::Advanced { done, total, .. } => {
            match total {
                Some(t) if !t.is_zero() => {
                    eprint!("\r[progress] {done}/{t} ({}%)   ", done.get().saturating_mul(100) / t.get())
                }
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

#[cfg(test)]
mod tests {
    use super::{from_browser, mode, Mode};

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn each_browser_gets_a_relay_and_only_safari_one_that_starts_no_daemon() {
        // Chrome and Edge pass the extension's origin (and on Windows a
        // parent window), Firefox the manifest's path and the extension's ID.
        let chrome = args(&["udm-host", "chrome-extension://abc/", "--parent-window=0"]);
        assert_eq!(mode(&chrome), Mode::Relay { spawn: true });
        let firefox = args(&["udm-host", "/x/com.udm.host.json", "udm@udm.local"]);
        assert_eq!(mode(&firefox), Mode::Relay { spawn: true });
        assert_eq!(mode(&args(&["udm-host"])), Mode::Relay { spawn: true });
        assert_eq!(mode(&args(&["udm-host", "safari"])), Mode::Relay { spawn: false });
        assert_eq!(mode(&args(&["udm-host", "download", "https://a/b"])), Mode::Download);
        assert_eq!(mode(&args(&["udm-host", "send", "-"])), Mode::Send);
    }

    fn op(frame: &[u8]) -> Option<serde_json::Value> {
        from_browser(frame).map(|f| serde_json::from_slice(&f).unwrap())
    }

    #[test]
    fn only_the_extensions_requests_pass() {
        assert!(op(br#"{"op":"add","url":"https://a/b.zip"}"#).is_some());
        // An add's fields pass unchanged, the parent page beside the referrer.
        let add = op(br#"{"op":"add","url":"https://a/b.zip","referrer":"https://a/f","page":"https://a/p"}"#).unwrap();
        assert_eq!((add["referrer"].as_str(), add["page"].as_str()), (Some("https://a/f"), Some("https://a/p")));
        assert!(op(br#"{"op":"set_settings","settings":{}}"#).is_some());
        // Requests only the window sends stay out.
        assert!(op(br#"{"op":"save_queue","queue":{}}"#).is_none());
        assert!(op(br#"{"op":"convert","id":1}"#).is_none());
        assert!(op(br#"not json"#).is_none());
        assert!(op(br#"{"url":"no op"}"#).is_none());
    }

    #[test]
    fn a_browser_is_never_taken_for_the_window() {
        let hello = op(br#"{"op":"hello","client":"window","version":"1"}"#).unwrap();
        assert_eq!(hello.get("client"), None);
        assert_eq!(hello["version"], "1");
    }
}
