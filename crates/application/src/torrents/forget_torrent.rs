use udm_domain::download::DownloadEntry;
use udm_domain::ports::{DownloadList, Torrents};

/// Once a torrent download left the list, the client forgets the torrent
/// (its seeding, kept file and resume data), unless another download in the
/// list is the same torrent. Returns whether it was forgotten.
pub struct ForgetTorrent<'a, D, T: ?Sized> {
    downloads: &'a D,
    torrents: &'a T,
}

impl<'a, D: DownloadList, T: Torrents + ?Sized> ForgetTorrent<'a, D, T> {
    pub fn new(downloads: &'a D, torrents: &'a T) -> Self {
        ForgetTorrent { downloads, torrents }
    }

    /// `removed` is the row as it was before it left the list.
    pub fn run(&self, removed: &DownloadEntry) -> bool {
        let Some(hash) = removed.torrent.as_ref().map(|t| t.info_hash) else {
            return false;
        };
        let other =
            self.downloads.find_last(|e| e.id != removed.id && e.torrent.as_ref().is_some_and(|t| t.info_hash == hash));
        if other.is_some() {
            return false;
        }
        self.torrents.forget(hash);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{row, Downloads};
    use crate::torrents::testing::{hash, magnet, Client};
    use udm_domain::download::Status;
    use udm_domain::torrent::TorrentEntry;

    fn torrent(id: u64) -> DownloadEntry {
        let mut e = row(id, Status::Done);
        e.url = magnet().into();
        e.torrent = TorrentEntry::for_link(&magnet());
        e
    }

    #[test]
    fn a_torrent_is_forgotten_with_its_last_download() {
        let client = Client::default();
        let downloads = Downloads::rows([torrent(2)]);
        assert!(!ForgetTorrent::new(&downloads, &client).run(&torrent(1)), "2 is the same torrent");
        let downloads = Downloads::rows([row(2, Status::Done)]);
        assert!(ForgetTorrent::new(&downloads, &client).run(&torrent(1)));
        assert!(!ForgetTorrent::new(&downloads, &client).run(&row(3, Status::Done)), "no torrent");
        assert_eq!(client.forgotten.lock().unwrap().as_slice(), [hash()]);
    }
}
