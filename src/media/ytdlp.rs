//! Videos on pages that name no video file: YouTube. Its player streams over
//! SABR, POST requests whose bodies the browser never shows as a video, a
//! playlist or a manifest, so there is nothing to catch. yt-dlp reads the
//! page instead: it lists the qualities (`yt-dlp -J`) and downloads the one
//! chosen, with ffmpeg joining the video and its sound.
//!
//! yt-dlp is found on `PATH`, next to UIDM's programs, or next to ffmpeg.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

use super::{estimate_size, quality_label, MediaChoice, MediaInfo, MediaKind, MediaOption, LIVE_REFUSED};

/// Why a page cannot be read without yt-dlp, in words for the user.
pub const MISSING: &str =
    "YouTube videos need yt-dlp. Install it (winget install yt-dlp.yt-dlp) or put yt-dlp.exe next to UIDM, then restart UIDM.";

/// Whether `url` is a video page yt-dlp downloads for UIDM: a YouTube
/// watch page, a Short or a youtu.be link.
pub fn supports(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else { return false };
    if !matches!(u.scheme(), "http" | "https") {
        return false;
    }
    let host = u.host_str().unwrap_or("").to_ascii_lowercase();
    let path = u.path();
    match host.as_str() {
        "youtu.be" => path.len() > 1,
        "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com" => {
            (path == "/watch" && u.query_pairs().any(|(k, v)| k == "v" && !v.is_empty()))
                || ["/shorts/", "/live/", "/embed/"].iter().any(|p| path.starts_with(p) && path.len() > p.len())
        }
        _ => false,
    }
}

/// yt-dlp: the first on `PATH`, else next to UIDM's programs, else next to
/// `ffmpeg`.
pub fn find(ffmpeg: Option<&Path>) -> Option<PathBuf> {
    let name = crate::platform::exe_name("yt-dlp");
    let on_path = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default();
    let beside = [
        std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)),
        ffmpeg.and_then(|f| f.parent().map(Path::to_path_buf)),
    ];
    on_path.into_iter().chain(beside.into_iter().flatten()).map(|dir| dir.join(&name)).find(|p| p.is_file())
}

/// Arguments every yt-dlp run gets. yt-dlp needs a JavaScript runtime for
/// YouTube and looks only for Deno unless told otherwise, so Node is named
/// when it is there and Deno is not.
fn common_args(cmd: &mut Command, proxy: Option<&str>) {
    cmd.args(["--no-playlist", "--no-warnings", "--no-color", "--ignore-config"]);
    let on_path = |name: &str| {
        std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(crate::platform::exe_name(name)).is_file()))
    };
    if !on_path("deno") && on_path("node") {
        cmd.args(["--js-runtimes", "node"]);
    }
    if let Some(proxy) = proxy {
        cmd.args(["--proxy", proxy]);
    }
}

/// The yt-dlp command for `url`, ready for the arguments that differ.
pub fn command(yt_dlp: &Path, proxy: Option<&str>) -> Command {
    let mut cmd = Command::new(yt_dlp);
    crate::platform::no_window(&mut cmd);
    common_args(&mut cmd, proxy);
    cmd.stdin(Stdio::null());
    cmd
}

/// The last thing yt-dlp complained about in `stderr`, made readable.
pub fn complaint(stderr: &str) -> String {
    let line = stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with("ERROR:"))
        .or_else(|| stderr.lines().rev().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("yt-dlp failed");
    let line = line.trim_start_matches("ERROR:").trim();
    format!("yt-dlp: {line} If this keeps happening, update yt-dlp (yt-dlp -U).")
}

/// What the page at `url` offers, read by yt-dlp.
pub fn probe(yt_dlp: &Path, url: &str, proxy: Option<&str>) -> MediaInfo {
    let out = command(yt_dlp, proxy).args(["-J", "--"]).arg(url).stdout(Stdio::piped()).stderr(Stdio::piped()).output();
    let out = match out {
        Ok(o) => o,
        Err(e) => return MediaInfo::failed(Some(MediaKind::Page), format!("could not run yt-dlp ({}): {e}", yt_dlp.display())),
    };
    if !out.status.success() {
        return MediaInfo::failed(Some(MediaKind::Page), complaint(&String::from_utf8_lossy(&out.stderr)));
    }
    match serde_json::from_slice::<Value>(&out.stdout) {
        Ok(v) => describe(&v),
        Err(e) => MediaInfo::failed(Some(MediaKind::Page), format!("yt-dlp gave an answer UIDM cannot read: {e}")),
    }
}

