//! What the windows show of torrents: the files in Download File Info, and
//! the files, peers, trackers, pieces and seeding in the download window.

use serde::{Deserialize, Serialize};
use specta::Type;

use super::{DownloadView, SaveAsView};

/// One of a torrent's files, as its lists show it.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TorrentFileRow {
    /// Its number in the torrent, which `pick_torrent_files` takes.
    pub index: u32,
    /// Its path inside the torrent, with `/` between its folders.
    pub path: String,
    pub size: String,
    /// Ticked: it downloads.
    pub chosen: bool,
    /// How much of it is here, such as "45.31%"; empty when not told.
    pub progress: String,
    /// The same, from 0 to 1.
    #[specta(type = Option<specta_typescript::Number>)]
    pub fraction: Option<f32>,
}

/// Download File Info's part for a torrent: its files to tick.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SaveAsTorrent {
    /// The file list has not come from the daemon yet.
    pub waiting: bool,
    /// Its files, sorted by path, padding left out.
    pub files: Vec<TorrentFileRow>,
    /// A torrent of one file keeps that file's name: the dialog edits only
    /// the folder, `folder`.
    pub fixed_name: Option<String>,
    pub folder: String,
    /// What the ticks add up to, such as "3 of 10 files, 1.20 GB of 4.50 GB".
    pub chosen: String,
    /// Nothing is ticked, so the download cannot start.
    pub none_chosen: bool,
    /// A private torrent finds peers through its trackers only.
    pub private: bool,
}

/// One peer a torrent is connected to.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PeerRow {
    /// `ip:port`, which also tells the rows apart.
    pub address: String,
    /// The program it says it is.
    pub client: String,
    pub down_speed: String,
    pub up_speed: String,
    pub downloaded: String,
    pub uploaded: String,
    /// TCP, uTP or SOCKS.
    pub transport: String,
}

/// One of a torrent's trackers.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TrackerRow {
    pub url: String,
    /// Used or skipped, in words.
    pub state: String,
    pub skipped: bool,
}

/// When a finished torrent stops seeding, as the download window edits it.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SeedLimitsView {
    /// Its own limits, rather than the settings'.
    pub own: bool,
    pub ratio_on: bool,
    /// The ratio and the minutes as the fields start with them.
    pub ratio: String,
    pub minutes_on: bool,
    pub minutes: String,
    /// The settings' limits, in words.
    pub defaults: String,
    /// How far it has seeded, in words; empty before it seeds.
    pub progress: String,
}

/// The download window's part for a torrent.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TorrentView {
    /// Where it is: getting its file list, checking, downloading, seeding
    /// or finished, in words.
    pub phase: String,
    /// It seeds now, so Stop seeding works.
    pub seeding: bool,
    pub down_speed: String,
    pub up_speed: String,
    pub time_left: String,
    pub ratio: String,
    pub uploaded: String,
    /// Peers connected, and every peer known.
    pub peers_line: String,
    /// How many pieces are here, and their size.
    pub pieces_line: String,
    pub private: bool,
    /// Why the files show no progress, or are not listed yet.
    pub files_note: String,
    /// The files, sorted by path, padding left out.
    pub files: Vec<TorrentFileRow>,
    /// The files can be ticked and unticked now.
    pub can_choose: bool,
    /// The busiest connected peers first.
    pub peers: Vec<PeerRow>,
    pub trackers: Vec<TrackerRow>,
    /// The piece map's cells, each 0 to 100: the share of its pieces here.
    pub pieces: Vec<u8>,
    pub limits: SeedLimitsView,
}

/// A torrent's own seeding limits, or the settings' (`own` false).
#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SeedLimitsInput {
    pub own: bool,
    pub ratio_on: bool,
    /// The ratio and the minutes as typed; the shell reads them.
    pub ratio: String,
    pub minutes_on: bool,
    pub minutes: String,
}

/// A change to the lists of a torrent's download window: the rows that
/// changed and the new order when rows came, went or moved; the trackers
/// and the piece map whole when they changed.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TorrentPatch {
    pub files: Vec<TorrentFileRow>,
    pub file_order: Option<Vec<u32>>,
    pub peers: Vec<PeerRow>,
    pub peer_order: Option<Vec<String>>,
    pub trackers: Option<Vec<TrackerRow>>,
    pub pieces: Option<Vec<u8>>,
}

/// A change to a download window: the view without its torrent's lists
/// when the rest changed, and the lists' changes.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DownloadPatch {
    /// The new view; its torrent's lists are empty, and the window keeps its
    /// own with `torrent`'s changes.
    pub view: Option<DownloadView>,
    pub torrent: Option<TorrentPatch>,
}

/// A change to Download File Info: the view without its torrent's files
/// when the rest changed, and the files that changed.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SaveAsPatch {
    /// The new view; its torrent's file list is empty, and the window keeps
    /// its own with `files` and `file_order`.
    pub view: Option<SaveAsView>,
    pub files: Vec<TorrentFileRow>,
    pub file_order: Option<Vec<u32>>,
}
