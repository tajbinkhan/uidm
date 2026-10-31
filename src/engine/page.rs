//! Downloading a video page with yt-dlp (see `media::ytdlp`): yt-dlp fetches
//! the chosen video and sound and ffmpeg joins them, while UIDM reads its
//! progress lines and stops it on cancel.
//!
//! yt-dlp keeps `.part` files of what it fetched, so a canceled download
//! carries on where it stopped when it is started again.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::{choose_output, http, stream, CancelToken, DownloadRequest, EngineError, Progress, Reporter, Saved, Shared};
use crate::media::ytdlp::{self, DONE_TAG, PROGRESS_TEMPLATE};
use crate::media::MediaChoice;

/// What yt-dlp's estimate of the whole download starts with.
const SIZE_TAG: &str = "UIDM-SIZE ";

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

/// End the process and the ones it started: yt-dlp.exe unpacks itself into
/// a second process, and ffmpeg runs under it while joining.
fn kill_tree(child: &mut Child) {
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

pub(super) fn run(
    id: u64,
    token: &CancelToken,
    req: &DownloadRequest,
    choice: &MediaChoice,
    reporter: &Reporter,
    shared: &Shared,
) -> Result<Saved, EngineError> {
    let yt_dlp = ytdlp::find(req.ffmpeg.as_deref()).ok_or_else(|| EngineError::Refused(ytdlp::MISSING.into()))?;
    if choice.audio_only.is_some() && req.ffmpeg.is_none() {
        return Err(EngineError::Refused(
            "Saving only the sound needs ffmpeg. Install it, or choose it in Options > General.".into(),
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
    let mut cmd = ytdlp::command(&yt_dlp, proxy.as_deref());
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
                    let mut t = tally.lock().unwrap();
                    t.update(done, size);
                    reporter.send(Progress::Advanced {
                        id,
                        url: url.clone(),
                        done: t.bytes(),
                        total: t.total.filter(|total| *total >= t.bytes()),
                        parts: None,
                        segments: None,
                    });
                } else if let Some(size) = line.strip_prefix(SIZE_TAG) {
                    tally.lock().unwrap().total = size.trim().parse::<f64>().ok().map(|s| s as u64);
                } else if let Some(path) = line.strip_prefix(DONE_TAG) {
                    *saved_as.lock().unwrap() = Some(path.to_string());
                } else if keep && !line.is_empty() {
                    let mut c = complaints.lock().unwrap();
                    c.push_str(line);
                    c.push('\n');
                }
            }
        })
    };
    let readers = [
        read(Box::new(child.stdout.take().expect("piped")), false),
        read(Box::new(child.stderr.take().expect("piped")), true),
    ];

    // Wait for yt-dlp, ending it when the download is canceled.
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if token.load(Ordering::Relaxed) {
            kill_tree(&mut child);
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
        return Err(EngineError::Io(ytdlp::complaint(&complaints.lock().unwrap())));
    }
    let saved = saved_as.lock().unwrap().take().map(std::path::PathBuf::from).filter(|p| p.is_file());
    let path = match saved {
        Some(saved) => settle(&saved, &output),
        None => output,
    };
    let note = req.ffmpeg.is_none().then(|| {
        "Saved with its sound in one file, the best quality YouTube sends that way. With ffmpeg (set in Options) UIDM saves the quality you chose."
            .to_string()
    });
    drop(claim);
    Ok(Saved { path, note })
}

/// The name yt-dlp works under for download `id`, without an extension:
/// the chosen file's, with the id added.
fn working_stem(output: &Path, id: u64) -> PathBuf {
    let mut stem = output.with_extension("").into_os_string();
    stem.push(format!(".uidm-{id}"));
    stem.into()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_stem_adds_the_id() {
        let stem = working_stem(Path::new("dl/Song.mp3"), 42);
        assert_eq!(stem, Path::new("dl/Song.uidm-42"));
        // Dots in the title stay; only the extension goes.
        assert_eq!(working_stem(Path::new("a.b.mp4"), 7), Path::new("a.b.uidm-7"));
    }

    #[test]
    fn settle_renames_unless_taken() {
        let dir = std::env::temp_dir().join(format!("uidm-settle-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let output = dir.join("Song.mp3");

        let saved = dir.join("Song.uidm-1.mp3");
        fs::write(&saved, b"new").unwrap();
        assert_eq!(settle(&saved, &output), output);
        assert_eq!(fs::read(&output).unwrap(), b"new");

        // yt-dlp's own extension is kept.
        let saved = dir.join("Song.uidm-2.m4a");
        fs::write(&saved, b"m4a").unwrap();
        assert_eq!(settle(&saved, &output), dir.join("Song.m4a"));

        // A name taken since leaves the file under its working name.
        let saved = dir.join("Song.uidm-3.mp3");
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
        use crate::media::MediaKind;
        use crate::settings::{FileExists, Folders, Settings};
        use std::sync::mpsc;

        let url = "https://www.youtube.com/watch?v=jNQXAC9IVRw";
        let ffmpeg = Settings::default().ffmpeg();
        let yt_dlp = ytdlp::find(ffmpeg.as_deref()).expect("yt-dlp");
        let info = ytdlp::probe(&yt_dlp, url, None);
        assert!(info.error.is_none(), "{:?}", info.error);
        assert!(!info.video.is_empty() && !info.audio.is_empty(), "{info:?}");

        let dir = std::env::temp_dir().join(format!("uidm_page_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let engine = Engine::new(Arc::new(move |p: Progress| {
            let _ = tx.lock().unwrap().send(p);
        }), 1);
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
            ignore_date: false,
            temp_dir: None,
            reuse_partial: None,
            host_limit: None,
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