/// The parts of one format in yt-dlp's `formats` list that matter here.
struct Format<'a> {
    id: &'a str,
    height: Option<u32>,
    fps: Option<f64>,
    /// Kilobits per second.
    tbr: Option<f64>,
    size: Option<u64>,
    video: bool,
    audio: bool,
    vcodec: &'a str,
    acodec: &'a str,
    language: Option<&'a str>,
    note: &'a str,
}

impl<'a> Format<'a> {
    fn read(f: &'a Value) -> Option<Self> {
        let s = |k: &str| f.get(k).and_then(Value::as_str);
        let n = |k: &str| f.get(k).and_then(Value::as_f64);
        let id = s("format_id")?;
        let vcodec = s("vcodec").unwrap_or("none");
        let acodec = s("acodec").unwrap_or("none");
        // Storyboards (thumbnails), manifests and DRM protected formats are
        // not downloads.
        let protocol = s("protocol").unwrap_or("");
        if f.get("has_drm").and_then(Value::as_bool) == Some(true) || protocol.contains("m3u8") || protocol == "mhtml" {
            return None;
        }
        Some(Format {
            id,
            height: n("height").filter(|h| *h > 0.0).map(|h| h as u32),
            fps: n("fps"),
            tbr: n("tbr").or_else(|| n("vbr")).or_else(|| n("abr")),
            size: n("filesize").or_else(|| n("filesize_approx")).filter(|s| *s > 0.0).map(|s| s as u64),
            video: vcodec != "none",
            audio: acodec != "none",
            vcodec,
            acodec,
            language: s("language").filter(|l| !l.is_empty()),
            note: s("format_note").unwrap_or(""),
        })
    }

    /// How well a video codec plays everywhere once in an MP4: H.264 first.
    fn video_rank(&self) -> u8 {
        match self.vcodec.split('.').next().unwrap_or("") {
            "avc1" | "h264" => 0,
            "vp9" | "vp09" => 1,
            "av01" => 2,
            _ => 3,
        }
    }

    /// AAC first, since it goes in an MP4 or an M4A as it is.
    fn audio_rank(&self) -> u8 {
        u8::from(!self.acodec.starts_with("mp4a"))
    }

    fn bits(&self) -> u64 {
        (self.tbr.unwrap_or(0.0) * 1000.0) as u64
    }
}

/// What a page offers, from yt-dlp's `-J` answer: one video option per
/// height (the most compatible codec, then the highest bit rate), best first,
/// and the sound per language, best first.
pub fn describe(v: &Value) -> MediaInfo {
    let page = Some(MediaKind::Page);
    if v.get("is_live").and_then(Value::as_bool) == Some(true) || v.get("live_status").and_then(Value::as_str) == Some("is_upcoming") {
        return MediaInfo::failed(page, LIVE_REFUSED.into());
    }
    let duration = v.get("duration").and_then(Value::as_f64);
    let all: Vec<Format> = v.get("formats").and_then(Value::as_array).into_iter().flatten().filter_map(Format::read).collect();
    if all.is_empty() {
        let drm = v.get("formats").and_then(Value::as_array).is_some_and(|f| !f.is_empty());
        let error = if drm { super::DRM_REFUSED } else { "yt-dlp found no video on this page." };
        return MediaInfo {
            drm,
            ..MediaInfo::failed(page, error.into())
        };
    }

    // The sound: the best of each language, the original language first.
    let mut sounds: Vec<&Format> = all.iter().filter(|f| f.audio && !f.video).collect();
    sounds.sort_by(|a, b| a.audio_rank().cmp(&b.audio_rank()).then(b.bits().cmp(&a.bits())));
    let mut audio: Vec<(&Format, MediaOption)> = Vec::new();
    for f in sounds {
        if audio.iter().any(|(g, _)| g.language == f.language) {
            continue;
        }
        let label = match f.language {
            Some(lang) if f.note.contains("original") => format!("{lang} (original)"),
            Some(lang) => lang.to_string(),
            None => quality_label(None, Some(f.bits())),
        };
        let size = f.size.or_else(|| estimate_size(Some(f.bits()), duration));
        let option = MediaOption {
            id: f.id.to_string(),
            label,
            bandwidth: Some(f.bits()).filter(|b| *b > 0),
            size,
            ..MediaOption::default()
        };
        audio.push((f, option));
    }
    audio.sort_by_key(|(f, _)| !f.note.contains("original"));
    let sound_size = audio.first().and_then(|(_, o)| o.size);

    // Video alone when YouTube offers it (every quality above 360p);
    // otherwise the formats that carry their own sound.
    let only: Vec<&Format> = all.iter().filter(|f| f.video && !f.audio).collect();
    let mut pictures = if only.is_empty() { all.iter().filter(|f| f.video).collect() } else { only };
    pictures.sort_by(|a, b| {
        b.height
            .cmp(&a.height)
            .then(a.video_rank().cmp(&b.video_rank()))
            .then(b.fps.unwrap_or(0.0).total_cmp(&a.fps.unwrap_or(0.0)))
            .then(b.bits().cmp(&a.bits()))
    });
    pictures.dedup_by_key(|f| f.height);
    let video = pictures
        .into_iter()
        .map(|f| {
            let mut label = quality_label(f.height, Some(f.bits()).filter(|b| *b > 0));
            if let (Some(h), Some(fps)) = (f.height, f.fps.filter(|r| *r > 30.0)) {
                label = label.replacen(&format!("{h}p"), &format!("{h}p{}", fps.round() as u32), 1);
            }
            let own = f.size.or_else(|| estimate_size(Some(f.bits()), duration));
            let sound = if f.audio { Some(0) } else { sound_size };
            MediaOption {
                id: f.id.to_string(),
                label,
                height: f.height,
                bandwidth: Some(f.bits()).filter(|b| *b > 0),
                size: own.zip(sound).map(|(a, b)| a + b).or(own),
            }
        })
        .collect();
    MediaInfo {
        kind: page,
        video,
        audio: audio.into_iter().map(|(_, o)| o).collect(),
        duration,
        ..MediaInfo::default()
    }
}

