# AGENTS.md

This file is the single source of truth for how any AI agent or developer writes code in UIDM. Read it before making any change. Rules use MUST, MUST NOT and SHOULD in their usual sense.

The architecture below is the target. The code today is one crate that does not follow it yet. Section 16 lists where it deviates. New code MUST follow these rules. Existing code MUST NOT be restructured to match them unless a task asks for it.

## 1. Project overview

- UIDM is a Rust desktop download manager that follows Internet Download Manager's behavior (see `GLOSSARY.md` for the names of its windows).
- It runs as three processes:
  - `uidm-host`, the native-messaging host the browser launches. It relays browser messages to the daemon.
  - `uidm-daemon`, the long-lived process that owns the download engine, so downloads outlive the browser.
  - `uidm-gui`, the desktop window. It is a client of the daemon, like the host.
- The browser extension lives in `extension/` (JavaScript). It is outside the Rust architecture but speaks the protocol defined in Rust.
- Architecture: Hexagonal (Ports and Adapters) as a Cargo workspace, with a UI layer at the edge.
- UI framework: **egui**, through `eframe` 0.31 (glow/OpenGL backend), with `egui_extras` for the download table, `tray-icon` for the tray, and `rfd` (behind the `FilePicker` port) for native file and folder pickers.

## 2. Workspace layout

```
crates/
  domain/          entities, value objects, business rules, port traits
  application/     use cases that coordinate the domain through ports
  infrastructure/  adapters: file storage, HTTP/FTP engine, OS integration, IPC
  ui/              egui views, UI state, messages, event handling
  app/             binaries: uidm-host, uidm-daemon, uidm-gui (main.rs files, config, wiring)
```

How UIDM's current concerns map onto these crates:

| Concern | Crate |
| --- | --- |
| Download states, queues and schedules, categories, file-name rules, batch URL patterns | `domain` |
| Add a download, pause, resume, run a queue, grab a site, export the list | `application` |
| HTTP/FTP transfers, HLS/DASH/HDS, settings and history files, DPAPI, registry, tray, toasts, power, recycle bin, browser registration, daemon IPC | `infrastructure` |
| Main window, save-as dialog, download windows, Options, translations | `ui` |
| Process startup, logging setup, wiring adapters to use cases | `app` |

The `app` crate MAY hold more than one binary (`[[bin]]` entries), since UIDM ships three. Each binary is its own composition root.

## 3. Dependency rule

This is the most important rule. Dependencies point inward only.

- `domain` depends on nothing internal. Allowed external crates are pure ones such as `thiserror`, `uuid`, `chrono`, `url` (parsing only) and `serde` (derive only). It MUST NOT depend on `tokio`, `reqwest`, `rusqlite`, `sqlx`, `eframe`, `egui`, `windows`, `libc` or any other I/O, async runtime or UI crate.
- `application` depends only on `domain`, plus `tracing` for logging. It MAY start threads with `std::thread` (see section 5).
- `infrastructure` depends on `domain`, and on `application` only to implement a port that `application` defines.
- `ui` depends on `application` and `domain`. It MUST NOT depend on `infrastructure`.
- `app` depends on everything and is the only crate that wires concrete adapters into use cases.

| Crate | MAY depend on | MUST NOT depend on |
| --- | --- | --- |
| `domain` | pure crates (`thiserror`, `uuid`, `chrono`, `url`, `serde` derive) | `application`, `infrastructure`, `ui`, `app`, any I/O, async or UI crate |
| `application` | `domain`, pure crates, `tracing` | `infrastructure`, `ui`, `app`, I/O crates, UI crates |
| `infrastructure` | `domain`, `application` (ports only), I/O crates (`reqwest`, `tokio`, `windows`, `rfd`, ...) | `ui`, `app`, UI crates |
| `ui` | `domain`, `application`, `eframe`, `egui`, `egui_extras`, pure crates (`serde`, `serde_json`, `url`, `percent-encoding`), drawing crates (`tray-icon`, `raw-window-handle`, platform crates under `cfg`) | `infrastructure`, `app`, I/O crates |
| `app` | every crate, `anyhow` | nothing is forbidden |

