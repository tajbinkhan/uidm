//! The rules of the windows' torrent controls: which files a tick leaves
//! chosen, what Download File Info answers with, and the seeding limits the
//! download window sets. Plain functions, tested without Tauri.

use std::collections::BTreeSet;

use udm_application::daemon::service::ServiceError;
use udm_application::daemon::Failure;
use udm_domain::download::DownloadError;
use udm_domain::id::DownloadId;
use udm_domain::torrent::{FileSelection, Ratio, SeedLimits, TorrentError, TorrentFiles, TorrentInfo};

use crate::contract::SeedLimitsInput;
use crate::state::AppState;

/// A torrent's files as the daemon sent them, with their order in the
/// windows' lists worked out once: by path, padding left out.
#[derive(Debug, Clone, PartialEq)]
pub struct KnownFiles {
    pub info: TorrentInfo,
    /// Each real file's number in the torrent and its path, sorted.
    pub sorted: Vec<(usize, String)>,
}

impl KnownFiles {
    pub fn new(info: TorrentInfo) -> Self {
        let mut sorted: Vec<(usize, String)> = info
            .files
            .as_slice()
            .iter()
            .enumerate()
            .filter(|(_, f)| !f.padding)
            .map(|(i, f)| (i, f.display_path()))
            .collect();
        sorted.sort_by_cached_key(|(i, path)| (path.to_lowercase(), *i));
        KnownFiles { info, sorted }
    }
}

/// Whether Download File Info of download `id` gives a folder only: its
/// torrent is one file, which keeps its name.
pub fn folder_only(state: &AppState, id: DownloadId) -> bool {
    state.torrent_info.get(&id).is_some_and(|k| k.info.files.single_name().is_some())
}

/// The files ticked in Download File Info of download `id`: the ones the
/// user ticked there, else the ones it was added with; `None` is every
/// file.
pub fn picks_of(state: &AppState, id: DownloadId) -> Option<BTreeSet<usize>> {
    state.torrent_picks.get(&id).cloned().or_else(|| {
        let known = state.torrent_info.get(&id)?;
        let selection = state.table.get(id)?.torrent.as_ref()?.selection.as_ref()?;
        Some(selection.indexes().filter(|i| is_real(&known.info.files, *i)).collect())
    })
}

/// The files torrent download `id` is to download once `indexes` are
/// ticked or unticked in its download window: from the choice sent last
/// while the row does not show it yet, else from the row's.
pub fn choose(
    state: &AppState,
    id: DownloadId,
    indexes: &[usize],
    chosen: bool,
) -> Result<FileSelection, TorrentError> {
    let known = state.torrent_info.get(&id).ok_or(TorrentError::NoFiles)?;
    let row = state.table.get(id).and_then(|e| e.torrent.as_ref()).and_then(|t| t.selection.as_ref());
    let current = state.torrent_chosen.get(&id).or(row);
    reselect(&known.info.files, current, indexes, chosen)
}

/// Whether `error` is the daemon saying it has no file list for a torrent.
pub fn no_file_list(error: &Failure) -> bool {
    matches!(error, Failure::Service(ServiceError::Download(DownloadError::Torrent(TorrentError::NoFiles))))
}

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

/// Why the download window's seeding limits cannot be set.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LimitsError {
    #[error("Type a share ratio from 0 to 100.")]
    Ratio,
    #[error("Type a number of minutes from 1 up.")]
    Minutes,
}