/// yt-dlp's `-f` for `choice`. Each pick falls back, so a format that went
/// away since the menu was made still gives a video: the chosen video with
/// the chosen (or best) sound, then the best of both. Without ffmpeg nothing
/// can be joined, so it is the best format that has both in one file.
pub fn format_spec(choice: &MediaChoice, ffmpeg: bool) -> String {
    let sound = match &choice.audio {
        Some(a) => format!("{a}/bestaudio[ext=m4a]/bestaudio"),
        None => "bestaudio[ext=m4a]/bestaudio".to_string(),
    };
    if choice.audio_only.is_some() {
        return format!("{sound}/best");
    }
    if !ffmpeg {
        return "best[ext=mp4]/best".to_string();
    }
    let audio_part = match &choice.audio {
        Some(a) => a.clone(),
        None => "bestaudio[ext=m4a]".to_string(),
    };
    match &choice.video {
        Some(v) => format!("{v}+{audio_part}/{v}+bestaudio/{v}/bestvideo*+bestaudio/best"),
        None => format!("bestvideo*[vcodec^=avc1]+{audio_part}/bestvideo*+bestaudio/best"),
    }
}

/// A progress line yt-dlp prints with `PROGRESS_TEMPLATE`: bytes done and
/// the format's size, when known.
pub fn progress_line(line: &str) -> Option<(u64, Option<u64>)> {
    let mut parts = line.strip_prefix(PROGRESS_TAG)?.split_whitespace();
    let num = |s: Option<&str>| s.and_then(|s| s.parse::<f64>().ok()).filter(|n| *n >= 0.0).map(|n| n as u64);
    let done = num(parts.next())?;
    let total = num(parts.next()).or_else(|| num(parts.next()));
    Some((done, total.filter(|t| *t > 0)))
}

/// What starts the lines `progress_line` reads, and the line naming the
/// finished file.
pub const PROGRESS_TAG: &str = "UIDM-PROGRESS ";
pub const DONE_TAG: &str = "UIDM-FILE ";
pub const PROGRESS_TEMPLATE: &str =
    "download:UIDM-PROGRESS %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s";

