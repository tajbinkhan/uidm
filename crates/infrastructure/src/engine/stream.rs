//! Downloading a stream: read its HLS playlist, DASH manifest or HDS
//! manifest, fetch the segments of the chosen quality in parallel, decrypt
//! AES-128 segments, join each track into one file (for HDS, an FLV of the
//! fragments' tags), and make one video with ffmpeg when it is there.
//!
//! Segments are saved one file each in a `<file>.udmparts` folder next to
//! the output (or in the temporary folder, when one is set), first as `.tmp` and renamed once complete, so a canceled or
//! failed download resumes segment by segment: complete ones are not fetched
//! again. The folder goes once the video is made.
//!
//! Without ffmpeg an HLS stream is saved as its joined `.ts` (or `.mp4` for
//! fragmented MP4 segments), an HDS stream as its `.flv`, and a stream with
//! separate audio is saved as two files, with a note saying so.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use udm_domain::bytes::ByteCount;
use udm_domain::id::DownloadId;
use udm_domain::sync::LockExt;

use url::Url;

use super::limit::Throttle;
use udm_domain::filename::{numbered_name, sanitize_filename};

use super::{
    backoff, choose_output, fetch_with, http, CancelToken, DownloadRequest, EngineError, Progress, Reporter, Saved,
    Shared, BUF_SIZE, MAX_CONNECTIONS, MAX_RETRIES,
};
use crate::media::{dash, hds, hls};
use crate::net::NetConfig;
use udm_domain::media::{
    sniff, MediaChoice, MediaFile, MediaInfo, MediaKind, MediaOption, Segment, Subtitle, Track, DRM_REFUSED,
};

/// Most bytes read of a playlist or manifest.
const MAX_PLAYLIST_BYTES: u64 = 16 * 1024 * 1024;
/// Report progress at most this often.
const REPORT_INTERVAL: Duration = Duration::from_millis(250);

/// The folder a stream's segments wait in while it downloads. One Pullbay
/// or UIDM left (`<output>.pullbayparts`, `<output>.uidmparts`) is renamed
/// to it first, so it resumes.
pub(super) fn parts_dir(output: &Path) -> PathBuf {
    let dir = super::resume::beside(output, ".udmparts");
    for old in [".pullbayparts", ".uidmparts"] {
        super::resume::adopt(&super::resume::beside(output, old), &dir);
    }
    dir
}

/// UDM will not download this stream, for the reason `why`, which the
/// user sees.
fn refused(why: impl std::fmt::Display) -> EngineError {
    EngineError::Refused(why.to_string())
}

/// The largest encrypted segment decrypted in memory. Real HLS segments are
/// a few megabytes.
const MAX_ENCRYPTED_SEGMENT: u64 = 256 * 1024 * 1024;

/// The download's headers for a request to `url`. The cookies and any
/// `Authorization` were given for `added` (the address the stream was added
/// with), so they go only to its origin, never to another server a playlist
/// or manifest names.
pub(super) fn headers_for<'a>(added: &str, headers: &'a [(String, String)], url: &str) -> Cow<'a, [(String, String)]> {
    let same_origin = match (Url::parse(added), Url::parse(url)) {
        (Ok(a), Ok(b)) => a.origin() == b.origin(),
        _ => false,
    };
    if same_origin {
        return Cow::Borrowed(headers);
    }
    let credential = |k: &str| k.eq_ignore_ascii_case("cookie") || k.eq_ignore_ascii_case("authorization");
    Cow::Owned(headers.iter().filter(|(k, _)| !credential(k)).cloned().collect())
}

/// Fetch and read a playlist or manifest as text.
fn get_text(agent: &http::Client, url: &str, headers: &[(String, String)]) -> Result<(String, String), EngineError> {
    let page = fetch_with(agent, url, headers, MAX_PLAYLIST_BYTES)?;
    Ok((page.url, String::from_utf8_lossy(&page.body).into_owned()))
}

/// The tracks `choice` asks for: the main one, apart from it its audio, and
/// the subtitles to keep.
type Tracks = (Track, Option<Track>, Vec<Subtitle>);

fn load_tracks(agent: &http::Client, req: &DownloadRequest, choice: &MediaChoice) -> Result<Tracks, EngineError> {
    let (base, text) = get_text(agent, &req.url, &req.headers)?;
    let kind = choice.kind.or_else(|| sniff(&text)).ok_or_else(|| refused(NOT_A_STREAM))?;
    match kind {
        MediaKind::Page => Err(refused("A video page is not an HLS playlist or a DASH manifest.")),
        MediaKind::Hds => Ok((hds_track(agent, req, &text, &base, choice.video.as_deref())?, None, Vec::new())),
        MediaKind::Dash => {
            let manifest = dash::parse(&text, &base).map_err(refused)?;
            let (video, audio) =
                dash::pick(&manifest, choice.video.as_deref(), choice.audio.as_deref()).map_err(refused)?;
            Ok((video, audio, dash::pick_subtitles(&manifest, choice.subtitles.as_deref())))
        }
        MediaKind::Hls => match hls::parse(&text, &base, false).map_err(refused)? {
            hls::Playlist::Media(track) => Ok((track, None, Vec::new())),
            hls::Playlist::Master(master) => {
                if master.drm {
                    return Err(refused(DRM_REFUSED));
                }
                let variant = hls::pick_variant(&master, choice.video.as_deref());
                let media = |url: &str, audio: bool| -> Result<Track, EngineError> {
                    let (base, text) = get_text(agent, url, &headers_for(&req.url, &req.headers, url))?;
                    match hls::parse(&text, &base, audio).map_err(refused)? {
                        hls::Playlist::Media(track) => Ok(track),
                        hls::Playlist::Master(_) => Err(refused("A quality's playlist lists more playlists.")),
                    }
                };
                let video = media(&variant.url, false)?;
                let audio = hls::pick_audio(&master, variant, choice.audio.as_deref())
                    .map(|url| media(url, true))
                    .transpose()?;
                // A subtitle playlist that cannot be read costs the subtitle,
                // not the video.
                let subtitles = hls::pick_subtitles(&master, variant, choice.subtitles.as_deref())
                    .into_iter()
                    .filter_map(|s| match media(&s.url, false) {
                        Ok(track) => Some(Subtitle {
                            label: s.name.clone(),
                            language: s.language.clone(),
                            segments: track.init.into_iter().chain(track.segments).collect(),
                        }),
                        Err(e) => {
                            tracing::warn!(subtitles = %s.name, error = %e, "skipping subtitles");
                            None
                        }
                    })
                    .collect();
                Ok((video, audio, subtitles))
            }
        },
    }
}

/// The track of the HDS manifest `text` (fetched from `base`) in the
/// quality `video`: its fragments, from its bootstrap info, which may be at
/// another address, as may the manifest that lists them (F4M 2.0).
fn hds_track(
    agent: &http::Client,
    req: &DownloadRequest,
    text: &str,
    base: &str,
    video: Option<&str>,
) -> Result<Track, EngineError> {
    let manifest = hds::parse(text, base).map_err(refused)?;
    let mut media = hds::pick(&manifest, video).clone();
    let mut duration = manifest.duration;
    if let hds::Source::Manifest(url) = &media.source {
        let (base, text) = get_text(agent, url, &headers_for(&req.url, &req.headers, url))?;
        let inner = hds::parse(&text, &base).map_err(refused)?;
        duration = duration.or(inner.duration);
        media = hds::pick(&inner, None).clone();
    }
    let bootstrap = match &media.source {
        hds::Source::Fragments { bootstrap: hds::Bootstrap::Inline(data), .. } => data.clone(),
        hds::Source::Fragments { bootstrap: hds::Bootstrap::Url(url), .. } => {
            fetch_with(agent, url, &headers_for(&req.url, &req.headers, url), MAX_PLAYLIST_BYTES)?.body
        }
        hds::Source::Manifest(_) => return Err(refused("A quality's manifest lists more manifests.")),
    };
    hds::track(&media, &bootstrap, duration).map_err(refused)
}

