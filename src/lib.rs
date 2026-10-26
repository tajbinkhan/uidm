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
//! - `notify`: completion notifications and sounds.
//! - `scan`: virus scans of finished downloads.
//! - `queue`: download queues, their schedules and calendar arithmetic.
//! - `power`: sleep, hibernate and shut down when a queue is done.

pub mod app;
pub mod dpapi;
pub mod engine;
pub mod history;
pub mod ipc;
pub mod notify;
pub mod power;
pub mod protocol;
pub mod queue;
pub mod registry;
pub mod scan;
pub mod settings;
pub mod state;
