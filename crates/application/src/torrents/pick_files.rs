use udm_domain::download::{Download, DownloadError, Status};
use udm_domain::id::DownloadId;
use udm_domain::ports::{DownloadControl, DownloadList, Torrents};
use udm_domain::torrent::{FileSelection, TorrentError};

/// What [`PickFiles`] did with the new choice.
#[derive(Debug, PartialEq, Eq)]
pub enum Picked {
    /// The running torrent takes it at once.
    Running,
    /// The finished torrent has new files to download: it started again.
    Restarted,
    /// It is kept for when the torrent starts.
    Kept,
}

/// Choose which of a torrent download's files to download, before it starts
/// or later: a running torrent changes at once, a finished one given more
/// files starts again for them, any other keeps the choice for its start.
pub struct PickFiles<'a, D, X, T: ?Sized> {
    downloads: &'a D,
    control: &'a X,
    torrents: &'a T,
}

impl<'a, D: DownloadList, X: DownloadControl, T: Torrents + ?Sized> PickFiles<'a, D, X, T> {
    pub fn new(downloads: &'a D, control: &'a X, torrents: &'a T) -> Self {
        PickFiles { downloads, control, torrents }
    }

    pub fn run(&self, id: DownloadId, files: FileSelection) -> Result<Picked, DownloadError> {
        let entry = self.downloads.entry(id).ok_or(DownloadError::NoDownload(id))?;
        let torrent = entry.torrent.as_ref().ok_or(DownloadError::Torrent(TorrentError::NoFiles))?;
        let before = torrent.selection.clone().or_else(|| FileSelection::all(torrent.files.len()));
        let hash = torrent.info_hash;
        let mut picked = Ok(());
        self.downloads.update(id, |e| {
            picked = e.select_torrent_files(files.clone());
            picked.is_ok()
        });
        picked.map_err(DownloadError::Torrent)?;
        if self.downloads.request(id).is_none() {
            self.downloads.keep_request(id, Download { url: entry.url.clone(), ..Download::default() });
        }
        self.downloads.edit_request(id, |r| r.torrent_files = Some(files.clone()));
        match entry.status {
            Status::Queued | Status::Downloading => {
                if let Err(e) = self.torrents.select(hash, &files) {
                    tracing::warn!(download_id = id.get(), error = %e, "the running torrent kept its files");
                }
                Ok(Picked::Running)
            }
            Status::Done if before.is_none_or(|b| files.adds_to(&b)) => {
                self.downloads.set_waiting(id);
                self.control.dispatch(id).map_err(|e| DownloadError::Unreachable(e.to_string()))?;
                Ok(Picked::Restarted)
            }
            _ => Ok(Picked::Kept),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{row, Control, Downloads};
    use crate::torrents::testing::{hash, info, magnet, Client};
    use udm_domain::download::DownloadEntry;

    fn torrent_row(id: u64, status: Status) -> DownloadEntry {
        let mut e = row(id, status);
        e.url = magnet().into();
        e.set_torrent_info(&info(&["a.flac", "b.flac", "c.flac"]));
        e
    }

    fn pick(downloads: &Downloads, control: &Control, client: &Client, id: u64, files: &[usize]) -> Picked {
        let files = FileSelection::parse(files, 3).unwrap();
        PickFiles::new(downloads, control, client).run(DownloadId::new(id), files).unwrap()
    }

    #[test]
    fn a_running_torrent_takes_the_choice_at_once() {
        let downloads = Downloads::rows([torrent_row(1, Status::Downloading)]);
        let (control, client) = (Control::default(), Client::default());
        assert_eq!(pick(&downloads, &control, &client, 1, &[0, 2]), Picked::Running);
        assert_eq!(client.selected.lock().unwrap().as_slice(), [(hash(), vec![0, 2])]);
        let e = downloads.entry(DownloadId::new(1)).unwrap();
        assert_eq!(e.torrent.unwrap().selection, FileSelection::parse(&[0, 2], 3).ok());
        assert_eq!(e.total.map(|t| t.get()), Some(200));
        assert_eq!(downloads.request(DownloadId::new(1)).unwrap().torrent_files, FileSelection::parse(&[0, 2], 3).ok());
    }

    #[test]
    fn a_finished_torrent_given_more_files_starts_again() {
        let mut done = torrent_row(1, Status::Done);
        done.select_torrent_files(FileSelection::parse(&[0], 3).unwrap()).unwrap();
        let downloads = Downloads::rows([done, torrent_row(2, Status::Done), torrent_row(3, Status::Canceled)]);
        let (control, client) = (Control::default(), Client::default());
        assert_eq!(pick(&downloads, &control, &client, 1, &[0, 1]), Picked::Restarted);
        assert_eq!(control.dispatched(), [1]);
        assert_eq!(pick(&downloads, &control, &client, 2, &[1]), Picked::Kept, "fewer files: nothing to fetch");
        assert_eq!(pick(&downloads, &control, &client, 3, &[1]), Picked::Kept);
        assert_eq!(control.dispatched(), [1]);
    }

    #[test]
    fn only_a_torrent_with_known_files_takes_a_choice() {
        let downloads = Downloads::rows([row(1, Status::Done)]);
        let (control, client) = (Control::default(), Client::default());
        let files = FileSelection::parse(&[0], 1).unwrap();
        let refused = PickFiles::new(&downloads, &control, &client).run(DownloadId::new(1), files.clone());
        assert_eq!(refused, Err(DownloadError::Torrent(TorrentError::NoFiles)));
        let mut unknown = row(2, Status::Queued);
        unknown.url = magnet().into();
        unknown.torrent = udm_domain::torrent::TorrentEntry::for_link(&magnet());
        let downloads = Downloads::rows([unknown]);
        let refused = PickFiles::new(&downloads, &control, &client).run(DownloadId::new(2), files);
        assert_eq!(refused, Err(DownloadError::Torrent(TorrentError::NoFiles)));
    }
}
