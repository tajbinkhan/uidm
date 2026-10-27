//! A list of downloads kept current by applying daemon events.
//!
//! The daemon applies every event it broadcasts, so it can answer `list`. The
//! desktop window applies the `list` snapshot and then the same events, so it
//! shows the same list. Events can arrive out of order across threads (a
//! download may report `started` before its `added` is applied), so each
//! update creates the entry if it is missing and `added` never overwrites one.

use std::collections::BTreeMap;

use crate::protocol::{DownloadEntry, Outgoing, Status};

#[derive(Debug, Default, Clone)]
pub struct DownloadTable {
    entries: BTreeMap<u64, DownloadEntry>,
}

impl DownloadTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one daemon message into the table. Messages that do not describe a
    /// download (`hello`, `pong`, `ack`, `error`) are ignored.
    pub fn apply(&mut self, msg: &Outgoing) {
        match msg {
            Outgoing::Added { id, url } => {
                self.entries
                    .entry(*id)
                    .or_insert_with(|| blank(*id, url, Status::Queued));
            }
            Outgoing::Started {
                id,
                url,
                path,
                total,
                resumable,
                connections,
            } => {
                let e = self.entry(*id, url);
                e.status = Status::Downloading;
                e.path = Some(path.clone());
                e.total = *total;
                e.resumable = *resumable;
                e.connections = *connections;
                e.error = None;
                e.scan = None;
                e.note = None;
            }
            Outgoing::Progress {
                id,
                url,
                done,
                total,
                parts,
            } => {
                let e = self.entry(*id, url);
                // A late progress event must not revive a finished download.
                if !e.status.is_terminal() {
                    e.status = Status::Downloading;
                }
                e.done = *done;
                if total.is_some() {
                    e.total = *total;
                }
                if parts.is_some() {
                    e.parts = *parts;
                }
            }
            Outgoing::Done { id, url, path, note } => {
                let e = self.entry(*id, url);
                e.status = Status::Done;
                e.path = Some(path.clone());
                if let Some(t) = e.total {
                    e.done = t;
                }
                if let Some([_, all]) = e.parts {
                    e.parts = Some([all, all]);
                }
                e.error = None;
                e.note = note.clone();
            }
            Outgoing::Failed { id, url, error } => {
                let e = self.entry(*id, url);
                e.status = Status::Failed;
                e.error = Some(error.clone());
            }
            Outgoing::Canceled { id, url } => {
                self.entry(*id, url).status = Status::Canceled;
            }
            Outgoing::NameTaken { id, url, path, total } => {
                let e = self.entry(*id, url);
                e.status = Status::Confirming;
                e.path = Some(path.clone());
                e.total = *total;
                e.error = None;
            }
            Outgoing::Removed { id } => {
                self.entries.remove(id);
            }
            Outgoing::Updated { item } => {
                self.entries.insert(item.id, item.clone());
            }
            Outgoing::Downloads { items } => {
                self.entries = items.iter().map(|e| (e.id, e.clone())).collect();
            }
            Outgoing::Hello { .. }
            | Outgoing::Pong
            | Outgoing::Ack { .. }
            | Outgoing::Error { .. }
            | Outgoing::Settings { .. }
            | Outgoing::Show
            | Outgoing::ShowDone { .. }
            | Outgoing::Queues { .. }
            | Outgoing::Countdown { .. }
            | Outgoing::CountdownCanceled
            | Outgoing::Quit
            | Outgoing::RefreshWaiting { .. }
            | Outgoing::RefreshMismatch { .. }
            | Outgoing::AddedMany { .. }
            | Outgoing::MediaInfo { .. }
            | Outgoing::GrabProgress { .. }
            | Outgoing::GrabDone { .. } => {}
        }
    }

    pub fn get(&self, id: u64) -> Option<&DownloadEntry> {
        self.entries.get(&id)
    }

    /// Every download, oldest first.
    pub fn entries(&self) -> impl Iterator<Item = &DownloadEntry> {
        self.entries.values()
    }

    /// A copy of every download, oldest first, for a `downloads` reply.
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
    pub fn max_id(&self) -> u64 {
        self.entries.keys().next_back().copied().unwrap_or(0)
    }

    fn entry(&mut self, id: u64, url: &str) -> &mut DownloadEntry {
        self.entries
            .entry(id)
            .or_insert_with(|| blank(id, url, Status::Queued))
    }
}

