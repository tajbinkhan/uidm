//! Torrents and magnet links: what identifies one (`InfoHash`, `MagnetLink`),
//! the files it holds (`TorrentFiles`) and which of them to download
//! (`FileSelection`), how it stands (`TorrentPhase`, `TorrentEntry` on a
//! download's row, `TorrentStats` for its window), and when seeding is done
//! (`SeedLimits`).
//!
//! A torrent download's address is always its magnet link: one added from a
//! `.torrent` file gets the magnet link of the file's info hash, name and
//! trackers, and the file itself is kept by the daemon. So `Address` and
//! `Link` carry torrents as they carry every other download, and a list or
//! history row names a torrent by text that parses back to it.
//!
//! A torrent's file names come from whoever made it, so they are untrusted:
//! each part of each path must be a name the file-name rules
//! (`filename::sanitize_filename`) leave as it is, or the whole torrent is
//! refused. Every file is then a path of clean names, which stays inside the
//! folder it is downloaded to.

use std::collections::{BTreeSet, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::bytes::ByteCount;
use crate::filename::sanitize_filename;

/// The most files a torrent may hold. Each file is opened while the torrent
/// runs, and the window lists them all; torrents with more are refused.
pub const MAX_TORRENT_FILES: usize = 10_000;

/// The largest `.torrent` file (or metadata from peers) UDM takes.
pub const MAX_METAINFO_BYTES: usize = 16 * 1024 * 1024;

/// The largest `.torrent` file a client may send the daemon itself
/// (`udm-cli get file.torrent`, the window's Open): base64 makes it a
/// third bigger, and the message must stay under the 4 MiB frame limit.
/// The daemon fetches larger ones from the web itself.
pub const MAX_SENT_METAINFO_BYTES: usize = 2816 * 1024;

/// The most bytes of file names a torrent's list may hold (every part of
/// every path), so the list fits in one message to the window.
pub const MAX_FILE_NAME_BYTES: usize = 1536 * 1024;

/// The longest magnet link UDM makes from a `.torrent` file: trackers that
/// do not fit are left to the kept file.
pub const MAX_MADE_MAGNET: usize = 4 * 1024;

/// The most trackers, web seeds or peers a magnet link may name; the rest
/// are left out.
pub const MAX_MAGNET_ENTRIES: usize = 200;

/// The most peers a torrent's window lists; the busiest come first.
pub const MAX_LISTED_PEERS: usize = 200;

/// The most cells the piece map has: each holds how much of its share of
/// the pieces is here.
pub const PIECE_MAP_CELLS: usize = 512;

/// Why a torrent or magnet link cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum TorrentError {
    #[error("{0} is not a magnet link")]
    NotMagnet(String),
    #[error("The magnet link names no BitTorrent info hash (xt=urn:btih:...).")]
    NoInfoHash,
    #[error("{0} is not a BitTorrent info hash")]
    BadInfoHash(String),
    #[error("The torrent holds no files.")]
    NoFiles,
    #[error("The torrent holds {count} files; UDM takes at most {max}.")]
    TooManyFiles { count: usize, max: usize },
    /// A part of one of its paths that names no file once cleaned.
    #[error("The torrent names a file UDM cannot save safely: {0}")]
    UnsafePath(String),
    #[error("The torrent file is larger than {} and was not read.", ByteCount::new(*max as u64).megabytes())]
    TooBig { max: usize },
    #[error("This is not a torrent file: {0}")]
    BadTorrent(String),
    #[error("Choose at least one of the torrent's files.")]
    NothingSelected,
    #[error("The torrent has no file number {0}.")]
    NoSuchFile(usize),
    #[error("{0} is not a share ratio between 0 and 100.")]
    BadRatio(String),
    /// Its file list's names, and the most UDM takes, in bytes.
    #[error("The torrent's file names take more than {}; UDM takes shorter lists.", ByteCount::new(*max as u64).megabytes())]
    FileListTooBig { max: usize },
    /// A torrent of one file saves it under the torrent's own name.
    #[error("A torrent of one file keeps its file's name, {0}; choose only its folder.")]
    NameFixed(String),
}

/// A torrent's BitTorrent v1 info hash: the SHA-1 of its info dictionary.
/// Written as 40 hexadecimal digits, lowercase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct InfoHash([u8; 20]);

impl InfoHash {
    /// The hash with these 20 bytes.
    pub const fn from_bytes(bytes: [u8; 20]) -> Self {
        InfoHash(bytes)
    }

    /// `text` as an info hash: 40 hexadecimal digits, or 32 base32
    /// characters (RFC 4648, as older magnet links write it), in any case.
    pub fn parse(text: &str) -> Result<InfoHash, TorrentError> {
        let bad = || TorrentError::BadInfoHash(text.to_string());
        let bytes = match text.len() {
            40 => hex20(text),
            32 => base32_20(text),
            _ => None,
        };
        bytes.map(InfoHash).ok_or_else(bad)
    }

    pub const fn bytes(&self) -> [u8; 20] {
        self.0
    }

    /// The 40 lowercase hexadecimal digits.
    pub fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl fmt::Display for InfoHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

impl TryFrom<String> for InfoHash {
    type Error = TorrentError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        InfoHash::parse(&text)
    }
}

impl From<InfoHash> for String {
    fn from(h: InfoHash) -> Self {
        h.hex()
    }
}

fn hex20(text: &str) -> Option<[u8; 20]> {
    let mut out = [0u8; 20];
    let digits = text.as_bytes();
    for (i, byte) in out.iter_mut().enumerate() {
        let hi = (*digits.get(2 * i)? as char).to_digit(16)?;
        let lo = (*digits.get(2 * i + 1)? as char).to_digit(16)?;
        *byte = u8::try_from(hi * 16 + lo).ok()?;
    }
    Some(out)
}

