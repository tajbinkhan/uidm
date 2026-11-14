//! Torrents and magnet links. A torrent downloads through the engine like
//! any download (so queues, the speed limit, the quota and pausing apply);
//! these use cases are the rest: turning a `.torrent` file into a download
//! of its magnet link, choosing its files, stopping seeding at its limits,
//! seeding again after a restart, and forgetting a removed torrent.

mod add_torrent;
mod forget_torrent;
mod pick_files;
mod resume_seeding;
mod seed_limits;

pub use add_torrent::{AddTorrent, TorrentSource};
pub use forget_torrent::ForgetTorrent;
pub use pick_files::{PickFiles, Picked};
pub use resume_seeding::ResumeSeeding;
pub use seed_limits::ApplySeedLimits;

#[cfg(test)]
pub(crate) mod testing {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use udm_domain::bytes::ByteCount;
    use udm_domain::download::Download;
    use udm_domain::id::DownloadId;
    use udm_domain::ports::{Seeding, Torrents, TorrentsError};
    use udm_domain::torrent::{FileSelection, InfoHash, MagnetLink, TorrentFiles, TorrentInfo};

    pub const HASH: &str = "c12fe1c06bba254a9dc9f519b335aa7c1367a88a";

    pub fn hash() -> InfoHash {
        InfoHash::parse(HASH).unwrap()
    }

    pub fn magnet() -> String {
        format!("magnet:?xt=urn:btih:{HASH}")
    }

    pub fn info(files: &[&str]) -> TorrentInfo {
        let raw = files.iter().map(|f| (vec![f.to_string()], 100, false)).collect();
        TorrentInfo {
            info_hash: hash(),
            name: "Album".into(),
            files: TorrentFiles::parse(raw).unwrap(),
            piece_size: ByteCount::new(16384),
            pieces: 1,
            private: false,
        }
    }

    /// A `seed` call: the download, its folder, its files and what it
    /// uploaded before.
    pub type Seeded = (DownloadId, PathBuf, Option<Vec<usize>>, ByteCount);

    /// The torrent client, remembering what it was asked.
    #[derive(Default)]
    pub struct Client {
        pub seeding: Mutex<Vec<Seeding>>,
        pub selected: Mutex<Vec<(InfoHash, Vec<usize>)>>,
        pub stopped: Mutex<Vec<InfoHash>>,
        pub seeded: Mutex<Vec<Seeded>>,
        pub forgotten: Mutex<Vec<InfoHash>>,
        pub fetched: Mutex<Vec<String>>,
    }

    impl Torrents for Client {
        fn add_file(&self, metainfo: &[u8]) -> Result<(MagnetLink, TorrentInfo), TorrentsError> {
            if metainfo.starts_with(b"d") {
                Ok((MagnetLink::new(hash()), info(&["a.flac", "b.flac"])))
            } else {
                Err(TorrentsError::Failed("not bencoded".into()))
            }
        }

        fn fetch_file(&self, download: &Download) -> Result<(MagnetLink, TorrentInfo), TorrentsError> {
            self.fetched.lock().unwrap().push(download.url.to_string());
            Ok((MagnetLink::new(hash()), info(&["a.flac"])))
        }

        fn metadata(&self, _: &MagnetLink) -> Result<TorrentInfo, TorrentsError> {
            Ok(info(&["a.flac", "b.flac"]))
        }

        fn select(&self, info_hash: InfoHash, files: &FileSelection) -> Result<(), TorrentsError> {
            self.selected.lock().unwrap().push((info_hash, files.indexes().collect()));
            Ok(())
        }

        fn seeding(&self) -> Vec<Seeding> {
            self.seeding.lock().unwrap().clone()
        }

        fn seed(
            &self,
            id: DownloadId,
            _: &MagnetLink,
            path: &Path,
            files: Option<&FileSelection>,
            uploaded: ByteCount,
        ) -> Result<(), TorrentsError> {
            let files = files.map(|f| f.indexes().collect());
            self.seeded.lock().unwrap().push((id, path.to_path_buf(), files, uploaded));
            Ok(())
        }

        fn stop_seeding(&self, info_hash: InfoHash) {
            self.stopped.lock().unwrap().push(info_hash);
        }

        fn forget(&self, info_hash: InfoHash) {
            self.forgotten.lock().unwrap().push(info_hash);
        }
    }
}
