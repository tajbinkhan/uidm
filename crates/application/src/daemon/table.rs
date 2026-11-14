//! Applying the daemon's events to the download list
//! (`udm_domain::table`).

use udm_domain::table::{Change, DownloadTable};

use super::Event;

/// The change `event` makes to the download list, if it makes one.
pub fn change_of(event: &Event) -> Option<Change<'_>> {
    Some(match event {
        Event::Added { id, url } => Change::Added { id: *id, url: url.as_str() },
        Event::Started { id, url, path, total, resumable, connections, partial, validator, country } => {
            Change::Started {
                id: *id,
                url: url.as_str(),
                path,
                total: *total,
                resumable: *resumable,
                connections: *connections,
                partial: partial.as_deref(),
                validator: validator.as_ref(),
                country: *country,
            }
        }
        Event::Progress { id, url, done, total, parts, .. } => {
            Change::Progress { id: *id, url: url.as_str(), done: *done, total: *total, parts: *parts }
        }
        Event::Done { id, url, path, note } => Change::Done { id: *id, url: url.as_str(), path, note: note.as_deref() },
        Event::Failed { id, url, error } => Change::Failed { id: *id, url: url.as_str(), error },
        Event::Canceled { id, url } => Change::Canceled { id: *id, url: url.as_str() },
        Event::NameTaken { id, url, path, total } => {
            Change::NameTaken { id: *id, url: url.as_str(), path, total: *total }
        }
        Event::Removed { id } => Change::Removed { id: *id },
        Event::TorrentInfo { id, url, info } => Change::TorrentInfo { id: *id, url: url.as_str(), info },
        Event::TorrentStats { id, url, stats } => Change::TorrentStats { id: *id, url: url.as_str(), stats },
        Event::Updated { item } => Change::Updated(item),
        Event::Downloads { items } => Change::Downloads(items),
        _ => return None,
    })
}

/// Fold `event` into `table`. `now` (seconds since 1970) dates a download
/// that starts. Events that do not describe a download leave it alone.
pub fn apply(table: &mut DownloadTable, event: &Event, now: u64) {
    if let Some(change) = change_of(event) {
        table.change(change, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use udm_domain::id::DownloadId;

    #[test]
    fn download_events_change_the_list_and_others_do_not() {
        let mut t = DownloadTable::new();
        apply(&mut t, &Event::Added { id: DownloadId::new(1), url: "u".into() }, 0);
        apply(&mut t, &Event::Error { error: crate::daemon::Failure::Other("x".into()) }, 0);
        apply(&mut t, &Event::Show, 0);
        assert_eq!(t.len(), 1);
        apply(&mut t, &Event::Failed { id: DownloadId::new(1), url: "u".into(), error: "timed out".into() }, 0);
        assert_eq!(t.get(DownloadId::new(1)).unwrap().error.as_deref(), Some("timed out"));
        apply(&mut t, &Event::Removed { id: DownloadId::new(1) }, 0);
        assert!(t.is_empty());
    }
}