fn blank(id: u64, url: &str, status: Status) -> DownloadEntry {
    DownloadEntry {
        id,
        url: url.to_string(),
        path: None,
        total: None,
        done: 0,
        status,
        error: None,
        resumable: false,
        connections: 0,
        scan: None,
        parts: None,
        note: None,
        duplicate_of: None,
        referrer: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(id: u64) -> Outgoing {
        Outgoing::Started {
            id,
            url: "https://x/f.zip".into(),
            path: "C:\\dl\\f.zip".into(),
            total: Some(1000),
            resumable: true,
            connections: 4,
        }
    }

    fn progress(id: u64, done: u64) -> Outgoing {
        Outgoing::Progress {
            id,
            url: "https://x/f.zip".into(),
            done,
            total: Some(1000),
            parts: None,
        }
    }

    #[test]
    fn full_lifecycle() {
        let mut t = DownloadTable::new();
        t.apply(&Outgoing::Added { id: 1, url: "https://x/f.zip".into() });
        assert_eq!(t.get(1).unwrap().status, Status::Queued);

        t.apply(&started(1));
        let e = t.get(1).unwrap();
        assert_eq!(e.status, Status::Downloading);
        assert_eq!(e.path.as_deref(), Some("C:\\dl\\f.zip"));
        assert_eq!(e.connections, 4);

        t.apply(&progress(1, 400));
        assert_eq!(t.get(1).unwrap().done, 400);

        t.apply(&Outgoing::Done {
            id: 1,
            url: "https://x/f.zip".into(),
            path: "C:\\dl\\f.zip".into(),
            note: None,
        });
        let e = t.get(1).unwrap();
        assert_eq!(e.status, Status::Done);
        assert_eq!(e.done, 1000, "done fills to the total");
    }

    #[test]
    fn started_before_added_is_not_reset() {
        // The engine can report `started` before the daemon applies `added`.
        let mut t = DownloadTable::new();
        t.apply(&started(7));
        t.apply(&Outgoing::Added { id: 7, url: "https://x/f.zip".into() });
        assert_eq!(t.get(7).unwrap().status, Status::Downloading);
    }

    #[test]
    fn late_progress_does_not_revive_a_canceled_download() {
        let mut t = DownloadTable::new();
        t.apply(&started(2));
        t.apply(&Outgoing::Canceled { id: 2, url: "u".into() });
        t.apply(&progress(2, 500));
        let e = t.get(2).unwrap();
        assert_eq!(e.status, Status::Canceled);
        assert_eq!(e.done, 500);
    }

    #[test]
    fn failure_records_error_and_removal_forgets() {
        let mut t = DownloadTable::new();
        t.apply(&started(3));
        t.apply(&Outgoing::Failed {
            id: 3,
            url: "u".into(),
            error: "http error: timed out".into(),
        });
        assert_eq!(t.get(3).unwrap().error.as_deref(), Some("http error: timed out"));
        t.apply(&Outgoing::Removed { id: 3 });
        assert!(t.get(3).is_none());
        assert!(t.is_empty());
    }

    #[test]
    fn a_taken_name_waits_for_the_save_as_dialog() {
        let mut t = DownloadTable::new();
        t.apply(&Outgoing::Added {
            id: 6,
            url: "u".into(),
        });
        t.apply(&Outgoing::NameTaken {
            id: 6,
            url: "u".into(),
            path: r"C:\dl\report.pdf".into(),
            total: Some(42),
        });
        let e = t.get(6).unwrap();
        assert_eq!(e.status, Status::Confirming);
        assert_eq!(e.path.as_deref(), Some(r"C:\dl\report.pdf"));
        assert_eq!(e.total, Some(42));
        assert_eq!(e.connections, 0, "never started, so a retry does not reuse the file");
    }

    #[test]
    fn snapshot_replaces_and_round_trips() {
        let mut a = DownloadTable::new();
        a.apply(&started(1));
        a.apply(&started(2));
        let snap = a.snapshot();
        assert_eq!(snap.iter().map(|e| e.id).collect::<Vec<_>>(), vec![1, 2]);

        // A client that joins late adopts the snapshot wholesale.
        let mut b = DownloadTable::new();
        b.apply(&started(9));
        b.apply(&Outgoing::Downloads { items: snap.clone() });
        assert_eq!(b.snapshot(), snap);

        // The snapshot survives the wire.
        let json = serde_json::to_string(&Outgoing::Downloads { items: snap.clone() }).unwrap();
        let back: Outgoing = serde_json::from_str(&json).unwrap();
        let mut c = DownloadTable::new();
        c.apply(&back);
        assert_eq!(c.snapshot(), snap);
    }

    #[test]
    fn ignores_non_download_messages() {
        let mut t = DownloadTable::new();
        t.apply(&Outgoing::Pong);
        t.apply(&Outgoing::Error { message: "x".into() });
        assert!(t.is_empty());
    }
}