/// What a stream address that is none of the formats is told.
const NOT_A_STREAM: &str = "This address is not an HLS playlist, a DASH manifest or an HDS manifest.";

/// Bytes read before deciding whether an address is a playlist, a manifest
/// or a media file.
const SNIFF_BYTES: u64 = 64 * 1024;

/// The playlist or manifest at `url` as text, or, when it is something else,
/// what to tell the browser about it: a whole media file (which pages load
/// under `.m3u8` names and playlist types too) or no stream at all. Only its
/// first bytes are read then, not a whole video.
fn read_playlist(
    agent: &http::Client,
    url: &str,
    headers: &[(String, String)],
) -> Result<Result<(String, String), MediaInfo>, EngineError> {
    let (base, size, reader) = http::get_sized(agent, url, headers)?;
    let mut reader = reader.take(MAX_PLAYLIST_BYTES);
    let mut body = Vec::new();
    (&mut reader).take(SNIFF_BYTES).read_to_end(&mut body)?;
    if sniff(&String::from_utf8_lossy(&body)).is_none() {
        if let Some(ext) = udm_domain::media::sniff_file(&body) {
            return Ok(Err(MediaInfo {
                file: Some(MediaFile { ext: ext.into(), size: size.map(ByteCount::new) }),
                ..MediaInfo::default()
            }));
        }
        return Ok(Err(MediaInfo { not_stream: true, ..MediaInfo::failed(None, NOT_A_STREAM.into()) }));
    }
    reader.read_to_end(&mut body)?;
    Ok(Ok((base, String::from_utf8_lossy(&body).into_owned())))
}

/// What a stream offers, for the browser's quality menu.
pub fn probe(url: &str, headers: &[(String, String)], net: &NetConfig) -> MediaInfo {
    let agent = match http::Client::new(net, url) {
        Ok(a) => a,
        Err(e) => return MediaInfo::failed(None, e.to_string()),
    };
    let (base, text) = match read_playlist(&agent, url, headers) {
        Ok(Ok(t)) => t,
        Ok(Err(info)) => return info,
        Err(e) => return MediaInfo::failed(None, e.to_string()),
    };
    match sniff(&text) {
        // read_playlist answers anything else itself; should a page get
        // through, it is not a stream either.
        None | Some(MediaKind::Page) => MediaInfo { not_stream: true, ..MediaInfo::failed(None, NOT_A_STREAM.into()) },
        Some(MediaKind::Dash) => match dash::parse(&text, &base) {
            Ok(m) => dash::describe(&m),
            Err(e) => MediaInfo::failed(Some(MediaKind::Dash), e.to_string()),
        },
        Some(MediaKind::Hds) => match hds::parse(&text, &base) {
            Ok(m) => hds::describe(&m),
            Err(e) => {
                MediaInfo { drm: e == hds::HdsError::Drm, ..MediaInfo::failed(Some(MediaKind::Hds), e.to_string()) }
            }
        },
        Some(MediaKind::Hls) => match hls::parse(&text, &base, false) {
            Err(e) => {
                MediaInfo { drm: e == hls::HlsError::Drm, ..MediaInfo::failed(Some(MediaKind::Hls), e.to_string()) }
            }
            Ok(hls::Playlist::Media(track)) => MediaInfo {
                kind: Some(MediaKind::Hls),
                video: vec![MediaOption { id: String::new(), label: "Default".into(), ..MediaOption::default() }],
                duration: track.duration,
                ..MediaInfo::default()
            },
            Ok(hls::Playlist::Master(master)) => {
                // The length, from the best quality's playlist; it also shows
                // whether the segments are DRM protected.
                let best = &master.variants[0].url;
                let first = get_text(&agent, best, &headers_for(url, headers, best))
                    .map_err(|e| e.to_string())
                    .and_then(|(b, t)| hls::parse(&t, &b, false).map_err(|e| e.to_string()));
                match first {
                    Err(e) if e == DRM_REFUSED => MediaInfo { drm: true, ..MediaInfo::failed(Some(MediaKind::Hls), e) },
                    Ok(hls::Playlist::Media(t)) => hls::describe(&master, t.duration),
                    _ => hls::describe(&master, None),
                }
            }
        },
    }
}

/// One segment to fetch into one file.
struct Job {
    path: PathBuf,
    segment: Segment,
}

