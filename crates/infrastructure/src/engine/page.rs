//! Downloading a video page with yt-dlp (see `media::ytdlp`): yt-dlp fetches
//! the chosen video and sound and ffmpeg joins them, while UDM reads its
//! progress lines and stops it on cancel.
//!
//! yt-dlp keeps `.part` files of what it fetched, so a canceled download
//! carries on where it stopped when it is started again.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use udm_domain::bytes::ByteCount;
use udm_domain::id::DownloadId;
use udm_domain::sync::LockExt;

use super::{
    choose_output, http, stream, CancelToken, DownloadRequest, EngineError, Progress, Reporter, Saved, Shared,
};
use crate::media::ytdlp::{self, DONE_TAG, PROGRESS_TEMPLATE};
use udm_domain::media::MediaChoice;

/// What yt-dlp's estimate of the whole download starts with.
const SIZE_TAG: &str = "UDM-SIZE ";

/// How far yt-dlp got. It fetches the video and then the sound, each from 0,
/// so bytes of finished files are kept apart.
#[derive(Default)]
struct Tally {
    earlier: u64,
    done: u64,
    size: Option<u64>,
    total: Option<u64>,
}

impl Tally {
    /// Take a progress line's `done` and `size` of the current file.
    fn update(&mut self, done: u64, size: Option<u64>) {
        if done < self.done {
            self.earlier += self.size.unwrap_or(self.done);
        }
        self.done = done;
        self.size = size;
    }

    fn bytes(&self) -> u64 {
        self.earlier + self.done
    }
}