fn base32_20(text: &str) -> Option<[u8; 20]> {
    let mut out = [0u8; 20];
    let (mut acc, mut bits, mut at) = (0u64, 0u32, 0usize);
    for c in text.chars() {
        let v = match c.to_ascii_uppercase() {
            c @ 'A'..='Z' => c as u64 - 'A' as u64,
            c @ '2'..='7' => c as u64 - '2' as u64 + 26,
            _ => return None,
        };
        acc = (acc << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            *out.get_mut(at)? = u8::try_from((acc >> bits) & 0xff).ok()?;
            at += 1;
        }
    }
    (at == 20).then_some(out)
}

/// A magnet link for a BitTorrent v1 torrent: its info hash, and what the
/// link adds to it (a name, trackers, web seeds, peers to try first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MagnetLink {
    pub info_hash: InfoHash,
    /// `dn`: the name to show until the torrent's own is known.
    pub name: Option<String>,
    /// `tr`: http, https and udp trackers, in order, once each.
    pub trackers: Vec<String>,
    /// `ws`: http and https web seeds.
    pub web_seeds: Vec<String>,
    /// `x.pe`: peers to try first, as `host:port`.
    pub peers: Vec<String>,
}

/// Whether `text` starts like a magnet link, in any case.
pub fn starts_like_magnet(text: &str) -> bool {
    text.get(..8).is_some_and(|s| s.eq_ignore_ascii_case("magnet:?"))
}

/// Whether `text` is a magnet link UDM can download: one with a BitTorrent
/// v1 info hash.
pub fn is_magnet(text: &str) -> bool {
    starts_like_magnet(text) && MagnetLink::parse(text).is_ok()
}

impl MagnetLink {
    /// The magnet link for `info_hash` alone.
    pub fn new(info_hash: InfoHash) -> Self {
        MagnetLink { info_hash, name: None, trackers: Vec::new(), web_seeds: Vec::new(), peers: Vec::new() }
    }

    /// `text` as a magnet link: `magnet:?` (in any case), then parameters
    /// of which one `xt` must be `urn:btih:` and a v1 info hash. Trackers
    /// and web seeds that are not web or udp addresses, and parameters UDM
    /// does not use, are left out.
    pub fn parse(text: &str) -> Result<MagnetLink, TorrentError> {
        if !starts_like_magnet(text) || text.len() > crate::download::MAX_URL {
            return Err(TorrentError::NotMagnet(text.to_string()));
        }
        let query = text.get(8..).unwrap_or("");
        let mut hash = None;
        let mut link = MagnetLink::new(InfoHash([0; 20]));
        let mut seen = HashSet::new();
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let value = decode(value);
            let key = key.to_ascii_lowercase();
            // `tr.1`, `tr.2`: numbered trackers, as some sites write them.
            let key = match key.split_once('.') {
                Some((name, _)) if name != "x" => name,
                _ => key.as_str(),
            };
            match key {
                "xt" => {
                    let btih = value.get(..9).is_some_and(|p| p.eq_ignore_ascii_case("urn:btih:"));
                    if btih && hash.is_none() {
                        hash = Some(InfoHash::parse(value.get(9..).unwrap_or(""))?);
                    }
                }
                "dn" if link.name.is_none() && !value.trim().is_empty() => link.name = Some(value.trim().to_string()),
                "tr" if is_tracker(&value)
                    && link.trackers.len() < MAX_MAGNET_ENTRIES
                    && seen.insert(value.clone()) =>
                {
                    link.trackers.push(value)
                }
                "ws" if crate::batch::is_web_url(&value) && link.web_seeds.len() < MAX_MAGNET_ENTRIES => {
                    link.web_seeds.push(value)
                }
                "x.pe" if is_peer(&value) && link.peers.len() < MAX_MAGNET_ENTRIES => link.peers.push(value),
                _ => {}
            }
        }
        link.info_hash = hash.ok_or(TorrentError::NoInfoHash)?;
        Ok(link)
    }

    /// The link as text: `magnet:?xt=urn:btih:` with the hash in
    /// hexadecimal, then its name, trackers, web seeds and peers.
    pub fn to_uri(&self) -> String {
        let mut uri = format!("magnet:?xt=urn:btih:{}", self.info_hash.hex());
        if let Some(name) = &self.name {
            uri.push_str("&dn=");
            uri.push_str(&encode(name));
        }
        for (key, values) in [("tr", &self.trackers), ("ws", &self.web_seeds), ("x.pe", &self.peers)] {
            for v in values {
                uri.push('&');
                uri.push_str(key);
                uri.push('=');
                uri.push_str(&encode(v));
            }
        }
        uri
    }
}

impl MagnetLink {
    /// The link as text, at most `max` bytes long when its hash and name
    /// fit: trackers, then peers and web seeds, are left out from the end
    /// until it does.
    pub fn to_uri_within(&self, max: usize) -> String {
        let mut link = self.clone();
        loop {
            let uri = link.to_uri();
            if uri.len() <= max {
                return uri;
            }
            if link.web_seeds.pop().is_none() && link.peers.pop().is_none() && link.trackers.pop().is_none() {
                return uri;
            }
        }
    }

    /// The link as logs may show it: its hash and name, without trackers
    /// (whose addresses may carry a private tracker's passkey) or peers.
    pub fn for_log(&self) -> String {
        MagnetLink { trackers: Vec::new(), web_seeds: Vec::new(), peers: Vec::new(), ..self.clone() }.to_uri()
    }
}

/// `text` as logs may show it: a magnet link without its trackers and peers
/// (`MagnetLink::for_log`), anything else as it is.
pub fn loggable(text: &str) -> String {
    match starts_like_magnet(text).then(|| MagnetLink::parse(text).ok()).flatten() {
        Some(m) => m.for_log(),
        None if starts_like_magnet(text) => {
            let lower = text.to_ascii_lowercase();
            let hash = lower
                .find("urn:btih:")
                .and_then(|at| text.get(at + 9..))
                .map(|rest| rest.split('&').next().unwrap_or(""));
            match hash {
                Some(hash) => format!("magnet:?xt=urn:btih:{hash}"),
                None => "magnet:?".to_string(),
            }
        }
        None => text.to_string(),
    }
}

