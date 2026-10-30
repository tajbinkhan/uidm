//! The download quota: "no more than N MB every N hours". The daemon counts
//! the bytes every download receives, per minute, and keeps the minutes of
//! the longest window in `%LOCALAPPDATA%\UIDM\quota.json`, so the count
//! survives a restart.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::settings::write_atomic;

/// Bytes received, by minute since 1970.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    minutes: VecDeque<(u64, u64)>,
    /// Each running download's progress at its last report, so only new
    /// bytes count. Not saved: a restart starts every download afresh.
    #[serde(skip)]
    last: HashMap<u64, u64>,
}

impl Usage {
    /// A download (re)started with `done` bytes already in hand; those do
    /// not count again.
    pub fn started(&mut self, id: u64, done: u64) {
        self.last.insert(id, done);
    }

    /// Download `id` reports `done` bytes at minute `now`: count what is new.
    pub fn progress(&mut self, id: u64, done: u64, now: u64) {
        let before = self.last.insert(id, done).unwrap_or(done);
        let new = done.saturating_sub(before);
        if new == 0 {
            return;
        }
        match self.minutes.back_mut() {
            Some((m, bytes)) if *m == now => *bytes += new,
            _ => self.minutes.push_back((now, new)),
        }
    }

    /// The download stopped; forget its progress.
    pub fn ended(&mut self, id: u64) {
        self.last.remove(&id);
    }

    /// Bytes received in the `window` minutes up to `now`. Older minutes are
    /// dropped.
    pub fn used(&mut self, now: u64, window: u64) -> u64 {
        let start = now.saturating_sub(window.saturating_sub(1));
        while self.minutes.front().is_some_and(|(m, _)| *m < start) {
            self.minutes.pop_front();
        }
        self.minutes.iter().map(|(_, b)| b).sum()
    }

    /// The first minute after `now` at which the window holds less than
    /// `limit` bytes again, if no more arrive.
    pub fn frees_at(&mut self, now: u64, window: u64, limit: u64) -> u64 {
        let mut used = self.used(now, window);
        for (m, b) in &self.minutes {
            if used < limit {
                break;
            }
            used -= b;
            // Minute `m` leaves the window `window` minutes after it.
            if used < limit {
                return m + window;
            }
        }
        now
    }

    pub fn load(path: &Path) -> Usage {
        fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        write_atomic(path, &bytes)
    }
}

/// `quota.json` in UIDM's data folder (`%LOCALAPPDATA%\UIDM` on Windows).
pub fn quota_path() -> io::Result<PathBuf> {
    Ok(crate::platform::data_dir()?.join("quota.json"))
}

/// Minutes since 1970.
pub fn now_minute() -> u64 {
    crate::state::unix_now() / 60
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_only_new_bytes_in_the_window() {
        let mut u = Usage::default();
        u.started(1, 500);
        u.progress(1, 700, 10);
        u.progress(1, 1000, 10);
        u.started(2, 0);
        u.progress(2, 50, 11);
        assert_eq!(u.used(11, 60), 550, "resumed bytes do not count");
        assert_eq!(u.used(70, 60), 50, "minute 10 left the window");
        assert_eq!(u.used(200, 60), 0);
    }

    #[test]
    fn says_when_the_window_frees_up() {
        let mut u = Usage::default();
        u.started(1, 0);
        u.progress(1, 600, 0);
        u.progress(1, 1000, 30);
        // 1000 bytes in an hour with a limit of 800: minute 0 must leave.
        assert_eq!(u.frees_at(40, 60, 800), 60);
        assert_eq!(u.frees_at(40, 60, 2000), 40, "already under the limit");
    }

    #[test]
    fn survives_a_save_and_load() {
        let mut u = Usage::default();
        u.started(1, 0);
        u.progress(1, 42, 5);
        let path = std::env::temp_dir().join(format!("uidm_quota_{}.json", std::process::id()));
        u.save(&path).unwrap();
        let mut back = Usage::load(&path);
        assert_eq!(back.used(5, 60), 42);
        let _ = fs::remove_file(&path);
        assert_eq!(Usage::load(&path), Usage::default(), "a missing file is no usage");
    }
}
