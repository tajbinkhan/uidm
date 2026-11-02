//! Streamed video: HLS playlists (`.m3u8`) and DASH manifests (`.mpd`).
//!
//! A stream is a list of short segments rather than one file. This module
//! reads playlists and manifests into `Track`s (the segments of one video or
//! audio rendition, with their keys) and describes the qualities a stream
//! offers. The engine fetches the segments and joins them (see
//! `engine::stream`).
//!
//! Streams protected by DRM, and live streams, are refused: UIDM does not get
//! around DRM, and a live stream has no end to download to.
//!
//! `convert` turns a finished video into an MP4, or its sound into an MP3
//! or M4A, with ffmpeg. `ytdlp` reads video pages that name no stream at
//! all (YouTube) with yt-dlp.

pub mod convert;
pub mod dash;
pub mod hls;
pub mod ytdlp;

use serde::{Deserialize, Serialize};

/// The two stream formats, and a video page yt-dlp reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Hls,
    Dash,
    Page,
}

/// Which stream to download, and in which quality.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaChoice {
    /// The format; `None` finds out from the playlist itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<MediaKind>,
    /// The video quality: an option `id` from `MediaInfo`. `None` takes the
    /// best.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<String>,
    /// The audio track: an option `id` from `MediaInfo`. `None` takes the
    /// default, or the best.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
    /// Keep only the sound, as this (`mp3` or `m4a`). Needs ffmpeg; only
    /// the audio track is fetched when the stream has one of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_only: Option<convert::Target>,
}

/// One quality or audio track a stream offers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MediaOption {
    /// What to put in `MediaChoice` to get this one.
    pub id: String,
    /// For people: "1080p, 5.2 Mbps" or "English".
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Bits per second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandwidth: Option<u64>,
    /// About how big it is, from its bit rate and the length of the video.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// What a stream offers, or why it cannot be downloaded.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<MediaKind>,
    /// Video qualities, best first.
    #[serde(default)]
    pub video: Vec<MediaOption>,
    /// Separate audio tracks, if the stream has them.
    #[serde(default)]
    pub audio: Vec<MediaOption>,
    /// Length in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    /// The stream is protected by DRM and will not be downloaded.
    #[serde(default)]
    pub drm: bool,
    /// Why the stream cannot be downloaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The address answered with something that is not a playlist or a
    /// manifest at all: a `.m3u8` a page's script decodes itself, or one the
    /// browser caught that is no stream. The menu leaves it out when there is
    /// something to download beside it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub not_stream: bool,
    /// The address is a whole video or audio file, not a stream, though its
    /// name or type said it was a playlist: the browser downloads it as a
    /// file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<MediaFile>,
}

/// A whole video or audio file found where a stream was expected.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaFile {
    /// Its type, as a file extension: `mp4`, `ts`, `webm`.
    pub ext: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

impl MediaInfo {
    pub fn failed(kind: Option<MediaKind>, error: String) -> Self {
        MediaInfo {
            kind,
            error: Some(error),
            ..MediaInfo::default()
        }
    }
}

/// The key of an AES-128 encrypted segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentKey {
    pub url: String,
    pub iv: [u8; 16],
}

/// One piece of a track: a URL, maybe only a byte range of it, maybe
/// encrypted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub url: String,
    /// Inclusive byte range.
    pub range: Option<(u64, u64)>,
    pub key: Option<SegmentKey>,
}

impl Segment {
    pub fn whole(url: impl Into<String>) -> Self {
        Segment {
            url: url.into(),
            range: None,
            key: None,
        }
    }
}

/// The segments of one video or audio rendition, in order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Track {
    /// A header every segment needs (fragmented MP4), fetched once.
    pub init: Option<Segment>,
    pub segments: Vec<Segment>,
    /// The file extension the joined track has: `ts`, `mp4`, `m4a`, `webm`.
    pub ext: &'static str,
    /// Length in seconds, when known.
    pub duration: Option<f64>,
}

/// Why a stream is refused, in words for the user.
pub const DRM_REFUSED: &str =
    "This video is protected by DRM (copy protection), so UIDM will not download it.";
pub const LIVE_REFUSED: &str =
    "This is a live stream. UIDM downloads recorded videos, which have an end.";

/// `url` resolved against `base`, as a browser would.
pub fn resolve(base: &str, url: &str) -> Result<String, String> {
    let base = url::Url::parse(base).map_err(|e| format!("bad address {base}: {e}"))?;
    let joined = base.join(url.trim()).map_err(|e| format!("bad address {url}: {e}"))?;
    match joined.scheme() {
        "http" | "https" => Ok(joined.to_string()),
        other => Err(format!("{other}: addresses are not supported in a stream")),
    }
}

/// "1080p, 5.2 Mbps", as much of it as is known.
pub fn quality_label(height: Option<u32>, bandwidth: Option<u64>) -> String {
    let mut parts = Vec::new();
    if let Some(h) = height {
        parts.push(format!("{h}p"));
    }
    if let Some(b) = bandwidth {
        if b >= 1_000_000 {
            parts.push(format!("{:.1} Mbps", b as f64 / 1e6));
        } else {
            parts.push(format!("{} kbps", (b / 1000).max(1)));
        }
    }
    if parts.is_empty() {
        "Default".to_string()
    } else {
        parts.join(", ")
    }
}