/// `text` with every magnet link in it as `loggable` shows it: no trackers
/// (a private tracker's address carries the user's passkey) and no peers.
pub fn scrub_magnets(text: &str) -> std::borrow::Cow<'_, str> {
    let lower = text.to_ascii_lowercase();
    if !lower.contains("magnet:?") {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut done = 0;
    let mut from = 0;
    while let Some(at) = lower[from..].find("magnet:?") {
        let start = from + at;
        let end = text[start..]
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ')' | ']' | ','))
            .map_or(text.len(), |e| start + e);
        out.push_str(&text[done..start]);
        out.push_str(&loggable(&text[start..end]));
        done = end;
        from = end;
    }
    out.push_str(&text[done..]);
    std::borrow::Cow::Owned(out)
}

impl fmt::Display for MagnetLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_uri())
    }
}

/// A tracker's address UDM may announce to: http, https or udp.
pub fn is_tracker(text: &str) -> bool {
    crate::batch::is_web_url(text) || text.get(..6).is_some_and(|s| s.eq_ignore_ascii_case("udp://"))
}

fn is_peer(text: &str) -> bool {
    text.rsplit_once(':').is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok_and(|p| p > 0))
}

fn decode(value: &str) -> String {
    let spaced = value.replace('+', " ");
    percent_encoding::percent_decode_str(&spaced).decode_utf8_lossy().into_owned()
}

fn encode(value: &str) -> String {
    const KEEP: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'~')
        .remove(b':')
        .remove(b'/');
    percent_encoding::utf8_percent_encode(value, KEEP).to_string()
}

/// Whether a download from `url`, sent with `content_type` and named
/// `filename` (the browser's name for it, from the server's
/// Content-Disposition), is a `.torrent` file: its type says so
/// (`application/x-bittorrent`), or its name or path ends in `.torrent`.
pub fn is_torrent_file(url: &str, content_type: Option<&str>, filename: Option<&str>) -> bool {
    let typed = content_type
        .and_then(|t| t.split(';').next())
        .is_some_and(|t| t.trim().eq_ignore_ascii_case("application/x-bittorrent"));
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let named = |name: &str| name.trim().to_ascii_lowercase().ends_with(".torrent");
    typed || named(path) || filename.is_some_and(named)
}

/// One file of a torrent: its path inside the torrent's folder, each part a
/// clean file name, and its size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TorrentFile {
    pub path: Vec<String>,
    pub size: ByteCount,
    /// A padding file (BEP 47), which only lines the next file up with a
    /// piece: never written, and not offered.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub padding: bool,
}

impl TorrentFile {
    /// The path with `/` between its parts, for showing.
    pub fn display_path(&self) -> String {
        self.path.join("/")
    }
}

/// A torrent's files, in the torrent's order (which file numbers follow),
/// each with a clean path no other file has.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TorrentFiles(Vec<TorrentFile>);

impl TorrentFiles {
    /// The files a torrent lists, as `(path parts, size, padding)`. Every
    /// part of every path must already be a clean file name: one the
    /// file-name rules would change (`..`, a separator, NUL, a character
    /// Windows forbids, a device name such as `CON`, a trailing dot or space)
    /// refuses the torrent, since the client writes the names as the torrent
    /// gives them. So do a path with no parts, two paths that differ only in
    /// case (one file on Windows and macOS), a file whose path is another
    /// file's folder, more than [`MAX_TORRENT_FILES`] files, and names that
    /// take more than [`MAX_FILE_NAME_BYTES`] in all. Padding files are never
    /// written, so their names are not looked at. (Names that differ only in
    /// their Unicode normalization are not told apart.)
    pub fn parse(raw: Vec<(Vec<String>, u64, bool)>) -> Result<TorrentFiles, TorrentError> {
        if raw.is_empty() {
            return Err(TorrentError::NoFiles);
        }
        if raw.len() > MAX_TORRENT_FILES {
            return Err(TorrentError::TooManyFiles { count: raw.len(), max: MAX_TORRENT_FILES });
        }
        let names: usize = raw.iter().flat_map(|(path, _, _)| path.iter()).map(|p| p.len() + 1).sum();
        if names > MAX_FILE_NAME_BYTES {
            return Err(TorrentError::FileListTooBig { max: MAX_FILE_NAME_BYTES });
        }
        let mut taken: HashSet<String> = HashSet::new();
        let mut folders: HashSet<String> = HashSet::new();
        let mut files = Vec::with_capacity(raw.len());
        for (path, size, padding) in raw {
            let shown = path.join("/");
            if !padding {
                let clean = !path.is_empty()
                    && path.iter().all(|part| sanitize_filename(part).is_some_and(|c| c.as_str() == part));
                if !clean || !taken.insert(shown.to_lowercase()) {
                    return Err(TorrentError::UnsafePath(shown));
                }
                for end in 1..path.len() {
                    folders.insert(path[..end].join("/").to_lowercase());
                }
            }
            files.push(TorrentFile { path, size: ByteCount::new(size), padding });
        }
        if let Some(both) = files.iter().find(|f| !f.padding && folders.contains(&f.display_path().to_lowercase())) {
            return Err(TorrentError::UnsafePath(both.display_path()));
        }
        Ok(TorrentFiles(files))
    }

    pub fn as_slice(&self) -> &[TorrentFile] {
        &self.0
    }

