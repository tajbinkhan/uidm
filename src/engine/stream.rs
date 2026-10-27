//! Downloading a stream: read its HLS playlist or DASH manifest, fetch the
//! segments of the chosen quality in parallel, decrypt AES-128 segments, join
//! each track into one file, and make one video with ffmpeg when it is there.
//!
//! Segments are saved one file each in a `<file>.uidmparts` folder next to
//! the output, first as `.tmp` and renamed once complete, so a canceled or
//! failed download resumes segment by segment: complete ones are not fetched
//! again. The folder goes once the video is made.
//!
//! Without ffmpeg an HLS stream is saved as its joined `.ts` (or `.mp4` for
//! fragmented MP4 segments), and a stream with separate audio is saved as two
//! files, with a note saying so.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use url::Url;

use super::limit::Throttle;

use super::{
    backoff, choose_output, derive_filename, fetch_with, http, numbered_name, sanitize_filename, CancelToken,
    DownloadRequest, EngineError, Progress, Reporter, Saved, Shared, BUF_SIZE, MAX_CONNECTIONS, MAX_RETRIES,
};
use crate::media::{dash, hls, sniff, MediaChoice, MediaInfo, MediaKind, MediaOption, Segment, Track, DRM_REFUSED};
use crate::net::NetConfig;

/// Most bytes read of a playlist or manifest.
const MAX_PLAYLIST_BYTES: u64 = 16 * 1024 * 1024;
/// Report progress at most this often.
const REPORT_INTERVAL: Duration = Duration::from_millis(250);
/// The flag that keeps ffmpeg from opening a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The folder a stream's segments wait in while it downloads.
fn parts_dir(output: &Path) -> PathBuf {
    let mut name = output.file_name().unwrap_or_default().to_os_string();
    name.push(".uidmparts");
    output.with_file_name(name)
}

fn refused(e: String) -> EngineError {
    EngineError::Refused(e)
}

/// The download's headers for a request to `url`. The cookies and any
/// `Authorization` were given for `added` (the address the stream was added
/// with), so they go only to its origin, never to another server a playlist
/// or manifest names.
fn headers_for<'a>(added: &str, headers: &'a [(String, String)], url: &str) -> Cow<'a, [(String, String)]> {
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

/// The tracks `choice` asks for: the main one and, apart from it, its audio.
fn load_tracks(
    agent: &http::Client,
    req: &DownloadRequest,
    choice: &MediaChoice,
) -> Result<(Track, Option<Track>), EngineError> {
    let (base, text) = get_text(agent, &req.url, &req.headers)?;
    let kind = choice
        .kind
        .or_else(|| sniff(&text))
        .ok_or_else(|| refused("This address is not an HLS playlist or a DASH manifest.".into()))?;
    match kind {
        MediaKind::Dash => {
            let manifest = dash::parse(&text, &base).map_err(refused)?;
            dash::pick(&manifest, choice.video.as_deref(), choice.audio.as_deref()).map_err(refused)
        }
        MediaKind::Hls => match hls::parse(&text, &base, false).map_err(refused)? {
            hls::Playlist::Media(track) => Ok((track, None)),
            hls::Playlist::Master(master) => {
                if master.drm {
                    return Err(refused(DRM_REFUSED.into()));
                }
                let variant = hls::pick_variant(&master, choice.video.as_deref());
                let media = |url: &str, audio: bool| -> Result<Track, EngineError> {
                    let (base, text) = get_text(agent, url, &headers_for(&req.url, &req.headers, url))?;
                    match hls::parse(&text, &base, audio).map_err(refused)? {
                        hls::Playlist::Media(track) => Ok(track),
                        hls::Playlist::Master(_) => Err(refused("A quality's playlist lists more playlists.".into())),
                    }
                };
                let video = media(&variant.url, false)?;
                let audio = hls::pick_audio(&master, variant, choice.audio.as_deref())
                    .map(|url| media(url, true))
                    .transpose()?;
                Ok((video, audio))
            }
        },
    }
}

