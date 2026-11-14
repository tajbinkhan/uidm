//! The rules of the windows' torrent controls: which files a tick leaves
//! chosen, what Download File Info answers with, and the seeding limits the
//! download window sets. Plain functions, tested without Tauri.

use std::collections::BTreeSet;

use udm_domain::torrent::{FileSelection, Ratio, SeedLimits, TorrentError, TorrentFiles, TorrentInfo};

use crate::contract::SeedLimitsInput;

/// The real files of a torrent (padding left out), by their numbers.
fn real(files: &TorrentFiles) -> impl Iterator<Item = usize> + '_ {
    files.as_slice().iter().enumerate().filter(|(_, f)| !f.padding).map(|(i, _)| i)
}

fn is_real(files: &TorrentFiles, i: usize) -> bool {
    files.as_slice().get(i).is_some_and(|f| !f.padding)
}

/// The files ticked once `indexes` are ticked (`chosen`) or unticked, from
/// `current`, or every file when there is no `current`. Padding files and
/// numbers the torrent does not have are left out.
pub fn pick(info: &TorrentInfo, current: Option<&BTreeSet<usize>>, indexes: &[usize], chosen: bool) -> BTreeSet<usize> {
    let mut set: BTreeSet<usize> = match current {
        Some(c) => c.clone(),
        None => real(&info.files).collect(),
    };
    for i in indexes.iter().copied().filter(|i| is_real(&info.files, *i)) {
        if chosen {
            set.insert(i);
        } else {
            set.remove(&i);
        }
    }
    set
}

/// The files Download File Info answers with: `None` (every file) when every
/// real file is ticked or nothing was changed, else the ticked ones. Refused
/// when none is ticked.
pub fn answer_files(
    info: &TorrentInfo,
    picks: Option<&BTreeSet<usize>>,
) -> Result<Option<FileSelection>, TorrentError> {
    let Some(picks) = picks else { return Ok(None) };
    if real(&info.files).all(|i| picks.contains(&i)) {
        return Ok(None);
    }
    FileSelection::parse(&picks.iter().copied().collect::<Vec<_>>(), info.files.len()).map(Some)
}

/// A torrent download's files once `indexes` are ticked or unticked in its
/// download window, from `current` (every file when `None`).
pub fn reselect(
    files: &TorrentFiles,
    current: Option<&FileSelection>,
    indexes: &[usize],
    chosen: bool,
) -> Result<FileSelection, TorrentError> {
    if files.is_empty() {
        return Err(TorrentError::NoFiles);
    }
    let mut set: BTreeSet<usize> = match current {
        Some(c) => c.indexes().filter(|i| is_real(files, *i)).collect(),
        None => real(files).collect(),
    };
    for i in indexes.iter().copied() {
        if !is_real(files, i) {
            return Err(TorrentError::NoSuchFile(i));
        }
        if chosen {
            set.insert(i);
        } else {
            set.remove(&i);
        }
    }
    FileSelection::parse(&set.into_iter().collect::<Vec<_>>(), files.len())
}

/// The seeding limits the download window sets: `None` follows the
/// settings'. A limit that is off does not stop seeding.
pub fn seed_limits(input: &SeedLimitsInput) -> Result<Option<SeedLimits>, TorrentError> {
    if !input.own {
        return Ok(None);
    }
    let ratio = if input.ratio_on { Some(Ratio::new(input.ratio)?) } else { None };
    let minutes = input.minutes_on.then_some(input.minutes);
    Ok(Some(SeedLimits { ratio, minutes }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use udm_domain::bytes::ByteCount;
    use udm_domain::torrent::InfoHash;

    fn info(paths: &[(&str, bool)]) -> TorrentInfo {
        let raw = paths.iter().map(|(p, pad)| (p.split('/').map(str::to_string).collect(), 10, *pad)).collect();
        TorrentInfo {
            info_hash: InfoHash::from_bytes([1; 20]),
            name: "Album".into(),
            files: TorrentFiles::parse(raw).unwrap(),
            piece_size: ByteCount::new(16_384),
            pieces: 4,
            private: false,
        }
    }

    #[test]
    fn ticks_start_from_every_real_file() {
        let i = info(&[("a", false), (".pad/1", true), ("b", false), ("c", false)]);
        let none = pick(&i, None, &[2], false);
        assert_eq!(none.into_iter().collect::<Vec<_>>(), [0, 3], "padding is never ticked");
        let back = pick(&i, Some(&[0, 3].into()), &[2, 1, 9], true);
        assert_eq!(back.iter().copied().collect::<Vec<_>>(), [0, 2, 3]);
        assert_eq!(answer_files(&i, Some(&back)), Ok(None), "every real file is every file");
        assert_eq!(answer_files(&i, None), Ok(None));
        assert_eq!(answer_files(&i, Some(&[2].into())), Ok(FileSelection::parse(&[2], 4).ok()));
        assert_eq!(answer_files(&i, Some(&BTreeSet::new())), Err(TorrentError::NothingSelected));
    }

    #[test]
    fn the_download_window_changes_the_rows_selection() {
        let i = info(&[("a", false), ("b", false), ("c", false)]);
        let s = reselect(&i.files, None, &[1], false).unwrap();
        assert_eq!(s.indexes().collect::<Vec<_>>(), [0, 2]);
        let s = reselect(&i.files, Some(&s), &[1], true).unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(
            reselect(&i.files, Some(&FileSelection::parse(&[0], 3).unwrap()), &[0], false),
            Err(TorrentError::NothingSelected)
        );
        assert_eq!(reselect(&i.files, None, &[7], true), Err(TorrentError::NoSuchFile(7)));
        assert_eq!(reselect(&TorrentFiles::default(), None, &[0], true), Err(TorrentError::NoFiles));
    }

    #[test]
    fn seed_limits_follow_the_settings_or_the_torrents_own() {
        let input =
            |own, ratio_on, ratio, minutes_on, minutes| SeedLimitsInput { own, ratio_on, ratio, minutes_on, minutes };
        assert_eq!(seed_limits(&input(false, true, 2.0, true, 5)), Ok(None));
        assert_eq!(
            seed_limits(&input(true, true, 1.5, false, 5)),
            Ok(Some(SeedLimits { ratio: Some(Ratio::new(1.5).unwrap()), minutes: None }))
        );
        assert_eq!(
            seed_limits(&input(true, false, 0.0, false, 0)),
            Ok(Some(SeedLimits { ratio: None, minutes: None }))
        );
        assert!(matches!(seed_limits(&input(true, true, 101.0, false, 0)), Err(TorrentError::BadRatio(_))));
    }
}
