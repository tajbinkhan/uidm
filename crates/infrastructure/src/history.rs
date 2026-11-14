//! The daemon's download list on disk, so it survives restarts.
//!
//! - `%LOCALAPPDATA%\UDM\history.json` holds every download's entry (URL,
//!   file, size, progress, status) and the next id to hand out. It never holds
//!   cookies or request headers.
//! - `%LOCALAPPDATA%\UDM\requests.bin` holds the original requests of
//!   downloads that can still be retried, cookies included, encrypted with
//!   DPAPI for the current user. It is deleted when no such request is left.
//!
//! Both are written through a temp file and a rename, so a crash never leaves
//! a half-written file.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::dpapi;
use crate::settings::write_atomic;
use udm_domain::download::{Download, DownloadEntry, Status};
use udm_domain::id::DownloadId;
use udm_domain::package::Package;
use udm_domain::queue::{Queue, MAIN_QUEUE};

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct History {
    /// The id the next download gets, so ids keep increasing across restarts.
    pub next_id: DownloadId,
    /// Every download, oldest first.
    pub items: Vec<DownloadEntry>,
    /// The queues, the main queue first.
    #[serde(default)]
    pub queues: Vec<Queue>,
    /// The packages, oldest first. Files from before packages have none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<Package>,
}

impl History {
    /// Read the history at `path`. A missing file is an empty history.
    /// Downloads that were queued or running when it was written are marked
    /// interrupted, since nothing is running for them any more, unless they
    /// belong to a queue: those wait in it again. See `tidy_queues` for the
    /// rest.
    pub fn load(path: &Path) -> io::Result<History> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let mut history = History::default();
                history.tidy_queues(false);
                return Ok(history);
            }
            Err(e) => return Err(e),
        };
        let mut history: History =
            serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        // Every daemon with queues saves at least the main one, so a history
        // without any is from before queues.
        let before_queues = history.queues.is_empty();
        history.tidy_queues(before_queues);
        let queued: std::collections::HashSet<DownloadId> =
            history.queues.iter().flat_map(|q| q.order.iter().copied()).collect();
        for e in &mut history.items {
            if !e.status.is_terminal() {
                e.status = if queued.contains(&e.id) { Status::Waiting } else { Status::Interrupted };
            }
            // The question it was waiting on went away with the daemon.
            e.duplicate_of = None;
            // Before the parent web page was kept apart from the referrer,
            // the referrer stood for both, so "By Parent Web Page" keeps
            // sorting old rows by it.
            if e.page.is_none() {
                e.page = e.referrer.clone();
            }
        }
        history.tidy_packages();
        let max_id = history.items.iter().map(|e| e.id).max().unwrap_or_default();
        history.next_id = history.next_id.max(max_id.next());
        Ok(history)
    }

    /// Make the queues consistent with the downloads: the main queue exists
    /// and comes first, every queue lists only downloads that exist, no
    /// download is in two queues, and finished downloads leave a one-time
    /// queue (a synchronizing queue keeps them, to check each run). With `before_queues` (a history saved before there were queues) every
    /// waiting download joins the main queue. Otherwise a waiting download
    /// outside every queue stays there: the user took it out with "Remove
    /// from queue", and putting it back would let a queue start it.
    pub fn tidy_queues(&mut self, before_queues: bool) {
        if !self.queues.iter().any(|q| q.id == MAIN_QUEUE) {
            self.queues.insert(0, Queue::main());
        }
        self.queues.sort_by_key(|q| q.id != MAIN_QUEUE);
        let status: HashMap<DownloadId, Status> = self.items.iter().map(|e| (e.id, e.status)).collect();
        let mut seen = std::collections::HashSet::new();
        for q in &mut self.queues {
            let keep_done = q.kind.is_sync();
            q.order.retain(|id| status.get(id).is_some_and(|s| keep_done || *s != Status::Done) && seen.insert(*id));
        }
        for e in self.items.iter().filter(|_| before_queues) {
            if e.status == Status::Waiting && !seen.contains(&e.id) {
                self.queues[0].order.push(e.id);
            }
        }
    }

    /// Make the packages consistent with the downloads: each lists only
    /// downloads that exist, once, a package left with none goes, and one
    /// being unpacked when the daemon stopped waits again.
    pub fn tidy_packages(&mut self) {
        let ids: std::collections::HashSet<DownloadId> = self.items.iter().map(|e| e.id).collect();
        let mut seen = std::collections::HashSet::new();
        for p in &mut self.packages {
            p.downloads.retain(|id| ids.contains(id) && seen.insert(*id));
            p.unpack = std::mem::take(&mut p.unpack).after_restart();
        }
        self.packages.retain(|p| !p.downloads.is_empty());
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        write_atomic(path, &bytes)
    }
}

