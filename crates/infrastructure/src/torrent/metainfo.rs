//! Reading `.torrent` files (metainfo) into the domain's `TorrentInfo`, and
//! writing one back with another list of trackers.

use librqbit_core::torrent_metainfo::torrent_from_bytes;
use udm_domain::bytes::ByteCount;
use udm_domain::filename::sanitize_filename;
use udm_domain::torrent::{InfoHash, MagnetLink, TorrentError, TorrentFiles, TorrentInfo, MAX_METAINFO_BYTES};

/// A `.torrent` file, read.
#[derive(Debug, Clone)]
pub struct Metainfo {
    pub info: TorrentInfo,
    /// The bencoded info dictionary, as the file has it (its SHA-1 is the
    /// info hash).
    pub raw_info: Vec<u8>,
    /// Its trackers, in order, once each.
    pub trackers: Vec<String>,
}

impl Metainfo {
    /// Read `bytes` as a `.torrent` file: at most [`MAX_METAINFO_BYTES`],
    /// with an info dictionary librqbit takes and files the domain's rules
    /// take (see `TorrentFiles::parse`).
    pub fn read(bytes: &[u8]) -> Result<Metainfo, TorrentError> {
        if bytes.len() > MAX_METAINFO_BYTES {
            return Err(TorrentError::TooBig { max: MAX_METAINFO_BYTES });
        }
        let parsed = torrent_from_bytes(bytes).map_err(|e| TorrentError::BadTorrent(e.to_string()))?;
        let mut trackers = Vec::new();
        for t in parsed.iter_announce() {
            if let Ok(url) = std::str::from_utf8(t.as_ref()) {
                if udm_domain::torrent::is_tracker(url) && !trackers.iter().any(|k| k == url) {
                    trackers.push(url.to_string());
                }
            }
        }
        let raw_info = parsed.info.raw_bytes.as_ref().to_vec();
        let info_hash = InfoHash::from_bytes(parsed.info_hash.0);
        let validated = parsed.info.data.validate().map_err(|e| TorrentError::BadTorrent(e.to_string()))?;
        let raw: Vec<(Vec<String>, u64, bool)> = validated
            .iter_file_details()
            .map(|f| (f.filename.iter_components().map(|c| c.into_owned()).collect(), f.len, f.attrs().padding))
            .collect();
        let files = TorrentFiles::parse(raw)?;
        let first = files.as_slice().first().map(|f| f.path.join("_")).unwrap_or_default();
        let name = validated
            .name()
            .and_then(|n| sanitize_filename(&n))
            .map(|n| n.into_string())
            .unwrap_or_else(|| sanitize_filename(&first).map_or_else(|| info_hash.hex(), |n| n.into_string()));
        let lengths = validated.lengths();
        let info = TorrentInfo {
            info_hash,
            name,
            files,
            piece_size: ByteCount::new(u64::from(validated.info().piece_length)),
            pieces: lengths.total_pieces(),
            private: validated.info().private,
        };
        Ok(Metainfo { info, raw_info, trackers })
    }

    /// The magnet link for it: its hash, name and trackers.
    pub fn magnet(&self) -> MagnetLink {
        MagnetLink {
            info_hash: self.info.info_hash,
            name: Some(self.info.name.clone()),
            trackers: self.trackers.iter().take(udm_domain::torrent::MAX_MAGNET_ENTRIES).cloned().collect(),
            web_seeds: Vec::new(),
            peers: Vec::new(),
        }
    }
}

/// A `.torrent` file of the info dictionary `raw_info` with `trackers` as
/// its trackers, in order.
pub fn build(raw_info: &[u8], trackers: &[String]) -> Vec<u8> {
    fn string(out: &mut Vec<u8>, s: &[u8]) {
        out.extend_from_slice(s.len().to_string().as_bytes());
        out.push(b':');
        out.extend_from_slice(s);
    }
    let mut out = Vec::with_capacity(raw_info.len() + 64 * (trackers.len() + 1));
    out.push(b'd');
    // A dictionary's keys go in sorted order.
    if let Some(first) = trackers.first() {
        string(&mut out, b"announce");
        string(&mut out, first.as_bytes());
        string(&mut out, b"announce-list");
        out.push(b'l');
        for t in trackers {
            out.push(b'l');
            string(&mut out, t.as_bytes());
            out.push(b'e');
        }
        out.push(b'e');
    }
    string(&mut out, b"info");
    out.extend_from_slice(raw_info);
    out.push(b'e');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-file torrent's info dictionary, by hand.
    fn info(name: &str, length: u64) -> Vec<u8> {
        let mut d =
            format!("d6:lengthi{length}e4:name{}:{name}12:piece lengthi16384e6:pieces20:", name.len()).into_bytes();
        d.extend_from_slice(&[7u8; 20]);
        d.push(b'e');
        d
    }

    #[test]
    fn a_built_file_reads_back_with_its_trackers() {
        let raw = info("a.iso", 1000);
        let trackers = vec!["udp://t.example:6969/announce".to_string(), "https://t2.example/a".to_string()];
        let bytes = build(&raw, &trackers);
        let read = Metainfo::read(&bytes).unwrap();
        assert_eq!(read.raw_info, raw);
        assert_eq!(read.trackers, trackers);
        assert_eq!(read.info.name, "a.iso");
        assert_eq!(read.info.files.len(), 1);
        assert_eq!(read.info.files.selected_size(None), ByteCount::new(1000));
        assert_eq!(read.info.pieces, 1);
        assert!(read.info.single_file());
        // The info hash is the SHA-1 of the info dictionary, whatever the
        // trackers.
        assert_eq!(Metainfo::read(&build(&raw, &[])).unwrap().info.info_hash, read.info.info_hash);
        let magnet = read.magnet();
        assert_eq!((magnet.name.as_deref(), magnet.trackers.len()), (Some("a.iso"), 2));
    }

    #[test]
    fn what_is_no_torrent_is_refused() {
        assert!(matches!(Metainfo::read(b"<html>"), Err(TorrentError::BadTorrent(_))));
        assert!(matches!(Metainfo::read(&vec![b'd'; MAX_METAINFO_BYTES + 1]), Err(TorrentError::TooBig { .. })));
        // A name librqbit takes but the file-name rules refuse.
        assert!(Metainfo::read(&build(&info(".", 10), &[])).is_err());
    }

    #[test]
    fn unsafe_names_are_refused() {
        let refused = Metainfo::read(&build(&info("CON:x?.txt", 10), &[]));
        assert!(matches!(refused, Err(TorrentError::UnsafePath(p)) if p == "CON:x?.txt"));
    }
}
