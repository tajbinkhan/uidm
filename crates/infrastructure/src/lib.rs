//! UDM: a download manager rebuilt in Rust.
//!
//! - `protocol`: message types and the length-prefixed JSON framing shared by
//!   browser <-> host and host <-> daemon.
//! - `engine`: segmented, resumable, cancelable downloads.
//! - `ipc`: locating, starting and connecting to the daemon.
//! - `app`: glue shared by the binaries (request building, event mapping).
//! - `state`: the download list that the daemon and the window both keep.
//! - `settings`: user settings and their file.
//! - `history`: the daemon's download list and saved requests on disk.
//! - `dpapi`: Windows DPAPI encryption for saved requests.
//! - `registry`: per-user registry values (start with Windows, toast app id).
//! - `browsers`: the browsers found and their native-messaging registration.
//! - `notify`: completion notifications and sounds.
//! - `scan`: virus scans of finished downloads.
//! - `queue`: download queues, their schedules and calendar arithmetic.
//! - `power`: sleep, hibernate and shut down when a queue is done.
//! - `net`: proxies and site logins.
//! - `pac`: proxy auto-config scripts, run by WinHTTP.
//! - `media`: HLS playlists and DASH manifests.
//! - `batch`: many URLs from a pattern or a list.
//! - `listfile`: exporting and importing the download list.
//! - `grab`: the site grabber's crawler.
//! - `recycle`: sending files to the Recycle Bin.
//! - `quota`: counting the bytes downloaded for the download quota.
//! - `zip`: listing a zip archive's files from its end, for a preview.
//! - `update`: checking GitHub for a newer release.
//! - `package`: keeping the daemon out of an app package's copy of AppData.
//! - `logging`: each program's log file in the data folder.
//! - `crash`: problem reports: the panic hook and the reports folder.
//! - `pickers`: the system's file and folder dialogs, for the window.
//! - `server`: the daemon's listening socket, its clients and their handshake.
//! - `wire`: mapping the wire messages to and from the application's commands and events.
//! - `probes`, `browser_bridge`, `crawler`: the `Probes`, `Browser` and
//!   `SiteCrawler` adapters (asking servers, the browser extension, and
//!   crawling for the site grabber).
//! - `tool`: `udm-cli`'s command line, what it prints and its exit codes.
//! - `daemon_args`: `udm-daemon`'s command line (`--server` and its folders).
//! - `mcp`: `udm-mcp`'s Model Context Protocol server over standard
//!   input and output, and its tools.
//! - `background`: the daemon's background threads (scheduler, history
//!   writer, notifications, awake keeper, idle exit).

// Tests may use todo!, unimplemented! and unreachable!, which clippy.toml's
// test exemptions do not cover (AGENTS.md section 9).
#![cfg_attr(test, allow(clippy::todo, clippy::unimplemented, clippy::unreachable))]

pub mod adapters;
pub mod app;
pub mod autostart;
pub mod background;
pub mod browser_bridge;
pub mod browsers;
pub mod cli;
pub mod clipboard;
pub mod clock;
pub mod connect;
pub mod conversions;
pub mod country;
pub mod crash;
pub mod crawler;
pub mod daemon_args;
pub mod daemon_client;
pub mod debrid;
pub mod doh;
pub mod dpapi;
pub mod elevate;
pub mod engine;
pub mod files;
pub mod from_old_names;
pub mod front;
pub mod grab;
pub mod hash;
pub mod history;
pub mod idle;
pub mod ipc;
pub mod listfile;
pub mod live_settings;
pub mod logging;
pub mod mcp;
pub mod media;
pub mod net;
pub mod notify;
pub mod ntlm;
pub mod pac;
#[cfg(windows)]
pub mod package;
pub mod paths;
pub mod pickers;
pub mod platform;
pub mod power;
pub mod power_control;
pub mod probes;
pub mod protocol;
pub mod queue_runs;
pub mod quota;
pub mod recycle;
#[cfg(windows)]
pub mod registry;
pub mod route;
pub mod scan;
pub mod server;
pub mod settings;
pub mod state;
pub mod store;
pub mod system;
pub mod themes;
pub mod tool;
pub mod torrent;
pub mod transfers;
pub mod unpack;
pub mod update;
pub mod update_in_place;
pub mod video_tools;
pub mod web_remote;
pub mod window_files;
pub mod wire;
pub mod zip;
