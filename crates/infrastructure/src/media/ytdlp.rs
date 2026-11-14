//! Videos on pages that name no video file: YouTube, Facebook, Instagram
//! and Vimeo, and any other page yt-dlp can read when the browser saw no
//! stream on it (the extension asks for those with `MediaKind::Page`). YouTube's player streams over SABR, POST requests whose bodies
//! the browser never shows as a video, a playlist or a manifest, so there is
//! nothing to catch; the others fetch signed pieces, or read their streams
//! from a player configuration, that only the page leads to. yt-dlp reads
//! the page instead: it lists the qualities (`yt-dlp -J`) and downloads the
//! one chosen, with ffmpeg joining the video and its sound.
//!
//! A playlist or channel page lists its videos instead (`--flat-playlist`,
//! so each video's page is not read yet), and the window picks among them.
//!
//! yt-dlp is found in UDM's tools folder (Options > Tools installs it
//! there), on `PATH`, next to UDM's programs, or next to ffmpeg.

use std::io::Read as _;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use udm_domain::bytes::ByteCount;

use serde_json::Value;

use udm_domain::media::{
    estimate_size, quality_label, MediaChoice, MediaInfo, MediaKind, MediaOption, Playlist, PlaylistEntry, Target,
    LIVE_REFUSED, MAX_PLAYLIST_ENTRIES,
};

/// Why a page cannot be read without yt-dlp, in words for the user.
pub const MISSING: &str =
    "Videos on YouTube, Facebook, Instagram, Vimeo and other sites need yt-dlp. Install it in UDM's Options > Tools, or put yt-dlp next to UDM.";

/// How long reading a page with yt-dlp may take before it is given up: a
/// little less than the extension waits for the answer (60 seconds).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(55);

/// Whether an address on a site's host is one video.
type VideoPage = fn(&url::Url) -> bool;

/// The sites whose video pages go to yt-dlp, by host; each takes the
/// address's path and query and says whether it is one video. The
/// extension's `VIDEO_SITES` (media.js) records the same pages.
const SITES: &[(&[&str], VideoPage)] = &[
    (&["youtu.be"], |u| u.path().len() > 1),
    (&["youtube.com", "www.youtube.com", "m.youtube.com", "music.youtube.com"], youtube),
    (&["facebook.com", "www.facebook.com", "m.facebook.com", "web.facebook.com"], facebook),
    (&["fb.watch"], |u| segments(u).first().is_some_and(|c| !c.is_empty())),
    (&["instagram.com", "www.instagram.com"], instagram),
    (&["vimeo.com", "www.vimeo.com"], vimeo),
    (&["player.vimeo.com"], |u| matches!(segments(u).as_slice(), ["video", id] if digits(id))),
];

/// The path's parts, without empty ones from a trailing slash.
fn segments(u: &url::Url) -> Vec<&str> {
    u.path().split('/').filter(|s| !s.is_empty()).collect()
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// A watch page, a Short, a live video or an embedded player.
fn youtube(u: &url::Url) -> bool {
    let path = u.path();
    (path == "/watch" && u.query_pairs().any(|(k, v)| k == "v" && !v.is_empty()))
        || ["/shorts/", "/live/", "/embed/"].iter().any(|p| path.starts_with(p) && path.len() > p.len())
}

/// `/<page>/videos/<id>`, `/watch?v=<id>` and `/reel/<id>`.
fn facebook(u: &url::Url) -> bool {
    match segments(u).as_slice() {
        ["watch"] => u.query_pairs().any(|(k, v)| k == "v" && digits(&v)),
        ["reel", id, ..] => digits(id),
        [_, "videos", rest @ ..] => rest.iter().any(|s| digits(s)),
        _ => false,
    }
}

/// A post, reel or IGTV video: `/p/<code>`, `/reel/<code>`, `/reels/<code>`,
/// `/tv/<code>`.
fn instagram(u: &url::Url) -> bool {
    matches!(segments(u).as_slice(), ["p" | "reel" | "reels" | "tv", code, ..] if !code.is_empty())
}

/// `/<id>` (maybe with an unlisted video's hash after it) and
/// `/channels/<name>/<id>`.
fn vimeo(u: &url::Url) -> bool {
    match segments(u).as_slice() {
        [id] | [id, _] => digits(id),
        ["channels", _, id] => digits(id),
        _ => false,
    }
}

/// Whether `url` is a video page yt-dlp downloads for UDM: a YouTube
/// watch page, Short or youtu.be link, or a Facebook, Instagram or Vimeo
/// video.
pub fn supports(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else { return false };
    if !matches!(u.scheme(), "http" | "https") {
        return false;
    }
    let host = u.host_str().unwrap_or("").to_ascii_lowercase();
    SITES.iter().any(|(hosts, matches)| hosts.contains(&host.as_str()) && matches(&u))
}

/// yt-dlp: the one UDM installed, else the first on `PATH`, else next
/// to UDM's programs, else next to `ffmpeg`.
pub fn find(ffmpeg: Option<&Path>) -> Option<PathBuf> {
    let name = crate::platform::exe_name("yt-dlp");
    let own = crate::paths::tools_dir();
    let on_path = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default();
    let on_path = own.into_iter().chain(on_path);
    let beside = [
        std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)),
        ffmpeg.and_then(|f| f.parent().map(Path::to_path_buf)),
    ];
    on_path.into_iter().chain(beside.into_iter().flatten()).map(|dir| dir.join(&name)).find(|p| p.is_file())
}