    /// The file's name, when the torrent is one file (saved under it, not
    /// in a folder).
    pub fn single_name(&self) -> Option<&str> {
        let mut real = self.0.iter().filter(|f| !f.padding);
        match (real.next(), real.next()) {
            (Some(f), None) if f.path.len() == 1 => f.path.first().map(String::as_str),
            _ => None,
        }
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The bytes of the files in `selection` (every file for `None`),
    /// padding left out.
    pub fn selected_size(&self, selection: Option<&FileSelection>) -> ByteCount {
        self.0
            .iter()
            .enumerate()
            .filter(|(i, f)| !f.padding && selection.is_none_or(|s| s.contains(*i)))
            .map(|(_, f)| f.size)
            .sum()
    }
}

/// Which of a torrent's files to download, by their numbers in the
/// torrent. Never empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<usize>", into = "Vec<usize>")]
pub struct FileSelection(BTreeSet<usize>);

impl FileSelection {
    /// The files numbered `indexes`, of a torrent with `count` files. Refused
    /// when empty or when a number is not one of its files.
    pub fn parse(indexes: &[usize], count: usize) -> Result<FileSelection, TorrentError> {
        if let Some(bad) = indexes.iter().find(|i| **i >= count) {
            return Err(TorrentError::NoSuchFile(*bad));
        }
        FileSelection::try_from(indexes.to_vec())
    }

    /// Every file of a torrent with `count` files.
    pub fn all(count: usize) -> Option<FileSelection> {
        (count > 0).then(|| FileSelection((0..count).collect()))
    }

    pub fn contains(&self, index: usize) -> bool {
        self.0.contains(&index)
    }

    pub fn indexes(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().copied()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Never true: a selection holds at least one file.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The numbers below `count`; `None` when none are.
    pub fn within(&self, count: usize) -> Option<FileSelection> {
        let kept: BTreeSet<usize> = self.0.iter().copied().filter(|i| *i < count).collect();
        (!kept.is_empty()).then_some(FileSelection(kept))
    }

    /// Whether it holds a file `other` does not.
    pub fn adds_to(&self, other: &FileSelection) -> bool {
        !self.0.is_subset(&other.0)
    }
}

impl TryFrom<Vec<usize>> for FileSelection {
    type Error = TorrentError;

    fn try_from(indexes: Vec<usize>) -> Result<Self, Self::Error> {
        if indexes.is_empty() {
            return Err(TorrentError::NothingSelected);
        }
        Ok(FileSelection(indexes.into_iter().collect()))
    }
}

impl From<FileSelection> for Vec<usize> {
    fn from(s: FileSelection) -> Self {
        s.0.into_iter().collect()
    }
}

/// A share ratio: bytes uploaded for each byte downloaded, kept in
/// hundredths, between 0 and 100.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Ratio(u32);

impl Ratio {
    /// The most a ratio may be.
    pub const MAX: Ratio = Ratio(100 * 100);

    /// The ratio `value`, rounded to hundredths: refused unless finite and
    /// between 0 and 100.
    pub fn new(value: f64) -> Result<Ratio, TorrentError> {
        if !value.is_finite() || !(0.0..=100.0).contains(&value) {
            return Err(TorrentError::BadRatio(value.to_string()));
        }
        // In range, so it fits: at most 10 000.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let hundredths = (value * 100.0).round() as u32;
        Ok(Ratio(hundredths))
    }

    /// The ratio `hundredths` / 100, at most 100.
    pub const fn from_hundredths(hundredths: u32) -> Ratio {
        Ratio(if hundredths > 10_000 { 10_000 } else { hundredths })
    }

    pub const fn hundredths(self) -> u32 {
        self.0
    }

    pub fn get(self) -> f64 {
        f64::from(self.0) / 100.0
    }

    /// The ratio of `uploaded` to `downloaded`, at most 100; 0 when nothing
    /// was downloaded.
    pub fn of(uploaded: ByteCount, downloaded: ByteCount) -> Ratio {
        if downloaded.is_zero() {
            return Ratio(0);
        }
        let hundredths = u128::from(uploaded.get()) * 100 / u128::from(downloaded.get());
        Ratio(u32::try_from(hundredths.min(10_000)).unwrap_or(10_000))
    }
}

impl TryFrom<f64> for Ratio {
    type Error = TorrentError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Ratio::new(value)
    }
}

impl From<Ratio> for f64 {
    fn from(r: Ratio) -> Self {
        r.get()
    }
}

impl fmt::Display for Ratio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:02}", self.0 / 100, self.0 % 100)
    }
}

/// When a finished torrent stops seeding: once it has uploaded `ratio`
/// times what it downloaded, or seeded for `minutes`, whichever comes
/// first. A limit that is `None` does not stop it; with both `None` it
/// seeds until it is stopped or removed. A ratio of 0 or 0 minutes stops it
/// as soon as it finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratio: Option<Ratio>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minutes: Option<u32>,
}

/// Seed to a ratio of 1.0: give back as much as was taken.
pub const DEFAULT_SEED_RATIO: Ratio = Ratio(100);

/// Seed for an hour at most, so a torrent nobody asks for does not keep the
/// computer uploading.
pub const DEFAULT_SEED_MINUTES: u32 = 60;

impl Default for SeedLimits {
    fn default() -> Self {
        SeedLimits { ratio: Some(DEFAULT_SEED_RATIO), minutes: Some(DEFAULT_SEED_MINUTES) }
    }
}

impl SeedLimits {
    /// Whether a torrent that downloaded `downloaded`, uploaded `uploaded`
    /// and has seeded for `seeded_minutes` has seeded enough.
    pub fn reached(&self, uploaded: ByteCount, downloaded: ByteCount, seeded_minutes: u64) -> bool {
        let by_ratio = self.ratio.is_some_and(|r| {
            r.hundredths() == 0
                || u128::from(uploaded.get()) * 100 >= u128::from(downloaded.get()) * u128::from(r.hundredths())
        });
        let by_time = self.minutes.is_some_and(|m| seeded_minutes >= u64::from(m));
        by_ratio || by_time
    }