/// About how many bytes `seconds` of a `bandwidth` bits-per-second stream
/// take.
pub fn estimate_size(bandwidth: Option<u64>, seconds: Option<f64>) -> Option<u64> {
    let (b, s) = (bandwidth?, seconds?);
    (s > 0.0).then(|| (b as f64 * s / 8.0) as u64)
}

/// Whether `text` looks like an HLS playlist or a DASH manifest.
pub fn sniff(text: &str) -> Option<MediaKind> {
    let head = text.trim_start_matches('\u{feff}').trim_start();
    if head.starts_with("#EXTM3U") {
        Some(MediaKind::Hls)
    } else if head.starts_with('<')
        && head.as_bytes()[..head.len().min(4096)].windows(4).any(|w| w == b"<MPD")
    {
        Some(MediaKind::Dash)
    } else {
        None
    }
}

/// The kind of video or audio file that starts with `head`, as a file
/// extension, or `None` when it is no media file UIDM knows.
pub fn sniff_file(head: &[u8]) -> Option<&'static str> {
    let at = |i: usize, sig: &[u8]| head.get(i..i + sig.len()) == Some(sig);
    if at(4, b"ftyp") {
        return Some(if at(8, b"M4A ") { "m4a" } else { "mp4" });
    }
    if at(0, &[0x1a, 0x45, 0xdf, 0xa3]) {
        let doc = &head[..head.len().min(64)];
        return Some(if doc.windows(8).any(|w| w == b"matroska") { "mkv" } else { "webm" });
    }
    // MPEG-TS: 188-byte packets, each starting with 0x47.
    if head.len() >= 377 && head[0] == 0x47 && head[188] == 0x47 && head[376] == 0x47 {
        return Some("ts");
    }
    if at(0, b"FLV") {
        return Some("flv");
    }
    if at(0, b"OggS") {
        return Some("ogg");
    }
    if at(0, b"RIFF") && at(8, b"WAVE") {
        return Some("wav");
    }
    if at(0, b"ID3") {
        return Some("mp3");
    }
    match head {
        [0xff, b, ..] if b & 0xf6 == 0xf0 => Some("aac"),
        [0xff, b, ..] if b & 0xe0 == 0xe0 => Some("mp3"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_sizes() {
        assert_eq!(quality_label(Some(1080), Some(5_200_000)), "1080p, 5.2 Mbps");
        assert_eq!(quality_label(None, Some(128_000)), "128 kbps");
        assert_eq!(quality_label(None, None), "Default");
        assert_eq!(estimate_size(Some(8_000_000), Some(10.0)), Some(10_000_000));
        assert_eq!(estimate_size(None, Some(10.0)), None);
    }

    #[test]
    fn resolving() {
        assert_eq!(resolve("https://x/a/b.m3u8", "c/1.ts").unwrap(), "https://x/a/c/1.ts");
        assert_eq!(resolve("https://x/a/b.m3u8", "/1.ts").unwrap(), "https://x/1.ts");
        assert_eq!(resolve("https://x/a/b.m3u8", "http://y/1.ts").unwrap(), "http://y/1.ts");
        assert!(resolve("https://x/a/b.m3u8", "file:///c:/x").is_err());
        assert!(resolve("https://x/a/b.m3u8", "data:text/plain,x").is_err());
    }

    #[test]
    fn sniffing() {
        assert_eq!(sniff("\u{feff}#EXTM3U\n"), Some(MediaKind::Hls));
        assert_eq!(sniff("<?xml version=\"1.0\"?>\n<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\">"), Some(MediaKind::Dash));
        assert_eq!(sniff("<html>"), None);
    }

    #[test]
    fn sniffing_files() {
        assert_eq!(sniff_file(b"\0\0\0\x20ftypisom\0\0\x02\0"), Some("mp4"));
        assert_eq!(sniff_file(b"\0\0\0\x20ftypM4A \0\0\0\0"), Some("m4a"));
        let mut ts = vec![0u8; 400];
        ts[0] = 0x47;
        ts[188] = 0x47;
        ts[376] = 0x47;
        assert_eq!(sniff_file(&ts), Some("ts"));
        assert_eq!(sniff_file(&[0x47; 100]), None);
        assert_eq!(sniff_file(b"\x1a\x45\xdf\xa3\x01\x42\x82\x84webm"), Some("webm"));
        assert_eq!(sniff_file(b"\x1a\x45\xdf\xa3\x01\x42\x82\x88matroska"), Some("mkv"));
        assert_eq!(sniff_file(b"ID3\x04"), Some("mp3"));
        assert_eq!(sniff_file(&[0xff, 0xf1, 0x50]), Some("aac"));
        assert_eq!(sniff_file(b"<html><body>"), None);
        assert_eq!(sniff_file(b"#EXTM3U"), None);
    }
}