Platform-specific dependencies (`windows`, `libc`, `gtk`, `trash`) belong in `infrastructure` (or `ui`, when they only serve drawing, such as the tray or drag and drop) under `[target.'cfg(...)'.dependencies]`.

## 4. Domain layer rules

- MUST use newtypes for every meaningful value, for example `DownloadId(u64)`, `QueueId`, `ByteCount(u64)`, `Url`, `FileName`. MUST NOT pass raw `String`, `u64` or `i64` for domain concepts.
- Parse, don't validate. Constructors return `Result`, for example `FileName::parse(&str) -> Result<FileName, DomainError>`. A value that exists is always valid.
- MUST model states with enums so illegal states cannot be represented. A download is `Queued`, `Downloading { .. }`, `Paused { .. }`, `Complete { .. }` or `Failed { reason }`, not a set of booleans.
- Ports are traits defined here, for example `trait DownloadRepository: Send + Sync` and `trait Transfer: Send + Sync`.
- MUST NOT perform I/O, start threads, use an async runtime, read the clock or environment directly, or log. Time comes in as a parameter or through a `Clock` port.

## 5. Application layer rules

- One use case per file and struct, for example `AddDownload`, `PauseDownload`, `RunQueue`, `GrabSite`.
- Use cases are generic over ports (`AddDownload<R: DownloadRepository>`), or take `Arc<dyn Port>` when dynamic dispatch is needed.
- Use cases orchestrate. Business rules live in the domain.
- Input and output DTOs live here, not in `ui`.
- MUST NOT depend on the wire protocol's JSON shapes. Mapping protocol messages to DTOs is an adapter's job.
- MAY log with `tracing` and MAY run work on `std::thread` threads, such as a service that answers a request in the background. It MUST NOT do I/O itself: the work on those threads goes through ports.

## 6. Infrastructure layer rules

- Each adapter implements a port trait from `domain` (or from `application`).
- File formats, file paths, HTTP details, Win32 calls and the IPC wire format MUST NOT leak out of this crate.
- UIDM stores its state in JSON files (`settings.json`, `history.json`, saved requests). Writes MUST be atomic (write to a temporary file, then rename). If a database is added later, its migrations live here and run at startup. Format changes to existing JSON files MUST read the old format.
- SHOULD use the `directories` crate for OS-specific data, config and log paths. MUST NOT hardcode paths or read `APPDATA`, `LOCALAPPDATA` or `XDG_*` outside the one module that resolves them.
- MUST map external errors (`io::Error`, `reqwest::Error`, `windows::core::Error`) into port error types at this boundary.

## 7. UI layer rules

- UI code is a thin adapter. It translates user events into use case calls and results into view state. MUST NOT put business logic in views, callbacks or update functions.
- MUST NOT block the UI thread. Daemon requests, file access and network work run on worker threads or tasks, and results come back as messages.
- Use unidirectional data flow: one state struct, changed only through messages.

egui-specific rules:

- Keep an `AppState` struct separate from the `eframe::App` implementation. `update` reads `AppState` to draw and turns clicks and keystrokes into `UiMessage` values. A single `apply(&mut AppState, UiMessage)` function changes the state.
- Talk to workers through channels (`std::sync::mpsc` or `crossbeam-channel`). Workers send results back and call `ctx.request_repaint()` so egui wakes up. `update` drains the channel with `try_recv`, never `recv`.
- MUST NOT call the daemon, read files or sleep inside `update` or any `show` function. egui calls these every frame.
- Every dialog and download window is drawn during the main window's frame (see `docs/adr/0001-cloak-the-main-window-in-the-tray.md`). Code that changes how windows are hidden MUST keep that frame running.
- User-facing text goes through the translation tables in `ui` (`crates/ui/src/lang/*.json`). MUST NOT hardcode English strings in views.
- Before copying an IDM window, dialog or tray behavior, observe IDM first (the `observe-idm` skill in `.claude/skills/`).

## 8. App (composition root) rules

- Each binary's `main.rs` loads config, initializes `tracing`, opens storage, runs any migrations, constructs adapters, injects them into use cases, and starts its loop (the egui window, the daemon's IPC server, or the host's relay).
- MUST NOT hold business logic.