/// `path` as a yt-dlp output template: `%` means something there.
pub fn literal_template(path: &str) -> String {
    path.replace('%', "%%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn youtube_video_pages_and_nothing_else() {
        for url in [
            "https://www.youtube.com/watch?v=jNQXAC9IVRw",
            "https://m.youtube.com/watch?v=jNQXAC9IVRw&t=4",
            "https://youtube.com/shorts/abc123",
            "https://youtu.be/jNQXAC9IVRw",
            "https://music.youtube.com/watch?v=x",
        ] {
            assert!(supports(url), "{url}");
        }
        for url in [
            "https://www.youtube.com/",
            "https://www.youtube.com/watch",
            "https://www.youtube.com/@jawed",
            "https://youtu.be/",
            "https://notyoutube.com/watch?v=x",
            "https://rr1---sn-x.googlevideo.com/videoplayback?mime=video%2Fmp4",
            "file:///C:/watch?v=x",
        ] {
            assert!(!supports(url), "{url}");
        }
    }

    fn sample() -> Value {
        serde_json::json!({
            "duration": 100.0,
            "formats": [
                {"format_id": "sb0", "vcodec": "none", "acodec": "none", "protocol": "mhtml"},
                {"format_id": "139", "vcodec": "none", "acodec": "mp4a.40.5", "abr": 48.0, "language": "en", "format_note": "low, original (default)"},
                {"format_id": "140", "vcodec": "none", "acodec": "mp4a.40.2", "abr": 128.0, "filesize": 1_600_000, "language": "en", "format_note": "medium, original (default)"},
                {"format_id": "251", "vcodec": "none", "acodec": "opus", "abr": 140.0, "language": "en", "format_note": "medium, original (default)"},
                {"format_id": "140-1", "vcodec": "none", "acodec": "mp4a.40.2", "abr": 128.0, "language": "es", "format_note": "Spanish"},
                {"format_id": "18", "vcodec": "avc1.42001E", "acodec": "mp4a.40.2", "height": 360, "tbr": 500.0},
                {"format_id": "137", "vcodec": "avc1.640028", "acodec": "none", "height": 1080, "fps": 30.0, "tbr": 4000.0, "filesize": 50_000_000},
                {"format_id": "248", "vcodec": "vp9", "acodec": "none", "height": 1080, "fps": 30.0, "tbr": 2500.0},
                {"format_id": "313", "vcodec": "vp9", "acodec": "none", "height": 2160, "fps": 60.0, "tbr": 18000.0},
                {"format_id": "hls-1", "vcodec": "avc1", "acodec": "mp4a", "height": 720, "protocol": "m3u8_native"}
            ]
        })
    }

    #[test]
    fn describes_qualities_best_first() {
        let info = describe(&sample());
        assert_eq!(info.kind, Some(MediaKind::Page));
        assert!(info.error.is_none());
        let ids: Vec<&str> = info.video.iter().map(|o| o.id.as_str()).collect();
        // One per height; H.264 wins at 1080p; the 360p with its own sound
        // is left out because video-only formats exist.
        assert_eq!(ids, ["313", "137"]);
        assert_eq!(info.video[0].label, "2160p60, 18.0 Mbps");
        assert_eq!(info.video[1].size, Some(51_600_000));
        let audio: Vec<&str> = info.audio.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(audio, ["140", "140-1"]);
        assert_eq!(info.duration, Some(100.0));
    }

    #[test]
    fn live_and_empty_pages_are_refused() {
        let live = describe(&serde_json::json!({"is_live": true, "formats": []}));
        assert_eq!(live.error.as_deref(), Some(LIVE_REFUSED));
        let drm = describe(&serde_json::json!({"formats": [{"format_id": "1", "vcodec": "avc1", "acodec": "none", "has_drm": true}]}));
        assert!(drm.drm);
    }

    #[test]
    fn format_specs_fall_back() {
        let pick = |video: Option<&str>, audio: Option<&str>| MediaChoice {
            kind: Some(MediaKind::Page),
            video: video.map(String::from),
            audio: audio.map(String::from),
            audio_only: None,
        };
        assert_eq!(format_spec(&pick(Some("137"), None), true), "137+bestaudio[ext=m4a]/137+bestaudio/137/bestvideo*+bestaudio/best");
        assert_eq!(format_spec(&pick(Some("137"), Some("140-1")), true), "137+140-1/137+bestaudio/137/bestvideo*+bestaudio/best");
        assert_eq!(format_spec(&pick(Some("137"), None), false), "best[ext=mp4]/best");
        let sound = MediaChoice {
            audio_only: Some(super::super::convert::Target::Mp3),
            ..pick(None, Some("140"))
        };
        assert_eq!(format_spec(&sound, true), "140/bestaudio[ext=m4a]/bestaudio/best");
    }

    #[test]
    fn reads_progress_and_complaints() {
        assert_eq!(progress_line("UIDM-PROGRESS 1024 4096 NA"), Some((1024, Some(4096))));
        assert_eq!(progress_line("UIDM-PROGRESS 1024 NA 8000.5"), Some((1024, Some(8000))));
        assert_eq!(progress_line("UIDM-PROGRESS 10 NA NA"), Some((10, None)));
        assert_eq!(progress_line("[download] 10%"), None);
        let why = complaint("WARNING: x\nERROR: [youtube] abc: Video unavailable\n");
        assert!(why.starts_with("yt-dlp: [youtube] abc: Video unavailable"), "{why}");
        assert_eq!(literal_template(r"C:\100% real\a"), r"C:\100%% real\a");
    }
}
