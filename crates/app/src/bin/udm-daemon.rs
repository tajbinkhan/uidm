//! UDM daemon: the long-lived process that owns the download engine.
//!
//! Clients (a host per browser connection, and the desktop window) connect over
//! loopback TCP and speak the same length-prefixed JSON frames as the browser.
//! Each connection starts with a mutual handshake over the token this daemon
//! wrote at start (see `udm_infrastructure::ipc`); until the client passes
//! it, it gets no events and its requests are not read. The listening socket,
//! the handshake and the client list are `udm_infrastructure::server`; the
//! background threads are `udm_infrastructure::background`.
//!
//! Replies to a request (`hello`, `pong`, `ack`, `downloads`, `added_many`,
//! `media_info`, the site grabber's `grab_progress` and `grab_done`, `error`)
//! go to the client that asked. Download events (`added`, `started`, `progress`, `done`,
//! `failed`, `canceled`, `removed`) go to every connected client. The daemon
//! also applies each event to its download store (`udm_infrastructure::store`),
//! so a client that connects late can ask for the whole list.
//!
//! Because downloads live here rather than in the host, they keep running when
//! the browser closes. The daemon exits on its own once it has had no clients
//! and no downloads for `background::IDLE_EXIT`.
//!
//! With `--server` (see `udm_infrastructure::daemon_args` and
//! `docs/server.md`) it runs on a server or in a container instead: it stays
//! running, takes its folders from the command line or the environment
//! (`udm_infrastructure::paths::Folders`), logs to standard output, and
//! starts no window and shows no notification (`Seat::Server`). It still
//! listens on loopback only, with the same token handshake.
//!
//! The list survives restarts: every change is written to the history file
//! (debounced, and again on exit), and the requests that may still be retried
//! go to a DPAPI-encrypted file next to it (see `udm_infrastructure::history`). Settings are
//! loaded at start and changed with `set_settings` (see `udm_infrastructure::settings`).
//!
//! With the save-as dialog on, a new download is probed first and waits as
//! `confirming` until a window answers with `confirm`. If no window is open
//! the daemon starts one; if none connects within `front::CONFIRM_FALLBACK`, the
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

// Tests may use todo!, unimplemented! and unreachable!, which clippy.toml's
// test exemptions do not cover (AGENTS.md section 9).
#![cfg_attr(test, allow(clippy::todo, clippy::unimplemented, clippy::unreachable))]

use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use tracing::{error, info, warn};
use udm_application::daemon::service::{
    confirm_answer, when_done, DaemonService, Note, Parts, RunProject, Served, Wiring,
};
use udm_application::daemon::Requests;
use udm_application::debrid::ResolveLink;
use udm_application::downloads::{RemoveDownload, RetryDownload, StartOrWait};
use udm_application::pairing::{PairingDesk, Phone};
use udm_domain::address::Address;
use udm_domain::download::Answer;
use udm_domain::id::DownloadId;
use udm_domain::ports::{Browser, Debrid, DebridKeys, Probes, SiteCrawler};
use udm_domain::seat::Seat;
use udm_domain::settings::Settings;
use udm_infrastructure::background;
use udm_infrastructure::browser_bridge::BrowserBridge;
use udm_infrastructure::clock::LocalClock;
use udm_infrastructure::connect::ProxyRetries;
use udm_infrastructure::conversions::Conversions;
use udm_infrastructure::crawler::Crawler;
use udm_infrastructure::daemon_args::{self, Invocation};
use udm_infrastructure::debrid::{DebridApi, KeyFile};
use udm_infrastructure::engine::Resolved;
use udm_infrastructure::files::DiskFiles;
use udm_infrastructure::front::{self, AnswerWait, Front, Hook, Notices, Notifier, Windows};
use udm_infrastructure::grab::{self, FoundFiles, ProjectFiles, ScheduledGrabs};
use udm_infrastructure::history;
use udm_infrastructure::idle::SystemIdle;
use udm_infrastructure::live_settings::{LiveSettings, QueueChimes};
use udm_infrastructure::logging;
use udm_infrastructure::net;
use udm_infrastructure::platform::Desktop;
use udm_infrastructure::power_control::{PowerOff, RunEnds};
use udm_infrastructure::probes::ServerProbes;
use udm_infrastructure::queue_runs::{PauseBook, RunBook};
use udm_infrastructure::quota::QuotaHold;
use udm_infrastructure::scan::Antivirus;
use udm_infrastructure::server::{next_client, Clients, Server};
use udm_infrastructure::settings;
use udm_infrastructure::store::{DownloadStore, Ended, StoreEvents, StoreFiles, StoreIntake};
use udm_infrastructure::transfers::{self, EarlyHandOver, GoOn, SyncChecker, Transfers};
use udm_infrastructure::update::{GitHub, LATEST_RELEASE};
use udm_infrastructure::web_remote::devices::{DeviceFile, OsKeys};
use udm_infrastructure::web_remote::WebRemote;

/// This copy's version.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The adapters this daemon runs the service on.
struct Wired;

impl Wiring for Wired {
    type Store = DownloadStore;
    type Intake = StoreIntake;
    type Transfers = Transfers;
    type Settings = LiveSettings;
    type Runs = RunBook;
    type Paused = PauseBook;
    type Clock = LocalClock;
    type Chimes = QueueChimes;
    type Sync = SyncChecker;
    type RunEnds = RunEnds;
    type Quota = QuotaHold;
    type Idle = SystemIdle;
    type Power = PowerOff;
    type Files = DiskFiles;
    type Front = Front;
    type Early = EarlyHandOver;
    type Answers = AnswerWait;
    type Converter = Conversions;
    type Releases = GitHub;
    type Projects = ProjectFiles;
    type GrabRuns = ScheduledGrabs;
    type Notifier = Notices;
    type Retries = ProxyRetries;
    type Found = FoundFiles;
}

/// The daemon's service, on this daemon's adapters.
type Daemon = DaemonService<Wired>;

/// Where the daemon keeps its files. A path is `None` when its environment
/// variable is missing; that data then lives in memory only.
struct Paths {
    settings: Option<PathBuf>,
    store: StoreFiles,
    /// The remote-control page's certificate and the paired devices.
    remote: Option<PathBuf>,
}

impl Paths {
    fn from_env() -> Self {
        let warn = |what: &str, r: io::Result<PathBuf>| r.map_err(|e| warn!(file = what, error = %e, "no file")).ok();
        Paths {
            settings: warn("settings", settings::settings_path()),
            store: StoreFiles {
                history: warn("history", history::history_path()),
                requests: warn("saved requests", history::requests_path()),
                quota: warn("quota", udm_infrastructure::quota::quota_path()),
            },
            remote: warn("remote control", udm_infrastructure::paths::remote_dir()),
        }
    }
}

/// The daemon, wired: what `new_daemon` returns.
struct Built {
    daemon: Daemon,
    /// The client list, as the server keeps it.
    clients: Arc<Clients>,
    /// Hears when the history needs writing.
    dirty: Receiver<()>,
    /// The notification thread's work.
    notes: Receiver<Note>,
    /// Wakes the scheduler.
    wake: Receiver<()>,
    /// The remote-control page's listener, which serves nothing until
    /// `run` hands it the service.
    remote: Arc<WebRemote>,
}

/// Load settings, history and saved requests, start the engine with them,
/// and wire the adapters into the service, for a daemon at `seat`.
fn new_daemon(paths: Paths, seat: Seat) -> Built {
    let mut settings = match &paths.settings {
        Some(path) => settings::load(path).unwrap_or_else(|e| {
            warn!(path = %path.display(), error = %e, "ignoring the settings file");
            Settings::default()
        }),
        None => Settings::default(),
    };
    // Settings from before each proxy had its own login: the HTTPS and
    // SOCKS proxies keep the shared password they used.
    if std::mem::take(&mut settings.proxy_logins_split) {
        let saved = net::split_proxy_password().map_err(|e| e.to_string()).and_then(|()| {
            paths.settings.as_ref().map_or(Ok(()), |p| settings::save(&settings, p).map_err(|e| e.to_string()))
        });
        if let Err(e) = saved {
            warn!(error = %e, "could not give each proxy its own login");
        }
    }
    // IDM's "Always turn on Speed Limiter on startup": the limit is on
    // after every start, even if it was turned off last time.
    if settings.speed_limit_on_startup {
        settings.speed_limit_enabled = true;
    }
    let (notes, notes_rx) = channel();
    let (wake, wake_rx) = channel();
    let clients = {
        let (notes, wake) = (notes.clone(), wake.clone());
        Arc::new(Clients::new(
            Box::new(move |how| {
                let note = match how {
                    Ended::Finished(id) => Note::Finished(id),
                    Ended::Failed(id) => Note::Failed(id),
                    Ended::NameTaken(id) => Note::NameTaken(id),
                };
                let _ = notes.send(note);
            }),
            Box::new(move || {
                let _ = wake.send(());
            }),
        ))
    };
    let (store, dirty_rx, next_id) = DownloadStore::open(paths.store.clone(), Arc::clone(&clients) as _);
    let store = Arc::new(store);
    let settings = Arc::new(LiveSettings::new(settings, paths.settings.clone()));
    let retries = Arc::new(ProxyRetries::default());
    let events: Arc<dyn StoreEvents> = Arc::clone(&clients) as _;
    let windows: Arc<dyn Windows> = Arc::clone(&clients) as _;
    let files = Arc::new(DiskFiles);
    let transfers = Arc::new(Transfers::new(
        Arc::clone(&store),
        Arc::clone(&events),
        Arc::clone(&settings),
        Arc::clone(&retries),
        next_id,
    ));
    let engine = Arc::clone(transfers.engine());
    let browser: Arc<dyn Browser> = Arc::new(BrowserBridge::new(Arc::clone(&clients)));
    let probes: Arc<dyn Probes> = Arc::new(ServerProbes::new(Arc::clone(&transfers)));
    let crawler: Arc<dyn SiteCrawler> =
        Arc::new(Crawler::new(Arc::clone(&transfers), Arc::clone(&settings), Arc::clone(&browser)));
    let chimes = QueueChimes(Arc::clone(&settings));
    let quota = QuotaHold::new(Arc::clone(&store), Arc::clone(&events));
    let power = {
        let (saver, shown) = (Arc::clone(&transfers), Arc::clone(&windows));
        Arc::new(PowerOff::new(
            Arc::clone(&events),
            Box::new(move || saver.save_history()),
            Box::new(move || front::show_window(&*shown, seat)),
        ))
    };
    let retry: Hook<DownloadId> = {
        let (store, transfers) = (Arc::clone(&store), Arc::clone(&transfers));
        Arc::new(move |id| {
            if let Err(e) = RetryDownload::new(&*store, &*store, &*transfers).run(id) {
                warn!(download_id = id.get(), error = %e, "could not retry from a notification");
            }
        })
    };
    let notifier = Notifier::new(Arc::clone(&store), Arc::clone(&windows), retry, seat);
    let front = Arc::new(Front::new(Arc::clone(&notifier), Arc::clone(&settings), Arc::clone(&windows)));
    let go_on: GoOn = {
        let (store, transfers, settings) = (Arc::clone(&store), Arc::clone(&transfers), Arc::clone(&settings));
        Arc::new(move |id, start, queue| {
            let later = settings.lock().later_queue;
            if let Err(e) = StartOrWait::new(&*store, &*store, &*transfers).run(id, start, queue, later) {
                warn!(download_id = id.get(), error = %e, "could not go on with the download");
            }
        })
    };
    let early = Arc::new(EarlyHandOver::new(Arc::clone(&transfers), go_on));
    let confirm: Hook<Answer> = {
        let (store, transfers, files, front, early) =
            (Arc::clone(&store), Arc::clone(&transfers), Arc::clone(&files), Arc::clone(&front), Arc::clone(&early));
        Arc::new(move |answer: Answer| {
            let id = answer.id;
            if let Err(e) = confirm_answer::<Wired>(&store, &transfers, &files, &front, &early, answer) {
                warn!(download_id = id.get(), error = %e, "could not start");
            }
        })
    };
    let answers = AnswerWait::new(Arc::clone(&store), Arc::clone(&windows), confirm, seat);
    let remove_original: Hook<DownloadId> = {
        let (store, transfers) = (Arc::clone(&store), Arc::clone(&transfers));
        Arc::new(move |id| {
            if let Err(e) = RemoveDownload::new(&*store, &*store, &*transfers, &DiskFiles).run(id, true) {
                warn!(download_id = id.get(), error = %e, "kept the original of a converted download");
            }
        })
    };
    let conversions = Conversions::new(Arc::clone(&engine), Arc::clone(&store), Arc::clone(&settings), remove_original);
    let sync = SyncChecker::new(Arc::clone(&transfers));
    let run_ends = {
        let power = Arc::clone(&power);
        RunEnds::new(
            Arc::clone(&settings),
            Box::new(move |action, queue: &str, force_close| when_done(&*power, action, queue, None, force_close)),
        )
    };
    let releases = {
        let (settings, retries) = (Arc::clone(&settings), Arc::clone(&retries));
        Box::new(move |url: &str| GitHub::new(Arc::clone(&settings), Arc::clone(&retries), url))
    };
    let tool_releases =
        Arc::new(udm_infrastructure::video_tools::ForgeTools::new(Arc::clone(&settings), Arc::clone(&retries)));
    let tool_folder = Arc::new(udm_infrastructure::video_tools::ToolDir::new(Arc::clone(&settings)));
    let remote = Arc::new(WebRemote::new(paths.remote.clone()));
    // The debrid account (experimental): each download asks `links` for
    // the address to start from, on its own thread.
    let debrid: Arc<dyn Debrid> = Arc::new(DebridApi::new(Arc::clone(&settings), Arc::clone(&retries)));
    let debrid_keys: Arc<dyn DebridKeys> = Arc::new(KeyFile::new());
    let links = Arc::new(ResolveLink::new(Arc::clone(&debrid), Arc::clone(&debrid_keys), Arc::new(LocalClock)));
    {
        let (links, settings) = (Arc::clone(&links), Arc::clone(&settings));
        transfers.resolve_with(Arc::new(move |url: &str| {
            let Ok(address) = Address::parse(url) else {
                return Ok(None);
            };
            let account = settings.get().debrid;
            match links.run(&account, &address) {
                Ok(found) => Ok(found.map(|u| Resolved { url: u.address.into_string(), filename: u.filename })),
                Err(refused) => Err(refused.to_string()),
            }
        }));
    }
    let pairing = Arc::new(PairingDesk::open(
        Arc::new(DeviceFile::in_dir(paths.remote.as_deref())),
        Arc::new(OsKeys),
        Arc::new(LocalClock),
    ));

    let daemon = DaemonService::new(Parts {
        intake: StoreIntake { store: Arc::clone(&store), engine },
        store,
        clients: Arc::clone(&clients) as _,
        clock: LocalClock,
        projects: grab::projects_dir().ok().map(|dir| Arc::new(ProjectFiles(dir))),
        files,
        scanner: Arc::new(Antivirus),
        hashes: Arc::new(udm_infrastructure::hash::DiskHashes),
        unpacker: Arc::new(udm_infrastructure::unpack::Unpacking),
        opener: Arc::new(Desktop),
        settings,
        transfers,
        notifier: Arc::new(Notices(notifier)),
        front,
        answers,
        early,
        conversions,
        sync,
        run_ends,
        browser,
        probes,
        crawler,
        tool_releases,
        tool_folder,
        platform: udm_infrastructure::video_tools::platform(),
        found: FoundFiles,
        browser_proxy_retries: retries,
        run_book: RunBook::default(),
        pause_book: PauseBook::default(),
        quota,
        idle: SystemIdle,
        power,
        chimes,
        releases,
        release_url: LATEST_RELEASE.to_string(),
        grab_runs_for: Box::new(|claims, run: RunProject| ScheduledGrabs::new(claims, run)),
        notes,
        wake,
        version: VERSION,
        pairing,
        listener: Arc::clone(&remote) as _,
        seat,
        debrid,
        debrid_keys,
        links,
    });
    Built { daemon, clients, dirty: dirty_rx, notes: notes_rx, wake: wake_rx, remote }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().unwrap_or_default();
    let (seat, folders) = match daemon_args::parse(&args, &cwd) {
        Ok(Invocation::Run { seat, folders }) => (seat, folders),
        Ok(Invocation::Move { from, to }) => move_elevated(&from, &to),
        // The program's output, for the person who ran it.
        Ok(Invocation::Help) => {
            println!("{}", daemon_args::USAGE);
            return;
        }
        Ok(Invocation::Version) => {
            println!("udm-daemon {VERSION}");
            return;
        }
        Err(e) => {
            eprintln!("udm-daemon: {e}\nSee udm-daemon --help.");
            std::process::exit(2);
        }
    };
    // Before anything looks a folder up.
    udm_infrastructure::paths::choose_folders(folders);
    if seat.has_desktop() {
        // Before the log, which would make UDM's data folder first.
        let moved = udm_infrastructure::from_old_names::move_from_old_names();
        logging::init(logging::Program::Daemon);
        moved.log();
    } else {
        // A service manager or container keeps a server's log.
        logging::init_stdout();
    }
    // A panic leaves a report in the data folder, which the window offers
    // (`crash`).
    udm_infrastructure::crash::install(logging::Program::Daemon);
    if let Err(e) = run(seat) {
        error!(error = format!("{e:#}"), "fatal");
        std::process::exit(1);
    }
}

