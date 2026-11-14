//! Entities, value objects, business rules and the port traits the other
//! layers implement. Nothing here performs I/O, reads the clock or logs.

// Tests may use todo!, unimplemented! and unreachable!, which clippy.toml's
// test exemptions do not cover (AGENTS.md section 9).
#![cfg_attr(test, allow(clippy::todo, clippy::unimplemented, clippy::unreachable))]

pub mod address;
pub mod batch;
pub mod bytes;
pub mod checksum;
pub mod command;
pub mod country;
pub mod debrid;
pub mod download;
pub mod filename;
pub mod gallery;
pub mod grab;
pub mod id;
pub mod media;
pub mod network;
pub mod package;
pub mod pairing;
pub mod ports;
pub mod queue;
pub mod quota;
pub mod report;
pub mod seat;
pub mod settings;
pub mod site;
pub mod sync;
pub mod table;
pub mod theme;
pub mod tools;
pub mod torrent;
pub mod update;
