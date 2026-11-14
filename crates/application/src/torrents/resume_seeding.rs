use std::path::Path;

use udm_domain::download::Status;
use udm_domain::id::DownloadId;
use udm_domain::ports::{DownloadList, Torrents};
use udm_domain::torrent::TorrentPhase;

/// When UDM starts: the finished torrents that were seeding when it last
/// stopped seed again, from their files. One that cannot (its file is gone,
/// say) is complete instead. Returns the downloads seeding again.
pub struct ResumeSeeding<'a, D, T: ?Sized> {
    downloads: &'a D,
    torrents: &'a T,
}

impl<'a, D: DownloadList, T: Torrents + ?Sized> ResumeSeeding<'a, D, T> {
    pub fn new(downloads: &'a D, torrents: &'a T) -> Self {
        ResumeSeeding { downloads, torrents }
    }

    pub fn run(&self) -> Vec<DownloadId> {
        let ids = self.downloads.ids_where(|_, s| s == Status::Done);
        let mut seeding = Vec::new();
        for id in ids {
            let Some(entry) = self.downloads.entry(id) else { continue };
            let Some(torrent) = entry.torrent.as_ref().filter(|t| t.phase == TorrentPhase::Seeding) else {
                continue;
            };
            let magnet = entry.url.address().and_then(|a| a.magnet());
            let result = match (magnet, entry.path.as_deref()) {
                (Some(magnet), Some(path)) => self
                    .torrents
                    .seed(id, &magnet, Path::new(path), torrent.selection.as_ref(), torrent.uploaded)
                    .map_err(|e| e.to_string()),
                _ => Err("it has no magnet link or no files".to_string()),
            };
            match result {
                Ok(()) => seeding.push(id),
                Err(error) => {
                    tracing::warn!(download_id = id.get(), error = %error, "does not seed again");
                    self.downloads.update(id, |e| match e.torrent.as_mut() {
                        Some(t) => {
                            t.phase = TorrentPhase::Complete;
                            true
                        }
                        None => false,
                    });
                }
            }
        }
        seeding
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{row, Downloads};
    use crate::torrents::testing::{info, magnet, Client};
    use udm_domain::bytes::ByteCount;
    use udm_domain::download::DownloadEntry;

    fn torrent(id: u64, phase: TorrentPhase, path: Option<&str>) -> DownloadEntry {
        let mut e = row(id, Status::Done);
        e.url = magnet().into();
        e.path = path.map(str::to_string);
        e.set_torrent_info(&info(&["a.flac"]));
        let t = e.torrent.as_mut().unwrap();
        t.phase = phase;
        t.uploaded = ByteCount::new(7);
        e
    }

    #[test]
    fn torrents_that_seeded_seed_again() {
        let downloads = Downloads::rows([
            torrent(1, TorrentPhase::Seeding, Some("/dl/Album")),
            torrent(2, TorrentPhase::Complete, Some("/dl/Other")),
            torrent(3, TorrentPhase::Seeding, None),
            row(4, Status::Done),
        ]);
        let client = Client::default();
        assert_eq!(ResumeSeeding::new(&downloads, &client).run(), [DownloadId::new(1)]);
        let seeded = client.seeded.lock().unwrap().clone();
        assert_eq!(seeded, [(DownloadId::new(1), "/dl/Album".into(), None, ByteCount::new(7))]);
        let gone = downloads.entry(DownloadId::new(3)).unwrap().torrent.unwrap();
        assert_eq!(gone.phase, TorrentPhase::Complete);
    }
}