    /// Whether it seeds at all once it finishes.
    pub fn seeds(&self) -> bool {
        !self.reached(ByteCount::ZERO, ByteCount::new(1), 0)
    }
}

/// Where a torrent download is, within its download's `Status`: while
/// `Downloading`, which of the first three; once `Done`, whether it still
/// seeds. A stopped or failed torrent keeps the phase it was in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TorrentPhase {
    /// Asking peers for the torrent's metadata (its files), from a magnet
    /// link.
    #[default]
    Metadata,
    /// Checking the data already on disk against the torrent's pieces.
    Checking,
    Downloading,
    /// Finished, and uploading to other peers.
    Seeding,
    /// Finished and no longer seeding: its limits were reached or it was
    /// stopped.
    Complete,
}

/// What a torrent download's row keeps besides what every download keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TorrentEntry {
    pub info_hash: InfoHash,
    #[serde(default)]
    pub phase: TorrentPhase,
    /// Its files, once its metadata is known. Never written down or sent
    /// with the row: a list can be long, and the daemon keeps the torrent's
    /// file, from which it fills this in (`Event::TorrentInfo`, which a
    /// window asks for with `Command::TorrentFiles`).
    #[serde(skip)]
    pub files: TorrentFiles,
    /// The files chosen to download; `None` is every file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<FileSelection>,
    /// Bytes uploaded to other peers, over every run.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub uploaded: ByteCount,
    /// When it started seeding, in seconds since 1970.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeding_since: Option<u64>,
    /// Its own seeding limits; `None` follows the settings'.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_limits: Option<SeedLimits>,
}

impl TorrentEntry {
    /// A torrent known only by its hash.
    pub fn new(info_hash: InfoHash) -> Self {
        TorrentEntry {
            info_hash,
            phase: TorrentPhase::Metadata,
            files: TorrentFiles::default(),
            selection: None,
            uploaded: ByteCount::ZERO,
            seeding_since: None,
            seed_limits: None,
        }
    }

    /// The row's torrent part for a download of `link`, if it is a magnet
    /// link.
    pub fn for_link(link: &str) -> Option<TorrentEntry> {
        starts_like_magnet(link).then(|| MagnetLink::parse(link).ok()).flatten().map(|m| TorrentEntry::new(m.info_hash))
    }

    /// The limits that stop its seeding: its own, else `settings`'.
    pub fn limits(&self, settings: &SeedLimits) -> SeedLimits {
        self.seed_limits.unwrap_or(*settings)
    }

    /// The files written under `root`, the download's path (its folder, or
    /// its one file): the chosen ones, padding left out.
    pub fn written_files(&self, root: &std::path::Path) -> Vec<std::path::PathBuf> {
        if self.files.single_name().is_some() {
            return vec![root.to_path_buf()];
        }
        self.files
            .as_slice()
            .iter()
            .enumerate()
            .filter(|(i, f)| !f.padding && self.selection.as_ref().is_none_or(|s| s.contains(*i)))
            .map(|(_, f)| f.path.iter().fold(root.to_path_buf(), |p, part| p.join(part)))
            .collect()
    }

    /// Bytes of the chosen files.
    pub fn selected_size(&self) -> ByteCount {
        self.files.selected_size(self.selection.as_ref())
    }

    /// Take in the latest stats: the phase, the bytes uploaded and when
    /// seeding began. `now` is seconds since 1970.
    pub fn observe(&mut self, stats: &TorrentStats, now: u64) {
        if stats.phase == TorrentPhase::Seeding && self.seeding_since.is_none() {
            self.seeding_since = Some(now);
        }
        self.phase = stats.phase;
        self.uploaded = self.uploaded.max(stats.uploaded);
    }

    /// Whether it has seeded enough by `limits` at `now` (seconds since
    /// 1970).
    pub fn seeded_enough(&self, limits: &SeedLimits, now: u64) -> bool {
        let minutes = self.seeding_since.map_or(0, |since| now.saturating_sub(since) / 60);
        let downloaded = self.selected_size().max(ByteCount::new(1));
        limits.reached(self.uploaded, downloaded, minutes)
    }
}

fn is_zero(b: &ByteCount) -> bool {
    b.is_zero()
}

/// What a torrent is, once its metadata is known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TorrentInfo {
    pub info_hash: InfoHash,
    /// Its name, made a safe file name: the folder a torrent of many files
    /// goes in, or its one file's name.
    pub name: String,
    pub files: TorrentFiles,
    pub piece_size: ByteCount,
    pub pieces: u32,
    /// A private torrent finds peers through its trackers only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
}

impl TorrentInfo {
    /// Whether it is one file, saved under its own name rather than in a
    /// folder.
    pub fn single_file(&self) -> bool {
        self.files.single_name().is_some()
    }
}

/// One peer a torrent is connected to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerInfo {
    /// Its address, `ip:port`.
    pub address: String,
    /// The program it says it is, when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    /// Bytes per second from it and to it.
    pub down_speed: u64,
    pub up_speed: u64,
    pub downloaded: ByteCount,
    pub uploaded: ByteCount,
    /// How the connection was made: `tcp`, `utp` or `socks`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
}

/// How UDM stands with one of a torrent's trackers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackerState {
    /// Announced to.
    Used,
    /// Left out: its name would have been looked up outside the chosen DNS
    /// over HTTPS resolver, or the settings turn trackers off.
    Skipped,
}

/// One of a torrent's trackers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackerInfo {
    pub url: String,
    pub state: TrackerState,
}

/// How much of a torrent's pieces are here, in at most
/// [`PIECE_MAP_CELLS`] cells: each cell is 0 to 100, the share of its
/// pieces this computer has.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PieceMap {
    pub pieces: u32,
    pub have: u32,
    pub cells: Vec<u8>,
}