/// Arguments every yt-dlp run gets. yt-dlp needs a JavaScript runtime for
/// YouTube and looks only for Deno unless told otherwise, so Node is named
/// when it is there and Deno is not. `source` is the address of the network
/// adapter downloads are bound to, which yt-dlp connects from.
fn common_args(cmd: &mut Command, proxy: Option<&str>, source: Option<IpAddr>) {
    cmd.args(["--no-playlist", "--no-warnings", "--no-color", "--ignore-config"]);
    let on_path = |name: &str| {
        std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(crate::platform::exe_name(name)).is_file()))
    };
    if !on_path("deno") && on_path("node") {
        cmd.args(["--js-runtimes", "node"]);
    }
    if let Some(proxy) = proxy {
        cmd.args(["--proxy", proxy]);
    }
    if let Some(source) = source {
        cmd.arg("--source-address").arg(source.to_string());
    }
}

/// The yt-dlp command for `url`, ready for the arguments that differ.
pub fn command(yt_dlp: &Path, proxy: Option<&str>, source: Option<IpAddr>) -> Command {
    let mut cmd = Command::new(yt_dlp);
    crate::platform::no_window(&mut cmd);
    common_args(&mut cmd, proxy, source);
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

/// What the page at `url` offers, read by yt-dlp: its video's qualities,
/// or a playlist's videos. Given up after `timeout`.
pub fn probe(yt_dlp: &Path, url: &str, proxy: Option<&str>, source: Option<IpAddr>, timeout: Duration) -> MediaInfo {
    let failed = |why: String| MediaInfo::failed(Some(MediaKind::Page), why);
    let child = command(yt_dlp, proxy, source)
        .args(["-J", "--flat-playlist", "--playlist-end", &MAX_PLAYLIST_ENTRIES.to_string(), "--"])
        .arg(url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return failed(format!("could not run yt-dlp ({}): {e}", yt_dlp.display())),
    };
    // Read both pipes aside, so a big answer cannot fill one and stall it.
    let pipe = |p: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(mut p) = p {
                let _ = p.read_to_end(&mut out);
            }
            out
        })
    };
    let stdout = pipe(child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>));
    let stderr = pipe(child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= timeout => {
                kill_tree(&mut child);
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return failed(format!("yt-dlp: {e}")),
        }
    };
    let Some(status) = status else {
        // A process yt-dlp started may still hold the pipes; their readers
        // end with it.
        return failed(format!("yt-dlp did not answer within {} seconds.", timeout.as_secs()));
    };
    let (out, err) = (stdout.join().unwrap_or_default(), stderr.join().unwrap_or_default());
    if !status.success() {
        return failed(complaint(&String::from_utf8_lossy(&err)));
    }
    match serde_json::from_slice::<Value>(&out) {
        Ok(v) => describe(&v),
        Err(e) => failed(format!("yt-dlp gave an answer UDM cannot read: {e}")),
    }
}