## 9. Error handling

- Each layer has its own error enum built with `thiserror`, with `From` conversions between layers.
- `anyhow` is allowed only in the `app` crate.
- MUST NOT use `Result<_, String>` as an error type in new code.
- MUST NOT use `unwrap()`, `expect()` or `panic!` in non-test code, except for invariants that are truly guaranteed and documented with a comment next to the call. A poisoned mutex counts as such an invariant only when the comment says why.
- Errors shown to users are mapped to friendly, translated messages in the UI layer.

## 10. Async and concurrency

- SHOULD prefer generics (static dispatch). Use `Arc<dyn Trait>` only when needed, and `async-trait` for async traits used as trait objects.
- Shared services are wrapped in `Arc`. SHOULD avoid `Mutex` in domain and application code and prefer message passing.
- The engine runs async HTTP on one shared `tokio` runtime inside `infrastructure`. Other crates MUST NOT create their own runtime or depend on `tokio`.

## 11. Logging and observability

- Use `tracing` with structured fields, for example `tracing::info!(download_id = %id, bytes, "segment done")`.
- MUST NOT commit `println!`, `eprintln!` or `dbg!`. The exception is CLI output that is the program's actual output (the `uidm-host` test mode).
- Logs go to a file in the app's data directory. Logs MUST NOT contain cookies, passwords, auth headers or the daemon token. Download addresses are allowed, since the log is readable only by the user.

## 12. Testing

- Domain: pure unit tests, no mocks needed.
- Application: test use cases with in-memory adapters that implement the port traits.
- Infrastructure: integration tests against temporary files and directories (and a temporary database, if one is added), and against the local test HTTP server (today `crates/infrastructure/src/engine/testserver.rs`). Tests MUST NOT reach the internet.
- Every new use case MUST come with tests.
- Changes to the browser extension MUST pass `node --test extension/test/*.test.js`.

## 13. Quality gates

