//! UIDM: a download manager rebuilt in Rust.
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

pub mod app;
pub mod autostart;
pub mod batch;
pub mod browsers;
pub mod dpapi;
pub mod engine;
pub mod grab;
pub mod history;
pub mod ipc;
pub mod listfile;
pub mod media;
pub mod net;
pub mod notify;
#[cfg(windows)]
pub mod package;
pub mod pac;
pub mod platform;
pub mod power;
pub mod protocol;
pub mod queue;
pub mod quota;
pub mod recycle;
#[cfg(windows)]
pub mod registry;
pub mod scan;
pub mod settings;
pub mod state;
pub mod update;
pub mod zip;
