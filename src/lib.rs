//! UIDM: a download manager rebuilt in Rust.
//!
//! - `protocol`: message types and the length-prefixed JSON framing shared by
//!   browser <-> host and host <-> daemon.
//! - `engine`: segmented, resumable, cancelable downloads.
//! - `ipc`: locating, starting and connecting to the daemon.
//! - `app`: glue shared by both binaries (request building, event mapping).

pub mod app;
pub mod engine;
pub mod ipc;
pub mod protocol;
