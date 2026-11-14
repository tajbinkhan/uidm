use udm_domain::download::Download;
use udm_domain::ports::{Torrents, TorrentsError};

/// Where a `.torrent` file comes from.
#[derive(Debug, Clone, Copy)]
pub enum TorrentSource<'a> {
    /// Its contents, which a client read from disk.
    File(&'a [u8]),
    /// The web address the download names, which the browser handed over:
    /// fetched with its cookies and headers.
    Web,
}

/// Turn a download of a `.torrent` file into a download of the torrent:
/// the client reads and keeps the file, and the download's address becomes
/// the torrent's magnet link, named as the torrent is. It is then added like
/// any download, so the save-as dialog (with the torrent's files) or the
/// queue follows.
pub struct AddTorrent<'a, T: ?Sized> {
    torrents: &'a T,
}

impl<'a, T: Torrents + ?Sized> AddTorrent<'a, T> {
    pub fn new(torrents: &'a T) -> Self {
        AddTorrent { torrents }
    }

    pub fn run(&self, source: TorrentSource<'_>, download: Download) -> Result<Download, TorrentsError> {
        let (magnet, info) = match source {
            TorrentSource::File(bytes) => self.torrents.add_file(bytes)?,
            TorrentSource::Web => self.torrents.fetch_file(&download)?,
        };
        tracing::info!(info_hash = %info.info_hash, files = info.files.len(), "torrent file read");
        Ok(download.into_torrent(&magnet, &info.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::torrents::testing::{magnet, Client};

    #[test]
    fn a_torrent_file_becomes_a_download_of_its_magnet_link() {
        let client = Client::default();
        let asked = Download {
            url: "https://site.example/a.torrent".into(),
            cookies: Some("sid=1".into()),
            folder: Some("/music".into()),
            description: Some("note".into()),
            start: Some(false),
            ..Download::default()
        };
        let added = AddTorrent::new(&client).run(TorrentSource::Web, asked).unwrap();
        assert_eq!(client.fetched.lock().unwrap().as_slice(), ["https://site.example/a.torrent"]);
        assert_eq!(added.url.as_str(), magnet());
        assert!(added.url.is_address());
        assert_eq!(added.filename.as_deref(), Some("Album"));
        assert_eq!((added.folder.as_deref(), added.description.as_deref()), (Some("/music"), Some("note")));
        assert_eq!((added.start, added.cookies), (Some(false), None), "the web part goes");

        let from_disk = AddTorrent::new(&client).run(TorrentSource::File(b"d4:info"), Download::default()).unwrap();
        assert_eq!(from_disk.url.as_str(), magnet());
        assert!(AddTorrent::new(&client).run(TorrentSource::File(b"<html>"), Download::default()).is_err());
    }
}