impl PieceMap {
    /// The map of `pieces` pieces, of which `has(i)` says whether piece `i`
    /// is here.
    pub fn of(pieces: u32, has: impl Fn(u32) -> bool) -> PieceMap {
        let total = pieces as usize;
        let cells = total.min(PIECE_MAP_CELLS);
        let mut map = PieceMap { pieces, have: 0, cells: Vec::with_capacity(cells) };
        if cells == 0 {
            return map;
        }
        for c in 0..cells {
            let (from, to) = (c * total / cells, (c + 1) * total / cells);
            let here = (from..to).filter(|i| u32::try_from(*i).is_ok_and(&has)).count();
            map.have += u32::try_from(here).unwrap_or(0);
            let share = here * 100 / (to - from).max(1);
            map.cells.push(u8::try_from(share).unwrap_or(100));
        }
        map
    }
}

/// How a running or seeding torrent is doing, for its window and its row.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TorrentStats {
    pub phase: TorrentPhase,
    /// Bytes of the chosen files here, and in all.
    pub done: ByteCount,
    pub total: ByteCount,
    /// Bytes per second, down and up.
    pub down_speed: u64,
    pub up_speed: u64,
    /// Bytes uploaded over every run.
    pub uploaded: ByteCount,
    /// Seconds until it finishes at this speed, when it can be told.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_seconds: Option<u64>,
    /// Peers connected now, and every peer known.
    pub peers_connected: u32,
    pub peers_known: u32,
    /// The busiest connected peers, at most [`MAX_LISTED_PEERS`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub peers: Vec<PeerInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trackers: Vec<TrackerInfo>,
    #[serde(default)]
    pub pieces: PieceMap,
    /// Bytes here of each file, in the torrent's order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_done: Vec<ByteCount>,
}