/// What a stream offers, for the browser's quality menu.
pub fn probe(url: &str, headers: &[(String, String)], net: &NetConfig) -> MediaInfo {
    let agent = match http::Client::new(net, url) {
        Ok(a) => a,
        Err(e) => return MediaInfo::failed(None, e.to_string()),
    };
    let (base, text) = match get_text(&agent, url, headers) {
        Ok(t) => t,
        Err(e) => return MediaInfo::failed(None, e.to_string()),
    };
    match sniff(&text) {
        None => MediaInfo::failed(None, "This address is not an HLS playlist or a DASH manifest.".into()),
        Some(MediaKind::Dash) => match dash::parse(&text, &base) {
            Ok(m) => dash::describe(&m),
            Err(e) => MediaInfo::failed(Some(MediaKind::Dash), e),
        },
        Some(MediaKind::Hls) => match hls::parse(&text, &base, false) {
            Err(e) => MediaInfo {
                drm: e == DRM_REFUSED,
                ..MediaInfo::failed(Some(MediaKind::Hls), e)
            },
            Ok(hls::Playlist::Media(track)) => MediaInfo {
                kind: Some(MediaKind::Hls),
                video: vec![MediaOption {
                    id: String::new(),
                    label: "Default".into(),
                    ..MediaOption::default()
                }],
                duration: track.duration,
                ..MediaInfo::default()
            },
            Ok(hls::Playlist::Master(master)) => {
                // The length, from the best quality's playlist; it also shows
                // whether the segments are DRM protected.
                let best = &master.variants[0].url;
                let first = get_text(&agent, best, &headers_for(url, headers, best))
                    .map_err(|e| e.to_string())
                    .and_then(|(b, t)| hls::parse(&t, &b, false));
                match first {
                    Err(e) if e == DRM_REFUSED => MediaInfo {
                        drm: true,
                        ..MediaInfo::failed(Some(MediaKind::Hls), e)
                    },
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
fn stem(req: &DownloadRequest) -> String {
    let name = req
        .filename
        .as_deref()
        .map(sanitize_filename)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| derive_filename(&req.url));
    let stem = Path::new(&name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or(name);
    if stem.is_empty() {
        "video".into()
    } else {
        stem
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    id: u64,
    token: &CancelToken,
    agent: &http::Client,
    req: &DownloadRequest,
    choice: &MediaChoice,
    reporter: &Reporter,
    shared: &Shared,
) -> Result<Saved, EngineError> {
    let (main, extra) = load_tracks(agent, req, choice)?;
    let stem = stem(req);
    let ext = match (&req.ffmpeg, main.ext) {
        (Some(_), "webm") => "webm",
        (Some(_), _) => "mp4",
        (None, ext) => ext,
    };
    let filename = format!("{stem}.{ext}");
    // The folder is the one for videos (or music, for audio alone), even
    // when the file stays a .ts.
    let kind = if matches!(main.ext, "m4a" | "aac") { "m4a" } else { "mp4" };
    let dir = req.folders.for_name(&format!("{stem}.{kind}"));
    fs::create_dir_all(&dir)?;
    let probe = http::Probe {
        final_url: req.url.clone(),
        total: None,
        resumable: false,
        etag: None,
        last_modified: None,
        suggested_filename: None,
    };
    let claim = choose_output(&shared.claimed, &dir, &filename, req, &probe)?;
    let output = claim.path.clone();
    let parts = parts_dir(&output);
    fs::create_dir_all(&parts)?;

    let mut jobs = Vec::new();
    let mut tracks = Vec::new();
    for (tag, track) in [("v", Some(&main)), ("a", extra.as_ref())] {
        let Some(track) = track else { continue };
        let first = jobs.len();
        if let Some(init) = &track.init {
            jobs.push(Job {
                path: parts.join(format!("{tag}-init")),
                segment: init.clone(),
            });
        }
        for (i, s) in track.segments.iter().enumerate() {
            jobs.push(Job {
                path: parts.join(format!("{tag}-{i:06}")),
                segment: s.clone(),
            });
        }
        tracks.push((parts.join(format!("{tag}.{}", track.ext)), first..jobs.len(), track.ext));
    }

    let workers = req.connections.clamp(1, MAX_CONNECTIONS).min(jobs.len());
    reporter.send(Progress::Started {
        id,
        url: req.url.clone(),
        path: output.clone(),
        total: None,
        resumable: true,
        connections: workers,
    });
    fetch_all(id, token, agent, req, &jobs, workers, reporter, shared)?;
    if token.load(Ordering::Relaxed) {
        return Ok(output.into());
    }

    for (joined, range, _) in &tracks {
        join(&jobs[range.clone()], joined)?;
    }
    let note = finish(req, &tracks, &output, &stem)?;
    let _ = fs::remove_dir_all(&parts);
    drop(claim);
    Ok(Saved { path: output, note })
}

/// Make the output from the joined tracks: with ffmpeg one video, without it
/// the tracks themselves.
fn finish(
    req: &DownloadRequest,
    tracks: &[(PathBuf, std::ops::Range<usize>, &'static str)],
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
        if tracks.len() > 1 {
            cmd.args(["-map", "0:v:0?", "-map", "1:a:0?"]);
        }
        cmd.args(["-c", "copy"]);
        if output.extension().is_some_and(|e| e == "mp4") && tracks.iter().any(|t| matches!(t.2, "ts" | "aac")) {
            // ADTS AAC from MPEG-TS needs reframing to go in an MP4.
            cmd.args(["-bsf:a", "aac_adtstoasc"]);
        }
        let out = cmd
            .arg(output)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
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
    let Some((audio, _, ext)) = tracks.get(1) else {
        return Ok(match main.2 {
            "ts" => Some("Saved as a .ts video. With ffmpeg (set in Options) it would be an .mp4.".into()),
            _ => None,
        });
    };
    let dir = output.parent().unwrap_or(Path::new("."));
    let name = if output.extension().is_some_and(|e| e == *ext) {
        format!("{stem} (audio).{ext}")
    } else {
        format!("{stem}.{ext}")
    };
    let audio_path = (0..)
        .map(|n| dir.join(numbered_name(&name, n)))
        .find(|p| !p.exists())
        .expect("some name is free");
    fs::rename(audio, &audio_path)?;
    Ok(Some(format!(
        "Saved as two files, video and audio ({}), because ffmpeg was not found. Install ffmpeg or set it in Options to get one video.",
        audio_path.file_name().unwrap_or_default().to_string_lossy()
    )))
}

/// Concatenate the segment files of one track, in order.
fn join(jobs: &[Job], joined: &Path) -> Result<(), EngineError> {
    let mut out = BufWriter::new(File::create(joined)?);
    for job in jobs {
        let mut part = File::open(&job.path)?;
        std::io::copy(&mut part, &mut out)?;
    }
    out.flush()?;
    Ok(())
}

/// What the segment threads share.
struct Fetching<'a> {
    id: u64,
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
            let mut last = self.last_report.lock().unwrap();
            if !force && last.elapsed() < REPORT_INTERVAL {
                return;
            }
            *last = Instant::now();
        }
        self.reporter.send(Progress::Advanced {
            id: self.id,
            url: self.req.url.clone(),
            done: self.done_bytes.load(Ordering::Relaxed),
            total: None,
            parts: Some([self.done_parts.load(Ordering::Relaxed), self.total_parts]),
            segments: None,
        });
    }

    /// The key at `url`, fetched once. The lock is held while fetching, so
    /// segments waiting for the same key do not each fetch it.
    fn key(&self, url: &str) -> Result<[u8; 16], EngineError> {
        let mut keys = self.keys.lock().unwrap();
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
    id: u64,
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
                if token.load(Ordering::Relaxed) || first_error.lock().unwrap().is_some() {
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
                        first_error.lock().unwrap().get_or_insert(e);
                        return;
                    }
                }
            });
        }
    });
    if let Some(e) = first_error.lock().unwrap().take() {
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
        assert_eq!(parts_dir(Path::new(r"C:\dl\movie.mp4")), PathBuf::from(r"C:\dl\movie.mp4.uidmparts"));
    }

    #[test]
    fn credentials_stay_on_the_added_origin() {
        let headers = vec![
            ("User-Agent".to_string(), "UIDM".to_string()),
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
        use crate::media::MediaKind;
        use crate::settings::{FileExists, Folders};
        use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
        use std::sync::mpsc;

        fn temp_dir(name: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!("uidm_stream_{}_{name}", std::process::id()));
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
            }
        }

        /// Where a download was saved and its note, or its error.
        type Outcome = Result<(PathBuf, Option<String>), String>;

        /// Run a download; the result and the segment counts it reported.
        fn run(req: DownloadRequest) -> (Outcome, Vec<[u64; 2]>) {
            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            let engine = Engine::new(Arc::new(move |p: Progress| {
                let _ = tx.lock().unwrap().send(p);
            }), 1);
            engine.enqueue(req);
            let mut parts = Vec::new();
            loop {
                match rx.recv_timeout(Duration::from_secs(30)).expect("download timed out") {
                    Progress::Advanced { parts: Some(p), .. } => parts.push(p),
                    Progress::Finished { path, note, .. } => return (Ok((path, note)), parts),
                    Progress::Failed { error, .. } => return (Err(error), parts),
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
            let choice = MediaChoice {
                kind: Some(MediaKind::Hls),
                video: Some(low),
                audio: None,
            };
            let (result, _) = run(request(format!("{}/v/master.m3u8", server.url), &dir, choice));
            let (path, _) = result.unwrap();
            assert_eq!(path, dir.join("Movie night (1).ts"));
            assert_eq!(fs::read(&path).unwrap(), b"low");
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
            let engine = Engine::new(Arc::new(move |p: Progress| {
                let _ = tx.lock().unwrap().send(p);
            }), 1);
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
            req.headers = vec![
                ("Cookie".into(), "session=secret".into()),
                ("Authorization".into(), "Bearer secret".into()),
            ];
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
            assert_eq!(served, fetched_segments + playlists, "only the missing segments, not the {first_two} bytes on disk");
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
            files.insert(
                "/d/master.m3u8".into(),
                b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nv.m3u8\n".to_vec(),
            );
            files.insert(
                "/d/v.m3u8".into(),
                b"#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://k\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\n#EXTINF:4,\n0.ts\n#EXT-X-ENDLIST\n".to_vec(),
            );
            let server = TestServer::start_files(files);
            let dir = temp_dir("drm");
            let url = format!("{}/d/master.m3u8", server.url);
            let (result, _) = run(request(url.clone(), &dir, MediaChoice::default()));
            assert_eq!(result.unwrap_err(), DRM_REFUSED);
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
            assert_eq!(info.video[0].size, Some(7_500_000));
            fs::remove_dir_all(&dir).ok();
        }
    
        /// A stand-in for ffmpeg: it records its arguments next to itself
        /// and writes the output file, or fails with a message.
        fn fake_ffmpeg(dir: &Path, fail: bool) -> PathBuf {
            let bat = dir.join("ffmpeg.bat");
            let body = if fail {
                "@echo off\r\necho Invalid data found when processing input 1>&2\r\nexit /b 1\r\n".to_string()
            } else {
                "@echo off\r\necho %* > \"%~dp0args.txt\"\r\nfor %%a in (%*) do set last=%%a\r\necho video> %last%\r\n".to_string()
            };
            fs::write(&bat, body).unwrap();
            bat
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
            let error = result.unwrap_err();
            assert!(error.contains("ffmpeg could not make the video: Invalid data"), "{error}");
            assert!(parts_dir(&dir.join("Movie night (1).mp4")).join("v-000000").exists());
            fs::remove_dir_all(&dir).ok();
            fs::remove_dir_all(&tools).ok();
        }
    }
}