A task is not done until all of these pass:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`

CI runs them on Windows, macOS and Linux (`.github/workflows/`). Code under `cfg(windows)` or `cfg(unix)` is only checked on that platform, so state which platforms you could not build on.

## 14. Workflow rules for AI agents

- Read this file before making any change.
- Before writing code, state which layer or layers the change belongs to.
- Add a feature in this order: domain types and ports, use case, adapter, UI, wiring in `app`, tests.
- MUST NOT add a dependency without stating why and confirming it respects the dependency rule.
- MUST NOT restructure crates or move files unless explicitly asked.
- If a request conflicts with these rules, stop and explain the conflict instead of silently breaking the architecture.
- Run the quality gates and report the results at the end of every task.

## 15. Forbidden patterns

- Business logic in egui views, `update` or click handlers.
- I/O, threads, clocks or logging in `domain`.
- `ui` importing `infrastructure`.
- Blocking the UI thread, including daemon calls or file reads inside `update`.
- `unwrap()`, `expect()` or `panic!` in production code without a documented invariant.
- Hardcoded paths, or reading `APPDATA` and similar variables outside the path module.
- Raw primitives for domain concepts.
- `Result<_, String>` errors in new code, and `anyhow` outside `app`.
- `println!` and `dbg!` in committed code.

## 16. Migration notes

Where the code on `main` deviates from the rules above. These are notes, not tasks.

- **Workspace in place, layers mostly empty.** The five crates exist. `ui` holds the window (see The daemon connection). `application` holds the first use cases: `queues::{SaveQueue, DeleteQueue, StartQueue, StopQueue, RunSchedules, RunQueues, FinishRun, MoveToQueue, RemoveFromQueue}` and `downloads::{AddLater, AddMany, HoldForAnswer, ConfirmAnswer, StartOrWait, CancelDownload, UseExisting, RemoveDownload, RetryDownload, Redownload, RefreshLink, SetProperties, SetSpeedLimit, SetLimitRemember, SetOnDone, AfterDownload, ConvertDownload, StopRunning, PauseAll, ResumeAll}`, `quota::{CheckQuota, QuotaStatus}`, `power::{RunWhenDone, CountdownNow, CancelCountdown}` `update::{CheckForUpdate, ClaimWeeklyCheck, InstallUpdate}` and `grab::RunGrabSchedules`, tested with the in-memory adapters in `application/src/memory.rs`. `domain` holds the pure rules moved so far: `queue` (queues, schedules, calendar arithmetic), `batch` (URL patterns), `site` (site and login patterns), `filename` (file-name cleaning and numbered names), `command` (the scanner command line), `settings` (the settings model, defaults, validation and folder rules), `media` (stream formats, qualities, tracks and the user's choice among them) `download` (`Download`, `DownloadEntry`, `Status`, `Answer` and the other download model types) `quota` (the byte count per minute and when the quota pauses, warns or resumes) `update` (releases, version comparison and the weekly check) and `grab` (the site grabber's request, filters, mirror paths, projects, templates and filters, without their files). The domain still uses raw primitives and `Result<_, String>`. `infrastructure` is the rest of the old `uidm` library and re-exports some moved items (`net::site_matches`, `scan::parse_command`, `notify::Sound`, `media::convert::Target`, `media::hds::sniff`, and all of `uidm_domain::settings`, `uidm_domain::media` and `uidm_domain::download` from `settings`, `media` and `protocol`) so its callers did not change. `uidm_infrastructure::settings` keeps `load`, `save`, the file-format migrations, `settings_path`, `default_downloads_dir`, `ffmpeg` (searches `PATH`) and `validate`, which adds the checks that look at the disk. Domain folder rules (`category_folder`, `folders`, `folders_for`) take the system's Downloads folder as a parameter. `uidm_infrastructure::protocol` keeps the wire messages (`Incoming`, `Outgoing`) and the framing; `CommandLine` and `ZipEntry` are in `uidm_domain::download`. `uidm_infrastructure::clock` reads the local time that `queue` takes as a parameter. `app` holds the three binaries; `uidm-gui`'s `main` only builds the window's adapters and calls `uidm_ui::run`. Paths below are relative to `crates/infrastructure/`, `crates/app/` or `crates/ui/`.
- **No layer boundaries.** `infrastructure/src/lib.rs` exposes about 30 flat modules. Domain rules and I/O sit in the same modules. Examples:
  - `engine/mod.rs` holds transfer code; its file-name rules are in `domain::filename` now.
- **Few ports.** `domain::ports` has what the queue use cases need: `QueueStore`, `DownloadList`, `DownloadControl`, `DownloadFiles`, `PausedDownloads`, `QueueRuns`, `QueueSounds`, `QueueFinish`, `SyncChecks`, `QuotaState`, `PowerControl`, `ReleaseSource`, `UpdateChecks`, `Converter`, `DownloadIntake`, `Attention`, `SettingsStore`, `EarlyStarts`, `Answers`, `GrabProjects`, `GrabRuns`, `Probes`, `Browser`, `SiteCrawler`, `SaveFolders`, `VirusScanner`, `FileOpener`, `Timestamps`, `TransferEngine`, `Notifications`, `ProxyFallback`, `GrabDownloads` and `Clock`; `uidm_application::daemon` adds `ClientList`, `DownloadFeed` and `Countdowns`, which carry `Event`s. `uidm_infrastructure::grab::ProjectFiles` implements `GrabProjects`; project files keep a request's login cookies sealed with DPAPI, which `save_project` and `load_project` do (the domain model holds them plain). `uidm_infrastructure::clock::LocalClock` implements `Clock`, and `uidm_infrastructure::files::DiskFiles` implements `DownloadFiles` (moving, recycling and resume records). `uidm_infrastructure::store::DownloadStore` holds the download list, the kept requests and the queues, saves them (`history.json`, the saved requests, the quota's usage), and implements `QueueStore`, `DownloadList` and, with `store::Intake`, `DownloadIntake`; it hands each change to a `StoreEvents` listener, which `server::Clients` implements by sending the change to every connected client. Other adapters in `infrastructure` hold the daemon's state: `live_settings::LiveSettings` (the current settings and their file, `UpdateChecks`) and `QueueChimes` (`QueueSounds`), `queue_runs::RunBook` (`QueueRuns`) and `PauseBook` (`PausedDownloads`), `quota::QuotaHold` (`QuotaState`), `power_control::PowerOff` (`PowerControl`, with the countdown), `update::GitHub` (`ReleaseSource`), and `connect::config_for` with `ProxyRetries` (the proxy and logins a download uses). `transfers::Transfers` owns the engine and implements `DownloadControl` and `SettingsStore`, with the early downloads started while a save-as dialog is open; `transfers::SyncChecker` (`SyncChecks`) and `EarlyHandOver` (`EarlyStarts`) sit beside it. `front::Front` (`Attention`), `AnswerWait` (`Answers`) and `Notifier` reach the windows and notifications through the `front::Windows` trait, which `server::Clients` implements. `conversions::Conversions` (`Converter`), `grab::ScheduledGrabs` (`GrabRuns`), `power_control::RunEnds` (`QueueFinish`) and `clock::StoppedClock` complete the set, so `uidm-daemon.rs` holds no port implementations. Where an adapter has to go on through a use case (starting a download after its early copy is handed over, answering a dialog nobody answered, retrying from a notification, removing a converted original, a queue's when-done action, a scheduled grab), the daemon passes it a closure at wiring. `probes::ServerProbes` (`Probes`) asks servers for the link picker, the save-as dialog, the media probe and the zip preview; `browser_bridge::BrowserBridge` (`Browser`) asks the browser extension for cookies and rendered pages through the client list; `crawler::Crawler` (`SiteCrawler`) runs a crawl. `files::DiskFiles` also implements `SaveFolders`, `scan::Antivirus` implements `VirusScanner`, `platform::Desktop` implements `FileOpener`, `clock::LocalClock` implements `Timestamps`, and `server::Clients` implements `uidm_application::daemon::ClientList`. The service runs those requests on threads of its own and keeps each client's stop flags. The scheduler pass (`DaemonService::tick`) is `RunSchedules` followed by `RunQueues`. The use cases return `domain::queue::QueueError` (the first `thiserror` enum) and take raw `u64` ids.
- **The daemon connection.** `uidm_application::daemon` defines what clients send the daemon (`Command`) and what they hear back (`Event`), with a port on each side: `DaemonClient`, the window's connection, and `Requests`, which the daemon's server hands each client's commands to, with a `Reply` for its answers. `uidm_infrastructure::wire` maps both to and from the wire messages (`protocol::Incoming`, `Outgoing`); `daemon_client::IpcDaemonClient` implements `DaemonClient` over `ipc` and `protocol` (connection, handshake, `hello`). The download list's rules are in `uidm_domain::table`, which the window feeds with `Event`s and the daemon (through `uidm_infrastructure::state`) with wire messages. Its views reach the clock, the Downloads folder, the write check, opening and revealing files and the window's own files (`window.json`, `droptarget.txt`, through `uidm_application::window::WindowFiles`, written on a thread by `uidm_infrastructure::window_files::WindowDir`) through `ports.rs`' `Ports`, which `main` hands `uidm_ui::run` in a `Start`. The grabber's projects, templates and filters (`GrabLibrary`), list import and export (`ListFiles`), the settings checks that read the disk (`SettingsChecks`) and the operating system's proxy, wake timers, sounds, credential store, fonts and capabilities (`SystemServices`) and the file and folder pickers (`FilePicker`, `uidm_infrastructure::pickers::NativePicker`) are ports in `uidm_application::window` too. Browser registration is `BrowserRegistry` (`uidm_infrastructure::browsers::Registry`), and `uidm-ui` does not depend on `uidm-infrastructure`; `main` also builds the daemon client and hands it to the window. Workers read the grabber's projects and templates (every few seconds, every second while the grabber is open) and Options' browsers (when it opens) into `AppState`; the category tree, the grabber's menus and Options draw from there. The grabber saves and opens projects, templates and filters, keeps a new password, Options identifies an added browser, and Save checks the settings on disk and registers browsers, each on a worker whose result a dialog takes with `pending.rs`' `Pending` (a one-shot channel read with `try_recv`). The Queues dialog's wake-timer check, Options' Get System proxy read and a batch's questions (the taken-name check among them) run on workers too, so the window reads the disk on its own thread only once at start, for its layout and the drop target's place.
- **Logic in the binaries.** None is left. The window's orchestration is in `uidm-ui`'s `lib.rs` (about 5,300 lines), which a later split can break up. The daemon is done: `uidm_application::daemon::service::DaemonService` does its work (each client's `Command`, the scheduler pass, the notification thread's `Note`s, start-up and idle checks) through ports, generic over a `Wiring` trait whose associated types name one adapter per port group. `src/bin/uidm-daemon.rs` implements `Wiring`, builds the adapters (`new_daemon`), starts the background threads (`uidm_infrastructure::background`) and serves clients (`uidm_infrastructure::server`, which reads each frame as a `Command` for the service's `Requests` port, `Served`). Its behavior tests run the service on the real adapters.
- **GUI threads.** The GUI sends daemon work to `thread::spawn` workers. `state.rs` holds `AppState` (the list, the daemon's news and the threads' results), `UiMessage` and `apply`: the connection thread, the clipboard watcher, the tray menu and the failed-download timer send `UiMessage`s on a channel that `update` drains with `try_recv`, and nothing outside the frame changes the state. Those threads keep a `Watch` of the daemon's events (the settings, which downloads asked for a dialog or failed) to wake a hidden window, which on macOS and Linux gets no frames until it is shown. The frame changes `AppState` only through `apply` too: what it is done with is a `UiMessage::Forget`, and what it takes to show (the next completion dialog's news, the crawl's progress, picked links) a `UiMessage::Take`, which `apply` answers with what it took. The dialogs' drafts are view state: they live in `App` and their dialogs edit them in place, and a dialog's result is an `Action` that `App::perform` carries out (#199).
- **Errors.** `domain::queue::QueueError`, `domain::download::DownloadError`, `domain::update::UpdateError`, the port errors (`ControlError`, `FilesError`) and the use case errors in `application::downloads` (`StartOrWaitError`, `ConfirmError`) use `thiserror`, and no crate uses `anyhow`. `EngineError` and `MoveError` are hand-written enums. About 130 functions return `Result<_, String>`. Most modules return `io::Result`.
- **unwrap and panics.** About 1,250 `unwrap()` and 28 `expect()` calls in `src`. Most are in tests, but roughly 350 sit before the test module of their file, mostly in `uidm-ui`'s `lib.rs` (about 90); the daemon's moved to `application` without any.
- **Logging.** The three binaries log with `tracing`. Each `main` calls `uidm_infrastructure::logging::init`, which writes `uidm-daemon.log`, `uidm-host.log` or `uidm-gui.log` in the data folder (`paths::daemon_log`, `host_log`, `gui_log`), appending each line in one write so many hosts can share a file, and hides passwords in addresses. The daemon moves a log over 5 MB to `.old` once it wins the bind and then hourly; the host also writes warnings to standard error, which the browser reads. `domain` does not log; `application` logs only in the daemon service. The `eprintln!` and `println!` calls left are `uidm-host`'s `download` and `send` modes, whose output is the program's actual output, and three in tests. Event fields are still raw ids and strings (see Raw primitives).
- **Paths.** `uidm_infrastructure::paths` resolves every folder UIDM uses (settings, data, grabber projects, the logs, the window's files, the sign-in entry, app packages' AppData copies and browser profile roots) on top of `directories`' `BaseDirs`, and is the only module that reads `APPDATA`, `LOCALAPPDATA`, `USERPROFILE` or `XDG_*`. On Windows it reads those variables before the known folders, because end-to-end runs point them at a scratch folder, and on Linux it uses any non-empty `XDG_CONFIG_HOME` and `XDG_DATA_HOME` as given (`directories` ignores relative ones), so no folder moved. It also owns `UIDM_INSTANCE` (`paths::instance`). The engine's part files still default to `std::env::temp_dir()` in `engine/mod.rs`.
- **Raw primitives.** Download ids, byte counts, URLs and file names travel as `u64` and `String`.
- **Storage.** State is JSON files, not SQLite. Section 6 is written for files. Nothing needs to change unless a database is added.
- **Module cycles.** One cycle is left inside `infrastructure`: `net` and `pac` (proxy rules and PAC scripts) call each other. Both are adapters that stay in this crate, so it does not block a split.