pub(super) fn run(
    id: DownloadId,
    token: &CancelToken,
    req: &DownloadRequest,
    choice: &MediaChoice,
    reporter: &Reporter,
    shared: &Shared,
) -> Result<Saved, EngineError> {
    // yt-dlp reads web pages only; it is never handed another kind of address.
    if !udm_domain::batch::is_web_url(&req.url) {
        return Err(EngineError::Refused(format!("{} is not an http or https page.", req.url)));
    }
    let yt_dlp = ytdlp::find(req.ffmpeg.as_deref()).ok_or_else(|| EngineError::Refused(ytdlp::MISSING.into()))?;
    if choice.audio_only.is_some() && req.ffmpeg.is_none() {
        return Err(EngineError::Refused(
            "Saving only the sound needs ffmpeg. Install it, or choose it in Options > Tools.".into(),
        ));
    }
    let stem = stream::stem(req);
    let ext = choice.audio_only.map_or("mp4", |t| t.extension());
    let filename = format!("{stem}.{ext}");
    let dir = req.folders.for_name(&filename);
    fs::create_dir_all(&dir)?;
    let probe = http::Probe {
        final_url: req.url.clone(),
        total: None,
        resumable: true,
        etag: None,
        last_modified: None,
        suggested_filename: None,
        content_type: None,
        server: None,
    };
    let claim = choose_output(&shared.claimed, &dir, &filename, req, &probe)?;
    let output = claim.path.clone();
    // yt-dlp leaves a file that is already there alone; the name was chosen
    // under the "file exists" setting, so it may go.
    if output.is_file() {
        fs::remove_file(&output)?;
    }

    // yt-dlp reuses any file already at its own names: an unrelated
    // "Song.m4a" next to "Song.mp3" would be converted, then deleted. A name
    // of this download's own keeps its files apart, and stays the same
    // between runs so yt-dlp can resume its parts.
    let working = working_stem(&output, id);
    let template = format!("{}.%(ext)s", ytdlp::literal_template(&working.to_string_lossy()));
    let proxy = req.net.proxy.as_ref().and_then(|p| p.proxy_for(&req.url));
    // Bound to the chosen network adapter through yt-dlp's own option; not
    // run at all while it is down.
    let source = req.net.route.source_address()?;
    let mut cmd = ytdlp::command(&yt_dlp, proxy.as_deref(), source);
    cmd.args(["-f", &ytdlp::format_spec(choice, req.ffmpeg.is_some())])
        .args(["-o", &template])
        .args(["--newline", "--progress", "--progress-template", PROGRESS_TEMPLATE])
        .args(["--print", &format!("before_dl:{SIZE_TAG}%(filesize,filesize_approx)s")])
        .args(["--print", &format!("after_move:{DONE_TAG}%(filepath)s")])
        .arg("--no-mtime");
    if let Some(ffmpeg) = &req.ffmpeg {
        cmd.arg("--ffmpeg-location").arg(ffmpeg).args(["--merge-output-format", "mp4"]);
    }
    if let Some(target) = choice.audio_only {
        cmd.args(["-x", "--audio-format", target.extension()]);
    }
    cmd.args(ytdlp::subtitle_args(choice, req.ffmpeg.is_some()));
    if let Some(limit) = req.speed_limit {
        cmd.args(["-r", &limit.to_string()]);
    }
    let mut child = cmd
        .arg("--")
        .arg(&req.url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| EngineError::Io(format!("could not run yt-dlp ({}): {e}", yt_dlp.display())))?;
    reporter.send(Progress::Started {
        id,
        url: req.url.clone(),
        path: output.clone(),
        total: None,
        resumable: true,
        connections: 1,
        partial: None,
        validator: None,
        country: None,
    });

    let tally = Arc::new(Mutex::new(Tally::default()));
    let saved_as = Arc::new(Mutex::new(None::<String>));
    let complaints = Arc::new(Mutex::new(String::new()));
    let read = |pipe: Box<dyn std::io::Read + Send>, keep: bool| {
        let (tally, saved_as, complaints, reporter, url) =
            (Arc::clone(&tally), Arc::clone(&saved_as), Arc::clone(&complaints), reporter.clone(), req.url.clone());
        thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                let line = line.trim();
                if let Some((done, size)) = ytdlp::progress_line(line) {
                    let mut t = tally.lock_or_recover();
                    t.update(done, size);
                    reporter.send(Progress::Advanced {
                        id,
                        url: url.clone(),
                        done: ByteCount::new(t.bytes()),
                        total: t.total.filter(|total| *total >= t.bytes()).map(ByteCount::new),
                        parts: None,
                        segments: None,
                    });
                } else if let Some(size) = line.strip_prefix(SIZE_TAG) {
                    tally.lock_or_recover().total = size.trim().parse::<f64>().ok().map(|s| s as u64);
                } else if let Some(path) = line.strip_prefix(DONE_TAG) {
                    *saved_as.lock_or_recover() = Some(path.to_string());
                } else if keep && !line.is_empty() {
                    let mut c = complaints.lock_or_recover();
                    c.push_str(line);
                    c.push('\n');
                }
            }
        })
    };
    // Invariant: the command above set Stdio::piped() for both, and a pipe
    // set that way is always there to take once.
    #[allow(clippy::expect_used)]
    let (stdout, stderr) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
    let readers = [read(Box::new(stdout), false), read(Box::new(stderr), true)];

    // Wait for yt-dlp, ending it when the download is canceled.
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if token.load(Ordering::Relaxed) {
            ytdlp::kill_tree(&mut child);
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(200));
    };
    for r in readers {
        let _ = r.join();
    }
    let Some(status) = status else {
        return Ok(output.into());
    };
    if !status.success() {
        return Err(EngineError::Io(ytdlp::complaint(&complaints.lock_or_recover())));
    }
    let saved = saved_as.lock_or_recover().take().map(std::path::PathBuf::from).filter(|p| p.is_file());
    let path = match saved {
        Some(saved) => settle(&saved, &output),
        None => output,
    };
    // Without ffmpeg the subtitles stay beside the video: give them its name.
    settle_subtitles(&working, &path);
    let note = req.ffmpeg.is_none().then(|| {
        "Saved with its sound in one file, the best quality the site sends that way. With ffmpeg (set in Options) UDM saves the quality you chose."
            .to_string()
    });
    drop(claim);
    Ok(Saved { path, note, complete: true })
}