/// End yt-dlp and the processes it started: yt-dlp.exe unpacks itself into
/// a second process, and ffmpeg runs under it while joining.
pub fn kill_tree(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        let _ = crate::platform::no_window(&mut Command::new("taskkill"))
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
}

/// A playlist's videos from yt-dlp's flat answer: each entry's page and
/// title, without the ones that are no web page.
fn playlist(v: &Value) -> Playlist {
    let s = |e: &Value, k: &str| e.get(k).and_then(Value::as_str).map(str::to_string);
    let entries = v
        .get("entries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let url =
                [s(e, "url"), s(e, "webpage_url")].into_iter().flatten().find(|u| udm_domain::batch::is_web_url(u))?;
            Some(PlaylistEntry {
                url,
                title: s(e, "title").unwrap_or_default(),
                duration: e.get("duration").and_then(Value::as_f64),
            })
        })
        .collect();
    Playlist { title: s(v, "title").unwrap_or_default(), entries }.cleaned()
}

/// The subtitle tracks yt-dlp lists (the uploader's own, not the
/// automatic ones), as menu options: each language's id and name.
fn subtitles(v: &Value) -> Vec<MediaOption> {
    let Some(subs) = v.get("subtitles").and_then(Value::as_object) else { return Vec::new() };
    let mut out: Vec<MediaOption> = subs
        .iter()
        .filter(|(lang, _)| safe_language(lang) && lang.as_str() != "live_chat")
        .map(|(lang, tracks)| {
            let name = tracks.as_array().and_then(|t| t.iter().find_map(|t| t.get("name").and_then(Value::as_str)));
            MediaOption {
                id: lang.clone(),
                label: name.filter(|n| !n.trim().is_empty()).unwrap_or(lang).to_string(),
                language: Some(lang.clone()),
                ..MediaOption::default()
            }
        })
        .collect();
    out.sort_by_key(|o| o.label.to_lowercase());
    out
}

/// Whether a subtitle language id is safe to hand yt-dlp, which reads
/// `--sub-langs` as patterns: letters, digits, `-` and `_` only.
fn safe_language(lang: &str) -> bool {
    !lang.is_empty() && lang.len() <= 32 && lang.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// yt-dlp's arguments for the subtitles `choice` keeps: only the languages
/// its probe listed and that are safe as patterns, each matched whole.
/// With ffmpeg they go in the video; without, beside it.
pub fn subtitle_args(choice: &MediaChoice, ffmpeg: bool) -> Vec<String> {
    if choice.audio_only.is_some() {
        return Vec::new();
    }
    let langs: Vec<String> =
        choice.subtitles.iter().flatten().filter(|l| safe_language(l)).take(64).map(|l| format!("^{l}$")).collect();
    if langs.is_empty() {
        return Vec::new();
    }
    let mut args = vec![
        "--write-subs".to_string(),
        "--sub-langs".to_string(),
        langs.join(","),
        "--sub-format".to_string(),
        "vtt/srt/best".to_string(),
    ];
    if ffmpeg {
        args.push("--embed-subs".to_string());
    }
    args
}

/// The parts of one format in yt-dlp's `formats` list that matter here.
struct Format<'a> {
    id: &'a str,
    height: Option<u32>,
    fps: Option<f64>,
    /// Kilobits per second.
    tbr: Option<f64>,
    size: Option<ByteCount>,
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
            size: n("filesize").or_else(|| n("filesize_approx")).filter(|s| *s > 0.0).map(|s| ByteCount::new(s as u64)),
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
    if matches!(v.get("_type").and_then(Value::as_str), Some("playlist" | "multi_video")) {
        let list = playlist(v);
        if list.entries.is_empty() {
            return MediaInfo::failed(page, "yt-dlp found no video on this page.".into());
        }
        return MediaInfo { kind: page, playlist: Some(list), ..MediaInfo::default() };
    }
    if v.get("is_live").and_then(Value::as_bool) == Some(true)
        || v.get("live_status").and_then(Value::as_str) == Some("is_upcoming")
    {
        return MediaInfo::failed(page, LIVE_REFUSED.into());
    }
    let duration = v.get("duration").and_then(Value::as_f64);
    let all: Vec<Format> =
        v.get("formats").and_then(Value::as_array).into_iter().flatten().filter_map(Format::read).collect();
    if all.is_empty() {
        let drm = v.get("formats").and_then(Value::as_array).is_some_and(|f| !f.is_empty());
        let error = if drm { udm_domain::media::DRM_REFUSED } else { "yt-dlp found no video on this page." };
        return MediaInfo { drm, ..MediaInfo::failed(page, error.into()) };
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
            let sound = if f.audio { Some(ByteCount::ZERO) } else { sound_size };
            MediaOption {
                id: f.id.to_string(),
                label,
                height: f.height,
                bandwidth: Some(f.bits()).filter(|b| *b > 0),
                size: own.zip(sound).map(|(a, b)| a + b).or(own),
                ..MediaOption::default()
            }
        })
        .collect();
    MediaInfo {
        kind: page,
        video,
        audio: audio.into_iter().map(|(_, o)| o).collect(),
        subtitles: subtitles(v),
        duration,
        ..MediaInfo::default()
    }
}

