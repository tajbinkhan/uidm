//! A list of downloads kept current by applying the daemon's events.
//!
//! The daemon applies every event it sends, so it can answer a list request. The
//! desktop window applies the `Downloads` snapshot and then the same events,
//! so it shows the same list. Events can arrive out of order across threads
//! (a download may report `Started` before its `Added` is applied), so each
//! change creates the entry if it is missing and `Added` never overwrites
//! one.

use std::collections::BTreeMap;

use crate::address::Link;
use crate::bytes::ByteCount;
use crate::country::CountryCode;
use crate::download::{DownloadEntry, Status, Validator};
use crate::id::DownloadId;

/// One change to the list, borrowed from whatever message carried it, so
/// applying one copies only what the table keeps.
#[derive(Debug, Clone, Copy)]
pub enum Change<'a> {
    Added {
        id: DownloadId,
        url: &'a str,
    },
    Started {
        id: DownloadId,
        url: &'a str,
        path: &'a str,
        total: Option<ByteCount>,
        resumable: bool,
        connections: usize,
        partial: Option<&'a str>,
        validator: Option<&'a Validator>,
        country: Option<CountryCode>,
    },
    Progress {
        id: DownloadId,
        url: &'a str,
        done: ByteCount,
        total: Option<ByteCount>,
        parts: Option<[u64; 2]>,
    },
    Done {
        id: DownloadId,
        url: &'a str,
        path: &'a str,
        note: Option<&'a str>,
    },
    Failed {
        id: DownloadId,
        url: &'a str,
        error: &'a str,
    },
    Canceled {
        id: DownloadId,
        url: &'a str,
    },
    NameTaken {
        id: DownloadId,
        url: &'a str,
        path: &'a str,
        total: Option<ByteCount>,
    },
    Removed {
        id: DownloadId,
    },
    Updated(&'a DownloadEntry),
    Downloads(&'a [DownloadEntry]),
}

#[derive(Debug, Default, Clone)]
pub struct DownloadTable {
    entries: BTreeMap<DownloadId, DownloadEntry>,
}

impl DownloadTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Make one change to the table. `now` (seconds since 1970) dates a
    /// download that starts.
    pub fn change(&mut self, change: Change<'_>, now: u64) {
        match change {
            Change::Added { id, url } => {
                self.entries.entry(id).or_insert_with(|| blank(id, url, Status::Queued));
            }
            Change::Started { id, url, path, total, resumable, connections, partial, validator, country } => {
                let e = self.entry(id, url);
                e.status = Status::Downloading;
                e.path = Some(path.to_string());
                e.partial = partial.map(str::to_string);
                e.total = total;
                e.resumable = resumable;
                e.connections = connections;
                // A server that says nothing about the file keeps what an
                // earlier check learned.
                if let Some(v) = validator {
                    e.validator = Some(v.clone());
                }
                e.country = country;
                e.error = None;
                e.scan = None;
                e.note = None;
                e.last_try = Some(now);
            }
            Change::Progress { id, url, done, total, parts } => {
                let e = self.entry(id, url);
                // A late progress event must not revive a finished download.
                if !e.status.is_terminal() {
                    e.status = Status::Downloading;
                }
                e.done = done;
                if total.is_some() {
                    e.total = total;
                }
                if parts.is_some() {
                    e.parts = parts;
                }
            }
            Change::Done { id, url, path, note } => {
                let e = self.entry(id, url);
                e.status = Status::Done;
                e.path = Some(path.to_string());
                e.partial = None;
                if let Some(t) = e.total {
                    e.done = t;
                }
                if let Some([_, all]) = e.parts {
                    e.parts = Some([all, all]);
                }
                e.error = None;
                e.note = note.map(str::to_string);
            }
            Change::Failed { id, url, error } => {
                let e = self.entry(id, url);
                e.status = Status::Failed;
                e.error = Some(error.to_string());
            }
            Change::Canceled { id, url } => {
                self.entry(id, url).status = Status::Canceled;
            }
            Change::NameTaken { id, url, path, total } => {
                let e = self.entry(id, url);
                e.status = Status::Confirming;
                e.path = Some(path.to_string());
                e.total = total;
                e.error = None;
            }
            Change::Removed { id } => {
                self.entries.remove(&id);
            }
            Change::Updated(item) => {
                self.entries.insert(item.id, item.clone());
            }
            Change::Downloads(items) => {
                self.entries = items.iter().map(|e| (e.id, e.clone())).collect();
            }
        }
    }

    pub fn get(&self, id: DownloadId) -> Option<&DownloadEntry> {
        self.entries.get(&id)
    }

    /// Every download, oldest first.
    pub fn entries(&self) -> impl Iterator<Item = &DownloadEntry> {
        self.entries.values()
    }

    /// A copy of every download, oldest first, for a `Downloads` reply.
    pub fn snapshot(&self) -> Vec<DownloadEntry> {
        self.entries.values().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Largest id in the table, or 0 when empty.
    pub fn max_id(&self) -> DownloadId {
        self.entries.keys().next_back().copied().unwrap_or_default()
    }

    fn entry(&mut self, id: DownloadId, url: &str) -> &mut DownloadEntry {
        self.entries.entry(id).or_insert_with(|| blank(id, url, Status::Queued))
    }
}

fn blank(id: DownloadId, url: &str, status: Status) -> DownloadEntry {
    DownloadEntry {
        id,
        url: Link::new(url),
        path: None,
        total: None,
        done: ByteCount::ZERO,
        status,
        error: None,
        resumable: false,
        connections: 0,
        scan: None,
        parts: None,
        note: None,
        duplicate_of: None,
        referrer: None,
        page: None,
        last_try: None,
        description: None,
        checksum: None,
        category: None,
        login_user: None,
        speed_limit: None,
        limit_remember: true,
        partial: None,
        from_browser: false,
        on_done: None,
        validator: None,
        country: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_700_000_000;
    const URL: &str = "https://x/f.zip";

    fn started(t: &mut DownloadTable, id: u64) {
        t.change(
            Change::Started {
                id: DownloadId::new(id),
                url: URL,
                path: "/dl/f.zip",
                total: Some(ByteCount::new(1000)),
                resumable: true,
                connections: 4,
                partial: None,
                validator: None,
                country: CountryCode::parse("NL"),
            },
            NOW,
        );
    }

    fn progress(id: u64, done: u64) -> Change<'static> {
        Change::Progress {
            id: DownloadId::new(id),
            url: URL,
            done: ByteCount::new(done),
            total: Some(ByteCount::new(1000)),
            parts: None,
        }
    }

    #[test]
    fn a_download_goes_from_added_to_done() {
        let mut t = DownloadTable::new();
        t.change(Change::Added { id: DownloadId::new(1), url: URL }, NOW);
        assert_eq!(t.get(DownloadId::new(1)).unwrap().status, Status::Queued);
        started(&mut t, 1);
        let e = t.get(DownloadId::new(1)).unwrap();
        assert_eq!(e.status, Status::Downloading);
        assert_eq!(e.connections, 4);
        assert_eq!(e.last_try, Some(NOW));
        assert_eq!(e.country.map(|c| c.to_string()).as_deref(), Some("NL"), "the server's country");
        t.change(progress(1, 400), NOW);
        assert_eq!(t.get(DownloadId::new(1)).unwrap().done, ByteCount::new(400));
        t.change(Change::Done { id: DownloadId::new(1), url: URL, path: "/dl/f.zip", note: None }, NOW);
        let e = t.get(DownloadId::new(1)).unwrap();
        assert_eq!(e.status, Status::Done);
        assert_eq!(e.done, ByteCount::new(1000), "done fills to the total");
    }

    #[test]
    fn started_before_added_is_not_reset() {
        let mut t = DownloadTable::new();
        started(&mut t, 7);
        t.change(Change::Added { id: DownloadId::new(7), url: URL }, NOW);
        assert_eq!(t.get(DownloadId::new(7)).unwrap().status, Status::Downloading);
    }

    #[test]
    fn late_progress_does_not_revive_a_canceled_download() {
        let mut t = DownloadTable::new();
        started(&mut t, 2);
        t.change(Change::Canceled { id: DownloadId::new(2), url: URL }, NOW);
        t.change(progress(2, 500), NOW);
        assert_eq!(t.get(DownloadId::new(2)).unwrap().status, Status::Canceled);
        assert_eq!(t.get(DownloadId::new(2)).unwrap().done, ByteCount::new(500));
    }

    #[test]
    fn a_taken_name_waits_for_the_save_as_dialog() {
        let mut t = DownloadTable::new();
        t.change(Change::Added { id: DownloadId::new(6), url: URL }, NOW);
        t.change(
            Change::NameTaken {
                id: DownloadId::new(6),
                url: URL,
                path: "/dl/report.pdf",
                total: Some(ByteCount::new(42)),
            },
            NOW,
        );
        let e = t.get(DownloadId::new(6)).unwrap();
        assert_eq!(e.status, Status::Confirming);
        assert_eq!(e.total, Some(ByteCount::new(42)));
        assert_eq!(e.connections, 0, "never started, so a retry does not reuse the file");
    }

    #[test]
    fn a_snapshot_replaces_the_list() {
        let mut a = DownloadTable::new();
        started(&mut a, 1);
        started(&mut a, 2);
        let snap = a.snapshot();
        let mut b = DownloadTable::new();
        started(&mut b, 9);
        b.change(Change::Downloads(&snap), NOW);
        assert_eq!(b.snapshot(), snap);
        b.change(Change::Removed { id: DownloadId::new(1) }, NOW);
        assert_eq!(b.max_id(), DownloadId::new(2));
        assert_eq!(b.len(), 1);
    }
}