/// The name yt-dlp works under for download `id`, without an extension:
/// the chosen file's, with the id added.
fn working_stem(output: &Path, id: DownloadId) -> PathBuf {
    let mut stem = output.with_extension("").into_os_string();
    stem.push(format!(".udm-{id}"));
    stem.into()
}

/// yt-dlp's working files for download `id` saved at `output`: the parts,
/// streams and subtitles it keeps under `working_stem`'s name
/// (`<stem>.udm-<id>.*`) until the download is done. A stopped download
/// keeps them to go on from, so they go only with the download itself.
/// Another download whose file has the same stem (`Clip.webm` beside
/// `Clip.mp4`) has its own id, so its files stay.
pub(super) fn working_files(output: &Path, id: DownloadId) -> Vec<PathBuf> {
    let (Some(dir), Some(stem)) = (output.parent(), output.file_stem()) else {
        return Vec::new();
    };
    let prefix = format!("{}.udm-{id}", stem.to_string_lossy());
    let Ok(entries) = fs::read_dir(if dir.as_os_str().is_empty() { Path::new(".") } else { dir }) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            // This id, then the end of the name or an extension.
            name.strip_prefix(&prefix).is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
        })
        .map(|entry| entry.path())
        .collect()
}

/// Move yt-dlp's finished file to the chosen name, keeping the extension
/// yt-dlp gave it. If that name is taken now, the file stays where it is.
fn settle(saved: &Path, output: &Path) -> PathBuf {
    let ext = saved.extension().unwrap_or_default();
    let wanted = output.with_extension(ext);
    if wanted.exists() || fs::rename(saved, &wanted).is_err() {
        return saved.to_path_buf();
    }
    wanted
}