/// The seeding limits the download window sets, from what was typed:
/// `None` follows the settings'. A limit that is off does not stop seeding;
/// one that is on stops it after at least a minute.
pub fn seed_limits(input: &SeedLimitsInput) -> Result<Option<SeedLimits>, LimitsError> {
    if !input.own {
        return Ok(None);
    }
    let ratio = if input.ratio_on {
        let typed = input.ratio.trim().replace(',', ".");
        Some(typed.parse::<f64>().ok().and_then(|r| Ratio::new(r).ok()).ok_or(LimitsError::Ratio)?)
    } else {
        None
    };
    let minutes = if input.minutes_on {
        Some(input.minutes.trim().parse::<u32>().ok().filter(|m| *m > 0).ok_or(LimitsError::Minutes)?)
    } else {
        None
    };
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
        let input = |own, ratio_on, ratio: &str, minutes_on, minutes: &str| SeedLimitsInput {
            own,
            ratio_on,
            ratio: ratio.into(),
            minutes_on,
            minutes: minutes.into(),
        };
        assert_eq!(seed_limits(&input(false, true, "x", true, "0")), Ok(None));
        assert_eq!(
            seed_limits(&input(true, true, " 1,5 ", false, "")),
            Ok(Some(SeedLimits { ratio: Some(Ratio::new(1.5).unwrap()), minutes: None }))
        );
        assert_eq!(
            seed_limits(&input(true, false, "", true, "30")),
            Ok(Some(SeedLimits { ratio: None, minutes: Some(30) }))
        );
        assert_eq!(
            seed_limits(&input(true, false, "", false, "")),
            Ok(Some(SeedLimits { ratio: None, minutes: None }))
        );
        assert_eq!(seed_limits(&input(true, true, "101", false, "")), Err(LimitsError::Ratio));
        assert_eq!(seed_limits(&input(true, true, "", false, "")), Err(LimitsError::Ratio));
        assert_eq!(seed_limits(&input(true, false, "", true, "0")), Err(LimitsError::Minutes));
        assert_eq!(seed_limits(&input(true, false, "", true, "ten")), Err(LimitsError::Minutes));
    }

    #[test]
    fn quick_ticks_in_the_download_window_build_on_each_other() {
        use crate::state::{apply, UiMessage};
        use std::time::Instant;
        use udm_application::daemon::Event;
        let mut s = AppState::default();
        let mut e = crate::testing::entry(1, "t", udm_domain::download::Status::Downloading, 30, 0);
        let i = info(&[("a", false), ("b", false), ("c", false)]);
        e.torrent = Some(udm_domain::torrent::TorrentEntry::new(i.info_hash));
        let send = |s: &mut AppState, event: Event| {
            apply(s, UiMessage::Daemon { event: Box::new(event), at: Instant::now(), unix: 0 })
        };
        send(&mut s, Event::Downloads { items: vec![e.clone()] });
        send(&mut s, Event::TorrentInfo { id: e.id, url: e.url.clone(), info: i });
        // Untick a, then b, before the daemon's row comes back.
        let first = choose(&s, e.id, &[0], false).unwrap();
        apply(&mut s, UiMessage::TorrentChosen { id: e.id, files: first.clone() });
        let second = choose(&s, e.id, &[1], false).unwrap();
        assert_eq!(second.indexes().collect::<Vec<_>>(), [2], "a stays unticked");
        apply(&mut s, UiMessage::TorrentChosen { id: e.id, files: second.clone() });
        // The row showing the first choice keeps the second pending.
        let row = |files: &FileSelection| {
            let mut r = e.clone();
            r.torrent.as_mut().unwrap().selection = Some(files.clone());
            Event::Updated { item: r }
        };
        send(&mut s, row(&first));
        assert_eq!(s.torrent_chosen.get(&e.id), Some(&second));
        send(&mut s, row(&second));
        assert!(s.torrent_chosen.is_empty(), "the row shows it now");
    }

    #[test]
    fn a_torrent_of_one_file_answers_with_its_folder_and_ticks_start_from_how_it_was_added() {
        use crate::state::{apply, UiMessage};
        use std::time::Instant;
        use udm_application::daemon::Event;
        let send = |s: &mut AppState, event: Event| {
            apply(s, UiMessage::Daemon { event: Box::new(event), at: Instant::now(), unix: 0 })
        };
        let mut s = AppState::default();
        let mut e = crate::testing::entry(1, "t", udm_domain::download::Status::Confirming, 30, 0);
        let many = info(&[("a", false), ("b", false), ("c", false)]);
        let mut t = udm_domain::torrent::TorrentEntry::new(many.info_hash);
        t.selection = FileSelection::parse(&[1], 3).ok();
        e.torrent = Some(t);
        send(&mut s, Event::Downloads { items: vec![e.clone()] });
        assert!(!folder_only(&s, e.id), "not known yet");
        send(&mut s, Event::TorrentInfo { id: e.id, url: e.url.clone(), info: many });
        assert!(!folder_only(&s, e.id));
        assert_eq!(picks_of(&s, e.id), Some([1].into()));
        apply(&mut s, UiMessage::PickTorrentFiles { id: e.id, indexes: vec![2], chosen: true });
        assert_eq!(picks_of(&s, e.id), Some([1, 2].into()));
        send(&mut s, Event::TorrentInfo { id: e.id, url: e.url.clone(), info: info(&[("disc.iso", false)]) });
        assert!(folder_only(&s, e.id));
    }

    #[test]
    fn a_missing_file_list_is_told_to_the_window_that_asked() {
        use crate::state::{apply, UiMessage};
        use std::time::Instant;
        use udm_application::daemon::Event;
        let mut s = AppState::default();
        let id = DownloadId::new(4);
        apply(&mut s, UiMessage::FilesAsked(id));
        let none = Failure::Service(ServiceError::Download(DownloadError::Torrent(TorrentError::NoFiles)));
        apply(&mut s, UiMessage::Daemon { event: Box::new(Event::Error { error: none }), at: Instant::now(), unix: 0 });
        assert!(s.notice.is_none() && s.files_missing.contains(&id) && s.files_asked.is_empty());
    }
}