/// yt-dlp's `-f` for `choice`. Each pick falls back, so a format that went
/// away since the menu was made still gives a video: the chosen video with
/// the chosen (or best) sound, then the best of both. Without ffmpeg nothing
/// can be joined, so it is the best format that has both in one file.
pub fn format_spec(choice: &MediaChoice, ffmpeg: bool) -> String {
    // Opus keeps the site's Opus sound as it is; FLAC starts from the best.
    let preferred = match choice.audio_only {
        Some(Target::Opus) => "bestaudio[acodec=opus]/bestaudio",
        Some(Target::Flac) => "bestaudio",
        _ => "bestaudio[ext=m4a]/bestaudio",
    };
    let sound = match &choice.audio {
        Some(a) => format!("{a}/{preferred}"),
        None => preferred.to_string(),
    };
    if choice.audio_only.is_some() {
        return format!("{sound}/best");
    }
    // A playlist's quality: no taller than this, else the smallest there is.
    let tall =
        choice.max_height.filter(|_| choice.video.is_none()).map(|h| format!("[height<={h}]")).unwrap_or_default();
    if !ffmpeg {
        return if tall.is_empty() {
            "best[ext=mp4]/best".to_string()
        } else {
            format!("best[ext=mp4]{tall}/best{tall}/worst")
        };
    }
    let audio_part = match &choice.audio {
        Some(a) => a.clone(),
        None => "bestaudio[ext=m4a]".to_string(),
    };
    match &choice.video {
        Some(v) => format!("{v}+{audio_part}/{v}+bestaudio/{v}/bestvideo*+bestaudio/best"),
        None if !tall.is_empty() => format!(
            "bestvideo*{tall}[vcodec^=avc1]+{audio_part}/bestvideo*{tall}+bestaudio/best{tall}/worstvideo*+bestaudio/worst"
        ),
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
pub const PROGRESS_TAG: &str = "UDM-PROGRESS ";
pub const DONE_TAG: &str = "UDM-FILE ";
pub const PROGRESS_TEMPLATE: &str =
    "download:UDM-PROGRESS %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s";

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

    #[test]
    fn facebook_instagram_and_vimeo_video_pages() {
        for url in [
            "https://www.facebook.com/NASA/videos/1234567890/",
            "https://m.facebook.com/NASA/videos/vb.11/1234567890/?type=3",
            "https://www.facebook.com/watch/?v=1234567890",
            "https://www.facebook.com/watch?v=1234567890",
            "https://www.facebook.com/reel/987654321",
            "https://fb.watch/aBc-12/",
            "https://www.instagram.com/p/CxYz_12-a/",
            "https://instagram.com/reel/CxYz12/",
            "https://www.instagram.com/reels/CxYz12/",
            "https://www.instagram.com/tv/CxYz12",
            "https://vimeo.com/76979871",
            "https://vimeo.com/76979871/a1b2c3d4e5",
            "https://vimeo.com/channels/staffpicks/76979871",
            "https://player.vimeo.com/video/76979871?h=abc123",
        ] {
            assert!(supports(url), "{url}");
        }
        for url in [
            "https://www.facebook.com/",
            "https://www.facebook.com/NASA",
            "https://www.facebook.com/NASA/videos/",
            "https://www.facebook.com/watch/?v=abc",
            "https://fb.watch/",
            "https://www.instagram.com/nasa/",
            "https://www.instagram.com/explore/",
            "https://vimeo.com/",
            "https://vimeo.com/staff",
            "https://vimeo.com/channels/staffpicks",
            "https://player.vimeo.com/video/abc",
            "https://notvimeo.com/76979871",
            "https://evil.net/www.facebook.com/x/videos/1",
            "ftp://vimeo.com/76979871",
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
        assert_eq!(info.video[1].size, Some(ByteCount::new(51_600_000)));
        let audio: Vec<&str> = info.audio.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(audio, ["140", "140-1"]);
        assert_eq!(info.duration, Some(100.0));
    }

    #[test]
    fn live_and_empty_pages_are_refused() {
        let live = describe(&serde_json::json!({"is_live": true, "formats": []}));
        assert_eq!(live.error.as_deref(), Some(LIVE_REFUSED));
        let drm = describe(
            &serde_json::json!({"formats": [{"format_id": "1", "vcodec": "avc1", "acodec": "none", "has_drm": true}]}),
        );
        assert!(drm.drm);
    }

    #[test]
    fn format_specs_fall_back() {
        let pick = |video: Option<&str>, audio: Option<&str>| MediaChoice {
            kind: Some(MediaKind::Page),
            video: video.map(String::from),
            audio: audio.map(String::from),
            audio_only: None,
            subtitles: None,
            max_height: None,
        };
        assert_eq!(
            format_spec(&pick(Some("137"), None), true),
            "137+bestaudio[ext=m4a]/137+bestaudio/137/bestvideo*+bestaudio/best"
        );
        assert_eq!(
            format_spec(&pick(Some("137"), Some("140-1")), true),
            "137+140-1/137+bestaudio/137/bestvideo*+bestaudio/best"
        );
        assert_eq!(format_spec(&pick(Some("137"), None), false), "best[ext=mp4]/best");
        let sound = MediaChoice { audio_only: Some(udm_domain::media::Target::Mp3), ..pick(None, Some("140")) };
        assert_eq!(format_spec(&sound, true), "140/bestaudio[ext=m4a]/bestaudio/best");
    }

    #[test]
    fn reads_progress_and_complaints() {
        assert_eq!(progress_line("UDM-PROGRESS 1024 4096 NA"), Some((1024, Some(4096))));
        assert_eq!(progress_line("UDM-PROGRESS 1024 NA 8000.5"), Some((1024, Some(8000))));
        assert_eq!(progress_line("UDM-PROGRESS 10 NA NA"), Some((10, None)));
        assert_eq!(progress_line("[download] 10%"), None);
        let why = complaint("WARNING: x\nERROR: [youtube] abc: Video unavailable\n");
        assert!(why.starts_with("yt-dlp: [youtube] abc: Video unavailable"), "{why}");
        assert_eq!(literal_template(r"C:\100% real\a"), r"C:\100%% real\a");
    }

    #[test]
    fn a_playlist_lists_its_videos() {
        let v = serde_json::json!({
            "_type": "playlist",
            "title": "Talks",
            "entries": [
                {"_type": "url", "url": "https://www.youtube.com/watch?v=a1", "title": "One", "duration": 61.0},
                {"_type": "url", "url": "b2", "webpage_url": "https://www.youtube.com/watch?v=b2", "title": "Two"},
                {"_type": "url", "url": "javascript:x", "title": "Bad"},
                {"_type": "url", "url": "https://www.youtube.com/watch?v=a1", "title": "One again"}
            ]
        });
        let info = describe(&v);
        assert!(info.error.is_none() && info.video.is_empty());
        let list = info.playlist.unwrap();
        assert_eq!(list.title, "Talks");
        let urls: Vec<&str> = list.entries.iter().map(|e| e.url.as_str()).collect();
        assert_eq!(urls, ["https://www.youtube.com/watch?v=a1", "https://www.youtube.com/watch?v=b2"]);
        assert_eq!(list.entries[0].duration, Some(61.0));
        let empty = describe(&serde_json::json!({"_type": "playlist", "entries": []}));
        assert!(empty.error.is_some() && empty.playlist.is_none());
    }

    #[test]
    fn subtitles_are_offered_and_asked_for_safely() {
        let mut v = sample();
        v["subtitles"] = serde_json::json!({
            "en": [{"ext": "vtt", "name": "English"}],
            "de-AT": [{"ext": "vtt"}],
            "live_chat": [{"ext": "json"}],
            "x.*": [{"ext": "vtt"}]
        });
        let info = describe(&v);
        let ids: Vec<&str> = info.subtitles.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, ["de-AT", "en"]);
        assert_eq!(info.subtitles[1].label, "English");
        let choice = |subs: &[&str]| MediaChoice {
            kind: Some(MediaKind::Page),
            subtitles: Some(subs.iter().map(|s| s.to_string()).collect()),
            ..MediaChoice::default()
        };
        assert_eq!(
            subtitle_args(&choice(&["en", "x.*", "de-AT"]), true),
            ["--write-subs", "--sub-langs", "^en$,^de-AT$", "--sub-format", "vtt/srt/best", "--embed-subs"]
        );
        assert_eq!(subtitle_args(&choice(&["en"]), false).last().map(String::as_str), Some("vtt/srt/best"));
        assert!(subtitle_args(&choice(&[]), true).is_empty());
        let sound = MediaChoice { audio_only: Some(Target::Mp3), ..choice(&["en"]) };
        assert!(subtitle_args(&sound, true).is_empty());
    }

    #[test]
    fn a_playlists_quality_and_the_sound_formats() {
        let tall = MediaChoice { kind: Some(MediaKind::Page), max_height: Some(720), ..MediaChoice::default() };
        assert_eq!(
            format_spec(&tall, true),
            "bestvideo*[height<=720][vcodec^=avc1]+bestaudio[ext=m4a]/bestvideo*[height<=720]+bestaudio/best[height<=720]/worstvideo*+bestaudio/worst"
        );
        assert_eq!(format_spec(&tall, false), "best[ext=mp4][height<=720]/best[height<=720]/worst");
        let opus = MediaChoice { audio_only: Some(Target::Opus), ..MediaChoice::default() };
        assert_eq!(format_spec(&opus, true), "bestaudio[acodec=opus]/bestaudio/best");
        let flac = MediaChoice { audio_only: Some(Target::Flac), ..MediaChoice::default() };
        assert_eq!(format_spec(&flac, true), "bestaudio/best");
    }

    /// A fake yt-dlp: a script that prints canned JSON, or sleeps.
    #[cfg(unix)]
    fn fake_yt_dlp(name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("udm_fake_ytdlp_{}_{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("yt-dlp");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn any_page_is_read_by_the_program_and_slow_ones_give_up() {
        let json = r#"{"duration": 5, "formats": [{"format_id": "1", "vcodec": "avc1", "acodec": "mp4a", "height": 480, "tbr": 900}]}"#;
        let ok = fake_yt_dlp("ok", &format!("cat <<'EOF'\n{json}\nEOF"));
        let info = probe(&ok, "https://videos.example/watch/1", None, None, Duration::from_secs(20));
        assert!(info.error.is_none(), "{:?}", info.error);
        assert_eq!(info.video[0].height, Some(480));

        let slow = fake_yt_dlp("slow", "sleep 30");
        let started = Instant::now();
        let info = probe(&slow, "https://videos.example/watch/2", None, None, Duration::from_millis(300));
        assert!(info.error.unwrap().contains("did not answer"));
        assert!(started.elapsed() < Duration::from_secs(10));

        let fails = fake_yt_dlp("fails", "echo 'ERROR: Unsupported URL: https://x/' >&2; exit 1");
        let info = probe(&fails, "https://x/", None, None, Duration::from_secs(20));
        assert!(info.error.unwrap().starts_with("yt-dlp: Unsupported URL"));
        for p in [ok, slow, fails] {
            let _ = std::fs::remove_dir_all(p.parent().unwrap());
        }
    }
}