/// yt-dlp's subtitle files beside the video it saved under `working`
/// (`<working>.<lang>.vtt`), renamed after the video at `video`
/// (`<video stem>.<lang>.vtt`). A name that is taken leaves the file as it is.
fn settle_subtitles(working: &Path, video: &Path) {
    let (Some(dir), Some(prefix)) =
        (working.parent(), working.file_name().map(|n| format!("{}.", n.to_string_lossy())))
    else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else { return };
    let stem = video.with_extension("");
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(&prefix) else { continue };
        let is_sub = [".vtt", ".srt", ".ass"].iter().any(|e| rest.to_ascii_lowercase().ends_with(e));
        if !is_sub || rest.contains(['/', '\\']) {
            continue;
        }
        let mut to = stem.clone().into_os_string();
        to.push(format!(".{rest}"));
        let to = PathBuf::from(to);
        if !to.exists() {
            let _ = fs::rename(entry.path(), &to);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtitles_take_the_videos_name() {
        let dir = std::env::temp_dir().join(format!("udm-subs-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let working = dir.join("Clip.udm-5");
        fs::write(dir.join("Clip.udm-5.en.vtt"), b"WEBVTT").unwrap();
        fs::write(dir.join("Clip.udm-5.de-AT.srt"), b"1").unwrap();
        fs::write(dir.join("Clip.udm-5.part"), b"x").unwrap();
        settle_subtitles(&working, &dir.join("Clip.mp4"));
        assert!(dir.join("Clip.en.vtt").is_file() && dir.join("Clip.de-AT.srt").is_file());
        assert!(dir.join("Clip.udm-5.part").is_file(), "only subtitles move");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn working_files_are_this_downloads_alone() {
        let dir = std::env::temp_dir().join(format!("udm-working-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for name in [
            "Clip.mp4",
            "Clip.udm-15.f137.mp4.part",
            "Clip.udm-15.f140.m4a",
            "Clip.udm-15.en.vtt",
            "Clip (2).udm-16.part",
            "Clip.udm-20.f137.webm.part",
            "Clip.udm-150.part",
            "Clip.udm-1.part",
            "Clip.udm-x.part",
            "Clip.udm-15x.part",
            "Clip.mp4.part",
        ] {
            fs::write(dir.join(name), b"x").unwrap();
        }
        let mut found: Vec<String> = working_files(&dir.join("Clip.mp4"), DownloadId::new(15))
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        found.sort();
        assert_eq!(found, ["Clip.udm-15.en.vtt", "Clip.udm-15.f137.mp4.part", "Clip.udm-15.f140.m4a"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn working_stem_adds_the_id() {
        let stem = working_stem(Path::new("dl/Song.mp3"), DownloadId::new(42));
        assert_eq!(stem, Path::new("dl/Song.udm-42"));
        // Dots in the title stay; only the extension goes.
        assert_eq!(working_stem(Path::new("a.b.mp4"), DownloadId::new(7)), Path::new("a.b.udm-7"));
    }

    #[test]
    fn settle_renames_unless_taken() {
        let dir = std::env::temp_dir().join(format!("udm-settle-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let output = dir.join("Song.mp3");

        let saved = dir.join("Song.udm-1.mp3");
        fs::write(&saved, b"new").unwrap();
        assert_eq!(settle(&saved, &output), output);
        assert_eq!(fs::read(&output).unwrap(), b"new");

        // yt-dlp's own extension is kept.
        let saved = dir.join("Song.udm-2.m4a");
        fs::write(&saved, b"m4a").unwrap();
        assert_eq!(settle(&saved, &output), dir.join("Song.m4a"));

        // A name taken since leaves the file under its working name.
        let saved = dir.join("Song.udm-3.mp3");
        fs::write(&saved, b"third").unwrap();
        assert_eq!(settle(&saved, &output), saved);
        assert_eq!(fs::read(&output).unwrap(), b"new");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn video_then_sound_add_up() {
        let mut t = Tally::default();
        t.update(500, Some(1000));
        t.update(1000, Some(1000));
        assert_eq!(t.bytes(), 1000);
        // The sound starts from 0.
        t.update(100, Some(300));
        assert_eq!(t.bytes(), 1100);
        t.update(300, Some(300));
        assert_eq!(t.bytes(), 1300);
    }

    /// Downloads YouTube's first video (19 seconds) through the engine. It
    /// needs yt-dlp, ffmpeg and the network, so it runs only when asked:
    /// `cargo test -- --ignored youtube`.
    #[test]
    #[ignore]
    fn downloads_a_youtube_video() {
        use super::super::Engine;
        use std::sync::mpsc;
        use udm_domain::media::MediaKind;
        use udm_domain::settings::{FileExists, Folders, Settings};

        let url = "https://www.youtube.com/watch?v=jNQXAC9IVRw";
        let ffmpeg = crate::settings::ffmpeg(&Settings::default());
        let yt_dlp = ytdlp::find(ffmpeg.as_deref()).expect("yt-dlp");
        let info = ytdlp::probe(&yt_dlp, url, None, None, ytdlp::PROBE_TIMEOUT);
        assert!(info.error.is_none(), "{:?}", info.error);
        assert!(!info.video.is_empty() && !info.audio.is_empty(), "{info:?}");

        let dir = std::env::temp_dir().join(format!("udm_page_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let engine = Engine::new(
            Arc::new(move |p: Progress| {
                let _ = tx.lock().unwrap().send(p);
            }),
            1,
        );
        engine.enqueue(DownloadRequest {
            url: url.into(),
            folders: Folders::fixed(dir.clone()),
            filename: Some("Me at the zoo.mp4".into()),
            headers: Vec::new(),
            connections: 1,
            reuse_path: None,
            file_exists: FileExists::Rename,
            post_body: None,
            net: Default::default(),
            media: Some(MediaChoice {
                kind: Some(MediaKind::Page),
                video: Some(info.video[0].id.clone()),
                ..MediaChoice::default()
            }),
            ffmpeg,
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
        });
        let mut advanced = 0;
        let path = loop {
            match rx.recv_timeout(Duration::from_secs(120)).expect("download timed out") {
                Progress::Advanced { .. } => advanced += 1,
                Progress::Finished { path, .. } => break path,
                Progress::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        };
        assert_eq!(path, dir.join("Me at the zoo.mp4"));
        assert!(fs::metadata(&path).unwrap().len() > 100_000);
        assert!(advanced > 0, "progress was reported");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1, "no parts left behind");
        fs::remove_dir_all(&dir).ok();
    }
}
