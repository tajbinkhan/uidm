//! What the windows show of torrents: the status a torrent's row and window
//! say, its files in Download File Info, its download window's tabs, and
//! the patches between two of those views.

use std::collections::HashMap;
use std::path::Path;

use udm_domain::bytes::ByteCount;
use udm_domain::download::{DownloadEntry, Status};
use udm_domain::torrent::{
    Ratio, SeedLimits, TorrentEntry, TorrentInfo, TorrentPhase, TorrentStats, TrackerState, DEFAULT_SEED_MINUTES,
    DEFAULT_SEED_RATIO,
};

use super::format;
use super::Ctx;
use crate::contract::{
    DownloadPatch, DownloadView, PeerRow, SaveAsPatch, SaveAsTorrent, SaveAsView, SeedLimitsView, TorrentFileRow,
    TorrentPatch, TorrentView, TrackerRow,
};
use crate::i18n::{fill, Language};
use crate::state::AppState;

/// The phase in words, for the download window.
fn phase_text(lang: &'static Language, phase: TorrentPhase) -> &'static str {
    lang.tr(match phase {
        TorrentPhase::Metadata => "Getting the file list from peers...",
        TorrentPhase::Checking => "Checking the files...",
        TorrentPhase::Downloading => "Downloading",
        TorrentPhase::Seeding => "Seeding",
        TorrentPhase::Complete => "Complete",
    })
}

/// A ratio as the windows write it: two decimals.
fn ratio_text(lang: &'static Language, ratio: Ratio) -> String {
    lang.number(ratio.get(), 2)
}

/// The share ratio so far: the latest stats', else from what the row kept.
fn ratio_of(e: &DownloadEntry, t: &TorrentEntry, stats: Option<&TorrentStats>) -> Ratio {
    match stats {
        Some(s) if !s.total.is_zero() => s.ratio().max(Ratio::of(t.uploaded, s.total)),
        _ => Ratio::of(t.uploaded, e.total.unwrap_or(e.done)),
    }
}

/// Whether `e` is a seeding torrent.
pub fn is_seeding(e: &DownloadEntry) -> bool {
    e.status == Status::Done && e.torrent.as_ref().is_some_and(|t| t.phase == TorrentPhase::Seeding)
}

/// What a torrent row's Status column says when it says more than a
/// download's: getting its file list, checking, or seeding with its ratio.
pub fn row_status(lang: &'static Language, e: &DownloadEntry, stats: Option<&TorrentStats>) -> Option<String> {
    let t = e.torrent.as_ref()?;
    match (e.status, t.phase) {
        (Status::Downloading, TorrentPhase::Metadata) => Some(lang.tr("Getting file list").to_string()),
        (Status::Downloading, TorrentPhase::Checking) => Some(match stats.filter(|s| !s.total.is_zero()) {
            Some(s) => {
                let share = (s.done.as_f64() / s.total.as_f64()).clamp(0.0, 1.0) * 100.0;
                fill(lang.tr("Checking {percent}%"), &[("percent", &lang.number(share, 0))])
            }
            None => lang.tr("Checking").to_string(),
        }),
        (Status::Done, TorrentPhase::Seeding) => {
            Some(fill(lang.tr("Seeding (ratio {ratio})"), &[("ratio", &ratio_text(lang, ratio_of(e, t, stats)))]))
        }
        _ => None,
    }
}

/// What a torrent's download window says it is doing, when it says more
/// than a download's.
pub fn status_line(lang: &'static Language, e: &DownloadEntry) -> Option<&'static str> {
    let t = e.torrent.as_ref()?;
    match (e.status, t.phase) {
        (Status::Downloading, TorrentPhase::Metadata | TorrentPhase::Checking | TorrentPhase::Downloading)
        | (Status::Done, TorrentPhase::Seeding) => Some(phase_text(lang, t.phase)),
        _ => None,
    }
}

/// Seeding limits in words.
fn limits_text(lang: &'static Language, l: &SeedLimits) -> String {
    if !l.seeds() {
        return lang.tr("No seeding: it stops as soon as it finishes").to_string();
    }
    match (l.ratio, l.minutes) {
        (Some(r), Some(m)) => fill(
            lang.tr("Until a share ratio of {ratio} or {minutes} minutes, whichever comes first"),
            &[("ratio", &ratio_text(lang, r)), ("minutes", &m.to_string())],
        ),
        (Some(r), None) => fill(lang.tr("Until a share ratio of {ratio}"), &[("ratio", &ratio_text(lang, r))]),
        (None, Some(m)) => fill(lang.tr("For {minutes} minutes"), &[("minutes", &m.to_string())]),
        (None, None) => lang.tr("Until you stop it").to_string(),
    }
}

/// The files a torrent lists, sorted by path, padding left out: each with
/// its number in the torrent.
fn sorted_files(info: &TorrentInfo) -> Vec<(usize, String)> {
    let mut files: Vec<(usize, String)> = info
        .files
        .as_slice()
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.padding)
        .map(|(i, f)| (i, f.display_path()))
        .collect();
    files.sort_by_cached_key(|(i, path)| (path.to_lowercase(), *i));
    files
}

/// Download File Info's part for a torrent download: its files to tick,
/// or that they have not come yet.
pub fn save_as_part(ctx: &Ctx, state: &AppState, e: &DownloadEntry, save_as: &str) -> Option<SaveAsTorrent> {
    e.torrent.as_ref()?;
    let lang = ctx.lang;
    let Some(info) = state.torrent_info.get(&e.id) else {
        return Some(SaveAsTorrent {
            waiting: true,
            files: Vec::new(),
            fixed_name: None,
            folder: String::new(),
            chosen: lang.tr("Getting the file list from peers... Starting now downloads every file.").to_string(),
            none_chosen: false,
            private: false,
        });
    };
    let picks = state.torrent_picks.get(&e.id);
    let chosen = |i: usize| picks.is_none_or(|p| p.contains(&i));
    let files: Vec<TorrentFileRow> = sorted_files(info)
        .into_iter()
        .map(|(i, path)| TorrentFileRow {
            index: u32::try_from(i).unwrap_or(u32::MAX),
            path,
            size: info.files.as_slice().get(i).map(|f| format::bytes(lang, f.size, 2)).unwrap_or_default(),
            chosen: chosen(i),
            progress: String::new(),
            fraction: None,
        })
        .collect();
    let count = files.iter().filter(|f| f.chosen).count();
    let selection = picks.map(|p| {
        let indexes: Vec<usize> = p.iter().copied().collect();
        udm_domain::torrent::FileSelection::try_from(indexes).ok()
    });
    let picked_size = match &selection {
        None => info.files.selected_size(None),
        Some(None) => ByteCount::ZERO,
        Some(Some(s)) => info.files.selected_size(Some(s)),
    };
    let folder = Path::new(save_as).parent().map(|p| p.display().to_string()).unwrap_or_default();
    Some(SaveAsTorrent {
        waiting: false,
        chosen: fill(
            lang.tr("{count} of {total} files, {size} of {all}"),
            &[
                ("count", &count.to_string()),
                ("total", &files.len().to_string()),
                ("size", &format::bytes(lang, picked_size, 2)),
                ("all", &format::bytes(lang, info.files.selected_size(None), 2)),
            ],
        ),
        none_chosen: count == 0,
        fixed_name: info.files.single_name().map(str::to_string),
        folder,
        private: info.private,
        files,
    })
}

/// The download window's part for a torrent download.
pub fn download_part(ctx: &Ctx, state: &AppState, e: &DownloadEntry) -> Option<TorrentView> {
    let t = e.torrent.as_ref()?;
    let lang = ctx.lang;
    let running = e.status == Status::Downloading || is_seeding(e);
    let stats = state.torrents.get(&e.id).filter(|_| running);
    let info = state.torrent_info.get(&e.id);
    let settings = state.settings.as_ref().map(|s| s.torrents.seed_limits).unwrap_or_default();
    let ratio = ratio_of(e, t, stats);
    let files = match info {
        Some(info) => file_rows(lang, e, t, info, stats),
        None => Vec::new(),
    };
    let files_note = match (info, stats) {
        (None, _) if t.phase == TorrentPhase::Metadata => phase_text(lang, t.phase).to_string(),
        (None, _) => lang.tr("The file list has not come from UDM's background service yet.").to_string(),
        (Some(info), Some(s)) if s.file_done.is_empty() && info.files.len() > 1 && e.status == Status::Downloading => {
            lang.tr("This torrent has too many files to show the progress of each.").to_string()
        }
        _ => String::new(),
    };
    let pieces = match (stats, info) {
        (Some(s), _) if !s.pieces.cells.is_empty() => s.pieces.cells.clone(),
        // A finished torrent has every piece.
        (_, Some(info)) if e.status == Status::Done => vec![100; (info.pieces as usize).clamp(1, 512)],
        _ => Vec::new(),
    };
    let pieces_line = match info {
        Some(info) => {
            let have = match stats {
                Some(s) if !s.pieces.cells.is_empty() => s.pieces.have,
                _ if e.status == Status::Done => info.pieces,
                _ => 0,
            };
            fill(
                lang.tr("{have} of {pieces} pieces, {size} each"),
                &[
                    ("have", &lang.number(f64::from(have), 0)),
                    ("pieces", &lang.number(f64::from(info.pieces), 0)),
                    ("size", &format::bytes(lang, info.piece_size, 0)),
                ],
            )
        }
        None => String::new(),
    };
    let shown = t.limits(&settings);
    let progress = match t.seeding_since {
        Some(since) if t.phase == TorrentPhase::Seeding => fill(
            lang.tr("Seeding for {time}, share ratio {ratio}"),
            &[
                ("time", &format::duration(lang, ctx.unix.saturating_sub(since) as f64)),
                ("ratio", &ratio_text(lang, ratio)),
            ],
        ),
        _ => String::new(),
    };
    let uploaded = stats.map_or(t.uploaded, |s| s.uploaded.max(t.uploaded));
    Some(TorrentView {
        phase: phase_text(lang, t.phase).to_string(),
        seeding: is_seeding(e),
        down_speed: stats.map(|s| format::rate(lang, s.down_speed as f64, 2)).unwrap_or_default(),
        up_speed: stats.map(|s| format::rate(lang, s.up_speed as f64, 2)).unwrap_or_default(),
        time_left: stats
            .filter(|_| e.status == Status::Downloading)
            .and_then(|s| s.eta_seconds)
            .map(|s| format::duration(lang, s as f64))
            .unwrap_or_default(),
        ratio: ratio_text(lang, ratio),
        uploaded: format::bytes(lang, uploaded, 2),
        peers_line: stats
            .map(|s| {
                fill(
                    lang.tr("{connected} connected, {known} known"),
                    &[("connected", &s.peers_connected.to_string()), ("known", &s.peers_known.to_string())],
                )
            })
            .unwrap_or_default(),
        pieces_line,
        private: info.is_some_and(|i| i.private),
        files_note,
        can_choose: state.connected() && info.is_some() && e.status != Status::Confirming,
        files,
        peers: stats.map(|s| s.peers.iter().map(|p| peer_row(lang, p)).collect()).unwrap_or_default(),
        trackers: stats
            .map(|s| {
                s.trackers
                    .iter()
                    .map(|tr| TrackerRow {
                        url: tr.url.clone(),
                        state: lang
                            .tr(match tr.state {
                                TrackerState::Used => "Used",
                                TrackerState::Skipped => "Skipped",
                            })
                            .to_string(),
                        skipped: tr.state == TrackerState::Skipped,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        pieces,
        limits: SeedLimitsView {
            own: t.seed_limits.is_some(),
            ratio_on: shown.ratio.is_some(),
            ratio: shown.ratio.unwrap_or(DEFAULT_SEED_RATIO).get(),
            minutes_on: shown.minutes.is_some(),
            minutes: shown.minutes.unwrap_or(DEFAULT_SEED_MINUTES),
            defaults: limits_text(lang, &settings),
            progress,
        },
    })
}

fn file_rows(
    lang: &'static Language,
    e: &DownloadEntry,
    t: &TorrentEntry,
    info: &TorrentInfo,
    stats: Option<&TorrentStats>,
) -> Vec<TorrentFileRow> {
    let all = info.files.as_slice();
    sorted_files(info)
        .into_iter()
        .map(|(i, path)| {
            let size = all.get(i).map(|f| f.size).unwrap_or_default();
            let chosen = t.selection.as_ref().is_none_or(|s| s.contains(i));
            let here = match stats.and_then(|s| s.file_done.get(i)) {
                Some(done) => Some(*done),
                None if e.status == Status::Done && chosen => Some(size),
                None => None,
            };
            let fraction =
                here.map(|d| if size.is_zero() { 1.0 } else { (d.as_f64() / size.as_f64()).clamp(0.0, 1.0) as f32 });
            TorrentFileRow {
                index: u32::try_from(i).unwrap_or(u32::MAX),
                path,
                size: format::bytes(lang, size, 2),
                chosen,
                progress: fraction.map(|f| format!("{}%", lang.number(f64::from(f) * 100.0, 2))).unwrap_or_default(),
                fraction,
            }
        })
        .collect()
}

fn peer_row(lang: &'static Language, p: &udm_domain::torrent::PeerInfo) -> PeerRow {
    PeerRow {
        address: p.address.clone(),
        client: p.client.clone().unwrap_or_default(),
        down_speed: if p.down_speed > 0 { format::rate(lang, p.down_speed as f64, 1) } else { String::new() },
        up_speed: if p.up_speed > 0 { format::rate(lang, p.up_speed as f64, 1) } else { String::new() },
        downloaded: format::bytes(lang, p.downloaded, 1),
        uploaded: format::bytes(lang, p.uploaded, 1),
        transport: match p.transport.as_deref() {
            Some("tcp") => "TCP".to_string(),
            Some("utp") => "uTP".to_string(),
            Some("socks") => "SOCKS".to_string(),
            Some(other) => other.to_string(),
            None => String::new(),
        },
    }
}

/// The rows of `after` that are new or changed from `before`, matched by
/// `key`, and the new order of keys when it is not `before`'s.
fn rows_diff<T: PartialEq + Clone, K: Eq + std::hash::Hash + Clone>(
    before: &[T],
    after: &[T],
    key: impl Fn(&T) -> K,
) -> (Vec<T>, Option<Vec<K>>) {
    let old: HashMap<K, &T> = before.iter().map(|r| (key(r), r)).collect();
    let changed = after.iter().filter(|r| old.get(&key(r)).is_none_or(|o| *o != *r)).cloned().collect();
    let same_order = before.len() == after.len() && before.iter().zip(after).all(|(a, b)| key(a) == key(b));
    (changed, (!same_order).then(|| after.iter().map(&key).collect()))
}

/// `view` without its torrent's lists.
fn download_head(view: &DownloadView) -> DownloadView {
    let mut head = view.clone();
    if let Some(t) = head.torrent.as_mut() {
        t.files.clear();
        t.peers.clear();
        t.trackers.clear();
        t.pieces.clear();
    }
    head
}

/// The patch from one download window's view to the next, if it changed.
pub fn diff_download(before: &DownloadView, after: &DownloadView) -> Option<DownloadPatch> {
    if before == after {
        return None;
    }
    let head = download_head(after);
    let view = (download_head(before) != head).then_some(head);
    let torrent = after.torrent.as_ref().map(|b| {
        let empty = TorrentView {
            files: Vec::new(),
            peers: Vec::new(),
            trackers: Vec::new(),
            pieces: Vec::new(),
            ..(**b).clone()
        };
        let a = before.torrent.as_deref().unwrap_or(&empty);
        let (files, file_order) = rows_diff(&a.files, &b.files, |r| r.index);
        let (peers, peer_order) = rows_diff(&a.peers, &b.peers, |r| r.address.clone());
        TorrentPatch {
            files,
            file_order,
            peers,
            peer_order,
            trackers: (a.trackers != b.trackers).then(|| b.trackers.clone()),
            pieces: (a.pieces != b.pieces).then(|| b.pieces.clone()),
        }
    });
    let torrent = torrent.filter(|p| {
        !p.files.is_empty()
            || p.file_order.is_some()
            || !p.peers.is_empty()
            || p.peer_order.is_some()
            || p.trackers.is_some()
            || p.pieces.is_some()
    });
    Some(DownloadPatch { view, torrent })
}

/// The patch from one Download File Info view to the next, if it changed.
pub fn diff_save_as(before: &SaveAsView, after: &SaveAsView) -> Option<SaveAsPatch> {
    if before == after {
        return None;
    }
    let head = |v: &SaveAsView| {
        let mut h = v.clone();
        if let Some(t) = h.torrent.as_mut() {
            t.files.clear();
        }
        h
    };
    let after_head = head(after);
    let view = (head(before) != after_head).then_some(after_head);
    let no_files: [TorrentFileRow; 0] = [];
    let a = before.torrent.as_ref().map_or(&no_files[..], |t| &t.files[..]);
    let b = after.torrent.as_ref().map_or(&no_files[..], |t| &t.files[..]);
    let (files, file_order) = rows_diff(a, b, |r| r.index);
    Some(SaveAsPatch { view, files, file_order })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Id, PeerRow};
    use crate::i18n::resolve;
    use crate::state::{apply, UiMessage};
    use std::time::Instant;
    use udm_application::daemon::Event;
    use udm_domain::id::DownloadId;
    use udm_domain::torrent::{InfoHash, PeerInfo, PieceMap, TorrentFiles, TrackerInfo};

    fn ctx() -> Ctx {
        Ctx {
            lang: resolve("en", &[]),
            offset: 0,
            today: "2026-11-14".into(),
            now: Instant::now(),
            unix: 1_700_000_000,
            downloads: std::path::PathBuf::from("/home/u/Downloads"),
            flags: true,
        }
    }

    fn send(s: &mut AppState, event: Event) {
        apply(s, UiMessage::Daemon { event: Box::new(event), at: Instant::now(), unix: 1_700_000_000 });
    }

    fn info(paths: &[(&str, u64, bool)]) -> TorrentInfo {
        let raw =
            paths.iter().map(|(p, size, pad)| (p.split('/').map(str::to_string).collect(), *size, *pad)).collect();
        TorrentInfo {
            info_hash: InfoHash::from_bytes([7; 20]),
            name: "Album".into(),
            files: TorrentFiles::parse(raw).unwrap(),
            piece_size: ByteCount::new(1024),
            pieces: 40,
            private: true,
        }
    }

    /// A state with torrent download 1, in `status` and `phase`, and its
    /// files known.
    fn with(status: Status, phase: TorrentPhase, files: &TorrentInfo) -> AppState {
        let mut s = AppState::default();
        send(&mut s, Event::Connected);
        let mut e = crate::testing::entry(1, "Album", status, 30_000, 10_000);
        e.url = format!("magnet:?xt=urn:btih:{}", files.info_hash.hex()).as_str().into();
        let mut t = TorrentEntry::new(files.info_hash);
        t.phase = phase;
        t.uploaded = ByteCount::new(15_000);
        e.torrent = Some(t);
        send(&mut s, Event::Downloads { items: vec![e.clone()] });
        send(&mut s, Event::TorrentInfo { id: e.id, url: e.url.clone(), info: files.clone() });
        s
    }

    fn album() -> TorrentInfo {
        info(&[("b/2.flac", 20_000, false), ("pad", 5, true), ("A/1.flac", 10_000, false), ("c.jpg", 0, false)])
    }

    fn entry(s: &AppState) -> &DownloadEntry {
        s.table.get(DownloadId::new(1)).unwrap()
    }

    #[test]
    fn rows_say_when_a_torrent_seeds_checks_or_waits_for_its_files() {
        let lang = resolve("en", &[]);
        let s = with(Status::Done, TorrentPhase::Seeding, &album());
        assert_eq!(row_status(lang, entry(&s), None).as_deref(), Some("Seeding (ratio 0.50)"));
        let stats =
            TorrentStats { total: ByteCount::new(10_000), uploaded: ByteCount::new(25_000), ..Default::default() };
        assert_eq!(row_status(lang, entry(&s), Some(&stats)).as_deref(), Some("Seeding (ratio 2.50)"));
        assert!(is_seeding(entry(&s)));
        let s = with(Status::Downloading, TorrentPhase::Checking, &album());
        let checked = TorrentStats { done: ByteCount::new(1), total: ByteCount::new(4), ..Default::default() };
        assert_eq!(row_status(lang, entry(&s), Some(&checked)).as_deref(), Some("Checking 25%"));
        assert_eq!(row_status(lang, entry(&s), None).as_deref(), Some("Checking"));
        // Knowing the files ends the metadata phase; a magnet link's row
        // waits for them.
        let mut s = with(Status::Downloading, TorrentPhase::Checking, &album());
        let mut e = entry(&s).clone();
        e.torrent.as_mut().unwrap().phase = TorrentPhase::Metadata;
        send(&mut s, Event::Updated { item: e });
        assert_eq!(row_status(lang, entry(&s), None).as_deref(), Some("Getting file list"));
        let s = with(Status::Done, TorrentPhase::Complete, &album());
        assert_eq!(row_status(lang, entry(&s), None), None, "a finished torrent reads as any download");
        // The list's own row and the main window's menus follow.
        let mut s = with(Status::Done, TorrentPhase::Seeding, &album());
        apply(&mut s, UiMessage::Select(vec![DownloadId::new(1)]));
        let can = super::super::main::can(&s);
        assert!(can.torrent && can.stop_seeding);
    }

    #[test]
    fn download_file_info_lists_the_files_sorted_without_padding() {
        let mut s = with(Status::Confirming, TorrentPhase::Checking, &album());
        let part = save_as_part(&ctx(), &s, entry(&s), "/home/u/Downloads/Album").unwrap();
        let paths: Vec<&str> = part.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["A/1.flac", "b/2.flac", "c.jpg"]);
        assert_eq!(part.files.iter().map(|f| f.index).collect::<Vec<_>>(), [2, 0, 3]);
        assert!(part.files.iter().all(|f| f.chosen) && part.private && !part.waiting);
        assert_eq!(part.chosen, "3 of 3 files, 29.30 KB of 29.30 KB");
        assert_eq!((part.fixed_name, part.folder.as_str()), (None, "/home/u/Downloads"));
        apply(&mut s, UiMessage::PickTorrentFiles { id: DownloadId::new(1), indexes: vec![0, 1], chosen: false });
        let part = save_as_part(&ctx(), &s, entry(&s), "/x/Album").unwrap();
        assert_eq!(part.chosen, "2 of 3 files, 9.77 KB of 29.30 KB");
        apply(&mut s, UiMessage::PickTorrentFiles { id: DownloadId::new(1), indexes: vec![2, 3], chosen: false });
        assert!(save_as_part(&ctx(), &s, entry(&s), "/x/Album").unwrap().none_chosen);
        // The answer carries the ticks, and they go once it is answered.
        let picks = s.torrent_picks.get(&DownloadId::new(1));
        assert_eq!(
            crate::torrents::answer_files(s.torrent_info.get(&DownloadId::new(1)).unwrap(), picks),
            Err(udm_domain::torrent::TorrentError::NothingSelected)
        );
        let mut started = entry(&s).clone();
        started.status = Status::Downloading;
        send(&mut s, Event::Updated { item: started });
        assert!(
            s.torrent_picks.is_empty() && s.torrent_info.contains_key(&DownloadId::new(1)),
            "an updated row keeps its files"
        );
    }

    #[test]
    fn a_torrent_of_one_file_keeps_its_name_and_a_magnet_link_waits() {
        let one = info(&[("disc.iso", 100, false)]);
        let s = with(Status::Confirming, TorrentPhase::Checking, &one);
        let part = save_as_part(&ctx(), &s, entry(&s), "/home/u/Downloads/disc.iso").unwrap();
        assert_eq!((part.fixed_name.as_deref(), part.folder.as_str()), (Some("disc.iso"), "/home/u/Downloads"));
        let mut s = with(Status::Confirming, TorrentPhase::Metadata, &one);
        s.torrent_info.clear();
        let part = save_as_part(&ctx(), &s, entry(&s), "/x").unwrap();
        assert!(part.waiting && part.files.is_empty() && !part.none_chosen);
        let view = super::super::dialogs::save_as(&ctx(), &s, DownloadId::new(1)).unwrap();
        assert!(view.torrent.is_some() && !view.can_preview);
    }

    fn stats() -> TorrentStats {
        let peer = |a: &str, down| PeerInfo {
            address: a.into(),
            client: Some("rqbit".into()),
            down_speed: down,
            up_speed: 0,
            downloaded: ByteCount::new(2048),
            uploaded: ByteCount::ZERO,
            transport: Some("utp".into()),
        };
        TorrentStats {
            phase: TorrentPhase::Downloading,
            done: ByteCount::new(10_000),
            total: ByteCount::new(30_000),
            down_speed: 2048,
            up_speed: 1024,
            uploaded: ByteCount::new(15_000),
            eta_seconds: Some(10),
            peers_connected: 2,
            peers_known: 9,
            peers: vec![peer("10.0.0.1:1", 1024), peer("10.0.0.2:2", 0)],
            trackers: vec![TrackerInfo {
                url: "https://t/a".into(),
                state: udm_domain::torrent::TrackerState::Skipped,
            }],
            pieces: PieceMap::of(40, |i| i < 10),
            file_done: [20_000, 5, 0, 0].map(ByteCount::new).to_vec(),
        }
    }

    #[test]
    fn the_download_window_shows_files_peers_trackers_pieces_and_limits() {
        let mut s = with(Status::Downloading, TorrentPhase::Downloading, &album());
        let url = entry(&s).url.clone();
        send(&mut s, Event::TorrentStats { id: DownloadId::new(1), url, stats: stats() });
        let v = download_part(&ctx(), &s, entry(&s)).unwrap();
        assert_eq!((v.down_speed.as_str(), v.time_left.as_str(), v.ratio.as_str()), ("2.00 KB/s", "10s", "0.50"));
        assert_eq!(v.peers_line, "2 connected, 9 known");
        assert_eq!(v.pieces_line, "10 of 40 pieces, 1 KB each");
        let files: Vec<(&str, &str)> = v.files.iter().map(|f| (f.path.as_str(), f.progress.as_str())).collect();
        assert_eq!(files, [("A/1.flac", "0.00%"), ("b/2.flac", "100.00%"), ("c.jpg", "100.00%")]);
        assert_eq!(v.peers[0].transport, "uTP");
        assert_eq!(v.peers[1].down_speed, "", "an idle peer shows no speed");
        assert_eq!((v.trackers[0].state.as_str(), v.trackers[0].skipped), ("Skipped", true));
        assert_eq!(v.pieces.len(), 40);
        assert!(v.can_choose && !v.seeding);
        assert_eq!(v.limits.defaults, "Until a share ratio of 1.00 or 60 minutes, whichever comes first");
        let window = super::super::dialogs::download(&ctx(), &s, DownloadId::new(1)).unwrap();
        assert_eq!(window.status, "Downloading");
        // Seeding: its own limits, how long it seeded, and every piece.
        let mut s = with(Status::Done, TorrentPhase::Seeding, &album());
        let mut e = entry(&s).clone();
        let t = e.torrent.as_mut().unwrap();
        t.seed_limits = Some(udm_domain::torrent::SeedLimits { ratio: None, minutes: Some(5) });
        t.seeding_since = Some(1_700_000_000 - 90);
        send(&mut s, Event::Updated { item: e });
        let v = download_part(&ctx(), &s, entry(&s)).unwrap();
        assert!(v.seeding && v.limits.own && !v.limits.ratio_on && v.limits.minutes == 5);
        assert_eq!(v.limits.progress, "Seeding for 1m 30s, share ratio 0.50");
        assert_eq!((v.pieces, v.pieces_line.as_str()), (vec![100; 40], "40 of 40 pieces, 1 KB each"));
        assert!(v.files.iter().all(|f| f.fraction == Some(1.0)), "a finished torrent has its chosen files");
    }

    #[test]
    fn patches_carry_only_the_rows_that_changed() {
        let mut s = with(Status::Downloading, TorrentPhase::Downloading, &album());
        let id = DownloadId::new(1);
        let url = entry(&s).url.clone();
        send(&mut s, Event::TorrentStats { id, url: url.clone(), stats: stats() });
        let before = super::super::dialogs::download(&ctx(), &s, id).unwrap();
        assert_eq!(diff_download(&before, &before), None);
        let mut next = stats();
        next.peers[1].down_speed = 4096;
        send(&mut s, Event::TorrentStats { id, url: url.clone(), stats: next.clone() });
        let after = super::super::dialogs::download(&ctx(), &s, id).unwrap();
        let p = diff_download(&before, &after).unwrap();
        let t = p.torrent.unwrap();
        assert_eq!(t.peers.iter().map(|r: &PeerRow| r.address.as_str()).collect::<Vec<_>>(), ["10.0.0.2:2"]);
        assert!(t.files.is_empty() && t.file_order.is_none() && t.peer_order.is_none());
        assert!(t.trackers.is_none() && t.pieces.is_none());
        assert!(p.view.is_none(), "nothing else changed");
        // A peer leaving changes the order; a file's progress changes its row.
        next.peers.remove(0);
        next.file_done[2] = ByteCount::new(5_000);
        next.down_speed = 8192;
        send(&mut s, Event::TorrentStats { id, url, stats: next });
        let last = super::super::dialogs::download(&ctx(), &s, id).unwrap();
        let p = diff_download(&after, &last).unwrap();
        let view = p.view.expect("the speed changed");
        assert!(view.torrent.is_some_and(|t| t.peers.is_empty() && t.files.is_empty()), "the lists stay out of it");
        let t = p.torrent.unwrap();
        assert_eq!(t.peer_order, Some(vec!["10.0.0.2:2".to_string()]));
        assert_eq!(t.files.iter().map(|f| f.index).collect::<Vec<_>>(), [2]);
        // Download File Info sends the ticked row alone.
        let mut s = with(Status::Confirming, TorrentPhase::Checking, &album());
        let a = super::super::dialogs::save_as(&ctx(), &s, id).unwrap();
        apply(&mut s, UiMessage::PickTorrentFiles { id, indexes: vec![3], chosen: false });
        let b = super::super::dialogs::save_as(&ctx(), &s, id).unwrap();
        let p = diff_save_as(&a, &b).unwrap();
        assert_eq!(p.files.iter().map(|f| (f.index, f.chosen)).collect::<Vec<_>>(), [(3, false)]);
        assert!(p.file_order.is_none());
        assert!(p.view.is_some_and(|v| v.id == Id(1) && v.torrent.is_some_and(|t| t.files.is_empty())));
    }

    #[test]
    fn a_removed_torrent_forgets_its_files_and_stats() {
        let mut s = with(Status::Downloading, TorrentPhase::Downloading, &album());
        let id = DownloadId::new(1);
        let url = entry(&s).url.clone();
        send(&mut s, Event::TorrentStats { id, url, stats: stats() });
        send(&mut s, Event::Removed { id });
        assert!(s.torrent_info.is_empty() && s.torrents.is_empty());
        // A finished torrent's window can open to show its seeding.
        let mut s = with(Status::Done, TorrentPhase::Seeding, &album());
        apply(&mut s, UiMessage::ShowProgress(id));
        assert!(s.progress.contains(&id));
        assert_eq!(crate::windows::size(&s, crate::windows::Label::Download(id)), (700.0, 400.0));
    }
}