/// Read the saved requests at `path`. A missing file means none.
pub fn load_requests(path: &Path) -> io::Result<HashMap<DownloadId, Download>> {
    let sealed = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(e),
    };
    let plain = dpapi::unprotect(&sealed)?;
    // The requests hold cookies and headers; serde's message could quote one.
    let list: Vec<(DownloadId, Download)> =
        serde_json::from_slice(&plain).map_err(|e| crate::logging::json_error(&e))?;
    Ok(list.into_iter().collect())
}

/// Encrypt and save `requests` to `path`, or delete the file when empty.
pub fn save_requests(path: &Path, requests: &HashMap<DownloadId, Download>) -> io::Result<()> {
    if requests.is_empty() {
        return match fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let mut list: Vec<(&DownloadId, &Download)> = requests.iter().collect();
    list.sort_by_key(|(id, _)| **id);
    let plain = serde_json::to_vec(&list).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write_atomic(path, &dpapi::protect(&plain)?)
}

fn local_data_dir() -> io::Result<PathBuf> {
    crate::paths::data_dir()
}

/// `%LOCALAPPDATA%\UDM\history.json`.
pub fn history_path() -> io::Result<PathBuf> {
    Ok(local_data_dir()?.join("history.json"))
}

/// `%LOCALAPPDATA%\UDM\requests.bin`.
pub fn requests_path() -> io::Result<PathBuf> {
    Ok(local_data_dir()?.join("requests.bin"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use udm_domain::bytes::ByteCount;
    use udm_domain::id::QueueId;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("udm_history_{}_{name}", std::process::id()))
    }

    fn entry(id: u64, status: Status) -> DownloadEntry {
        DownloadEntry {
            id: DownloadId::new(id),
            url: format!("https://x/{id}.zip").into(),
            path: Some(format!("C:\\dl\\{id}.zip")),
            total: Some(ByteCount::new(1000)),
            done: ByteCount::new(400),
            status,
            error: None,
            resumable: true,
            connections: 4,
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

    #[test]
    fn missing_file_is_empty() {
        let h = History::load(&temp_path("missing.json")).unwrap();
        assert!(h.items.is_empty());
        assert!(load_requests(&temp_path("missing.bin")).unwrap().is_empty());
    }

    #[test]
    fn old_entries_take_the_referrer_as_their_page() {
        let path = temp_path("page.json");
        let mut old = serde_json::to_value(entry(1, Status::Done)).unwrap();
        old["referrer"] = "https://x/list".into();
        assert!(old.get("page").is_none(), "saved before pages were kept");
        let mut new = entry(2, Status::Done);
        new.referrer = Some("https://x/frame".into());
        new.page = Some("https://x/article".into());
        let history = serde_json::json!({ "next_id": 3, "items": [old, new] });
        fs::write(&path, serde_json::to_vec(&history).unwrap()).unwrap();
        let h = History::load(&path).unwrap();
        assert_eq!(h.items[0].referrer.as_deref(), Some("https://x/list"));
        assert_eq!(h.items[0].page.as_deref(), Some("https://x/list"));
        assert_eq!(h.items[1].referrer.as_deref(), Some("https://x/frame"));
        assert_eq!(h.items[1].page.as_deref(), Some("https://x/article"));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn running_downloads_load_as_interrupted() {
        let path = temp_path("interrupted.json");
        History {
            packages: Vec::new(),
            queues: Vec::new(),
            next_id: DownloadId::new(5),
            items: vec![
                entry(1, Status::Done),
                entry(2, Status::Downloading),
                entry(3, Status::Queued),
                entry(4, Status::Failed),
            ],
        }
        .save(&path)
        .unwrap();
        let h = History::load(&path).unwrap();
        let statuses: Vec<Status> = h.items.iter().map(|e| e.status).collect();
        assert_eq!(statuses, [Status::Done, Status::Interrupted, Status::Interrupted, Status::Failed]);
        assert_eq!(h.items[1].done, ByteCount::new(400), "progress is kept");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn queues_load_tidy_and_keep_their_downloads_waiting() {
        let path = temp_path("queues.json");
        let mut night = Queue::new(QueueId::new(2), "Night");
        night.order =
            vec![DownloadId::new(3), DownloadId::new(1), DownloadId::new(2), DownloadId::new(99), DownloadId::new(3)];
        night.running = true;
        History {
            next_id: DownloadId::new(6),
            items: vec![
                entry(1, Status::Done),
                entry(2, Status::Downloading),
                entry(3, Status::Waiting),
                entry(4, Status::Waiting),
                entry(5, Status::Downloading),
            ],
            queues: vec![night],
            packages: Vec::new(),
        }
        .save(&path)
        .unwrap();
        let h = History::load(&path).unwrap();
        assert_eq!(h.queues[0].id, MAIN_QUEUE, "the main queue is added first");
        assert_eq!(
            h.queues[1].order,
            [DownloadId::new(3), DownloadId::new(2)],
            "finished, missing and repeated ids are dropped"
        );
        assert!(h.queues[0].order.is_empty(), "a download taken out of its queue stays out");
        assert!(h.queues[1].running, "a running queue stays started");
        let status = |id: u64| h.items.iter().find(|e| e.id == DownloadId::new(id)).unwrap().status;
        assert_eq!(status(2), Status::Waiting, "a queued download waits again");
        assert_eq!(status(4), Status::Waiting, "and one outside every queue still waits");
        assert_eq!(status(5), Status::Interrupted, "a download outside queues is interrupted");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn a_synchronizing_queue_keeps_its_finished_downloads() {
        let path = temp_path("sync_queue.json");
        let mut sync = Queue::new(QueueId::new(2), "Mirror");
        sync.kind = udm_domain::queue::QueueKind::Synchronize;
        sync.order = vec![DownloadId::new(1), DownloadId::new(2)];
        let mut once = Queue::new(QueueId::new(3), "Once");
        once.order = vec![DownloadId::new(3), DownloadId::new(4)];
        History {
            next_id: DownloadId::new(5),
            items: vec![
                entry(1, Status::Done),
                entry(2, Status::Waiting),
                entry(3, Status::Done),
                entry(4, Status::Waiting),
            ],
            queues: vec![Queue::main(), sync, once],
            packages: Vec::new(),
        }
        .save(&path)
        .unwrap();
        let h = History::load(&path).unwrap();
        assert_eq!(h.queues[1].kind, udm_domain::queue::QueueKind::Synchronize, "the type is saved");
        assert_eq!(
            h.queues[1].order,
            [DownloadId::new(1), DownloadId::new(2)],
            "a finished download stays to be checked"
        );
        assert_eq!(h.queues[2].order, [DownloadId::new(4)], "a one-time queue lets it go");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn packages_are_kept_and_tidied_and_older_files_have_none() {
        use udm_domain::id::PackageId;
        use udm_domain::package::{PackageName, Unpack};
        let path = temp_path("packages.json");
        let package = |id: u64, downloads: &[u64], unpack: Unpack| {
            let mut p = Package::new(
                PackageId::new(id),
                PackageName::parse(&format!("P{id}")).unwrap(),
                Some("/dl/P".into()),
                downloads.iter().map(|d| DownloadId::new(*d)).collect(),
            );
            p.unpack = unpack;
            p
        };
        History {
            next_id: DownloadId::new(4),
            items: vec![entry(1, Status::Done), entry(2, Status::Done), entry(3, Status::Done)],
            queues: Vec::new(),
            packages: vec![
                package(1, &[1, 99, 2], Unpack::Unpacking),
                package(2, &[2, 3], Unpack::Done { folder: "/dl/P/P2".into(), files: 3 }),
                package(3, &[98], Unpack::Waiting),
            ],
        }
        .save(&path)
        .unwrap();
        let h = History::load(&path).unwrap();
        assert_eq!(h.packages.len(), 2, "a package left with no downloads goes");
        assert_eq!(h.packages[0].downloads, [DownloadId::new(1), DownloadId::new(2)], "a missing download goes");
        assert_eq!(h.packages[0].unpack, Unpack::Waiting, "unpacking stopped with the daemon");
        assert_eq!(h.packages[1].downloads, [DownloadId::new(3)], "a download is in one package only");
        assert_eq!(h.packages[1].folder.as_deref(), Some("/dl/P"));
        assert!(matches!(h.packages[1].unpack, Unpack::Done { files: 3, .. }));

        // A file from before packages loads without any, and is saved
        // without the field.
        let old = serde_json::json!({ "next_id": 2, "items": [entry(1, Status::Done)], "queues": [] });
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let h = History::load(&path).unwrap();
        assert!(h.packages.is_empty());
        h.save(&path).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(saved.get("packages").is_none());
        fs::remove_file(&path).ok();
    }

    #[test]
    fn waiting_downloads_from_before_queues_join_the_main_queue() {
        let path = temp_path("before_queues.json");
        History {
            next_id: DownloadId::new(3),
            items: vec![entry(1, Status::Waiting), entry(2, Status::Failed)],
            queues: Vec::new(),
            packages: Vec::new(),
        }
        .save(&path)
        .unwrap();
        let h = History::load(&path).unwrap();
        assert_eq!(h.queues.len(), 1);
        assert_eq!(h.queues[0].order, [DownloadId::new(1)], "only the waiting download joins");
        fs::remove_file(&path).ok();
    }

    /// A history saved before ids had types loads with the same numbers
    /// and is saved back in the same shape.
    #[test]
    fn ids_keep_their_file_shape() {
        let path = temp_path("id_shape.json");
        let mut item = serde_json::to_value(entry(5, Status::Waiting)).unwrap();
        item["id"] = 5.into();
        let mut night = serde_json::to_value(Queue::new(QueueId::new(2), "Night")).unwrap();
        night["order"] = serde_json::json!([5]);
        let main = serde_json::to_value(Queue::main()).unwrap();
        let old = serde_json::json!({ "next_id": 7, "items": [item], "queues": [main, night] });
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let h = History::load(&path).unwrap();
        assert_eq!(h.next_id, DownloadId::new(7));
        assert_eq!(h.items[0].id, DownloadId::new(5));
        assert_eq!((h.queues[1].id, h.queues[1].order.as_slice()), (QueueId::new(2), &[DownloadId::new(5)][..]));
        h.save(&path).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["next_id"], 7);
        assert_eq!(saved["items"][0]["id"], 5);
        assert_eq!(saved["queues"][0]["id"], 1);
        assert_eq!(saved["queues"][1]["id"], 2);
        assert_eq!(saved["queues"][1]["order"], serde_json::json!([5]));
        fs::remove_file(&path).ok();
    }

    /// Byte counts in the history (`total`, `done`, the validator's size)
    /// read from an old file as plain numbers and are saved back the same.
    #[test]
    fn byte_counts_keep_their_file_shape() {
        let path = temp_path("byte_shape.json");
        let mut item = serde_json::to_value(entry(5, Status::Waiting)).unwrap();
        item["total"] = 4_000_000_000u64.into();
        item["done"] = 1_048_576.into();
        item["validator"] = serde_json::json!({ "size": 4_000_000_000u64, "etag": "x" });
        let old = serde_json::json!({ "next_id": 7, "items": [item.clone()], "queues": [] });
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let h = History::load(&path).unwrap();
        assert_eq!((h.items[0].total, h.items[0].done), (Some(ByteCount::new(4_000_000_000)), ByteCount::mib(1)));
        assert_eq!(h.items[0].validator.as_ref().and_then(|v| v.size), Some(ByteCount::new(4_000_000_000)));
        h.save(&path).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["items"][0], item);
        fs::remove_file(&path).ok();
    }

    /// Addresses and file names in the history read from an old file as
    /// plain strings and are saved back the same, even an address UDM
    /// would not take now: the row is kept, not dropped with the file.
    #[test]
    fn addresses_keep_their_file_shape_and_odd_ones_load() {
        let path = temp_path("address_shape.json");
        let mut good = serde_json::to_value(entry(5, Status::Done)).unwrap();
        good["url"] = "HTTPS://x/a b.zip?q=1#f".into();
        good["path"] = "/dl/a b.zip".into();
        let mut odd = serde_json::to_value(entry(6, Status::Failed)).unwrap();
        odd["url"] = "file:///etc/passwd".into();
        let mut empty = serde_json::to_value(entry(7, Status::Canceled)).unwrap();
        empty["url"] = "".into();
        let old = serde_json::json!({ "next_id": 8, "items": [good, odd, empty], "queues": [] });
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let h = History::load(&path).unwrap();
        assert_eq!(h.items.len(), 3, "no row is dropped");
        assert!(h.items[0].url.is_address());
        assert_eq!((h.items[1].url.as_str(), h.items[1].url.is_address()), ("file:///etc/passwd", false));
        assert!(h.items[2].url.is_empty());
        h.save(&path).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["items"], old["items"]);
        fs::remove_file(&path).ok();
    }

    /// The saved requests keep their addresses and suggested file names as
    /// they were, odd ones too.
    #[test]
    fn saved_requests_keep_odd_addresses_and_names() {
        let req_path = temp_path("odd-requests.bin");
        let plain = br#"[[2,{"url":"javascript:void(0)","filename":"../a:b.zip"}],[3,{"url":"ftp://f/c.iso"}]]"#;
        fs::write(&req_path, dpapi::protect(plain).unwrap()).unwrap();
        let back = load_requests(&req_path).unwrap();
        assert_eq!(back[&DownloadId::new(2)].url.as_str(), "javascript:void(0)");
        assert_eq!(back[&DownloadId::new(2)].filename.as_deref(), Some("../a:b.zip"));
        assert!(back[&DownloadId::new(3)].url.is_address());
        save_requests(&req_path, &back).unwrap();
        let again = dpapi::unprotect(&fs::read(&req_path).unwrap()).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&again).unwrap(),
            serde_json::from_slice::<serde_json::Value>(plain).unwrap()
        );
        fs::remove_file(&req_path).ok();
    }

    #[test]
    fn next_id_is_never_below_a_saved_id() {
        let path = temp_path("next_id.json");
        History {
            next_id: DownloadId::new(1),
            items: vec![entry(41, Status::Done)],
            queues: Vec::new(),
            packages: Vec::new(),
        }
        .save(&path)
        .unwrap();
        assert_eq!(History::load(&path).unwrap().next_id, DownloadId::new(42));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn a_bad_sealed_request_does_not_show_its_secrets() {
        let req_path = temp_path("bad-requests.bin");
        let plain = br#"[[2, {"url": "https://x/2.zip", "headers": "Authorization: Bearer hunter2"}]]"#;
        fs::write(&req_path, dpapi::protect(plain).unwrap()).unwrap();
        let e = load_requests(&req_path).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert!(!e.to_string().contains("hunter2"), "{e}");
        assert!(e.to_string().contains("line 1"), "{e}");
        fs::remove_file(&req_path).unwrap();
    }

    #[test]
    fn requests_are_encrypted_and_never_in_the_history() {
        let mut requests = HashMap::new();
        requests.insert(
            DownloadId::new(2),
            Download {
                url: "https://x/2.zip".into(),
                cookies: Some("session=topsecret".into()),
                headers: [("Authorization".to_string(), "Bearer hunter2".to_string())].into(),
                ..Download::default()
            },
        );
        let req_path = temp_path("requests.bin");
        save_requests(&req_path, &requests).unwrap();
        let raw = fs::read(&req_path).unwrap();
        for secret in [&b"topsecret"[..], b"hunter2"] {
            assert!(!raw.windows(secret.len()).any(|w| w == secret));
        }
        let back = load_requests(&req_path).unwrap();
        assert_eq!(back[&DownloadId::new(2)].cookies.as_deref(), Some("session=topsecret"));

        // Saving none deletes the file.
        save_requests(&req_path, &HashMap::new()).unwrap();
        assert!(!req_path.exists());

        let hist_path = temp_path("no-secrets.json");
        History {
            next_id: DownloadId::new(3),
            items: vec![entry(2, Status::Downloading)],
            queues: Vec::new(),
            packages: Vec::new(),
        }
        .save(&hist_path)
        .unwrap();
        let text = fs::read_to_string(&hist_path).unwrap().to_lowercase();
        assert!(!text.contains("cookie") && !text.contains("header"));
        fs::remove_file(&hist_path).ok();
    }
}