/// The name the video gets, without its extension: the request's file name,
/// or the playlist's.
pub(super) fn stem(req: &DownloadRequest) -> String {
    let name = req
        .filename
        .as_deref()
        .and_then(sanitize_filename)
        .unwrap_or_else(|| udm_domain::filename::name_from_url(&req.url))
        .into_string();
    let stem = Path::new(&name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(name);
    if stem.is_empty() {
        "video".into()
    } else {
        stem
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    id: DownloadId,
    token: &CancelToken,
    agent: &http::Client,
    req: &DownloadRequest,
    choice: &MediaChoice,
    reporter: &Reporter,
    shared: &Shared,
) -> Result<Saved, EngineError> {
    if choice.audio_only.is_some() && req.ffmpeg.is_none() {
        return Err(refused("Saving only the sound needs ffmpeg. Install it, or choose it in Options > Tools."));
    }
    let (main, extra, subtitles) = load_tracks(agent, req, choice)?;
    // For the sound alone, a separate audio track is all that is fetched.
    let (main, extra) = match (choice.audio_only, extra) {
        (Some(_), Some(audio)) => (audio, None),
        (_, extra) => (main, extra),
    };
    let subtitles = if choice.audio_only.is_some() { Vec::new() } else { subtitles };
    let stem = stem(req);
    let ext = match (choice.audio_only, &req.ffmpeg, main.ext) {
        (Some(target), _, _) => target.extension(),
        (None, Some(_), "webm") => "webm",
        (None, Some(_), _) => "mp4",
        (None, None, ext) => ext,
    };
    let filename = format!("{stem}.{ext}");
    // The folder is the one for videos (or music, for audio alone), even
    // when the file stays a .ts.
    let kind = if choice.audio_only.is_some() || matches!(main.ext, "m4a" | "aac") { "m4a" } else { "mp4" };
    let dir = req.folders.for_name(&format!("{stem}.{kind}"));
    fs::create_dir_all(&dir)?;
    let probe = http::Probe {
        final_url: req.url.clone(),
        total: None,
        resumable: false,
        etag: None,
        last_modified: None,
        suggested_filename: None,
        content_type: None,
        server: None,
    };
    let claim = choose_output(&shared.claimed, &dir, &filename, req, &probe)?;
    let output = claim.path.clone();
    let beside = parts_dir(&output);
    let parts = match (&req.reuse_partial, &req.temp_dir) {
        (Some(p), _) if p.is_dir() => p.clone(),
        (_, Some(dir)) => super::temp_name(dir, &output, ".udmparts"),
        _ => beside.clone(),
    };
    fs::create_dir_all(&parts)?;

    let mut jobs = Vec::new();
    let mut tracks = Vec::new();
    for (tag, track) in [("v", Some(&main)), ("a", extra.as_ref())] {
        let Some(track) = track else { continue };
        let first = jobs.len();
        if let Some(init) = &track.init {
            jobs.push(Job { path: parts.join(format!("{tag}-init")), segment: init.clone() });
        }
        for (i, s) in track.segments.iter().enumerate() {
            jobs.push(Job { path: parts.join(format!("{tag}-{i:06}")), segment: s.clone() });
        }
        tracks.push((parts.join(format!("{tag}.{}", track.ext)), first..jobs.len(), track.ext));
    }
    let mut subs = Vec::new();
    for (n, sub) in subtitles.iter().enumerate() {
        let first = jobs.len();
        for (i, s) in sub.segments.iter().enumerate() {
            jobs.push(Job { path: parts.join(format!("s{n}-{i:06}")), segment: s.clone() });
        }
        subs.push((parts.join(format!("s{n}.vtt")), first..jobs.len(), sub));
    }

    let workers = req.connections.clamp(1, MAX_CONNECTIONS).min(jobs.len());
    reporter.send(Progress::Started {
        id,
        url: req.url.clone(),
        path: output.clone(),
        total: None,
        resumable: true,
        connections: workers,
        partial: (parts != beside).then(|| parts.clone()),
        validator: None,
        country: None,
    });
    fetch_all(id, token, agent, req, &jobs, workers, reporter, shared)?;
    if token.load(Ordering::Relaxed) {
        return Ok(output.into());
    }

    // Only the main track can be HDS: its manifests have no separate audio.
    let headers = [main.hds_header.as_deref(), extra.as_ref().and_then(|t| t.hds_header.as_deref())];
    for ((joined, range, _), header) in tracks.iter().zip(headers) {
        join(&jobs[range.clone()], joined, header)?;
    }
    let subs: Vec<Joined> = subs
        .into_iter()
        .filter_map(|(joined, range, sub)| match join_subtitle(&jobs[range], &joined) {
            Ok(true) => Some(Joined { path: joined, label: &sub.label, language: sub.language.as_deref() }),
            Ok(false) => {
                tracing::warn!(subtitles = %sub.label, "skipping subtitles: not WebVTT");
                None
            }
            Err(e) => {
                tracing::warn!(subtitles = %sub.label, error = %e, "skipping subtitles");
                None
            }
        })
        .collect();
    let note = match (choice.audio_only, &req.ffmpeg) {
        (Some(target), Some(ffmpeg)) => {
            crate::media::convert::convert(ffmpeg, &tracks[0].0, &output, target, token, |_| {})
                .map_err(|e| EngineError::Io(e.to_string()))?;
            None
        }
        _ => finish(req, &tracks, &subs, &output, &stem)?,
    };
    let _ = fs::remove_dir_all(&parts);
    drop(claim);
    Ok(Saved { path: output, note, complete: true })
}

/// A subtitle track joined into one WebVTT file.
struct Joined<'a> {
    path: PathBuf,
    label: &'a str,
    language: Option<&'a str>,
}

/// Join a subtitle track's WebVTT segments into `joined`. `false` when they
/// are not WebVTT text (WebVTT in MP4 segments, say), which is skipped.
fn join_subtitle(jobs: &[Job], joined: &Path) -> Result<bool, EngineError> {
    let mut parts = Vec::with_capacity(jobs.len());
    for job in jobs {
        let text = String::from_utf8_lossy(&fs::read(&job.path)?).into_owned();
        if !text.trim_start_matches('\u{feff}').trim_start().starts_with("WEBVTT") {
            return Ok(false);
        }
        parts.push(text);
    }
    fs::write(joined, udm_domain::media::join_webvtt(&parts))?;
    Ok(true)
}

/// Where a subtitle track goes beside a video saved without ffmpeg:
/// `<stem>.<language>.vtt`, or a number when it has no language.
fn subtitle_name(stem: &str, sub: &Joined, n: usize) -> String {
    let tag = sub.language.and_then(udm_domain::media::language_code).unwrap_or_else(|| (n + 1).to_string());
    format!("{stem}.{tag}.vtt")
}

/// Make the output from the joined tracks: with ffmpeg one video with the
/// subtitles in it, without it the tracks themselves and the subtitles as
/// `.vtt` files beside them.
fn finish(
    req: &DownloadRequest,
    tracks: &[(PathBuf, std::ops::Range<usize>, &'static str)],
    subs: &[Joined],
    output: &Path,
    stem: &str,
) -> Result<Option<String>, EngineError> {
    let main = &tracks[0];
    if let Some(ffmpeg) = &req.ffmpeg {
        let mut cmd = Command::new(ffmpeg);
        cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
        for (joined, _, _) in tracks {
            cmd.arg("-i").arg(joined);
        }
        for sub in subs {
            cmd.arg("-i").arg(&sub.path);
        }
        if tracks.len() > 1 {
            cmd.args(["-map", "0:v:0?", "-map", "1:a:0?"]);
        } else if !subs.is_empty() {
            cmd.args(["-map", "0:v:0?", "-map", "0:a:0?"]);
        }
        for i in 0..subs.len() {
            cmd.arg("-map").arg(format!("{}:s:0", tracks.len() + i));
        }
        cmd.args(["-c", "copy"]);
        if !subs.is_empty() {
            // MP4 holds subtitles as timed text; WebM keeps WebVTT.
            let webm = output.extension().is_some_and(|e| e == "webm");
            cmd.args(["-c:s", if webm { "webvtt" } else { "mov_text" }]);
            for (i, sub) in subs.iter().enumerate() {
                let lang = if webm { udm_domain::media::language_code } else { udm_domain::media::iso639_2 };
                if let Some(lang) = sub.language.and_then(lang) {
                    cmd.arg(format!("-metadata:s:s:{i}")).arg(format!("language={lang}"));
                }
                cmd.arg(format!("-metadata:s:s:{i}")).arg(format!("title={}", sub.label));
            }
        }
        if output.extension().is_some_and(|e| e == "mp4") && tracks.iter().any(|t| matches!(t.2, "ts" | "aac")) {
            // ADTS AAC from MPEG-TS needs reframing to go in an MP4.
            cmd.args(["-bsf:a", "aac_adtstoasc"]);
        }
        let out = crate::platform::no_window(&mut cmd)
            .arg(output)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| EngineError::Io(format!("could not run ffmpeg ({}): {e}", ffmpeg.display())))?;
        if !out.status.success() {
            let _ = fs::remove_file(output);
            let why = String::from_utf8_lossy(&out.stderr);
            let why = why.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no reason given");
            return Err(EngineError::Io(format!("ffmpeg could not make the video: {why}")));
        }
        return Ok(None);
    }

    fs::rename(&main.0, output)?;
    let dir = output.parent().unwrap_or(Path::new("."));
    for (n, sub) in subs.iter().enumerate() {
        let name = subtitle_name(stem, sub, n);
        let to = free_name(dir, &name);
        fs::rename(&sub.path, to)?;
    }
    let Some((audio, _, ext)) = tracks.get(1) else {
        return Ok(match main.2 {
            "ts" => Some("Saved as a .ts video. With ffmpeg (set in Options) it would be an .mp4.".into()),
            "flv" => Some("Saved as an .flv video. With ffmpeg (set in Options) it would be an .mp4.".into()),
            _ => None,
        });
    };
    let name = if output.extension().is_some_and(|e| e == *ext) {
        format!("{stem} (audio).{ext}")
    } else {
        format!("{stem}.{ext}")
    };
    let audio_path = free_name(dir, &name);
    fs::rename(audio, &audio_path)?;
    Ok(Some(format!(
        "Saved as two files, video and audio ({}), because ffmpeg was not found. Install ffmpeg or set it in Options to get one video.",
        audio_path.file_name().unwrap_or_default().to_string_lossy()
    )))
}

/// The first of `name`, `name (1)`, `name (2)` and so on that `dir` does not
/// hold yet; `name` itself, to be replaced, if every number is taken.
fn free_name(dir: &Path, name: &str) -> PathBuf {
    (0..=u32::MAX).map(|k| dir.join(numbered_name(name, k))).find(|p| !p.exists()).unwrap_or_else(|| dir.join(name))
}

/// Concatenate the segment files of one track, in order. With an HDS
/// `header`, the file is that header followed by each F4F fragment's FLV
/// tags, which is what makes it an FLV video.
fn join(jobs: &[Job], joined: &Path, header: Option<&[u8]>) -> Result<(), EngineError> {
    let mut out = BufWriter::new(File::create(joined)?);
    if let Some(header) = header {
        out.write_all(header)?;
    }
    for job in jobs {
        let mut part = File::open(&job.path)?;
        if header.is_some() {
            hds::copy_mdat(&mut part, &mut out)?;
        } else {
            std::io::copy(&mut part, &mut out)?;
        }
    }
    out.flush()?;
    Ok(())
}

/// What the segment threads share.
struct Fetching<'a> {
    id: DownloadId,
    token: &'a CancelToken,
    agent: &'a http::Client,
    req: &'a DownloadRequest,
    shared: &'a Shared,
    reporter: &'a Reporter,
    keys: Mutex<HashMap<String, [u8; 16]>>,
    done_parts: AtomicU64,
    done_bytes: AtomicU64,
    total_parts: u64,
    last_report: Mutex<Instant>,
}

impl Fetching<'_> {
    fn report(&self, force: bool) {
        {
            let mut last = self.last_report.lock_or_recover();
            if !force && last.elapsed() < REPORT_INTERVAL {
                return;
            }
            *last = Instant::now();
        }
        self.reporter.send(Progress::Advanced {
            id: self.id,
            url: self.req.url.clone(),
            done: ByteCount::new(self.done_bytes.load(Ordering::Relaxed)),
            total: None,
            parts: Some([self.done_parts.load(Ordering::Relaxed), self.total_parts]),
            segments: None,
        });
    }

    /// The key at `url`, fetched once. The lock is held while fetching, so
    /// segments waiting for the same key do not each fetch it.
    fn key(&self, url: &str) -> Result<[u8; 16], EngineError> {
        let mut keys = self.keys.lock_or_recover();
        if let Some(k) = keys.get(url) {
            return Ok(*k);
        }
        let page = fetch_with(self.agent, url, &headers_for(&self.req.url, &self.req.headers, url), 64)?;
        let key: [u8; 16] = page
            .body
            .as_slice()
            .try_into()
            .map_err(|_| EngineError::Protocol(format!("the key at {url} is not 16 bytes")))?;
        keys.insert(url.to_string(), key);
        Ok(key)
    }

    /// Fetch one segment into its file, retrying with backoff.
    fn segment(&self, job: &Job) -> Result<(), EngineError> {
        let mut attempt = 0;
        loop {
            let mut counted = 0u64;
            match self.segment_once(job, &mut counted) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    self.done_bytes.fetch_sub(counted, Ordering::Relaxed);
                    if self.token.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    attempt += 1;
                    if e.is_permanent() || attempt > MAX_RETRIES {
                        return Err(e);
                    }
                    thread::sleep(backoff(attempt));
                }
            }
        }
    }

    fn segment_once(&self, job: &Job, counted: &mut u64) -> Result<(), EngineError> {
        let s = &job.segment;
        let headers = headers_for(&self.req.url, &self.req.headers, &s.url);
        let mut reader = match s.range {
            Some((a, b)) => http::fetch_range(self.agent, &s.url, &headers, a, b)?,
            None => http::fetch_whole(self.agent, &s.url, &headers)?,
        };
        let tmp = job.path.with_extension("tmp");
        let mut file = BufWriter::new(File::create(&tmp)?);
        let mut buf = vec![0u8; BUF_SIZE];
        let own = self.shared.own_limiter(self.id);
        let limiter = Throttle::new(&self.shared.limiter, &own);
        loop {
            if self.token.load(Ordering::Relaxed) {
                return Ok(());
            }
            let want = limiter.chunk_size(buf.len());
            let n = reader.read(&mut buf[..want])?;
            if n == 0 {
                break;
            }
            limiter.consume(n, self.token);
            file.write_all(&buf[..n])?;
            *counted += n as u64;
            self.done_bytes.fetch_add(n as u64, Ordering::Relaxed);
            self.report(false);
        }
        file.flush()?;
        drop(file);
        if let Some(key) = &s.key {
            let k = self.key(&key.url)?;
            // Decrypting reads the segment whole, and the server picks its
            // size: a huge one would take all the memory.
            if fs::metadata(&tmp)?.len() > MAX_ENCRYPTED_SEGMENT {
                return Err(EngineError::Protocol("an encrypted segment is too large to decrypt".into()));
            }
            let mut data = fs::read(&tmp)?;
            let plain = decrypt(&k, &key.iv, &mut data)?;
            fs::write(&tmp, plain)?;
        }
        fs::rename(&tmp, &job.path)?;
        Ok(())
    }
}