impl TorrentStats {
    /// The share ratio so far.
    pub fn ratio(&self) -> Ratio {
        Ratio::of(self.uploaded, self.total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "c12fe1c06bba254a9dc9f519b335aa7c1367a88a";

    #[test]
    fn info_hashes_parse_from_hex_or_base32() {
        let h = InfoHash::parse(HEX).unwrap();
        assert_eq!(h.hex(), HEX);
        assert_eq!(InfoHash::parse(&HEX.to_uppercase()).unwrap(), h);
        // The same 20 bytes in base32.
        assert_eq!(InfoHash::parse("YEX6DQDLXISUVHOJ6UM3GNNKPQJWPKEK").unwrap(), h);
        assert_eq!(InfoHash::parse("yex6dqdlxisuvhoj6um3gnnkpqjwpkek").unwrap(), h);
        for bad in ["", "abc", &HEX[..39], &format!("{}g", &HEX[..39]), "YEX6DQDLXISUVHOJ6UM3GNNKPQJWPKE1"] {
            assert!(InfoHash::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(serde_json::to_string(&h).unwrap(), format!("\"{HEX}\""));
        assert_eq!(serde_json::from_str::<InfoHash>(&format!("\"{HEX}\"")).unwrap(), h);
    }

    #[test]
    fn magnet_links_keep_their_hash_name_trackers_seeds_and_peers() {
        let text = format!(
            "magnet:?xt=urn:btih:{HEX}&dn=Some+File%20Name&tr=udp%3A%2F%2Ft.example%3A1337&tr.1=https://t2.example/a\
             &tr=wss://no.example&tr=udp://t.example:1337&ws=https://seed.example/f&ws=ftp://x/&x.pe=10.0.0.1:6881\
             &x.pe=nope&xl=123&foo=bar"
        );
        let m = MagnetLink::parse(&text).unwrap();
        assert_eq!(m.info_hash.hex(), HEX);
        assert_eq!(m.name.as_deref(), Some("Some File Name"));
        assert_eq!(m.trackers, ["udp://t.example:1337", "https://t2.example/a"]);
        assert_eq!(m.web_seeds, ["https://seed.example/f"]);
        assert_eq!(m.peers, ["10.0.0.1:6881"]);
        assert!(is_magnet(&text) && is_magnet("MAGNET:?xt=urn:btih:YEX6DQDLXISUVHOJ6UM3GNNKPQJWPKEK"));
        let again = MagnetLink::parse(&m.to_uri()).unwrap();
        assert_eq!(again, m, "the canonical form parses back to the same link");
        assert!(m.to_uri().starts_with(&format!("magnet:?xt=urn:btih:{HEX}&dn=Some%20File%20Name&tr=udp://")));
    }

    #[test]
    fn magnet_links_without_a_v1_hash_are_refused() {
        assert_eq!(MagnetLink::parse("magnet:?dn=x"), Err(TorrentError::NoInfoHash));
        assert_eq!(MagnetLink::parse("magnet:?xt=urn:ed2k:abcdef"), Err(TorrentError::NoInfoHash));
        assert_eq!(MagnetLink::parse("magnet:?xt=urn:btmh:1220abcd"), Err(TorrentError::NoInfoHash));
        assert!(matches!(MagnetLink::parse("magnet:?xt=urn:btih:xyz"), Err(TorrentError::BadInfoHash(_))));
        assert!(matches!(MagnetLink::parse("https://x/a"), Err(TorrentError::NotMagnet(_))));
        assert!(!is_magnet("magnet:?dn=x") && !is_magnet("magnet:x"));
        // A v1 hash beside a v2 one (a hybrid torrent) is taken.
        assert!(MagnetLink::parse(&format!("magnet:?xt=urn:btmh:1220ab&xt=urn:btih:{HEX}")).is_ok());
    }

    #[test]
    fn torrent_file_addresses_are_told_by_type_or_name() {
        assert!(is_torrent_file("https://x/a.torrent", None, None));
        assert!(is_torrent_file("https://x/A.TORRENT?k=1", None, None));
        assert!(is_torrent_file("https://x/get?id=3", Some("application/x-bittorrent; charset=binary"), None));
        assert!(is_torrent_file("https://x/download.php?id=3", Some("application/octet-stream"), Some("x.torrent")));
        assert!(!is_torrent_file("https://x/a.torrent.html", Some("text/html"), None));
        assert!(!is_torrent_file("https://x/a.zip", None, Some("a.zip")));
    }

    fn raw(paths: &[&str]) -> Vec<(Vec<String>, u64, bool)> {
        paths.iter().map(|p| (p.split('/').map(str::to_string).collect(), 10, false)).collect()
    }

    #[test]
    fn only_clean_file_paths_are_taken() {
        let files = TorrentFiles::parse(raw(&["a/b.txt", "a/c d.txt", "e (1).iso"])).unwrap();
        let paths: Vec<String> = files.as_slice().iter().map(TorrentFile::display_path).collect();
        assert_eq!(paths, ["a/b.txt", "a/c d.txt", "e (1).iso"]);
        assert_eq!(files.selected_size(None), ByteCount::new(30));
        for bad in [
            &["a/../b"][..],
            &["."],
            &["a/"],
            &["..."],
            &["a/CON.txt"],
            &["c:d.iso"],
            &["what?.txt"],
            &["x. "],
            &["a/b.txt", "A/B.txt"],
            &["a", "A/b"],
            &["x/y/z", "x/Y"],
        ] {
            assert!(matches!(TorrentFiles::parse(raw(bad)), Err(TorrentError::UnsafePath(_))), "{bad:?}");
        }
        let padded = vec![(vec!["a".into()], 1, false), (vec![".pad".into(), "5".into()], 5, true)];
        assert_eq!(TorrentFiles::parse(padded).unwrap().selected_size(None), ByteCount::new(1));
        assert!(matches!(TorrentFiles::parse(vec![(vec!["a\\b".into()], 1, false)]), Err(TorrentError::UnsafePath(_))));
        assert!(matches!(TorrentFiles::parse(vec![(vec!["a\0".into()], 1, false)]), Err(TorrentError::UnsafePath(_))));
        assert!(matches!(TorrentFiles::parse(vec![(vec![], 1, false)]), Err(TorrentError::UnsafePath(_))));
        assert_eq!(TorrentFiles::parse(vec![]), Err(TorrentError::NoFiles));
        let many = (0..=MAX_TORRENT_FILES).map(|i| (vec![format!("{i}")], 1, false)).collect();
        assert!(matches!(TorrentFiles::parse(many), Err(TorrentError::TooManyFiles { .. })));
    }

    #[test]
    fn a_long_file_list_is_refused() {
        let long = "n".repeat(200);
        let list = (0..MAX_FILE_NAME_BYTES / 200).map(|i| (vec![format!("{i}{long}")], 1, false)).collect();
        assert_eq!(TorrentFiles::parse(list), Err(TorrentError::FileListTooBig { max: MAX_FILE_NAME_BYTES }));
    }

    #[test]
    fn made_links_stay_short_and_logs_lose_trackers() {
        let mut m = MagnetLink::new(InfoHash::parse(HEX).unwrap());
        m.name = Some("Album".into());
        m.trackers = (0..500).map(|i| format!("udp://t{i}.example:6969/announce?passkey=secret{i}")).collect();
        let short = m.to_uri_within(MAX_MADE_MAGNET);
        assert!(short.len() <= MAX_MADE_MAGNET && short.contains("&tr=udp") && short.contains("dn=Album"));
        assert_eq!(MagnetLink::parse(&short).unwrap().info_hash, m.info_hash);
        let logged = loggable(&m.to_uri());
        assert!(!logged.contains("secret") && logged.contains(HEX), "{logged}");
        assert_eq!(loggable("https://x/a?b"), "https://x/a?b");
        let line = format!(r#"add url="{}" ok"#, m.to_uri());
        let scrubbed = scrub_magnets(&line);
        assert!(
            !scrubbed.contains("secret")
                && scrubbed.starts_with("add url=\"magnet:?xt=urn:btih:")
                && scrubbed.ends_with("\" ok")
        );
        assert_eq!(scrub_magnets("nothing here"), "nothing here");
        assert!(!loggable("magnet:?tr=udp://t/passkey").contains("passkey"));
    }

    #[test]
    fn a_rows_written_files_are_its_chosen_ones() {
        let mut e = TorrentEntry::for_link(&format!("magnet:?xt=urn:btih:{HEX}")).unwrap();
        e.files = TorrentFiles::parse(raw(&["a/x", "b", "c"])).unwrap();
        e.selection = FileSelection::parse(&[0, 2], 3).ok();
        let root = std::path::Path::new("/dl/Album");
        assert_eq!(e.written_files(root), [root.join("a").join("x"), root.join("c")]);
        e.files = TorrentFiles::parse(raw(&["one.iso"])).unwrap();
        e.selection = None;
        assert_eq!(e.written_files(std::path::Path::new("/dl/one.iso")), [std::path::Path::new("/dl/one.iso")]);
    }

    #[test]
    fn a_torrent_in_the_list_is_never_added_twice() {
        use crate::download::{Download, DownloadEntry, DuplicateStep};
        use crate::settings::Duplicate;
        let link = format!("magnet:?xt=urn:btih:{HEX}&dn=a");
        let old: DownloadEntry = serde_json::from_value(serde_json::json!({
            "id": 1, "url": format!("magnet:?xt=urn:btih:{}", HEX.to_uppercase()), "status": "done",
            "connections": 1, "done": 0, "resumable": true,
        }))
        .unwrap();
        assert!(crate::download::same_address(&old.url, &link.as_str().into()), "the same hash");
        for choice in [Duplicate::Ask, Duplicate::NewName, Duplicate::Overwrite, Duplicate::Existing] {
            let mut d = Download { url: link.as_str().into(), ..Download::default() };
            assert_eq!(d.settle_duplicate(&old, choice), DuplicateStep::UseExisting, "{choice:?}");
        }
    }

    #[test]
    fn a_one_file_torrent_keeps_its_name() {
        use crate::download::{DownloadEntry, DownloadError};
        let mut e: DownloadEntry = serde_json::from_value(serde_json::json!({
            "id": 1, "url": format!("magnet:?xt=urn:btih:{HEX}"), "status": "confirming",
            "connections": 0, "done": 0, "resumable": true, "path": "/dl/a.iso",
        }))
        .unwrap();
        let mut t = TorrentEntry::new(InfoHash::parse(HEX).unwrap());
        t.files = TorrentFiles::parse(raw(&["a.iso"])).unwrap();
        e.torrent = Some(t);
        let refused = e.clone().apply_answer(Some("b.iso"), None, None, None);
        assert_eq!(refused, Err(DownloadError::Torrent(TorrentError::NameFixed("a.iso".into()))));
        // An absolute folder on each system: "/other" is relative on Windows.
        let other = if cfg!(windows) { r"C:\other" } else { "/other" };
        assert!(e.apply_answer(Some("a.iso"), Some(other), None, None).is_ok());
        assert_eq!(e.path.as_deref(), Some(std::path::Path::new(other).join("a.iso").to_str().unwrap()));
    }

    #[test]
    fn selections_name_files_that_exist() {
        let s = FileSelection::parse(&[2, 0, 2], 3).unwrap();
        assert_eq!(s.indexes().collect::<Vec<_>>(), [0, 2]);
        assert_eq!(FileSelection::parse(&[], 3), Err(TorrentError::NothingSelected));
        assert_eq!(FileSelection::parse(&[3], 3), Err(TorrentError::NoSuchFile(3)));
        assert_eq!(serde_json::to_string(&s).unwrap(), "[0,2]");
        assert!(serde_json::from_str::<FileSelection>("[]").is_err());
        assert!(FileSelection::all(3).unwrap().adds_to(&s) && !s.adds_to(&FileSelection::all(3).unwrap()));
        assert_eq!(s.within(1), FileSelection::parse(&[0], 1).ok());
        let files = TorrentFiles::parse(raw(&["a", "b", "c"])).unwrap();
        assert_eq!(files.selected_size(Some(&s)), ByteCount::new(20));
    }

    #[test]
    fn seeding_stops_at_the_ratio_or_the_time() {
        let limits = SeedLimits::default();
        let (mb, ten) = (ByteCount::new(1_000_000), ByteCount::new(10_000_000));
        assert!(!limits.reached(mb, ten, 59));
        assert!(limits.reached(ten, ten, 0), "ratio 1.0");
        assert!(limits.reached(mb, ten, 60), "an hour");
        let ratio_only = SeedLimits { ratio: Some(Ratio::new(2.5).unwrap()), minutes: None };
        assert!(!ratio_only.reached(ByteCount::new(24), ByteCount::new(10), 100_000));
        assert!(ratio_only.reached(ByteCount::new(25), ByteCount::new(10), 0));
        let forever = SeedLimits { ratio: None, minutes: None };
        assert!(!forever.reached(ten, mb, u64::MAX) && forever.seeds());
        assert!(!SeedLimits { ratio: Some(Ratio::new(0.0).unwrap()), minutes: None }.seeds());
        assert!(!SeedLimits { ratio: None, minutes: Some(0) }.seeds());
        assert!(Ratio::new(-1.0).is_err() && Ratio::new(100.5).is_err() && Ratio::new(f64::NAN).is_err());
        assert_eq!(Ratio::of(ByteCount::new(3), ByteCount::new(2)).to_string(), "1.50");
        assert_eq!(serde_json::to_string(&Ratio::new(1.25).unwrap()).unwrap(), "1.25");
    }

    #[test]
    fn a_row_follows_its_torrents_stats() {
        let mut e = TorrentEntry::for_link(&format!("magnet:?xt=urn:btih:{HEX}")).unwrap();
        assert!(TorrentEntry::for_link("https://x/a").is_none());
        e.files = TorrentFiles::parse(raw(&["a", "b"])).unwrap();
        let mut stats =
            TorrentStats { phase: TorrentPhase::Seeding, uploaded: ByteCount::new(5), ..Default::default() };
        e.observe(&stats, 1000);
        assert_eq!((e.phase, e.seeding_since, e.uploaded), (TorrentPhase::Seeding, Some(1000), ByteCount::new(5)));
        stats.uploaded = ByteCount::new(2);
        e.observe(&stats, 2000);
        assert_eq!((e.seeding_since, e.uploaded), (Some(1000), ByteCount::new(5)), "uploads only add up");
        let limits = SeedLimits { ratio: None, minutes: Some(10) };
        assert!(!e.seeded_enough(&limits, 1000 + 599) && e.seeded_enough(&limits, 1000 + 600));
        assert!(e.seeded_enough(&SeedLimits { ratio: Some(Ratio::new(0.25).unwrap()), minutes: None }, 1000));
    }

    #[test]
    fn the_piece_map_shares_pieces_out_over_its_cells() {
        let map = PieceMap::of(4, |i| i % 2 == 0);
        assert_eq!((map.pieces, map.have, map.cells), (4, 2, vec![100, 0, 100, 0]));
        let big = PieceMap::of(2048, |i| i < 1024);
        assert_eq!(big.cells.len(), PIECE_MAP_CELLS);
        assert_eq!((big.have, big.cells[0], big.cells[PIECE_MAP_CELLS - 1]), (1024, 100, 0));
        assert!(PieceMap::of(0, |_| true).cells.is_empty());
    }
}