/// Started by a download saved with administrator rights (see
/// `udm_infrastructure::elevate`): move its finished file into place and
/// exit.
fn move_elevated(from: &Path, to: &Path) -> ! {
    logging::init(logging::Program::Daemon);
    if let Err(e) = udm_infrastructure::elevate::move_file(from, to) {
        error!(from = %from.display(), to = %to.display(), error = %e, "could not move the file");
        std::process::exit(1);
    }
    std::process::exit(0);
}

fn run(seat: Seat) -> anyhow::Result<()> {
    use anyhow::Context as _;
    // Binding is the single-instance lock.
    let Some(server) = Server::bind().context("could not listen for clients")? else {
        // Another daemon has the list; a client started this one for
        // nothing. A server's is started by a service manager that should
        // hear that it did not run.
        if seat.has_desktop() {
            return Ok(());
        }
        anyhow::bail!("another UDM daemon is already running for this user and instance");
    };
    // Only the daemon holding the bind moves the logs aside, so no two
    // programs move one at once.
    logging::trim();
    info!(version = VERSION, "daemon started");
    if let Some(folder) = udm_infrastructure::paths::portable_root() {
        info!(folder = %folder.display(), "running as a portable copy");
    }
    if seat.has_desktop() {
        match udm_infrastructure::browsers::register_found() {
            Ok(names) if !names.is_empty() => info!(browsers = %names.join(", "), "registered with the browsers"),
            Ok(_) => {}
            Err(e) => warn!(error = %e, "could not register with the browsers"),
        }
    } else {
        let shown = |r: io::Result<PathBuf>| r.map(|p| p.display().to_string()).unwrap_or_else(|e| e.to_string());
        info!(
            settings = %shown(udm_infrastructure::paths::config_dir()),
            data = %shown(udm_infrastructure::paths::data_dir()),
            default_downloads = %settings::default_downloads_dir().display(),
            "running as a server"
        );
    }

    let Built { daemon, clients, dirty, notes, wake, remote } = new_daemon(Paths::from_env(), seat);
    transfers::clean_early_folders(&daemon.store);
    let daemon = Arc::new(daemon);
    {
        let daemon = Arc::clone(&daemon);
        background::spawn_scheduler(wake, move || daemon.scheduler_pass());
    }
    {
        let transfers = Arc::clone(&daemon.transfers);
        background::spawn_debounced(dirty, move || transfers.save_history());
    }
    {
        let daemon = Arc::clone(&daemon);
        background::spawn_each(notes, move |note| daemon.on_note(note));
    }
    #[cfg(windows)]
    if seat.has_desktop() {
        if let Err(e) = udm_infrastructure::notify::register_app() {
            warn!(error = %e, "could not register for notifications");
        }
    }
    daemon.start_up();
    {
        let daemon = Arc::clone(&daemon);
        background::spawn_awake_keeper(move || daemon.wants_awake());
    }
    {
        let (idle, joined, transfers) = (Arc::clone(&daemon), Arc::clone(&clients), Arc::clone(&daemon.transfers));
        background::spawn_idle_monitor(
            move || idle.may_exit(),
            move || joined.joined(),
            move || transfers.save_history(),
        );
    }
    if !seat.exits_when_idle() {
        // Nothing else stops a server's daemon; `docker stop` asks.
        let transfers = Arc::clone(&daemon.transfers);
        background::spawn_stop_on_signal(move || transfers.save_history());
    }

    // The remote-control page asks the service what a client would, inside
    // this process; it listens only while the settings have it on.
    let requests: Arc<dyn Requests> = Arc::new(Served(Arc::clone(&daemon)));
    remote.serve_with(Arc::new(Phone::new(Arc::clone(&daemon.pairing), Arc::clone(&requests), Box::new(next_client))));
    {
        let settings = Arc::clone(&daemon.settings);
        remote.watch(move || settings.get());
    }

    server.serve(clients, requests);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Mutex;
    use std::thread;
    use std::time::{Duration, Instant};
    use udm_application::daemon::service::{ServiceError, COMMAND_LINE_WAIT};
    use udm_application::daemon::ClientId;
    use udm_application::daemon::{Event, Failure, Reply};
    use udm_domain::bytes::ByteCount;
    use udm_domain::download::DownloadLogin;
    use udm_domain::download::{Download, DownloadEntry, OnDone, Properties, Status};
    use udm_domain::grab::GrabRequest;
    use udm_domain::id::QueueId;
    use udm_domain::ports::QuotaState;
    use udm_domain::queue::{self, Queue, WhenDone, MAIN_QUEUE};
    use udm_domain::quota::Usage;
    use udm_domain::settings::{Duplicate, FileExists};
    use udm_infrastructure::engine;
    use udm_infrastructure::protocol::Outgoing;
    use udm_infrastructure::server;

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
                        .and_then(|(a, b)| {
                            Some((a.parse::<u64>().ok()?, b.parse::<u64>().unwrap_or(len - 1).min(len - 1)))
                        });
                    let (status, start, end, extra) = match range {
                        Some((a, b)) => {
                            ("206 Partial Content", a, b, format!("Content-Range: bytes {a}-{b}/{len}\r\n"))
                        }
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

    /// Hand `raw` to the daemon as client `client` sent it.
    fn send(raw: &[u8], t: &Setup, client: u64, reply: &Reply) {
        server::receive(&t.connections, &Served(Arc::clone(&t.daemon)), ClientId::new(client), raw, reply);
    }

    struct Setup {
        daemon: Arc<Daemon>,
        /// The client list, as the server keeps it.
        connections: Arc<Clients>,
        dir: PathBuf,
        /// What a pretend window receives.
        window: Receiver<Vec<u8>>,
    }

    /// A daemon with no files, saving to a fresh folder, with a pretend
    /// window connected so nothing opens on screen.
    fn setup(name: &str, choice: Duplicate) -> Setup {
        let dir = std::env::temp_dir().join(format!("udm_dup_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let Built { daemon, clients: connections, .. } =
            new_daemon(Paths { settings: None, store: StoreFiles::default(), remote: None }, Seat::Desktop);
        {
            let mut s = daemon.settings.lock();
            s.download_dir = Some(dir.display().to_string());
            s.ask_where_to_save = false;
            s.if_duplicate = choice;
        }
        let (_reply, window) = connections.connect(ClientId::new(1));
        connections.add_window(ClientId::new(1));
        Setup { daemon: Arc::new(daemon), connections, dir, window }
    }

    impl Setup {
        /// Put a finished download of `url` in the list, its file at `path`
        /// holding `old`.
        fn finished(&self, url: &str, path: &Path) -> DownloadId {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"old").unwrap();
            let id = self.daemon.transfers.engine().reserve_id();
            let url = udm_domain::address::Link::new(url);
            self.daemon.store.publish(&Outgoing::Added { id, url: url.clone() });
            self.daemon.store.publish(&Outgoing::Done { id, url, path: path.display().to_string(), note: None });
            id
        }

        fn failed(&self, url: &str) -> DownloadId {
            let id = self.daemon.transfers.engine().reserve_id();
            let url = udm_domain::address::Link::new(url);
            self.daemon.store.publish(&Outgoing::Added { id, url: url.clone() });
            self.daemon.store.publish(&Outgoing::Failed { id, url, error: "gone".into() });
            id
        }

        fn add(&self, url: &str) -> DownloadId {
            self.daemon.add(Download { url: url.into(), ..Download::default() }).0
        }

        fn len(&self) -> usize {
            self.daemon.store.table().len()
        }

        /// Wait for download `id` to reach `status`, and return it.
        fn wait(&self, id: DownloadId, status: Status) -> DownloadEntry {
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
            self.window.try_iter().filter_map(|f| serde_json::from_slice::<Outgoing>(&f).ok()).any(|m| wanted(&m))
        }
    }

    #[test]
    fn the_windows_hear_when_the_browser_extension_connects_and_goes() {
        let t = setup("browser_link", Duplicate::Ask);
        assert!(matches!(t.daemon.clients.browser_link(), Event::BrowserLink { connected: false, last_seen: None }));

        // The extension says hello with no `client`.
        let (tx, _browser) = t.connections.connect(ClientId::new(2));
        send(br#"{"op":"hello","version":"1"}"#, &t, 2, &tx);
        assert!(t.window_got(|m| matches!(m, Outgoing::BrowserLink { connected: true, .. })));
        assert!(t.connections.browser_seen().is_some());

        // A second one is no news; the first going is none either.
        let (tx3, _browser3) = t.connections.connect(ClientId::new(3));
        send(br#"{"op":"hello"}"#, &t, 3, &tx3);
        t.connections.client_gone(ClientId::new(2));
        assert!(!t.window_got(|m| matches!(m, Outgoing::BrowserLink { .. })));

        // Anything it sends keeps it seen.
        t.connections.set_browser_seen(Some(1));
        send(br#"{"op":"ping"}"#, &t, 3, &tx3);
        assert!(t.connections.browser_seen().is_some_and(|s| s > 1));

        // The last one going is, with when.
        t.connections.client_gone(ClientId::new(3));
        assert!(t.window_got(|m| matches!(m, Outgoing::BrowserLink { connected: false, last_seen: Some(s) } if *s > 1)));
    }

    #[test]
    fn a_command_line_runs_in_the_open_window_or_waits_for_the_new_one() {
        let t = setup("command_line", Duplicate::NewName);
        let entry = |url: &str| t.daemon.store.table().entries().find(|e| e.url == url).cloned();
        let ask = |url: &str| -> (Outgoing, Receiver<Vec<u8>>) {
            let (tx, rx) = server::reply_channel();
            let msg =
                format!(r#"{{"op":"command_line","download":{{"url":"{url}","start":false}},"quit_after":true}}"#);
            send(msg.as_bytes(), &t, 9, &tx);
            let reply = rx.try_iter().filter_map(|f| serde_json::from_slice::<Outgoing>(&f).ok()).last().unwrap();
            (reply, rx)
        };

        // A window is open: it is added now, and /q does nothing, as it was
        // not this call that started UDM.
        let (reply, _) = ask("https://example.com/open.bin");
        assert!(matches!(reply, Outgoing::CommandLine { window_open: true }));
        let e = entry("https://example.com/open.bin").expect("added now");
        assert_eq!(e.status, Status::Waiting, "/a only adds it");
        assert_eq!(e.on_done, None);

        // No window: it waits for the one this call opens, then has /q.
        t.connections.client_gone(ClientId::new(1));
        let (reply, _) = ask("https://example.com/new.bin");
        assert!(matches!(reply, Outgoing::CommandLine { window_open: false }));
        assert!(entry("https://example.com/new.bin").is_none(), "held for the window");
        let (tx, _rx) = server::reply_channel();
        send(br#"{"op":"hello","client":"window"}"#, &t, 1, &tx);
        let e = entry("https://example.com/new.bin").expect("added once the window says hello");
        assert_eq!(e.on_done.map(|o| o.then), Some(WhenDone::Exit));

        // One held too long belonged to a window that never came.
        t.connections.client_gone(ClientId::new(1));
        ask("https://example.com/stale.bin");
        let held = t.daemon.command_line.lock().unwrap().take().unwrap().0;
        let old = Instant::now().checked_sub(COMMAND_LINE_WAIT + Duration::from_secs(1)).unwrap();
        *t.daemon.command_line.lock().unwrap() = Some((held, old));
        send(br#"{"op":"hello","client":"window"}"#, &t, 1, &tx);
        assert!(entry("https://example.com/stale.bin").is_none());
    }

    #[test]
    fn a_server_daemon_runs_on_when_it_has_nothing_to_do() {
        let built =
            |seat| new_daemon(Paths { settings: None, store: StoreFiles::default(), remote: None }, seat).daemon;
        let desktop = built(Seat::Desktop);
        assert!(desktop.idle() && desktop.may_exit(), "a desktop daemon with nothing to do exits");
        let server = built(Seat::Server);
        assert!(server.idle(), "the server has nothing to do either");
        assert!(!server.may_exit(), "but runs until it is stopped");
        // Remote control keeps a desktop's daemon running too, and a server's
        // serves the phone page the same way.
        desktop.settings.lock().remote_enabled = true;
        assert!(!desktop.idle() && !desktop.may_exit(), "a phone may ask at any time");
    }

    #[test]
    fn starting_udm_again_brings_the_open_window_forward() {
        let t = setup("again", Duplicate::NewName);
        let ask = || -> Outgoing {
            let (tx, rx) = server::reply_channel();
            send(br#"{"op":"command_line"}"#, &t, 9, &tx);
            rx.try_iter().filter_map(|f| serde_json::from_slice::<Outgoing>(&f).ok()).last().unwrap()
        };

        // A window is open: it comes forward, and the new copy ends.
        assert!(matches!(ask(), Outgoing::CommandLine { window_open: true }));
        assert!(t.window_got(|m| matches!(m, Outgoing::Show)));

        // None is: the new copy opens one, and nothing waits for it.
        t.connections.client_gone(ClientId::new(1));
        assert!(matches!(ask(), Outgoing::CommandLine { window_open: false }));
        assert!(t.daemon.command_line.lock().unwrap().is_none());
    }

    #[test]
    fn only_http_https_and_ftp_addresses_are_added() {
        let t = setup("schemes", Duplicate::NewName);
        for url in ["javascript:alert(1)", "file:///etc/passwd", "data:text/plain,hi"] {
            let (tx, rx) = server::reply_channel();
            send(format!(r#"{{"op":"add","url":"{url}"}}"#).as_bytes(), &t, 2, &tx);
            let reply = rx.try_iter().filter_map(|f| serde_json::from_slice::<Outgoing>(&f).ok()).last().unwrap();
            assert!(
                matches!(&reply, Outgoing::Rejected { url: u, message, .. } if u == url && message.contains("is not an http, https or ftp address")),
                "{url}: {reply:?}"
            );
            // The window hears the kind too, to say it in its language.
            let Outgoing::Rejected { message, error_kind, .. } = reply else { unreachable!() };
            assert_eq!(
                udm_infrastructure::wire::failure(message, error_kind),
                Failure::Service(ServiceError::NotDownloadAddress(url.to_string()))
            );
        }
        assert_eq!(t.len(), 0, "nothing was added");
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
        let old = t.daemon.transfers.engine().reserve_id();
        t.daemon.store.publish(&Outgoing::Added { id: old, url: url.as_str().into() });
        assert_eq!(t.daemon.transfers.status(old), Some(Status::Queued));
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
        t.daemon.confirm(Answer { duplicate: Some(Duplicate::Existing), ..Answer::start(new) }).unwrap();
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
            urls.iter().map(|u| Download { url: (*u).into(), ..Download::default() }).collect::<Vec<_>>()
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
        let get = Download { url: url.into(), ..Download::default() };
        assert!(t.daemon.duplicate_of(&get).is_some());
        let post = Download { method: Some("POST".into()), ..get };
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
            login: Some(DownloadLogin { username: "me".into(), password: Some("pw".into()) }),
            ..Properties::default()
        };
        t.daemon.set_properties(id, changes).unwrap();
        let e = t.daemon.entry(id).unwrap();
        assert_eq!(e.url, "https://y/new.bin");
        assert_eq!(e.referrer.as_deref(), Some("https://y/page"));
        assert_eq!(e.login_user.as_deref(), Some("me"));
        let req = t.daemon.store.requests().get(&id).cloned().unwrap();
        assert_eq!(req.url, "https://y/new.bin");
        assert_eq!(req.referrer.as_deref(), Some("https://y/page"));

        // A new user name without a password keeps the old password.
        let rename = Properties {
            login: Some(DownloadLogin { username: "other".into(), password: None }),
            ..Properties::default()
        };
        t.daemon.set_properties(id, rename).unwrap();
        let req = t.daemon.store.requests().get(&id).cloned().unwrap();
        let login = req.login.clone().unwrap();
        assert_eq!((login.username.as_str(), login.password.as_deref()), ("other", Some("pw")));
        let net = t.daemon.transfers.net_for(&req, &Settings::default());
        let found = net::find_login(&net.logins, "https://y/new.bin").unwrap();
        assert_eq!(found.username, "other");

        // An empty user name removes the login.
        let clear = Properties { login: Some(DownloadLogin::default()), ..Properties::default() };
        t.daemon.set_properties(id, clear).unwrap();
        assert_eq!(t.daemon.entry(id).unwrap().login_user, None);
        assert!(t.daemon.store.requests()[&id].login.is_none());
    }

    /// A stopped download moved to another folder takes its partial file
    /// and sidecar along, and resumes and finishes there without fetching
    /// its bytes again.
    #[test]
    fn properties_move_a_stopped_download_with_its_partial_file() {
        const LEN: usize = 1024 * 1024;
        let t = setup("props_move_partial", Duplicate::NewName);
        t.daemon.settings.lock().connections = 2;
        let body: Arc<Vec<u8>> = Arc::new((0..LEN).map(|i| (i % 251) as u8).collect());
        let sent = Arc::new(AtomicU64::new(0));
        let id = t.add(&serve_ranges(Arc::clone(&body), Arc::clone(&sent)));
        let until = Instant::now() + Duration::from_secs(30);
        while t.daemon.transfers.status(id) != Some(Status::Downloading)
            || sent.load(Ordering::Relaxed) < LEN as u64 / 4
        {
            assert!(Instant::now() < until, "the download never got going");
            thread::sleep(Duration::from_millis(20));
        }
        // Running, it cannot move.
        let elsewhere = t.dir.join("elsewhere");
        let move_it = || Properties { folder: Some(elsewhere.display().to_string()), ..Properties::default() };
        assert!(t.daemon.set_properties(id, move_it()).is_err());
        t.daemon.cancel(id);
        let stopped = t.wait(id, Status::Canceled);
        let old = PathBuf::from(stopped.path.unwrap());
        assert_eq!(old, t.dir.join("big.bin"));
        assert!(old.is_file() && engine::sidecar_path(&old).is_file());
        let kept = std::fs::metadata(&old).unwrap().len();

        t.daemon.set_properties(id, move_it()).unwrap();
        let moved = elsewhere.join("big.bin");
        assert_eq!(t.daemon.entry(id).unwrap().path.map(PathBuf::from), Some(moved.clone()));
        assert!(!old.exists() && !engine::sidecar_path(&old).exists(), "nothing is left behind");
        assert!(moved.is_file() && engine::sidecar_path(&moved).is_file());
        assert_eq!(std::fs::metadata(&moved).unwrap().len(), kept);

        let before = sent.load(Ordering::Relaxed);
        let (resumed, _) = t.daemon.retry(id).unwrap();
        let done = t.wait(resumed, Status::Done);
        assert_eq!(done.path.map(PathBuf::from), Some(moved.clone()));
        assert_eq!(std::fs::read(&moved).unwrap(), *body);
        assert!(!old.exists(), "it did not start over in the old folder");
        let again = sent.load(Ordering::Relaxed) - before;
        assert!(again < LEN as u64 - LEN as u64 / 8, "sent {again} bytes again; it did not resume");
    }

    #[test]
    fn properties_move_a_download_that_never_started() {
        let t = setup("props_move_unstarted", Duplicate::NewName);
        let id = t.failed("https://x/a.bin");
        let elsewhere = t.dir.join("later");
        let changes = Properties { folder: Some(elsewhere.display().to_string()), ..Properties::default() };
        t.daemon.set_properties(id, changes).unwrap();
        let req = t.daemon.store.requests().get(&id).cloned().unwrap();
        assert_eq!(req.folder.map(PathBuf::from), Some(elsewhere.clone()), "the next attempt saves there");

        // A proposed file that was never started may be somebody else's: it
        // stays where it is.
        let other = t.dir.join("theirs.bin");
        std::fs::write(&other, b"theirs").unwrap();
        t.daemon
            .store
            .update(id, |e| {
                e.path = Some(other.display().to_string());
                true
            })
            .unwrap();
        let back = Properties { folder: Some(t.dir.join("again").display().to_string()), ..Properties::default() };
        t.daemon.set_properties(id, back).unwrap();
        assert_eq!(std::fs::read(&other).unwrap(), b"theirs");
        assert_eq!(t.daemon.entry(id).unwrap().path.map(PathBuf::from), Some(t.dir.join("again").join("theirs.bin")));
        let relative = Properties { folder: Some("rel".into()), ..Properties::default() };
        assert!(t.daemon.set_properties(id, relative).is_err());
        let rename = Properties { filename: Some("b.bin".into()), ..Properties::default() };
        assert!(t.daemon.set_properties(id, rename).is_err(), "only a finished file is renamed");
    }

    #[test]
    fn the_link_picker_hears_each_links_type_and_size() {
        let t = setup("probe_links", Duplicate::NewName);
        let link = |url: String| Download { url: url.into(), ..Download::default() };
        let links = vec![link(serve()), link("http://127.0.0.1:1/gone.zip".into()), link(serve())];
        let (tx, rx) = server::reply_channel();
        t.daemon.probe_links(ClientId::new(7), 42, links, tx);
        let mut heard = std::collections::BTreeMap::new();
        while heard.len() < 3 {
            let frame = rx.recv_timeout(Duration::from_secs(10)).expect("every link is answered");
            if let Ok(Outgoing::LinkInfo { request: 42, index, total, content_type, error, .. }) =
                serde_json::from_slice(&frame)
            {
                heard.insert(index, (total, content_type, error));
            }
        }
        let found = (Some(ByteCount::new(BODY.len() as u64)), Some("application/octet-stream".to_string()), None);
        assert_eq!(heard[&0], found);
        assert_eq!(heard[&2], found);
        assert!(heard[&1].2.is_some(), "a link that cannot be reached says why");

        // A new request stops the last; closing the picker stops the new one.
        let (tx, _rx) = server::reply_channel();
        let before = Arc::clone(&t.daemon.link_probes.lock().unwrap()[&ClientId::new(7)]);
        t.daemon.probe_links(ClientId::new(7), 43, vec![link(serve())], tx);
        assert!(before.load(Ordering::Relaxed));
        let now = Arc::clone(&t.daemon.link_probes.lock().unwrap()[&ClientId::new(7)]);
        t.daemon.stop_probe_links(ClientId::new(7));
        assert!(now.load(Ordering::Relaxed) && t.daemon.link_probes.lock().unwrap().is_empty());
    }

    #[test]
    fn the_browsers_proxy_can_wait_for_a_failure() {
        let t = setup("browser_proxy", Duplicate::NewName);
        let d = Download { url: "https://x/a.zip".into(), proxy: Some("http://bp:3128".into()), ..Download::default() };
        let through = |s: &Settings| t.daemon.transfers.net_for(&d, s).proxy.and_then(|p| p.proxy_for(d.url.as_str()));
        let after = Settings { use_browser_proxy: true, browser_proxy_after_errors: true, ..Settings::default() };
        assert_eq!(through(&after), None, "not before it failed");
        t.daemon.browser_proxy_retries.insert(d.url.to_string());
        assert_eq!(through(&after).as_deref(), Some("http://bp:3128"), "once it failed");
        // A download that never failed is not retried through it.
        assert!(!t.daemon.retry_through_browser_proxy(DownloadId::new(999), &after));
    }

    #[test]
    fn a_browser_download_shows_the_size_the_browser_saw() {
        let t = setup("browser_size", Duplicate::NewName);
        let (id, _) = t.daemon.add_later(Download {
            url: "https://x/big.iso".into(),
            size: Some(ByteCount::new(4_000_000_000)),
            content_type: Some("application/x-iso9660-image".into()),
            ..Download::default()
        });
        assert_eq!(t.daemon.entry(id).unwrap().total, Some(ByteCount::new(4_000_000_000)));
    }

    #[test]
    fn the_browser_changes_only_the_video_button() {
        let t = setup("browser_settings", Duplicate::NewName);
        let asked = Settings {
            video_button: false,
            video_button_excluded_sites: vec!["v.net".into()],
            video_auto_capture: true,
            browser_menu_hidden_firefox: vec!["download".into()],
            capture_force_key: udm_domain::settings::CaptureKey {
                delete: true,
                ..udm_domain::settings::CaptureKey::NONE
            },
            ..Settings::default()
        };
        let kept = t.daemon.with_video_button(&asked);
        assert!(!kept.video_button);
        assert_eq!(kept.video_button_excluded_sites, ["v.net"]);
        assert!(!kept.video_auto_capture, "web players' files are the window's to turn on");
        assert!(kept.browser_menu_hidden_firefox.is_empty());
        assert_eq!(kept.capture_force_key, udm_domain::settings::CaptureKey::CTRL);
    }

    #[test]
    fn only_the_window_turns_on_capture_in_private_windows() {
        let t = setup("browser_private", Duplicate::NewName);
        assert!(!t.daemon.settings.lock().capture_private, "off by default");
        let asked = Settings { capture_private: true, ..t.daemon.settings.lock().clone() };
        // A browser client's set_settings keeps the setting as it was...
        t.daemon.transfers.set_settings(t.daemon.with_video_button(&asked)).unwrap();
        assert!(!t.daemon.settings.lock().capture_private);
        // ...while the window's goes through.
        t.daemon.transfers.set_settings(asked).unwrap();
        assert!(t.daemon.settings.lock().capture_private);
        let off = Settings { capture_private: false, ..Settings::default() };
        assert!(t.daemon.with_video_button(&off).capture_private, "nor can the browser turn it off");
    }

    #[test]
    fn properties_refuse_running_downloads_and_bad_links() {
        let t = setup("props_refuse", Duplicate::NewName);
        let running = t.daemon.transfers.engine().reserve_id();
        t.daemon.store.publish(&Outgoing::Added { id: running, url: "https://x/a".into() });
        let describe = Properties { description: Some("x".into()), ..Properties::default() };
        assert!(t.daemon.set_properties(running, describe).is_err());
        let failed = t.failed("https://x/b");
        let bad = Properties { url: Some("file://x/b".into()), ..Properties::default() };
        assert!(t.daemon.set_properties(failed, bad).is_err());
        let rename = Properties { filename: Some("c".into()), ..Properties::default() };
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
        let sidecar = t.dir.join("gone.bin.udmpart");
        std::fs::write(&sidecar, b"{}").unwrap();
        t.daemon.remove(id, true).unwrap();
        assert!(!gone.exists() && !sidecar.exists());
        assert!(t.daemon.entry(id).is_none());

        // A download that never started only proposed its path.
        let other = t.dir.join("someone_elses.bin");
        std::fs::write(&other, b"theirs").unwrap();
        let id = t.failed("https://x/someone_elses.bin");
        t.daemon
            .store
            .update(id, |e| {
                e.path = Some(other.display().to_string());
                true
            })
            .unwrap();
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
            .set_properties(id, Properties { description: Some("monthly".into()), ..Properties::default() })
            .unwrap();
        t.daemon.redownload(id).unwrap();
        let done = t.wait(id, Status::Done);
        assert_eq!(done.description.as_deref(), Some("monthly"));
        assert_eq!(done.path.map(PathBuf::from), Some(t.dir.join("kept").join("f (1).bin")));
        assert_eq!(std::fs::read(&path).unwrap(), b"old", "the old file is a taken name");
        assert_eq!(t.len(), 1, "still one row");

        let t = setup("again_overwrite", Duplicate::NewName);
        t.daemon.settings.lock().file_exists = FileExists::Overwrite;
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
        let id = t.daemon.transfers.engine().reserve_id();
        t.daemon.store.publish(&Outgoing::Added { id, url: "https://x/a".into() });
        assert!(t.daemon.redownload(id).is_err());
        assert!(t.daemon.redownload(DownloadId::new(999)).is_err());
    }

    /// A tar holding one file, `name` with `text`, as tar writes it.
    fn tar_of(name: &str, text: &[u8]) -> Vec<u8> {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..107].copy_from_slice(b"0000644");
        h[108..115].copy_from_slice(b"0000000");
        h[116..123].copy_from_slice(b"0000000");
        h[124..135].copy_from_slice(format!("{:011o}", text.len()).as_bytes());
        h[136..147].copy_from_slice(b"00000000000");
        h[156] = b'0';
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        let mut out = h.to_vec();
        out.extend_from_slice(text);
        out.resize(out.len().div_ceil(512) * 512 + 1024, 0);
        out
    }

    #[test]
    fn a_split_archive_is_kept_as_a_package_and_unpacked_once_its_parts_are_in() {
        use udm_application::downloads::KeepPackages;
        use udm_domain::package::{Packaging, Unpack};
        use udm_domain::ports::PackageStore;
        let t = setup("package", Duplicate::NewName);
        {
            let mut s = t.daemon.settings.lock();
            s.sound_on_done = false;
            s.sound_on_failed = false;
            s.notify = udm_domain::settings::Notify::Off;
            s.unpack_auto = true;
        }
        let tar = tar_of("hello.txt", b"hello from a package");
        let (first, second) = tar.split_at(tar.len() / 2);
        let mut added = Vec::new();
        for (name, part) in [("bundle.tar.001", first), ("bundle.tar.002", second)] {
            let url = serve_ranges(Arc::new(part.to_vec()), Arc::new(AtomicU64::new(0)));
            let download = Download { url: url.as_str().into(), filename: Some(name.into()), ..Download::default() };
            added.push((t.daemon.add(download).0, name.to_string()));
        }
        let kept = KeepPackages::new(&*t.daemon.store).keep(&added, &Packaging::Archives);
        assert_eq!(kept.len(), 1, "the two pieces are one package");
        for (id, _) in &added {
            t.wait(*id, Status::Done);
            t.daemon.on_note(Note::Finished(*id));
        }
        let until = Instant::now() + Duration::from_secs(10);
        let state = loop {
            let state = t.daemon.store.edit_packages(|list| list[0].unpack.clone());
            if !matches!(state, Unpack::Waiting | Unpack::Unpacking) || Instant::now() > until {
                break state;
            }
            thread::sleep(Duration::from_millis(20));
        };
        let folder = t.dir.join("bundle");
        assert_eq!(state, Unpack::Done { folder: folder.display().to_string(), files: 1 });
        assert_eq!(std::fs::read_to_string(folder.join("hello.txt")).unwrap(), "hello from a package");
        assert!(t.dir.join("bundle.tar.001").exists(), "the archives stay unless Options says otherwise");
        assert!(t.window_got(|m| matches!(m, Outgoing::Packages { items } if !items.is_empty())));
    }

    #[test]
    fn a_finished_download_is_checked_against_its_checksum() {
        use udm_domain::checksum::{to_hex, Algorithm, Checksum};
        let t = setup("checksum", Duplicate::NewName);
        {
            // No sound or toast from a test.
            let mut s = t.daemon.settings.lock();
            s.sound_on_done = false;
            s.sound_on_failed = false;
            s.notify = udm_domain::settings::Notify::Off;
        }
        let url = serve();
        let digest = udm_infrastructure::hash::digest_of(BODY, Algorithm::Sha256).unwrap();
        let right = Checksum::parse(&format!("sha256:{}", to_hex(&digest))).unwrap();
        let wrong = Checksum::parse(&format!("sha256:{}", "0".repeat(64))).unwrap();
        let mut ids = Vec::new();
        for (name, checksum) in [("good.bin", right), ("bad.bin", wrong)] {
            let download = Download {
                url: url.as_str().into(),
                filename: Some(name.into()),
                checksum: Some(checksum),
                ..Download::default()
            };
            let id = t.daemon.add(download).0;
            let e = t.wait(id, Status::Done);
            assert!(e.checksum.is_some(), "the row keeps the checksum it was added with");
            t.daemon.on_note(Note::Finished(id));
            ids.push(id);
        }
        let bad = t.wait(ids[1], Status::Failed);
        assert!(bad.error.unwrap().starts_with("The file does not match its SHA-256 checksum"));
        thread::sleep(Duration::from_millis(200));
        assert_eq!(t.daemon.entry(ids[0]).unwrap().status, Status::Done, "a matching file stays done");
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
        assert!(t.daemon.entry(new_id).unwrap().limit_remember, "remembering is on by default");
        t.daemon.cancel(new_id);
        t.daemon.set_download_limit(new_id, None).unwrap();
        assert_eq!(t.daemon.entry(new_id).unwrap().speed_limit, None);
    }

    #[test]
    fn a_limit_not_remembered_is_dropped_on_stop_and_resume() {
        let t = setup("forget_limit", Duplicate::NewName);
        let id = t.failed("https://x/slow.bin");
        t.daemon.set_download_limit(id, Some(64 * 1024)).unwrap();
        t.daemon.set_limit_remember(id, false).unwrap();
        let (new_id, _) = t.daemon.retry(id).unwrap();
        // Give the new entry time to take what its request carries.
        thread::sleep(Duration::from_millis(200));
        let e = t.daemon.entry(new_id).unwrap();
        assert_eq!(e.speed_limit, None, "the limit went with the stop");
        assert!(!e.limit_remember, "the choice stays with the download");
        t.daemon.cancel(new_id);

        // Stopping its queue and starting it again is a stop and resume too.
        let id = t.daemon.transfers.engine().reserve_id();
        t.daemon.store.publish(&Outgoing::Added { id, url: "https://x/q.bin".into() });
        t.daemon.set_download_limit(id, Some(64 * 1024)).unwrap();
        t.daemon.set_limit_remember(id, false).unwrap();
        t.daemon.store.hold(id);
        t.daemon.store.publish(&Outgoing::Canceled { id, url: "https://x/q.bin".into() });
        let e = t.daemon.entry(id).unwrap();
        assert_eq!(e.status, Status::Waiting);
        assert_eq!(e.speed_limit, None);

        // Remembered, it stays.
        let id = t.daemon.transfers.engine().reserve_id();
        t.daemon.store.publish(&Outgoing::Added { id, url: "https://x/r.bin".into() });
        t.daemon.set_download_limit(id, Some(64 * 1024)).unwrap();
        t.daemon.store.hold(id);
        t.daemon.store.publish(&Outgoing::Canceled { id, url: "https://x/r.bin".into() });
        assert_eq!(t.daemon.entry(id).unwrap().speed_limit, Some(64 * 1024));
    }

    #[test]
    fn the_speed_limit_can_turn_on_at_every_start() {
        let dir = std::env::temp_dir().join(format!("udm_limit_startup_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let start = |on_startup: bool| {
            let s = Settings {
                speed_limit_enabled: false,
                speed_limit_kbps: 300,
                speed_limit_on_startup: on_startup,
                speed_limit_per_file_kbps: Some(100),
                ..Settings::default()
            };
            settings::save(&s, &path).unwrap();
            let paths = Paths { settings: Some(path.clone()), store: StoreFiles::default(), remote: None };
            let daemon = new_daemon(paths, Seat::Desktop).daemon;
            let s = daemon.settings.lock().clone();
            (s, daemon.transfers.engine().per_file_limit())
        };
        let (s, per_file) = start(false);
        assert!(!s.speed_limit_enabled, "off as it was left");
        assert_eq!(per_file, None, "the limit per file is off with it");
        let (s, per_file) = start(true);
        assert!(s.speed_limit_enabled, "on, though it was left off");
        assert_eq!(s.speed_limit_bytes(), Some(300 * 1024));
        assert_eq!(per_file, Some(100 * 1024));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_limit_per_file_caps_each_download_without_its_own() {
        let t = setup("per_file", Duplicate::NewName);
        let mut s = t.daemon.settings.lock().clone();
        s.speed_limit_enabled = true;
        s.speed_limit_kbps = 100_000;
        s.speed_limit_per_file_kbps = Some(32);
        t.daemon.transfers.set_settings(s.clone()).unwrap();
        assert_eq!(t.daemon.transfers.engine().per_file_limit(), Some(32 * 1024));

        // 48 KiB at 32 KiB/s takes about 1.5s; unlimited, a fraction of that.
        let started = Instant::now();
        let id = t.add(&serve_ranges(Arc::new(vec![3u8; 48 * 1024]), Arc::new(AtomicU64::new(0))));
        t.wait(id, Status::Done);
        let secs = started.elapsed().as_secs_f64();
        assert!(secs >= 1.0, "took {secs:.2}s; the limit per file did not hold");

        // It only applies while the speed limit is on.
        s.speed_limit_enabled = false;
        t.daemon.transfers.set_settings(s).unwrap();
        assert_eq!(t.daemon.transfers.engine().per_file_limit(), None);
    }

    #[test]
    fn a_download_does_what_it_asked_once_finished() {
        let t = setup("on_done", Duplicate::NewName);
        let id = t.failed("https://x/a.bin");
        let asked = OnDone { dialog: true, then: WhenDone::Nothing, force_close: false };
        t.daemon.set_on_done(id, Some(asked)).unwrap();
        assert_eq!(t.daemon.entry(id).unwrap().on_done, Some(asked));
        t.daemon.after_download(id);
        assert_eq!(t.daemon.entry(id).unwrap().on_done, Some(asked), "a failed download keeps it for its retry");

        t.daemon
            .store
            .update(id, |e| {
                e.status = Status::Done;
                true
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
        t.daemon.set_on_done(id, Some(OnDone { dialog: false, then: WhenDone::ShutDown, force_close: true })).unwrap();
        t.daemon.set_on_done(id, Some(OnDone::default())).unwrap();
        assert_eq!(t.daemon.entry(id).unwrap().on_done, None);
    }

    #[test]
    fn the_save_as_folder_is_remembered_for_its_category() {
        let t = setup("remember_folder", Duplicate::NewName);
        t.daemon.settings.lock().categories[0].remember_folder = true;
        let (id, _) = t.daemon.add_later(Download { url: "https://x/a.zip".into(), ..Download::default() });
        let chosen = t.dir.join("chosen");
        t.daemon
            .confirm(Answer {
                filename: Some("a.zip".into()),
                folder: Some(chosen.display().to_string()),
                start: false,
                ..Answer::start(id)
            })
            .unwrap();
        let s = t.daemon.settings.lock().clone();
        assert_eq!(s.categories[0].last_folder.as_deref(), Some(chosen.display().to_string().as_str()));
        assert_eq!(s.folders(&udm_infrastructure::settings::default_downloads_dir()).for_name("b.zip"), chosen);
    }

    #[test]
    fn a_download_keeps_its_referrer_and_page_apart() {
        let t = setup("page", Duplicate::NewName);
        let (id, _) = t.daemon.add_later(Download {
            url: "https://cdn.x/a.zip".into(),
            referrer: Some("https://x/frame".into()),
            page: Some("https://x/article".into()),
            ..Download::default()
        });
        let e = t.daemon.entry(id).unwrap();
        assert_eq!(e.referrer.as_deref(), Some("https://x/frame"));
        assert_eq!(e.page.as_deref(), Some("https://x/article"));
        // Only the referrer goes to the server.
        let req = t.daemon.store.requests().get(&id).cloned().unwrap();
        let headers = t.daemon.transfers.request_for(req).headers;
        let sent: Vec<&str> = headers.iter().filter(|(k, _)| k == "Referer").map(|(_, v)| v.as_str()).collect();
        assert_eq!(sent, ["https://x/frame"]);
        assert!(!headers.iter().any(|(_, v)| v.contains("article")));

        // A source that knows only the referrer found the link on it.
        let (id, _) = t.daemon.add_later(Download {
            url: "https://x/b.zip".into(),
            referrer: Some("https://x/list".into()),
            ..Download::default()
        });
        assert_eq!(t.daemon.entry(id).unwrap().page.as_deref(), Some("https://x/list"));
    }

    #[test]
    fn the_save_as_dialog_keeps_a_category_and_description() {
        let t = setup("category", Duplicate::NewName);
        t.daemon.settings.lock().categories[2].remember_folder = true;
        let (id, _) = t.daemon.add_later(Download { url: "https://x/a.zip".into(), ..Download::default() });
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
        let s = t.daemon.settings.lock().clone();
        assert_eq!(s.categories[2].last_folder.as_deref(), Some(chosen.display().to_string().as_str()));
        assert_eq!(s.categories[0].last_folder, None);
        // A retry keeps both.
        let (again, _) = t.daemon.retry(id).unwrap();
        let e = t.daemon.entry(again).unwrap();
        assert_eq!(e.category.as_deref(), Some("Music"));
        assert_eq!(e.description.as_deref(), Some("the album"));
    }

    #[test]
    fn saving_a_queue_keeps_force_close() {
        let t = setup("queue_force_close", Duplicate::NewName);
        let id = t
            .daemon
            .save_queue(Queue {
                when_done: WhenDone::ShutDown,
                force_close: true,
                ..Queue::new(QueueId::new(0), "Night")
            })
            .unwrap();
        let saved = |t: &Setup| t.daemon.store.queues().iter().find(|q| q.id == id).cloned().unwrap();
        assert!(saved(&t).force_close, "a new queue keeps it");

        let mut edited = saved(&t);
        edited.force_close = false;
        t.daemon.save_queue(edited.clone()).unwrap();
        assert!(!saved(&t).force_close, "turning it off is kept");
        edited.force_close = true;
        t.daemon.save_queue(edited).unwrap();
        assert!(saved(&t).force_close, "turning it on is kept");

        // It survives a restart: the saved form reads back the same.
        let json = serde_json::to_string(&saved(&t)).unwrap();
        let back: Queue = serde_json::from_str(&json).unwrap();
        assert!(back.force_close);
    }

    #[test]
    fn the_save_as_dialog_remembers_its_choices() {
        let t = setup("remember_choices", Duplicate::Ask);
        let queue = t.daemon.save_queue(Queue::new(QueueId::new(0), "Night")).unwrap();

        // "Remember this path" for General, and "Don't ask me again" for the
        // queue of a download added for later.
        let (id, _) = t.daemon.add_later(Download { url: "https://x/notes.xyz".into(), ..Download::default() });
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
        let s = t.daemon.settings.lock().clone();
        assert!(s.general_remember_folder);
        assert_eq!(s.folders(&udm_infrastructure::settings::default_downloads_dir()).for_name("other.xyz"), chosen);
        assert_eq!(s.later_queue, Some(queue));
        // Download later now goes to that queue without being told.
        let (later, _) = t.daemon.add_later(Download { url: "https://x/more.xyz".into(), ..Download::default() });
        assert_eq!(queue::queue_of(&t.daemon.store.queues(), later), Some(queue));

        // "Remember my selection" for an address already in the list.
        let url = serve();
        let old = t.finished(&url, &t.dir.join("twice.bin"));
        let again = t.add(&url);
        assert_eq!(t.wait(again, Status::Confirming).duplicate_of, Some(old));
        t.daemon
            .confirm(Answer { duplicate: Some(Duplicate::NewName), remember_duplicate: true, ..Answer::start(again) })
            .unwrap();
        assert_eq!(t.daemon.settings.lock().if_duplicate, Duplicate::NewName);
    }

    #[test]
    fn the_quota_pauses_downloads_and_lets_them_go_again() {
        let t = setup("quota", Duplicate::NewName);
        {
            let mut s = t.daemon.settings.lock();
            s.quota_enabled = true;
            s.quota_mb = 1;
            s.quota_hours = 1;
        }
        let url = serve();
        let id = t.failed(&url);
        // 2 MB came in this minute.
        {
            let mut q = t.daemon.store.quota();
            q.started(DownloadId::new(99), ByteCount::ZERO);
            q.progress(DownloadId::new(99), ByteCount::new(2 << 20), udm_infrastructure::quota::now_minute());
        }
        t.daemon.check_quota();
        assert!(t.window_got(|m| matches!(m, Outgoing::Quota { paused: true, resumes_at: Some(_), .. })));
        assert!(t.daemon.quota.paused());
        // A download started while paused is stopped on the next check.
        t.daemon.retry(id).unwrap();
        t.daemon.check_quota();
        let (queues, downloads) = t.daemon.quota.release();
        assert!(!downloads.is_empty(), "the new download is held too");
        t.daemon.quota.hold(&queues, &downloads.into_iter().collect::<Vec<_>>());

        *t.daemon.store.quota() = Usage::default();
        t.daemon.check_quota();
        assert!(!t.daemon.quota.paused());
        assert!(t.window_got(|m| matches!(m, Outgoing::Quota { paused: false, .. })));
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
        let request =
            GrabRequest { start: format!("http://{addr}/"), depth: 0, delay_ms: 200, ..GrabRequest::default() };
        let project: udm_domain::grab::Project = serde_json::from_value(serde_json::json!({
            "name": "nightly",
            "request": request,
            "run": "explore_and_download",
        }))
        .unwrap();
        let dir = t.dir.join("projects");
        t.daemon.run_scheduled_grab(&grab::ProjectFiles(dir.clone()), project, 1);
        let saved = grab::load_project(&dir, "nightly").unwrap();
        assert_eq!(saved.seen, [format!("http://{addr}/report.pdf")]);
        assert_eq!(t.len(), 1, "the new find was added");
        assert!(
            t.daemon.store.queues().iter().any(|q| q.id == MAIN_QUEUE && q.max_concurrent == 3),
            "the grabber's files at once"
        );
        t.daemon.run_scheduled_grab(&grab::ProjectFiles(dir.clone()), saved.clone(), 2);
        assert_eq!(t.len(), 1, "nothing new the second time");

        // Downloading the ticked files reads no page.
        let checked = udm_domain::grab::Project {
            name: "ticked".into(),
            run: udm_domain::grab::RunAction::DownloadChecked,
            request: GrabRequest { start: "http://127.0.0.1:1/".into(), ..request },
            files: vec![
                udm_domain::grab::GrabFile {
                    url: format!("http://{addr}/a.pdf"),
                    name: "a.pdf".into(),
                    page: String::new(),
                    size: None,
                    text: None,
                },
                udm_domain::grab::GrabFile {
                    url: format!("http://{addr}/b.pdf"),
                    name: "b.pdf".into(),
                    page: String::new(),
                    size: None,
                    text: None,
                },
            ],
            checked: vec![true, false],
            ..saved
        };
        t.daemon.run_scheduled_grab(&grab::ProjectFiles(dir.clone()), checked, 3);
        assert_eq!(t.len(), 2, "only the ticked file was added");
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
                    let body = format!(
                        r#"{{"tag_name":"{tag}","html_url":"https://github.com/tajbinkhan/UDM/releases/tag/{tag}"}}"#
                    );
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
            Event::Update { newer: Some(r), error: None } => assert_eq!(r.version, "99.0.0"),
            other => panic!("expected a newer release: {other:?}"),
        }
        assert!(matches!(t.daemon.check_update(&serve_tag("v0.0.1")), Event::Update { newer: None, error: None }));
        // Why not comes typed, for the window to translate.
        let failed = t.daemon.check_update("http://127.0.0.1:1/none");
        assert!(
            matches!(
                failed,
                Event::Update {
                    error: Some(Failure::Service(ServiceError::Update(udm_domain::update::UpdateError::Unreachable(
                        _
                    )))),
                    ..
                }
            ),
            "{failed:?}"
        );
        assert!(t.daemon.settings.lock().last_update_check > 0, "the check was noted");
    }

    #[test]
    fn options_hears_where_the_video_tools_are() {
        let t = setup("tools", Duplicate::NewName);
        let tools = t.daemon.tools();
        let names: Vec<_> = tools.iter().map(|s| s.tool).collect();
        assert_eq!(names, udm_domain::tools::Tool::ALL);
        assert!(tools.iter().all(|s| !s.command.is_empty()));
    }

    #[test]
    fn a_dropped_link_always_gets_the_save_as_dialog() {
        let t = setup("ask_flag", Duplicate::NewName);
        assert!(!t.daemon.settings.lock().ask_where_to_save);
        let (id, _) = t.daemon.add(Download { url: serve().into(), ask: Some(true), ..Download::default() });
        t.wait(id, Status::Confirming);
    }

    #[test]
    fn some_types_start_without_the_dialog_and_add_only_waits() {
        let t = setup("auto_start", Duplicate::NewName);
        {
            let mut s = t.daemon.settings.lock();
            s.ask_where_to_save = true;
            s.auto_start_types = vec!["bin".into()];
        }
        let id = t.add(&serve());
        assert_eq!(t.wait(id, Status::Done).path.map(PathBuf::from), Some(t.dir.join("f.bin")));

        t.daemon.settings.lock().add_only = true;
        let id = t.add(&serve());
        assert_eq!(t.daemon.transfers.status(id), Some(Status::Waiting));
        assert_eq!(queue::queue_of(&t.daemon.store.queues(), id), Some(MAIN_QUEUE));
    }

    #[test]
    fn a_download_removed_from_its_queue_stays_listed_and_waits_for_a_hand_start() {
        let t = setup("remove_from_queue", Duplicate::NewName);
        let (id, _) = t.daemon.add_later(Download { url: serve().into(), ..Download::default() });
        let (kept, _) = t.daemon.add_later(Download { url: format!("{}#kept", serve()).into(), ..Download::default() });
        assert_eq!(queue::queue_of(&t.daemon.store.queues(), id), Some(MAIN_QUEUE));

        t.daemon.remove_from_queue(id).unwrap();
        assert_eq!(queue::queue_of(&t.daemon.store.queues(), id), None, "it left the queue");
        assert_eq!(t.daemon.transfers.status(id), Some(Status::Waiting), "and still waits in the list");
        assert!(t.daemon.remove_from_queue(id).is_err(), "it is in no queue now");

        // Starting the queue runs what is left in it, not the removed one.
        t.daemon.start_queue(MAIN_QUEUE, None).unwrap();
        t.daemon.tick();
        t.wait(kept, Status::Done);
        t.daemon.tick();
        assert_eq!(t.daemon.transfers.status(id), Some(Status::Waiting), "no queue started it");

        // Move to queue puts it back.
        t.daemon.move_to_queue(id, MAIN_QUEUE, None).unwrap();
        assert_eq!(queue::queue_of(&t.daemon.store.queues(), id), Some(MAIN_QUEUE));
        t.daemon.remove_from_queue(id).unwrap();

        // Starting it by hand still works.
        t.daemon.confirm(Answer::start(id)).unwrap();
        t.wait(id, Status::Done);
    }

    /// A server whose one file is `body` with ETag `etag`, both changeable
    /// meanwhile; with no ETag set it answers 500.
    fn serve_versions(file: Arc<Mutex<(Vec<u8>, Option<String>)>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let file = Arc::clone(&file);
                thread::spawn(move || {
                    let mut req = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut buf) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => req.extend_from_slice(&buf[..n]),
                        }
                    }
                    let (body, etag) = file.lock().unwrap().clone();
                    let Some(etag) = etag else {
                        let _ =
                            stream.write_all(b"HTTP/1.1 500 Oops\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        return;
                    };
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: {etag}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    if !req.starts_with(b"HEAD") {
                        let _ = stream.write_all(&body);
                    }
                });
            }
        });
        format!("http://{addr}/mirror.bin")
    }

    impl Setup {
        /// Start queue `q` and run the scheduler until the run ends.
        fn run_queue(&self, q: u64) {
            self.daemon.start_queue(QueueId::new(q), None).unwrap();
            let until = Instant::now() + Duration::from_secs(10);
            while self.daemon.store.queues().iter().any(|x| x.id == QueueId::new(q) && x.running) {
                assert!(Instant::now() < until, "the run never ended");
                self.daemon.tick();
                thread::sleep(Duration::from_millis(20));
            }
        }
    }

    #[test]
    fn a_synchronizing_queue_downloads_again_only_what_changed() {
        let t = setup("sync_queue", Duplicate::NewName);
        let file = Arc::new(Mutex::new((b"version 1".to_vec(), Some("\"v1\"".to_string()))));
        let url = serve_versions(Arc::clone(&file));
        let id = t.add(&url);
        let first = t.wait(id, Status::Done);
        let path = PathBuf::from(first.path.clone().unwrap());
        assert_eq!(first.validator.as_ref().and_then(|v| v.etag.as_deref()), Some("\"v1\""), "the ETag is kept");

        // A one-time queue takes only what is still to do.
        assert!(t.daemon.move_to_queue(id, MAIN_QUEUE, None).is_err());
        let mut mirror = Queue::new(QueueId::new(0), "Mirror");
        mirror.kind = queue::QueueKind::Synchronize;
        let q = t.daemon.save_queue(mirror.clone()).unwrap();
        t.daemon.move_to_queue(id, q, None).unwrap();
        assert_eq!(t.daemon.transfers.status(id), Some(Status::Done), "it stays finished in the queue");

        // Unchanged: left alone.
        t.run_queue(q.get());
        assert_eq!(t.daemon.transfers.status(id), Some(Status::Done));
        assert_eq!(std::fs::read(&path).unwrap(), b"version 1");
        assert_eq!(queue::queue_of(&t.daemon.store.queues(), id), Some(q), "a finished download stays in its queue");

        // Same size, new ETag: downloaded again over the same file, in the
        // same row.
        *file.lock().unwrap() = (b"version 2".to_vec(), Some("\"v2\"".to_string()));
        t.run_queue(q.get());
        let again = t.wait(id, Status::Done);
        assert_eq!(again.path, first.path, "the same file");
        assert_eq!(std::fs::read(&path).unwrap(), b"version 2");
        assert_eq!(again.validator.as_ref().and_then(|v| v.etag.as_deref()), Some("\"v2\""));
        assert_eq!(std::fs::read_dir(&t.dir).unwrap().count(), 1, "no second copy or leftover");
        assert_eq!(queue::queue_of(&t.daemon.store.queues(), id), Some(q), "and it stays in the queue");

        // A check that fails keeps the file.
        file.lock().unwrap().1 = None;
        t.run_queue(q.get());
        let kept = t.wait(id, Status::Done);
        assert_eq!(std::fs::read(&path).unwrap(), b"version 2");
        assert!(kept.note.is_some_and(|n| n.contains("newer copy")), "it says why");

        // A gone file is downloaded again.
        file.lock().unwrap().1 = Some("\"v2\"".to_string());
        std::fs::remove_file(&path).unwrap();
        t.run_queue(q.get());
        t.wait(id, Status::Done);
        assert_eq!(std::fs::read(&path).unwrap(), b"version 2");

        // Made one-time again, it lets its finished downloads go.
        mirror.id = q;
        mirror.kind = queue::QueueKind::OneTime;
        t.daemon.save_queue(mirror).unwrap();
        assert_eq!(queue::queue_of(&t.daemon.store.queues(), id), None);
    }

    #[test]
    fn a_synchronizing_project_downloads_changed_finds_again() {
        let t = setup("sync_project", Duplicate::Existing);
        let file = Arc::new(Mutex::new((b"manual 1".to_vec(), Some("\"m1\"".to_string()))));
        let url = serve_versions(Arc::clone(&file));
        let id = t.add(&url);
        let path = PathBuf::from(t.wait(id, Status::Done).path.unwrap());
        let found = udm_domain::grab::GrabFile {
            url: url.clone(),
            name: "mirror.bin".into(),
            page: String::new(),
            size: None,
            text: None,
        };
        let mut project: udm_domain::grab::Project = serde_json::from_value(serde_json::json!({
            "name": "manuals",
            "request": GrabRequest { start: "http://127.0.0.1:1/".into(), ..GrabRequest::default() },
            "run": "download_checked",
        }))
        .unwrap();
        project.files = vec![found];
        project.checked = vec![true];
        let dir = t.dir.join("projects");

        // One-time: what is in the list already is not looked at.
        *file.lock().unwrap() = (b"manual 2!".to_vec(), Some("\"m2\"".to_string()));
        t.daemon.run_scheduled_grab(&grab::ProjectFiles(dir.clone()), project.clone(), 1);
        assert_eq!(t.len(), 1);
        assert_eq!(t.daemon.transfers.status(id), Some(Status::Done));
        assert_eq!(std::fs::read(&path).unwrap(), b"manual 1");

        // Synchronizing: it is checked and downloaded again in its row, even
        // where a duplicate would get a new name.
        t.daemon.settings.lock().if_duplicate = Duplicate::NewName;
        project.kind = queue::QueueKind::Synchronize;
        t.daemon.run_scheduled_grab(&grab::ProjectFiles(dir.clone()), project, 2);
        assert_eq!(t.len(), 1, "no new row");
        let until = Instant::now() + Duration::from_secs(10);
        while std::fs::read(&path).unwrap() != b"manual 2!" {
            assert!(Instant::now() < until, "never downloaded again: {:?}", t.daemon.entry(id));
            t.daemon.tick();
            thread::sleep(Duration::from_millis(20));
        }
        t.wait(id, Status::Done);
        assert_eq!(
            grab::load_project(&dir, "manuals").unwrap().kind,
            queue::QueueKind::Synchronize,
            "the type is saved"
        );
    }

    #[test]
    fn a_scheduled_project_stops_its_downloads_at_its_stop_time() {
        let t = setup("grab_stop_time", Duplicate::NewName);
        let url = serve_ranges(Arc::new(vec![7u8; 4 * 1024 * 1024]), Arc::new(AtomicU64::new(0)));
        let today = udm_infrastructure::clock::local_now().day;
        let mut project: udm_domain::grab::Project = serde_json::from_value(serde_json::json!({
            "name": "slow",
            "request": GrabRequest { start: "http://127.0.0.1:1/".into(), ..GrabRequest::default() },
            "run": "download_checked",
            "schedule": { "start": "00:00", "stop": "01:00" },
        }))
        .unwrap();
        project.files = vec![udm_domain::grab::GrabFile {
            url,
            name: "slow.bin".into(),
            page: String::new(),
            size: None,
            text: None,
        }];
        project.checked = vec![true];
        let dir = t.dir.join("projects");
        // Tomorrow's run, so its stop time is still ahead.
        t.daemon.run_scheduled_grab(&grab::ProjectFiles(dir.clone()), project, today + 1);
        let main = || t.daemon.store.queues().iter().find(|q| q.id == MAIN_QUEUE).cloned().unwrap();
        assert_eq!(main().stop_at_once, Some((today + 1) * 1440 + 60), "the queue's run has the project's stop time");
        assert!(main().running);
        let id = *main().order.first().unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        while t.daemon.transfers.status(id) != Some(Status::Downloading) {
            assert!(Instant::now() < until, "never started: {:?}", t.daemon.entry(id));
            t.daemon.tick();
            thread::sleep(Duration::from_millis(20));
        }

        // The stop time comes: the queue stops, its download waits again.
        t.daemon
            .store
            .update_queue(MAIN_QUEUE, |q| {
                q.stop_at_once = Some(udm_infrastructure::clock::local_now().total());
            })
            .unwrap();
        t.daemon.tick();
        assert!(!main().running, "the queue stopped");
        assert_eq!(main().stop_at_once, None, "the stop time was for that run only");
        t.wait(id, Status::Waiting);
        assert!(t.daemon.power.running().is_none(), "a stopped run does not do its when-done action");
    }

    #[test]
    fn a_scheduled_project_leaves_a_run_the_user_started_alone() {
        let t = setup("grab_user_run", Duplicate::NewName);
        let url = serve_ranges(Arc::new(vec![7u8; 4 * 1024 * 1024]), Arc::new(AtomicU64::new(0)));
        let today = udm_infrastructure::clock::local_now().day;
        let mut project: udm_domain::grab::Project = serde_json::from_value(serde_json::json!({
            "name": "joins",
            "request": GrabRequest { start: "http://127.0.0.1:1/".into(), ..GrabRequest::default() },
            "run": "download_checked",
            "schedule": { "start": "00:00", "stop": "01:00" },
            "force_close": true,
        }))
        .unwrap();
        project.files = vec![udm_domain::grab::GrabFile {
            url,
            name: "joins.bin".into(),
            page: String::new(),
            size: None,
            text: None,
        }];
        project.checked = vec![true];
        t.daemon.start_queue(MAIN_QUEUE, None).unwrap();
        t.daemon.run_scheduled_grab(&grab::ProjectFiles(t.dir.join("projects").clone()), project, today + 1);
        let main = t.daemon.store.queues().iter().find(|q| q.id == MAIN_QUEUE).cloned().unwrap();
        assert!(main.running);
        assert_eq!(main.order.len(), 1, "the project's file joined the run");
        assert_eq!(main.stop_at_once, None, "the user's run keeps going past the project's stop time");
        assert_eq!(main.force_close_once, None);
        assert_eq!(main.when_done_once, None);
    }

    #[test]
    fn a_scheduled_project_force_closes_when_asked() {
        // Never really shut down, even if a check below fails.
        std::env::set_var(udm_infrastructure::power_control::POWER_DRY_RUN_ENV, "1");
        let t = setup("grab_force_close", Duplicate::NewName);
        let dir = t.dir.join("projects");
        let mut project: udm_domain::grab::Project = serde_json::from_value(serde_json::json!({
            "name": "shutdown",
            "request": GrabRequest { start: "http://127.0.0.1:1/".into(), ..GrabRequest::default() },
            "run": "download_checked",
            "when_done": "shut_down",
            "force_close": true,
        }))
        .unwrap();
        assert!(project.force_close, "saved with the project");
        let countdown_forces = |t: &Setup| {
            let forced = t.daemon.power.running().map(|r| (r.action, r.force_close));
            t.daemon.cancel_countdown();
            forced
        };

        // Nothing to download: done at once.
        t.daemon.run_scheduled_grab(&grab::ProjectFiles(dir.clone()), project.clone(), 1);
        assert_eq!(countdown_forces(&t), Some((WhenDone::ShutDown, true)));

        // Done once the queue is, which does not force on its own.
        let file = Arc::new(Mutex::new((b"data".to_vec(), Some("\"d1\"".to_string()))));
        let url = serve_versions(file);
        project.files =
            vec![udm_domain::grab::GrabFile { url, name: "d.bin".into(), page: String::new(), size: None, text: None }];
        project.checked = vec![true];
        t.daemon.run_scheduled_grab(&grab::ProjectFiles(dir.clone()), project, 2);
        assert_eq!(t.daemon.store.queues()[0].force_close_once, Some(true));
        let until = Instant::now() + Duration::from_secs(10);
        while t.daemon.store.queues().iter().any(|q| q.id == MAIN_QUEUE && q.running) {
            assert!(Instant::now() < until, "the run never ended");
            t.daemon.tick();
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(countdown_forces(&t), Some((WhenDone::ShutDown, true)));
        let main = t.daemon.store.queues()[0].clone();
        assert!(
            !main.force_close && main.force_close_once.is_none(),
            "the queue's own choice is back for its next run"
        );
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
                let mut s = t.daemon.settings.lock();
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
            while t.daemon.entry(id).unwrap().done < ByteCount::new(wanted) {
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
            let mut s = t.daemon.settings.lock();
            s.ask_where_to_save = true;
            s.start_while_asking = true;
        }
        let body: Arc<Vec<u8>> = Arc::new(vec![7u8; 1024 * 1024]);
        let id = t.add(&serve_ranges(body, Arc::new(AtomicU64::new(0))));
        t.wait(id, Status::Confirming);
        let until = Instant::now() + Duration::from_secs(30);
        while t
            .daemon
            .transfers
            .early()
            .early_of(id)
            .is_none_or(|s| t.daemon.transfers.early().runs()[&s].file.is_none())
        {
            assert!(Instant::now() < until, "the early download never started");
            thread::sleep(Duration::from_millis(20));
        }
        let shadow = t.daemon.transfers.early().early_of(id).unwrap();
        let dir = t.daemon.transfers.early().runs()[&shadow].dir.clone();
        t.daemon.cancel(id);
        t.wait(id, Status::Canceled);
        let until = Instant::now() + Duration::from_secs(10);
        while dir.exists() || t.daemon.transfers.early().runs().contains_key(&shadow) {
            assert!(Instant::now() < until, "the early download was not dropped");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_finished_video_converts_into_a_row_of_its_own() {
        let Some(ffmpeg) = settings::ffmpeg(&Settings::default()) else {
            eprintln!("no ffmpeg; skipped");
            return;
        };
        let t = setup("convert", Duplicate::NewName);
        let clip = t.dir.join("talk.mkv");
        let id = t.finished("https://x/talk.mkv", &clip);
        let made = udm_infrastructure::platform::no_window(&mut std::process::Command::new(&ffmpeg))
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=1:size=160x120:rate=10",
            ])
            .args(["-f", "lavfi", "-i", "sine=duration=1", "-c:v", "libx264", "-c:a", "aac", "-shortest"])
            .arg(&clip)
            .status()
            .unwrap();
        assert!(made.success());
        t.daemon.convert(id, udm_domain::media::Target::M4a, true).unwrap();
        let new = t.daemon.store.table().max_id();
        let done = t.wait(new, Status::Done);
        let out = PathBuf::from(done.path.unwrap());
        assert_eq!(out, t.dir.join("talk.m4a"));
        assert!(std::fs::metadata(&out).unwrap().len() > 0);
        assert_eq!(done.note.as_deref(), Some("Converted from talk.mkv"));
        assert!(t.daemon.entry(id).is_none() && !clip.exists(), "the original went, as asked");
        assert!(t.daemon.convert(DownloadId::new(new.get() + 100), udm_domain::media::Target::Mp3, false).is_err());
    }

    /// Each request's path and the Cookie header it brought.
    type CookieLog = Arc<Mutex<Vec<(String, Option<String>)>>>;

    /// A page server that serves `pages` (path, body) and keeps the Cookie
    /// header each request brought, by path.
    fn serve_pages(pages: Vec<(&'static str, String)>) -> (u16, CookieLog) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => req.extend_from_slice(&buf[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&req).to_string();
                let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
                let cookie = text
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("cookie:"))
                    .map(|l| l[7..].trim().to_string());
                log.lock().unwrap().push((path.clone(), cookie));
                let (status, body) = match pages.iter().find(|(p, _)| *p == path) {
                    Some((_, body)) => ("200 OK", body.clone()),
                    None => ("404 Not Found", String::new()),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body.as_bytes());
            }
        });
        (port, seen)
    }

    #[test]
    fn a_crawl_sends_the_login_sites_cookies_to_that_site_only() {
        let (other, other_seen) = serve_pages(vec![("/elsewhere", "<p>other</p>".to_string())]);
        let site_pages = vec![
            ("/", format!("<a href=\"/members\">m</a> <a href=\"http://localhost:{other}/elsewhere\">o</a>")),
            ("/members", "<a href=\"a.pdf\">a</a>".to_string()),
        ];
        let (site, site_seen) = serve_pages(site_pages);
        let t = setup("grab_login_cookies", Duplicate::NewName);
        let req = GrabRequest {
            start: format!("http://127.0.0.1:{site}/"),
            depth: 2,
            same_site: false,
            delay_ms: 200,
            login_url: Some(format!("http://127.0.0.1:{site}/login")),
            // No extension is connected, so the pasted cookies are used.
            login_cookies: Some("Cookie: session=abc".into()),
            ..GrabRequest::default()
        };
        let stop = AtomicBool::new(false);
        let login = t.daemon.login_cookies(&req);
        let out = t.daemon.crawler.crawl(&req, login.as_deref(), &stop, &mut |_| {});
        assert_eq!(out.files.len(), 1, "the members page was read: {:?}", out.files);
        let site_seen = site_seen.lock().unwrap().clone();
        let pages: Vec<_> = site_seen.iter().filter(|(p, _)| p != "/robots.txt").collect();
        assert!(pages.len() >= 2 && pages.iter().all(|(_, c)| c.as_deref() == Some("session=abc")), "{site_seen:?}");
        let other_seen = other_seen.lock().unwrap().clone();
        assert!(other_seen.iter().any(|(p, _)| p == "/elsewhere"), "the other site was read: {other_seen:?}");
        assert!(other_seen.iter().all(|(_, c)| c.is_none()), "no cookies to another host: {other_seen:?}");
        // The files it adds carry them on the login site only.
        let elsewhere =
            udm_domain::grab::GrabFile { url: format!("http://localhost:{other}/b.pdf"), ..out.files[0].clone() };
        let ids = t.daemon.download_in_queue(
            &req,
            login.as_deref(),
            &[out.files[0].clone(), elsewhere],
            queue::MAIN_QUEUE,
            false,
        );
        let requests = t.daemon.store.requests();
        let cookies: Vec<_> = ids.iter().map(|id| requests.get(id).and_then(|d| d.cookies.clone())).collect();
        assert_eq!(cookies, [Some("session=abc".to_string()), None]);
    }

    /// `udm-cli`'s connection, in this process: its commands go to the
    /// daemon's server code as frames, and what the server queues for it
    /// comes back as events, as over the socket.
    struct Program {
        daemon: Arc<Daemon>,
        connections: Arc<Clients>,
        client: ClientId,
        reply: Reply,
        frames: Mutex<Option<Receiver<Vec<u8>>>>,
    }

    impl Program {
        fn new(t: &Setup, client: u64) -> Self {
            let client = ClientId::new(client);
            let (reply, frames) = t.connections.connect(client);
            Program {
                daemon: Arc::clone(&t.daemon),
                connections: Arc::clone(&t.connections),
                client,
                reply,
                frames: Mutex::new(Some(frames)),
            }
        }
    }

    impl udm_application::daemon::DaemonClient for Program {
        fn connect(&self, _restart: bool, mut on_event: Box<dyn FnMut(Event) + Send>) {
            let frames = self.frames.lock().unwrap().take().expect("connected once");
            on_event(Event::Connected);
            thread::spawn(move || {
                for frame in frames {
                    if let Ok(msg) = serde_json::from_slice::<Outgoing>(&frame) {
                        on_event(udm_infrastructure::wire::event(msg));
                    }
                }
            });
            self.send(udm_application::daemon::Command::Hello { version: None, client: Some("udm-cli".into()) });
        }

        fn send(&self, command: udm_application::daemon::Command) -> bool {
            let raw = serde_json::to_vec(&udm_infrastructure::wire::incoming(command)).unwrap();
            server::receive(&self.connections, &Served(Arc::clone(&self.daemon)), self.client, &raw, &self.reply);
            true
        }

        fn run_command_line(
            &self,
            _command: udm_domain::download::CommandLine,
        ) -> Result<bool, udm_application::daemon::DaemonError> {
            unimplemented!("udm-cli sends no command line")
        }
    }

    /// Progress nobody watches.
    struct Unwatched;

    impl udm_application::remote::Watch for Unwatched {
        fn added(&mut self, _id: DownloadId) {}
        fn started(&mut self, _path: &str, _total: Option<ByteCount>, _connections: usize) {}
        fn progress(&mut self, _done: ByteCount, _total: Option<ByteCount>) {}
        fn needs_answer(&mut self, _path: Option<&str>) {}
    }

    #[test]
    fn the_command_line_tool_downloads_a_file_checks_it_and_forgets_it() {
        use udm_application::remote::{ForgetDownload, GetFile, GetRequest, Got, ListDownloads, Session};
        use udm_domain::ports::FileHashes;
        let t = setup("cli_get", Duplicate::Ask);
        // Asked for by a program, it starts at once even when the settings
        // say to ask.
        t.daemon.settings.lock().ask_where_to_save = true;
        let url = serve();
        let program = Program::new(&t, 7);
        let session = Session::open(&program, Duration::from_secs(10)).unwrap();
        let mut request = GetRequest::new(udm_domain::address::Address::parse(&url).unwrap());
        let folder = t.dir.join("from the tool");
        request.folder = Some(folder.display().to_string());
        request.connections = Some(3);
        let reference = t.dir.join("reference");
        std::fs::write(&reference, BODY).unwrap();
        let algorithm = udm_domain::checksum::Algorithm::Sha256;
        let digest = udm_infrastructure::hash::DiskHashes.digest(&reference, algorithm).unwrap();
        let sum = format!("sha256:{}", udm_domain::checksum::to_hex(&digest));
        request.checksum = Some(udm_domain::checksum::Checksum::parse(&sum).unwrap());

        let got = GetFile::new(&session, &udm_infrastructure::hash::DiskHashes).run(&request, &mut Unwatched);
        let Ok(Got::Saved { id, path, .. }) = got else {
            panic!("not saved: {got:?}");
        };
        assert_eq!(Path::new(&path), folder.join("f.bin"));
        assert_eq!(std::fs::read(&path).unwrap(), BODY);

        // The same address again is a second file, not a question.
        let again = GetFile::new(&session, &udm_infrastructure::hash::DiskHashes).run(&request, &mut Unwatched);
        assert!(
            matches!(&again, Ok(Got::Saved { path, .. }) if Path::new(path) == folder.join("f (1).bin")),
            "{again:?}"
        );

        let listing = ListDownloads::new(&session).run().unwrap();
        assert_eq!(listing.downloads.iter().filter(|e| e.status == Status::Done).count(), 2);
        ForgetDownload::new(&session).run(id, true).unwrap();
        assert!(t.daemon.entry(id).is_none());
    }

    #[test]
    fn the_command_line_tool_pauses_and_resumes_a_running_download() {
        use udm_application::remote::{GetFile, GetRequest, Got, PauseDownload, ResumeDownload, Resumed, Session};
        let t = setup("cli_pause", Duplicate::NewName);
        let body = Arc::new(vec![7u8; 4 * 1024 * 1024]);
        let url = serve_ranges(Arc::clone(&body), Arc::new(AtomicU64::new(0)));
        let program = Program::new(&t, 8);
        let session = Session::open(&program, Duration::from_secs(10)).unwrap();
        let request = GetRequest {
            follow: false,
            connections: Some(2),
            ..GetRequest::new(udm_domain::address::Address::parse(&url).unwrap())
        };
        let got = GetFile::new(&session, &udm_infrastructure::hash::DiskHashes).run(&request, &mut Unwatched);
        let Ok(Got::Added(id)) = got else {
            panic!("not added: {got:?}");
        };
        let running = t.wait(id, Status::Downloading);
        assert_eq!(running.connections, 2);
        PauseDownload::new(&session).run(id).unwrap();
        assert_eq!(t.daemon.entry(id).map(|e| e.status), Some(Status::Canceled));
        let Ok(Resumed::Retried { new, .. }) = ResumeDownload::new(&session).run(id) else {
            panic!("not resumed");
        };
        t.wait(new, Status::Downloading);
        // The retry keeps the download's own number of connections.
        assert_eq!(t.daemon.store.requests().get(&new).and_then(|d| d.connections), Some(2));
        assert_eq!(ResumeDownload::new(&session).run(new), Ok(Resumed::Running(new)));
        PauseDownload::new(&session).run(new).unwrap();
    }

    #[test]
    fn an_assistant_adds_lists_and_removes_downloads_through_the_mcp_server() {
        use serde_json::{json, Value};
        use udm_application::remote::Assistant;
        use udm_infrastructure::mcp::Server;
        let t = setup("mcp", Duplicate::Ask);
        // An assistant's download starts with no dialog even under ask.
        t.daemon.settings.lock().ask_where_to_save = true;
        let url = serve();
        let program = Program::new(&t, 9);
        let mut assistant = Assistant::new(&program, Duration::from_secs(10));
        let mut server = Server::new(&mut assistant, "test");
        let mut ask = |id: u64, method: &str, params: Value| -> Value {
            let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
            let answer = server.handle(&line).unwrap();
            assert_eq!(answer["id"], id, "{answer}");
            answer["result"].clone()
        };
        let init = ask(1, "initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {} }));
        assert_eq!(init["protocolVersion"], "2025-06-18");
        let call = |name: &str, arguments: Value| json!({ "name": name, "arguments": arguments });

        // One to start now, into a folder of its own.
        let folder = t.dir.join("from the assistant");
        let added = ask(2, "tools/call", call("add_download", json!({ "url": url, "folder": folder })));
        assert_eq!(added["isError"], false, "{added}");
        let now = DownloadId::new(added["structuredContent"]["id"].as_u64().unwrap());
        let done = t.wait(now, Status::Done);
        assert_eq!(done.path.map(PathBuf::from), Some(folder.join("f.bin")));

        // One to wait in the main queue.
        let queued = ask(3, "tools/call", call("add_download", json!({ "url": url, "queue": MAIN_QUEUE.get() })));
        let later = DownloadId::new(queued["structuredContent"]["id"].as_u64().unwrap());
        assert_eq!(t.daemon.entry(later).map(|e| e.status), Some(Status::Waiting));

        let listed = ask(4, "tools/call", call("list_downloads", json!({ "status": "waiting" })));
        let waiting = listed["structuredContent"]["downloads"].as_array().unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0]["id"], later.get());
        let main = &listed["structuredContent"]["queues"][0];
        assert!(main["downloads"].as_array().unwrap().contains(&json!(later.get())));

        // Removing keeps the file unless deleting it is asked for.
        let removed = ask(5, "tools/call", call("remove_download", json!({ "id": now.get() })));
        assert_eq!(removed["structuredContent"]["files_deleted"], false, "{removed}");
        assert!(t.daemon.entry(now).is_none());
        assert!(folder.join("f.bin").exists());
        let refused = ask(6, "tools/call", call("pause_download", json!({ "id": now.get() })));
        assert_eq!(refused["isError"], true);
        ask(7, "tools/call", call("remove_download", json!({ "id": later.get() })));
        assert!(t.daemon.entry(later).is_none());
    }

    /// A phone pairs through the code the window asked the daemon for, and
    /// then lists, adds, pauses, resumes and removes downloads through the
    /// real service, until the window removes it.
    #[test]
    fn a_paired_phone_controls_the_daemon_until_it_is_removed() {
        phone_controls(Seat::Desktop, "desktop");
    }

    /// A server's daemon (`--server`, no window) serves the phone page the
    /// same way, its pairing asked for as `udm-cli pair` asks.
    #[test]
    fn a_servers_daemon_serves_the_phone_page_too() {
        phone_controls(Seat::Server, "server");
    }

    fn phone_controls(seat: Seat, name: &str) {
        use serde_json::{json, Value};
        use udm_application::daemon::Command;
        use udm_application::pairing::Phone;
        use udm_domain::id::DeviceId;
        use udm_domain::settings::Notify;
        use udm_infrastructure::web_remote::{http, serve};

        let dir = std::env::temp_dir().join(format!("udm_phone_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let Built { daemon, clients: connections, remote, .. } =
            new_daemon(Paths { settings: None, store: StoreFiles::default(), remote: Some(dir.join("remote")) }, seat);
        {
            let mut s = daemon.settings.lock();
            s.download_dir = Some(dir.display().to_string());
            s.ask_where_to_save = false;
            s.notify = Notify::Off;
            s.if_duplicate = Duplicate::NewName;
        }
        let (_window_reply, _window) = connections.connect(ClientId::new(1));
        connections.add_window(ClientId::new(1));
        let daemon = Arc::new(daemon);
        let requests: Arc<dyn Requests> = Arc::new(Served(Arc::clone(&daemon)));
        let gate = Arc::new(Phone::new(Arc::clone(&daemon.pairing), Arc::clone(&requests), Box::new(next_client)));
        remote.serve_with(Arc::clone(&gate) as _);

        // Nothing to pair with while remote control is off.
        let (reply, frames) = server::reply_channel();
        requests.handle(ClientId::new(1), Command::StartPairing, &reply);
        let heard = |frames: &Receiver<Vec<u8>>| -> Vec<Outgoing> {
            frames.try_iter().filter_map(|f| serde_json::from_slice::<Outgoing>(&f).ok()).collect()
        };
        assert!(heard(&frames).iter().any(|m| matches!(m, Outgoing::Error { .. })));

        remote.apply(&Settings {
            remote_enabled: true,
            remote_port: 0,
            remote_addresses: vec!["127.0.0.1".into()],
            ..Settings::default()
        });
        requests.handle(ClientId::new(1), Command::StartPairing, &reply);
        let offer = heard(&frames)
            .into_iter()
            .find_map(|m| match m {
                Outgoing::Remote { state } => state.pairing,
                _ => None,
            })
            .expect("a pairing code");
        assert!(offer.link.starts_with("https://127.0.0.1:") && offer.link.ends_with(offer.code.as_str()));

        let ask = |method: &str, path: &str, credential: Option<&str>, body: Value| -> (u16, Value) {
            let auth = credential.map(|c| format!("Authorization: Bearer {c}\r\n")).unwrap_or_default();
            let body = if body.is_null() { String::new() } else { body.to_string() };
            let raw = format!(
                "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let request =
                http::read_request(&mut raw.as_bytes(), &mut Vec::new(), Instant::now() + Duration::from_secs(5))
                    .unwrap();
            let answer = serve::route(&request, "127.0.0.1".parse().unwrap(), &*gate);
            (answer.status, serde_json::from_slice(&answer.body).unwrap_or(Value::Null))
        };
        let wait = |id: DownloadId, status: Status| {
            let until = Instant::now() + Duration::from_secs(10);
            while daemon.entry(id).is_none_or(|e| e.status != status) {
                assert!(Instant::now() < until, "#{id} never became {status:?}: {:?}", daemon.entry(id));
                thread::sleep(Duration::from_millis(20));
            }
        };

        assert_eq!(ask("GET", "/api/downloads", None, Value::Null).0, 401);
        let (status, paired) = ask("POST", "/api/pair", None, json!({ "code": offer.code.as_str(), "name": "Phone" }));
        assert_eq!(status, 200, "{paired}");
        let me = paired["credential"].as_str().unwrap().to_string();
        assert_eq!(ask("POST", "/api/pair", None, json!({ "code": offer.code.as_str() })).0, 403, "used up");

        let url = serve_ranges(Arc::new(vec![7u8; 4 * 1024 * 1024]), Arc::new(AtomicU64::new(0)));
        let (status, added) = ask("POST", "/api/add", Some(&me), json!({ "url": url }));
        assert_eq!(status, 200, "{added}");
        let id = DownloadId::new(added["id"].as_u64().unwrap());
        wait(id, Status::Downloading);
        let (_, list) = ask("GET", "/api/downloads", Some(&me), Value::Null);
        let row = list["downloads"].as_array().unwrap().iter().find(|d| d["id"] == id.get()).cloned().unwrap();
        assert_eq!((row["name"].as_str(), row["host"].as_str()), (Some("big.bin"), Some("127.0.0.1")));
        assert!(!list.to_string().contains(&dir.display().to_string()), "no folders");

        assert_eq!(ask("POST", "/api/pause", Some(&me), json!({ "id": id.get() })).0, 200);
        wait(id, Status::Canceled);
        assert_eq!(ask("POST", "/api/resume", Some(&me), json!({ "id": id.get() })).0, 200);
        let until = Instant::now() + Duration::from_secs(10);
        let resumed = loop {
            let running = daemon.store.table().entries().find(|e| e.status == Status::Downloading).cloned();
            if let Some(e) = running {
                break e.id;
            }
            assert!(Instant::now() < until, "never resumed");
            thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(ask("POST", "/api/pause", Some(&me), json!({ "id": resumed.get() })).0, 200);
        wait(resumed, Status::Canceled);
        assert_eq!(ask("POST", "/api/remove", Some(&me), json!({ "id": resumed.get() })).0, 200);
        assert!(daemon.entry(resumed).is_none());

        requests.handle(ClientId::new(1), Command::RemoveDevice { id: DeviceId::new(1) }, &reply);
        let state = heard(&frames).into_iter().find_map(|m| match m {
            Outgoing::Remote { state } => Some(state),
            _ => None,
        });
        assert!(state.is_some_and(|s| s.devices.is_empty()));
        assert_eq!(ask("GET", "/api/downloads", Some(&me), Value::Null).0, 401, "removed at once");
        remote.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
