//! Use cases (add a download, pause, run a queue, ...) that coordinate the
//! domain through its ports, one per file.

// Tests may use todo!, unimplemented! and unreachable!, which clippy.toml's
// test exemptions do not cover (AGENTS.md section 9).
#![cfg_attr(test, allow(clippy::todo, clippy::unimplemented, clippy::unreachable))]

pub mod daemon;
pub mod debrid;
pub mod downloads;
pub mod gallery;
pub mod grab;
pub mod pairing;
pub mod power;
pub mod queues;
pub mod quota;
pub mod remote;
pub mod report;
pub mod themes;
pub mod tools;
pub mod update;
pub mod window;

#[cfg(test)]
mod memory;