/// Decrypt an AES-128-CBC segment with PKCS#7 padding, in place.
pub(crate) fn decrypt<'a>(key: &[u8; 16], iv: &[u8; 16], data: &'a mut [u8]) -> Result<&'a [u8], EngineError> {
    use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
    cbc::Decryptor::<aes::Aes128>::new(key.into(), iv.into())
        .decrypt_padded_mut::<Pkcs7>(data)
        .map_err(|_| EngineError::Protocol("a segment did not decrypt; its key may be wrong".into()))
}

/// Fetch every job not already on disk, `workers` at a time.
#[allow(clippy::too_many_arguments)]
fn fetch_all(
    id: DownloadId,
    token: &CancelToken,
    agent: &http::Client,
    req: &DownloadRequest,
    jobs: &[Job],
    workers: usize,
    reporter: &Reporter,
    shared: &Shared,
) -> Result<(), EngineError> {
    let mut done = vec![false; jobs.len()];
    let (mut parts, mut bytes) = (0u64, 0u64);
    for (i, job) in jobs.iter().enumerate() {
        if let Ok(m) = fs::metadata(&job.path) {
            done[i] = true;
            parts += 1;
            bytes += m.len();
        }
    }
    let f = Fetching {
        id,
        token,
        agent,
        req,
        shared,
        reporter,
        keys: Mutex::new(HashMap::new()),
        done_parts: AtomicU64::new(parts),
        done_bytes: AtomicU64::new(bytes),
        total_parts: jobs.len() as u64,
        last_report: Mutex::new(Instant::now() - REPORT_INTERVAL),
    };
    f.report(true);
    let todo: Vec<usize> = (0..jobs.len()).filter(|i| !done[*i]).collect();
    let next = AtomicUsize::new(0);
    let first_error: Arc<Mutex<Option<EngineError>>> = Arc::new(Mutex::new(None));
    thread::scope(|scope| {
        for _ in 0..workers.max(1) {
            scope.spawn(|| loop {
                if token.load(Ordering::Relaxed) || first_error.lock_or_recover().is_some() {
                    return;
                }
                let Some(&i) = todo.get(next.fetch_add(1, Ordering::Relaxed)) else {
                    return;
                };
                match f.segment(&jobs[i]) {
                    Ok(()) if !token.load(Ordering::Relaxed) => {
                        f.done_parts.fetch_add(1, Ordering::Relaxed);
                        f.report(false);
                    }
                    Ok(()) => return,
                    Err(e) => {
                        first_error.lock_or_recover().get_or_insert(e);
                        return;
                    }
                }
            });
        }
    });
    if let Some(e) = first_error.lock_or_recover().take() {
        return Err(e);
    }
    f.report(true);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aes_128_round_trip() {
        use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
        let key = [7u8; 16];
        let iv = 3u128.to_be_bytes();
        let plain = b"a segment of an MPEG transport stream".to_vec();
        let mut buf = plain.clone();
        buf.resize(plain.len() + 16, 0);
        let enc = cbc::Encryptor::<aes::Aes128>::new(&key.into(), &iv.into())
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plain.len())
            .unwrap()
            .to_vec();
        let mut data = enc;
        assert_eq!(decrypt(&key, &iv, &mut data).unwrap(), plain.as_slice());
    }

    #[test]
    fn parts_folder_sits_next_to_the_output() {
        assert_eq!(parts_dir(Path::new(r"C:\dl\movie.mp4")), PathBuf::from(r"C:\dl\movie.mp4.udmparts"));
    }

    #[test]
    fn credentials_stay_on_the_added_origin() {
        let headers = vec![
            ("User-Agent".to_string(), "UDM".to_string()),
            ("Cookie".to_string(), "session=1".to_string()),
            ("authorization".to_string(), "Bearer t".to_string()),
        ];
        let added = "https://video.example/v/master.m3u8";
        for same in ["https://video.example/v/1.ts", "https://video.example:443/keys/k", "https://VIDEO.example/x"] {
            assert_eq!(headers_for(added, &headers, same).as_ref(), headers.as_slice(), "{same}");
        }
        for other in [
            "https://cdn.example/v/1.ts",
            "http://video.example/v/1.ts",
            "https://video.example:8443/v/1.ts",
            "https://sub.video.example/v/1.ts",
            "not a url",
        ] {
            let kept = headers_for(added, &headers, other);
            assert_eq!(kept.as_ref(), &headers[..1], "{other}");
        }
    }

    /// Whole streams against a local server.
    mod live {
        use super::super::super::testserver::TestServer;
        use super::super::super::{Engine, Progress};
        use super::*;
        use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
        use std::sync::mpsc;
        use udm_domain::media::MediaKind;
        use udm_domain::settings::{FileExists, Folders};

        fn temp_dir(name: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!("udm_stream_{}_{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn request(url: String, dir: &Path, choice: MediaChoice) -> DownloadRequest {
            DownloadRequest {
                url,
                folders: Folders::fixed(dir.to_path_buf()),
                filename: Some("Movie night".into()),
                headers: Vec::new(),
                connections: 3,
                reuse_path: None,
                file_exists: FileExists::Rename,
                post_body: None,
                net: Default::default(),
                media: Some(choice),
                ffmpeg: None,
                speed_limit: None,
                server_date: false,
                server_date_created: false,
                ignore_date: false,
                temp_dir: None,
                reuse_partial: None,
                host_limit: None,
                elevate: false,
                resolve: None,
                torrent_files: None,
                torrent_uploaded: ByteCount::ZERO,
            }
        }

        /// Where a download was saved and its note, or its error.
        type Outcome = Result<(PathBuf, Option<String>), Failed>;

        /// A download's failure, as its progress reported it.
        #[derive(Debug, PartialEq)]
        struct Failed(String);

        /// Run a download; the result and the segment counts it reported.
        fn run(req: DownloadRequest) -> (Outcome, Vec<[u64; 2]>) {
            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            let engine = Engine::new(
                Arc::new(move |p: Progress| {
                    let _ = tx.lock().unwrap().send(p);
                }),
                1,
            );
            engine.enqueue(req);
            let mut parts = Vec::new();
            loop {
                match rx.recv_timeout(Duration::from_secs(30)).expect("download timed out") {
                    Progress::Advanced { parts: Some(p), .. } => parts.push(p),
                    Progress::Finished { path, note, .. } => return (Ok((path, note)), parts),
                    Progress::Failed { error, .. } => return (Err(Failed(error)), parts),
                    Progress::Canceled { .. } => panic!("canceled"),
                    _ => {}
                }
            }
        }

        fn segment(i: usize) -> Vec<u8> {
            (0..40_000 + i * 1000).map(|b| ((b * 7 + i) % 256) as u8).collect()
        }

        fn encrypt(key: &[u8; 16], iv: &[u8; 16], plain: &[u8]) -> Vec<u8> {
            let mut buf = plain.to_vec();
            buf.resize(plain.len() + 16, 0);
            cbc::Encryptor::<aes::Aes128>::new(key.into(), iv.into())
                .encrypt_padded_mut::<Pkcs7>(&mut buf, plain.len())
                .unwrap()
                .to_vec()
        }

        /// A master playlist with two qualities; the 720p one has five
        /// segments, the last two encrypted.
        fn hls_site() -> (HashMap<String, Vec<u8>>, Vec<u8>) {
            let key = [9u8; 16];
            let mut files = HashMap::new();
            files.insert(
                "/v/master.m3u8".into(),
                b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=900000,RESOLUTION=640x360\nlow/index.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=3000000,RESOLUTION=1280x720\nhigh/index.m3u8\n".to_vec(),
            );
            let mut playlist = String::from("#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:0\n");
            let mut plain = Vec::new();
            for i in 0..5 {
                if i == 3 {
                    playlist.push_str("#EXT-X-KEY:METHOD=AES-128,URI=\"/keys/k\"\n");
                }
                playlist.push_str(&format!("#EXTINF:4,\n{i}.ts\n"));
                let data = segment(i);
                plain.extend_from_slice(&data);
                let body = if i >= 3 { encrypt(&key, &(i as u128).to_be_bytes(), &data) } else { data };
                files.insert(format!("/v/high/{i}.ts"), body);
            }
            playlist.push_str("#EXT-X-ENDLIST\n");
            files.insert("/v/high/index.m3u8".into(), playlist.into_bytes());
            files.insert("/v/low/index.m3u8".into(), b"#EXTM3U\n#EXTINF:4,\nonly.ts\n#EXT-X-ENDLIST\n".to_vec());
            files.insert("/v/low/only.ts".into(), b"low".to_vec());
            files.insert("/keys/k".into(), key.to_vec());
            (files, plain)
        }

        #[test]
        fn hls_joins_and_decrypts_segments() {
            let (files, plain) = hls_site();
            let server = TestServer::start_files(files);
            let dir = temp_dir("hls");
            let url = format!("{}/v/master.m3u8", server.url);
            let (result, parts) = run(request(url, &dir, MediaChoice::default()));
            let (path, note) = result.unwrap();
            assert_eq!(path, dir.join("Movie night.ts"), "the best quality, joined as .ts without ffmpeg");
            assert_eq!(fs::read(&path).unwrap(), plain);
            assert!(note.unwrap().contains("ffmpeg"));
            assert_eq!(parts.last(), Some(&[5, 5]));
            assert!(!parts_dir(&path).exists(), "the segments are cleaned up");

            // The other quality, by its id.
            let low = format!("{}/v/low/index.m3u8", server.url);
            let choice = MediaChoice { kind: Some(MediaKind::Hls), video: Some(low), ..MediaChoice::default() };
            let (result, _) = run(request(format!("{}/v/master.m3u8", server.url), &dir, choice));
            let (path, _) = result.unwrap();
            assert_eq!(path, dir.join("Movie night (1).ts"));
            assert_eq!(fs::read(&path).unwrap(), b"low");
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn the_sound_alone_needs_ffmpeg() {
            let dir = temp_dir("sound_only");
            let choice = MediaChoice {
                kind: Some(MediaKind::Hls),
                video: None,
                audio: None,
                audio_only: Some(udm_domain::media::Target::Mp3),
                subtitles: None,
                max_height: None,
            };
            // Refused before anything is fetched, so no server is needed.
            let (result, _) = run(request("http://127.0.0.1:1/master.m3u8".into(), &dir, choice));
            let Failed(err) = result.unwrap_err();
            assert!(err.contains("ffmpeg"), "{err}");
            fs::remove_dir_all(&dir).ok();
        }
        #[test]
        fn a_taken_name_under_ask_stops_before_any_segment() {
            let (files, _) = hls_site();
            let server = TestServer::start_files(files);
            let dir = temp_dir("ask");
            let existing = dir.join("Movie night.ts");
            fs::write(&existing, b"last week's movie").unwrap();
            let mut req = request(format!("{}/v/master.m3u8", server.url), &dir, MediaChoice::default());
            req.file_exists = FileExists::Ask;

            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            let engine = Engine::new(
                Arc::new(move |p: Progress| {
                    let _ = tx.lock().unwrap().send(p);
                }),
                1,
            );
            engine.enqueue(req);
            let taken = loop {
                match rx.recv_timeout(Duration::from_secs(30)).expect("download timed out") {
                    Progress::NameTaken { path, .. } => break path,
                    Progress::Advanced { .. } | Progress::Finished { .. } => panic!("the stream downloaded"),
                    Progress::Failed { error, .. } => panic!("failed: {error}"),
                    _ => {}
                }
            };
            assert_eq!(taken, existing);
            assert_eq!(fs::read(&existing).unwrap(), b"last week's movie");
            assert!(!parts_dir(&existing).exists(), "no segments fetched");
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn cookies_go_only_to_the_playlists_origin() {
            // Another server holds two of the segments and the key.
            let key = [5u8; 16];
            let mut cdn_files = HashMap::new();
            cdn_files.insert("/c/1.ts".to_string(), segment(1));
            cdn_files.insert("/c/2.ts".to_string(), encrypt(&key, &2u128.to_be_bytes(), &segment(2)));
            cdn_files.insert("/keys/k".to_string(), key.to_vec());
            let cdn = TestServer::start_files(cdn_files);

            let mut files = HashMap::new();
            files.insert(
                "/v/master.m3u8".to_string(),
                b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=900000\nindex.m3u8\n".to_vec(),
            );
            let playlist = format!(
                "#EXTM3U\n#EXTINF:4,\n0.ts\n#EXTINF:4,\n{c}/c/1.ts\n#EXT-X-KEY:METHOD=AES-128,URI=\"{c}/keys/k\"\n#EXTINF:4,\n{c}/c/2.ts\n#EXT-X-ENDLIST\n",
                c = cdn.url
            );
            files.insert("/v/index.m3u8".to_string(), playlist.into_bytes());
            files.insert("/v/0.ts".to_string(), segment(0));
            let site = TestServer::start_files(files);

            let dir = temp_dir("cookies");
            let mut req = request(format!("{}/v/master.m3u8", site.url), &dir, MediaChoice::default());
            req.headers =
                vec![("Cookie".into(), "session=secret".into()), ("Authorization".into(), "Bearer secret".into())];
            let (result, _) = run(req);
            let (path, _) = result.unwrap();
            let plain: Vec<u8> = (0..3).flat_map(segment).collect();
            assert_eq!(fs::read(&path).unwrap(), plain);

            let mut paths: Vec<String> = site.cookies_seen.lock().unwrap().iter().map(|(p, _)| p.clone()).collect();
            paths.sort();
            assert_eq!(paths, ["/v/0.ts", "/v/index.m3u8", "/v/master.m3u8"]);
            assert_eq!(site.auth_seen.lock().unwrap().len(), 3);
            assert!(cdn.cookies_seen.lock().unwrap().is_empty(), "no cookies to the other server");
            assert!(cdn.auth_seen.lock().unwrap().is_empty(), "no Authorization to the other server");
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn hls_resumes_segment_by_segment() {
            let (files, plain) = hls_site();
            let first_two = files["/v/high/0.ts"].len() + files["/v/high/1.ts"].len();
            let server = TestServer::start_files(files.clone());
            let dir = temp_dir("hls_resume");
            // An earlier run fetched the first two segments, and left the
            // third half written.
            let parts = parts_dir(&dir.join("Movie night.ts"));
            fs::create_dir_all(&parts).unwrap();
            fs::write(parts.join("v-000000"), &files["/v/high/0.ts"]).unwrap();
            fs::write(parts.join("v-000001"), &files["/v/high/1.ts"]).unwrap();
            fs::write(parts.join("v-000002.tmp"), b"half").unwrap();

            let url = format!("{}/v/master.m3u8", server.url);
            let (result, reported) = run(request(url, &dir, MediaChoice::default()));
            let (path, _) = result.unwrap();
            assert_eq!(fs::read(&path).unwrap(), plain);
            assert_eq!(reported.first(), Some(&[2, 5]), "progress starts from the segments on disk");
            let fetched_segments: usize = (2..5).map(|i| files[&format!("/v/high/{i}.ts")].len()).sum();
            let served = server.served.load(Ordering::Relaxed) as usize;
            let playlists = files["/v/master.m3u8"].len() + files["/v/high/index.m3u8"].len() + 16;
            assert_eq!(
                served,
                fetched_segments + playlists,
                "only the missing segments, not the {first_two} bytes on disk"
            );
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn dash_without_ffmpeg_saves_two_files() {
            let mut files = HashMap::new();
            files.insert(
                "/m/manifest.mpd".into(),
                br#"<MPD type="static" mediaPresentationDuration="PT8S"><Period>
                  <AdaptationSet mimeType="video/mp4">
                    <SegmentTemplate timescale="1" duration="4" initialization="$RepresentationID$-init.mp4" media="$RepresentationID$-$Number$.m4s"/>
                    <Representation id="v1" bandwidth="1000000" height="720"/>
                  </AdaptationSet>
                  <AdaptationSet mimeType="audio/mp4">
                    <Representation id="a1" bandwidth="64000">
                      <SegmentList><Initialization sourceURL="a.m4a" range="0-9"/>
                        <SegmentURL media="a.m4a" mediaRange="10-19"/><SegmentURL media="a.m4a" mediaRange="20-29"/></SegmentList>
                    </Representation>
                  </AdaptationSet></Period></MPD>"#
                    .to_vec(),
            );
            files.insert("/m/v1-init.mp4".into(), b"VINIT".to_vec());
            files.insert("/m/v1-1.m4s".into(), b"V1".to_vec());
            files.insert("/m/v1-2.m4s".into(), b"V2".to_vec());
            files.insert("/m/a.m4a".into(), b"AAAAAAAAAABBBBBBBBBBCCCCCCCCCC".to_vec());
            let server = TestServer::start_files(files);
            let dir = temp_dir("dash");
            let (result, parts) = run(request(format!("{}/m/manifest.mpd", server.url), &dir, MediaChoice::default()));
            let (path, note) = result.unwrap();
            assert_eq!(path, dir.join("Movie night.mp4"));
            assert_eq!(fs::read(&path).unwrap(), b"VINITV1V2");
            assert_eq!(fs::read(dir.join("Movie night.m4a")).unwrap(), b"AAAAAAAAAABBBBBBBBBBCCCCCCCCCC");
            assert!(note.unwrap().contains("two files"));
            assert_eq!(parts.last(), Some(&[6, 6]));
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn drm_streams_are_refused_and_probed() {
            let mut files = HashMap::new();
            files.insert("/d/master.m3u8".into(), b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nv.m3u8\n".to_vec());
            files.insert(
                "/d/v.m3u8".into(),
                b"#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://k\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\n#EXTINF:4,\n0.ts\n#EXT-X-ENDLIST\n".to_vec(),
            );
            let server = TestServer::start_files(files);
            let dir = temp_dir("drm");
            let url = format!("{}/d/master.m3u8", server.url);
            let (result, _) = run(request(url.clone(), &dir, MediaChoice::default()));
            assert_eq!(result.unwrap_err(), Failed(DRM_REFUSED.into()));
            let info = probe(&url, &[], &NetConfig::default());
            assert!(info.drm);
            assert_eq!(info.error.as_deref(), Some(DRM_REFUSED));

            let (files, _) = hls_site();
            let server = TestServer::start_files(files);
            let info = probe(&format!("{}/v/master.m3u8", server.url), &[], &NetConfig::default());
            assert_eq!(info.kind, Some(MediaKind::Hls));
            assert!(!info.drm);
            assert_eq!(info.duration, Some(20.0));
            let labels: Vec<&str> = info.video.iter().map(|o| o.label.as_str()).collect();
            assert_eq!(labels, ["720p, 3.0 Mbps", "360p, 900 kbps"]);
            assert_eq!(info.video[0].size, Some(ByteCount::new(7_500_000)));
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn a_file_under_a_playlist_name_is_probed_as_a_file() {
            let mut mp4 = b"\0\0\0\x20ftypisom".to_vec();
            mp4.resize(300_000, 7);
            let mut files = HashMap::new();
            files.insert("/f/movie.m3u8".into(), mp4);
            files.insert("/f/page.m3u8".into(), b"<html>no</html>".to_vec());
            let server = TestServer::start_files(files);
            let info = probe(&format!("{}/f/movie.m3u8", server.url), &[], &NetConfig::default());
            assert_eq!(info.file, Some(MediaFile { ext: "mp4".into(), size: Some(ByteCount::new(300_000)) }));
            assert_eq!(info.error, None);
            let info = probe(&format!("{}/f/page.m3u8", server.url), &[], &NetConfig::default());
            assert!(info.not_stream);
            assert_eq!(info.file, None);
        }

        /// A stand-in for ffmpeg: it records its arguments next to itself
        /// and writes the output file, or fails with a message.
        /// A stand-in for ffmpeg that notes its arguments in `args.txt`
        /// and writes "video" to the last one, or fails like ffmpeg.
        #[cfg(windows)]
        fn fake_ffmpeg(dir: &Path, fail: bool) -> PathBuf {
            let bat = dir.join("ffmpeg.bat");
            let body = if fail {
                "@echo off\r\necho Invalid data found when processing input 1>&2\r\nexit /b 1\r\n".to_string()
            } else {
                "@echo off\r\necho %* > \"%~dp0args.txt\"\r\nfor %%a in (%*) do set last=%%a\r\necho video> %last%\r\n"
                    .to_string()
            };
            fs::write(&bat, body).unwrap();
            bat
        }

        #[cfg(unix)]
        fn fake_ffmpeg(dir: &Path, fail: bool) -> PathBuf {
            use std::os::unix::fs::PermissionsExt;
            let script = dir.join("ffmpeg");
            let body = if fail {
                "#!/bin/sh\necho Invalid data found when processing input >&2\nexit 1\n".to_string()
            } else {
                "#!/bin/sh\necho \"$@\" > \"$(dirname \"$0\")/args.txt\"\nfor last; do :; done\necho video > \"$last\"\n".to_string()
            };
            fs::write(&script, body).unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
            script
        }

        fn dash_site() -> HashMap<String, Vec<u8>> {
            let mut files = HashMap::new();
            files.insert(
                "/m/manifest.mpd".into(),
                br#"<MPD type="static" mediaPresentationDuration="PT8S"><Period>
                  <AdaptationSet mimeType="video/mp4"><Representation id="v" bandwidth="1">
                    <SegmentTemplate timescale="1" duration="4" media="v-$Number$.m4s"/></Representation></AdaptationSet>
                  <AdaptationSet mimeType="audio/mp4"><Representation id="a" bandwidth="1"><BaseURL>a.m4a</BaseURL></Representation></AdaptationSet>
                  </Period></MPD>"#
                    .to_vec(),
            );
            files.insert("/m/v-1.m4s".into(), b"V1".to_vec());
            files.insert("/m/v-2.m4s".into(), b"V2".to_vec());
            files.insert("/m/a.m4a".into(), b"A".to_vec());
            files
        }

        /// An HDS stream with two bit rates: the manifest (signed with a
        /// query, as Akamai's are), inline bootstrap info, metadata, and two
        /// F4F fragments for the higher one. Also the FLV it makes.
        fn hds_site() -> (HashMap<String, Vec<u8>>, Vec<u8>) {
            use crate::media::hds::tests::{abst, fragment};
            use base64::Engine as _;
            let b64 = |d: &[u8]| base64::engine::general_purpose::STANDARD.encode(d);
            let meta = b"\x02\x00\x0aonMetaData\x08\x00\x00\x00\x00".to_vec();
            let manifest = format!(
                r#"<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns="http://ns.adobe.com/f4m/1.0">
  <duration>8</duration>
  <bootstrapInfo profile="named" id="b">{boot}</bootstrapInfo>
  <media url="low_" bitrate="300" height="240" bootstrapInfoId="b"/>
  <media url="high_" bitrate="1200" height="720" bootstrapInfoId="b"><metadata>{meta}</metadata></media>
</manifest>"#,
                boot = b64(&abst(false, &[(1, 2)], 1)),
                meta = b64(&meta)
            );
            let tags = [segment(1), segment(2)];
            let mut files = HashMap::new();
            files.insert("/h/manifest.f4m?hdnea=tok".to_string(), manifest.into_bytes());
            files.insert("/h/high_Seg1-Frag1?hdnea=tok".to_string(), fragment(&tags[0]));
            files.insert("/h/high_Seg1-Frag2?hdnea=tok".to_string(), fragment(&tags[1]));
            let mut flv = hds::flv_header(Some(&meta));
            flv.extend_from_slice(&tags[0]);
            flv.extend_from_slice(&tags[1]);
            (files, flv)
        }

        #[test]
        fn hds_fragments_become_an_flv() {
            let (files, flv) = hds_site();
            let server = TestServer::start_files(files);
            let dir = temp_dir("hds");
            let url = format!("{}/h/manifest.f4m?hdnea=tok", server.url);

            let info = probe(&url, &[], &NetConfig::default());
            assert_eq!(info.kind, Some(MediaKind::Hds));
            let labels: Vec<&str> = info.video.iter().map(|o| o.label.as_str()).collect();
            assert_eq!(labels, ["720p, 1.2 Mbps", "240p, 300 kbps"]);
            assert_eq!(info.video[0].size, Some(ByteCount::new(1_200_000)));

            let (result, parts) = run(request(url.clone(), &dir, MediaChoice::default()));
            let (path, note) = result.unwrap();
            assert_eq!(path, dir.join("Movie night.flv"));
            let saved = fs::read(&path).unwrap();
            assert_eq!(saved, flv, "the FLV header, the metadata, then each fragment's tags");
            assert_eq!(udm_domain::media::sniff_file(&saved), Some("flv"));
            assert!(note.unwrap().contains(".flv"));
            assert_eq!(parts.last(), Some(&[2, 2]));
            assert!(!parts_dir(&path).exists());

            // An F4M 2.0 manifest pointing at that one gives the same video.
            let mut files = hds_site().0;
            files.insert(
                "/h/set.f4m?hdnea=tok".into(),
                br#"<manifest xmlns="http://ns.adobe.com/f4m/2.0"><media href="manifest.f4m" bitrate="1200"/></manifest>"#.to_vec(),
            );
            let set = TestServer::start_files(files);
            let (result, _) = run(request(format!("{}/h/set.f4m?hdnea=tok", set.url), &dir, MediaChoice::default()));
            assert_eq!(fs::read(result.unwrap().0).unwrap(), flv);

            // With ffmpeg it is remuxed into an MP4.
            let tools = temp_dir("hds_tools");
            let mut req = request(url, &dir, MediaChoice::default());
            req.ffmpeg = Some(fake_ffmpeg(&tools, false));
            let (result, _) = run(req);
            let (path, note) = result.unwrap();
            assert_eq!((path.file_name().unwrap().to_str().unwrap(), note), ("Movie night.mp4", None));
            let args = fs::read_to_string(tools.join("args.txt")).unwrap();
            assert!(args.contains("v.flv") && args.contains("-c copy"), "{args}");
            fs::remove_dir_all(&dir).ok();
            fs::remove_dir_all(&tools).ok();
        }

        /// An HLS stream with English (the default) and German subtitles,
        /// each two WebVTT segments, and a French track in MP4 segments.
        fn hls_subtitle_site() -> HashMap<String, Vec<u8>> {
            let mut files = HashMap::new();
            files.insert(
                "/s/master.m3u8".into(),
                concat!(
                    "#EXTM3U\n",
                    "#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",LANGUAGE=\"en-US\",DEFAULT=YES,AUTOSELECT=YES,URI=\"en/index.m3u8\"\n",
                    "#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"Deutsch\",LANGUAGE=\"de\",URI=\"de/index.m3u8\"\n",
                    "#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"Francais\",LANGUAGE=\"fr\",URI=\"fr/index.m3u8\"\n",
                    "#EXT-X-STREAM-INF:BANDWIDTH=900000,RESOLUTION=640x360,SUBTITLES=\"subs\"\n",
                    "v/index.m3u8\n"
                )
                .as_bytes()
                .to_vec(),
            );
            files.insert("/s/v/index.m3u8".into(), b"#EXTM3U\n#EXTINF:4,\n0.ts\n#EXT-X-ENDLIST\n".to_vec());
            files.insert("/s/v/0.ts".into(), b"VIDEO".to_vec());
            for (lang, first, second) in [("en", "Hello", "World"), ("de", "Hallo", "Welt")] {
                files.insert(
                    format!("/s/{lang}/index.m3u8"),
                    b"#EXTM3U\n#EXTINF:4,\n0.vtt\n#EXTINF:4,\n1.vtt\n#EXT-X-ENDLIST\n".to_vec(),
                );
                let map = "X-TIMESTAMP-MAP=MPEGTS:900000,LOCAL:00:00:00.000";
                files.insert(
                    format!("/s/{lang}/0.vtt"),
                    format!("WEBVTT\n{map}\n\n00:00:01.000 --> 00:00:02.000\n{first}\n").into_bytes(),
                );
                files.insert(
                    format!("/s/{lang}/1.vtt"),
                    format!("WEBVTT\n{map}\n\n00:00:05.000 --> 00:00:06.000\n{second}\n").into_bytes(),
                );
            }
            files.insert("/s/fr/index.m3u8".into(), b"#EXTM3U\n#EXTINF:4,\n0.mp4\n#EXT-X-ENDLIST\n".to_vec());
            files.insert("/s/fr/0.mp4".into(), b"\x00\x00\x00\x18ftypiso6".to_vec());
            files
        }

        #[test]
        fn hls_subtitles_are_saved_beside_the_video_without_ffmpeg() {
            let server = TestServer::start_files(hls_subtitle_site());
            let dir = temp_dir("hls_subs");
            let url = format!("{}/s/master.m3u8", server.url);
            let (result, _) = run(request(url.clone(), &dir, MediaChoice::default()));
            let (path, _) = result.unwrap();
            assert_eq!(fs::read(&path).unwrap(), b"VIDEO");
            assert_eq!(
                fs::read_to_string(dir.join("Movie night.en.vtt")).unwrap(),
                "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHello\n\n00:00:05.000 --> 00:00:06.000\nWorld\n",
                "the default track, its segments joined"
            );
            assert!(!dir.join("Movie night.de.vtt").exists(), "only the default without a choice");

            // German by choice; French is WebVTT in MP4, which is skipped
            // without failing the download.
            let ids = vec![format!("{}/s/de/index.m3u8", server.url), format!("{}/s/fr/index.m3u8", server.url)];
            let choice = MediaChoice { subtitles: Some(ids), ..MediaChoice::default() };
            let (result, _) = run(request(url.clone(), &dir, choice));
            result.unwrap();
            assert!(fs::read_to_string(dir.join("Movie night.de.vtt")).unwrap().contains("Hallo"));
            assert!(!dir.join("Movie night.fr.vtt").exists());

            // None at all.
            let choice = MediaChoice { subtitles: Some(Vec::new()), ..MediaChoice::default() };
            let before = fs::read_dir(&dir).unwrap().count();
            let (result, _) = run(request(url, &dir, choice));
            result.unwrap();
            assert_eq!(fs::read_dir(&dir).unwrap().count(), before + 1, "only the video");
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn ffmpeg_puts_subtitles_in_the_video() {
            let server = TestServer::start_files(hls_subtitle_site());
            let dir = temp_dir("ffmpeg_subs");
            let tools = temp_dir("ffmpeg_subs_tools");
            let ids = vec![format!("{}/s/en/index.m3u8", server.url), format!("{}/s/de/index.m3u8", server.url)];
            let choice = MediaChoice { subtitles: Some(ids), ..MediaChoice::default() };
            let mut req = request(format!("{}/s/master.m3u8", server.url), &dir, choice);
            req.ffmpeg = Some(fake_ffmpeg(&tools, false));
            let (result, _) = run(req);
            let (path, _) = result.unwrap();
            assert_eq!(path, dir.join("Movie night.mp4"));
            assert!(!dir.join("Movie night.en.vtt").exists(), "in the video, not beside it");
            // Windows quotes arguments with `=` in them.
            let args = fs::read_to_string(tools.join("args.txt")).unwrap().replace('"', "");
            assert!(args.contains("s0.vtt") && args.contains("s1.vtt"), "{args}");
            assert!(args.contains("-map 0:v:0? -map 0:a:0? -map 1:s:0 -map 2:s:0 -c copy -c:s mov_text"), "{args}");
            assert!(args.contains("-metadata:s:s:0 language=eng -metadata:s:s:0 title=English"), "{args}");
            assert!(args.contains("-metadata:s:s:1 language=deu"), "{args}");
            fs::remove_dir_all(&dir).ok();
            fs::remove_dir_all(&tools).ok();
        }

        #[test]
        fn dash_webvtt_subtitles_are_offered_and_saved() {
            let mut files = dash_site();
            files.insert(
                "/m/manifest.mpd".into(),
                br#"<MPD type="static" mediaPresentationDuration="PT8S"><Period>
                  <AdaptationSet mimeType="video/mp4"><Representation id="v" bandwidth="1">
                    <SegmentTemplate timescale="1" duration="4" media="v-$Number$.m4s"/></Representation></AdaptationSet>
                  <AdaptationSet contentType="text" mimeType="text/vtt" lang="es"><Label>Espanol</Label><Role schemeIdUri="urn:mpeg:dash:role:2011" value="main"/>
                    <Representation id="sub-es" bandwidth="1"><BaseURL>es.vtt</BaseURL></Representation></AdaptationSet>
                  <AdaptationSet contentType="text" mimeType="application/ttml+xml" lang="it">
                    <Representation id="sub-it" bandwidth="1"><BaseURL>it.ttml</BaseURL></Representation></AdaptationSet>
                  </Period></MPD>"#
                    .to_vec(),
            );
            files.insert("/m/es.vtt".into(), b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHola\n".to_vec());
            let server = TestServer::start_files(files);
            let url = format!("{}/m/manifest.mpd", server.url);
            let info = probe(&url, &[], &NetConfig::default());
            assert_eq!(info.subtitles.len(), 1, "TTML is left out");
            assert_eq!(info.subtitles[0].id, "sub-es");
            assert_eq!(info.subtitles[0].label, "Espanol");
            assert!(info.subtitles[0].default);

            let dir = temp_dir("dash_subs");
            let (result, _) = run(request(url, &dir, MediaChoice::default()));
            result.unwrap();
            assert!(fs::read_to_string(dir.join("Movie night.es.vtt")).unwrap().contains("Hola"));
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn ffmpeg_joins_video_and_audio() {
            let server = TestServer::start_files(dash_site());
            let dir = temp_dir("ffmpeg");
            let tools = temp_dir("ffmpeg_tools");
            let mut req = request(format!("{}/m/manifest.mpd", server.url), &dir, MediaChoice::default());
            req.ffmpeg = Some(fake_ffmpeg(&tools, false));
            let (result, _) = run(req);
            let (path, note) = result.unwrap();
            assert_eq!(path, dir.join("Movie night.mp4"));
            assert_eq!(note, None);
            assert!(fs::read_to_string(&path).unwrap().starts_with("video"));
            assert!(!dir.join("Movie night.m4a").exists(), "one file");
            let args = fs::read_to_string(tools.join("args.txt")).unwrap();
            assert!(args.contains("v.mp4") && args.contains("a.m4a"), "{args}");
            assert!(args.contains("-map 0:v:0? -map 1:a:0? -c copy"), "{args}");
            assert!(!parts_dir(&path).exists());

            // ffmpeg failing fails the download and keeps the segments for a
            // retry.
            let mut req = request(format!("{}/m/manifest.mpd", server.url), &dir, MediaChoice::default());
            req.ffmpeg = Some(fake_ffmpeg(&tools, true));
            let (result, _) = run(req);
            let Failed(error) = result.unwrap_err();
            assert!(error.contains("ffmpeg could not make the video: Invalid data"), "{error}");
            assert!(parts_dir(&dir.join("Movie night (1).mp4")).join("v-000000").exists());
            fs::remove_dir_all(&dir).ok();
            fs::remove_dir_all(&tools).ok();
        }
    }
}
