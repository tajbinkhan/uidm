//! Image galleries read by gallery-dl. gallery-dl knows hundreds of
//! picture sites (Wikimedia Commons, Flickr, DeviantArt, ...); here it only
//! lists a page's files, and UDM's own engine downloads the ones the
//! user ticks.
//!
//! The page is read with `gallery-dl --config-ignore --no-input
//! --resolve-json -o output.jsonl=true --range 1-1000 -- URL`:
//! `--resolve-json` (`-J`) prints what it finds as JSON, following an
//! album's or a user page's links to their pictures, and `output.jsonl`
//! prints one message per line as it goes, so UDM can stop it at
//! `MAX_PICTURES` or at its timeout and keep what came. Each message is an
//! array: `[2, {...}]` opens a directory (a post or an album, with its
//! metadata), `[3, "url", {...}]` is a file with its `filename`,
//! `extension`, `width` and `height`, `[6, "url", {...}]` a page no
//! extractor could follow, and `[-1, {"error", "message"}]` why it stopped.
//! A gallery-dl too old for `output.jsonl` prints the same messages as one
//! array when it ends, which is read too. Exit status 64 means it knows no
//! site like the page's.
//!
//! The user's own gallery-dl configuration is not read (`--config-ignore`),
//! as yt-dlp's is not, so its answer is the same for everyone; no cookies
//! are handed to it.
//!
//! gallery-dl is found in UDM's tools folder (Options > Tools
//! installs it there), on `PATH`, or next to UDM's programs.

use std::io::{BufRead as _, BufReader, Read as _};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use udm_domain::gallery::{Gallery, GalleryError, Picture, MAX_PICTURES};
use udm_domain::sync::LockExt;

/// How long reading a page may take before it is given up.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(55);

/// The most of gallery-dl's answer kept, in bytes, for an old gallery-dl
/// that prints it all at the end.
const MAX_ANSWER: usize = 64 * 1024 * 1024;

/// The end of its complaints kept, in bytes.
const MAX_STDERR: usize = 64 * 1024;

/// gallery-dl: the one UDM installed, else the first on `PATH`, else
/// next to UDM's programs.
pub fn find() -> Option<PathBuf> {
    let name = crate::platform::exe_name("gallery-dl");
    let own = crate::paths::tools_dir();
    let on_path = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default();
    let beside = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf));
    own.into_iter().chain(on_path).chain(beside).map(|dir| dir.join(&name)).find(|p| p.is_file())
}

/// The arguments that read `url`, with the settings' `proxy`, the address
/// of the network adapter downloads are bound to (`source`) and the
/// browser's `user_agent`.
pub fn args(url: &str, proxy: Option<&str>, source: Option<IpAddr>, user_agent: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = [
        "--config-ignore",
        "--no-input",
        "--resolve-json",
        "-o",
        "output.jsonl=true",
        "--range",
        &format!("1-{MAX_PICTURES}"),
        "--http-timeout",
        "20",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(ua) = user_agent {
        args.extend(["--user-agent".to_string(), ua.to_string()]);
    }
    if let Some(proxy) = proxy {
        args.extend(["--proxy".to_string(), proxy.to_string()]);
    }
    if let Some(source) = source {
        args.extend(["--source-address".to_string(), source.to_string()]);
    }
    args.extend(["--".to_string(), url.to_string()]);
    args
}

/// What gallery-dl listed so far.
#[derive(Default)]
struct Found {
    title: String,
    pictures: Vec<Picture>,
    /// Why it stopped, from a `-1` message.
    error: Option<String>,
}

impl Found {
    /// Take in one message.
    fn take(&mut self, msg: &Value) {
        let Some(parts) = msg.as_array() else { return };
        match parts.first().and_then(Value::as_i64) {
            Some(2) => {
                if self.title.is_empty() {
                    self.title = parts.get(1).map(title_of).unwrap_or_default();
                }
            }
            Some(3) if self.pictures.len() < MAX_PICTURES => {
                let (Some(url), meta) = (parts.get(1).and_then(Value::as_str), parts.get(2)) else { return };
                let meta = meta.cloned().unwrap_or(Value::Null);
                let text = |k: &str| meta.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                let size = |k: &str| {
                    let v = meta.get(k)?;
                    v.as_u64().or_else(|| v.as_str()?.trim().parse().ok()).and_then(|n| u32::try_from(n).ok())
                };
                if self.title.is_empty() {
                    self.title = title_of(&meta);
                }
                self.pictures.push(Picture {
                    url: url.to_string(),
                    name: text("filename"),
                    extension: text("extension"),
                    width: size("width").filter(|&w| w > 0),
                    height: size("height").filter(|&h| h > 0),
                });
            }
            Some(-1) => {
                let why = parts.get(1).and_then(|m| m.get("message")).and_then(Value::as_str).unwrap_or_default();
                if !why.trim().is_empty() {
                    self.error = Some(why.trim().chars().take(500).collect());
                }
            }
            _ => {}
        }
    }

    fn full(&self) -> bool {
        self.pictures.len() >= MAX_PICTURES
    }
}

/// A gallery's name in a message's metadata: its album's or gallery's
/// title, else its own.
fn title_of(meta: &Value) -> String {
    let named = |v: Option<&Value>| -> Option<String> {
        let v = v?;
        let s = v.as_str().or_else(|| v.get("title").and_then(Value::as_str)).or_else(|| v.get("name")?.as_str())?;
        Some(s.trim().to_string()).filter(|s| !s.is_empty())
    };
    named(meta.get("album"))
        .or_else(|| named(meta.get("gallery")))
        .or_else(|| named(meta.get("collection")))
        .or_else(|| named(meta.get("title")))
        .unwrap_or_default()
}

/// The gallery in gallery-dl's whole answer: one message per line, or one
/// array of them.
pub fn read_answer(out: &[u8]) -> (Gallery, Option<String>) {
    let mut found = Found::default();
    match serde_json::from_slice::<Value>(out) {
        Ok(Value::Array(all)) if all.iter().all(Value::is_array) => all.iter().for_each(|m| found.take(m)),
        _ => {
            for line in out.split(|&b| b == b'\n') {
                if let Ok(msg) = serde_json::from_slice::<Value>(line) {
                    found.take(&msg);
                }
            }
        }
    }
    (Gallery { title: found.title, pictures: found.pictures }.cleaned(), found.error)
}

/// The last thing gallery-dl complained about, without its `[...][error]`
/// prefixes.
pub fn complaint(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let line = lines.iter().rev().find(|l| l.contains("[error]")).or_else(|| lines.last()).copied().unwrap_or("");
    let mut line = line;
    while let Some(rest) = line.strip_prefix('[').and_then(|r| r.split_once(']')).map(|(_, r)| r.trim_start()) {
        line = rest;
    }
    let line: String = line.chars().take(300).collect();
    if line.is_empty() {
        "it failed without saying why.".into()
    } else {
        line
    }
}

/// End gallery-dl and what it started. A standalone gallery-dl unpacks
/// itself and runs as a second process; on Windows `taskkill /T` ends the
/// tree, elsewhere it runs in a process group of its own, which is ended.
fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        if let Ok(pid) = i32::try_from(child.id()) {
            // SAFETY: kill(2) takes plain integers and touches no memory;
            // the negative id names the process group made at spawn, whose
            // leader is our child, still unreaped.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
    crate::media::ytdlp::kill_tree(child);
}

/// The pictures of the gallery at `url`, read by `program`. Given up after
/// `timeout`, keeping what was listed by then.
pub fn probe(
    program: &Path,
    url: &str,
    proxy: Option<&str>,
    source: Option<IpAddr>,
    user_agent: Option<&str>,
    timeout: Duration,
) -> Result<Gallery, GalleryError> {
    let mut cmd = Command::new(program);
    crate::platform::no_window(&mut cmd);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    let child = cmd
        .args(args(url, proxy, source, user_agent))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child =
        child.map_err(|e| GalleryError::Failed(format!("could not run gallery-dl ({}): {e}", program.display())))?;
    let found = Arc::new(Mutex::new(Found::default()));
    let raw = Arc::new(Mutex::new(Vec::new()));
    let full = Arc::new(AtomicBool::new(false));
    // Read each line as it comes, so a big answer can neither stall
    // gallery-dl nor be lost when it is stopped.
    if let Some(out) = child.stdout.take() {
        let (found, raw, full) = (Arc::clone(&found), Arc::clone(&raw), Arc::clone(&full));
        std::thread::spawn(move || {
            let mut reader = BufReader::new(out);
            let mut line = Vec::new();
            while reader.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
                if let Ok(msg) = serde_json::from_slice::<Value>(&line) {
                    let mut f = found.lock_or_recover();
                    f.take(&msg);
                    if f.full() {
                        full.store(true, Ordering::SeqCst);
                        return;
                    }
                } else {
                    let mut kept = raw.lock_or_recover();
                    if kept.len() + line.len() <= MAX_ANSWER {
                        kept.extend_from_slice(&line);
                    }
                }
                line.clear();
            }
        });
    }
    let stderr = child.stderr.take().map(|mut err| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            let _ = err.read_to_end(&mut out);
            let start = out.len().saturating_sub(MAX_STDERR);
            String::from_utf8_lossy(&out[start..]).into_owned()
        })
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if full.load(Ordering::SeqCst) || started.elapsed() >= timeout => {
                kill_tree(&mut child);
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(GalleryError::Failed(e.to_string())),
        }
    };
    // Its output may still be on its way after it exits; wait a little.
    let settle = Instant::now();
    let take = |found: &Mutex<Found>| {
        let f = std::mem::take(&mut *found.lock_or_recover());
        (Gallery { title: f.title, pictures: f.pictures }.cleaned(), f.error)
    };
    let Some(status) = status else {
        let (gallery, _) = take(&found);
        if !gallery.pictures.is_empty() {
            return Ok(gallery);
        }
        return Err(GalleryError::TimedOut(timeout.as_secs()));
    };
    while Arc::strong_count(&raw) > 1 && settle.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let (mut gallery, mut error) = take(&found);
    if gallery.pictures.is_empty() {
        // An older gallery-dl printed one array at the end.
        let (whole, why) = read_answer(&raw.lock_or_recover());
        gallery = whole;
        error = error.or(why);
    }
    if !gallery.pictures.is_empty() {
        return Ok(gallery);
    }
    let complaint = stderr.and_then(|h| h.join().ok()).map(|e| complaint(&e)).unwrap_or_default();
    if status.code().is_some_and(|c| c & 64 != 0) {
        return Err(GalleryError::Unsupported);
    }
    match (status.success(), error) {
        (_, Some(why)) => Err(GalleryError::Failed(why)),
        (true, None) => Err(GalleryError::NothingFound),
        (false, None) => Err(GalleryError::Failed(complaint)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_arguments_read_without_downloading() {
        let a = args("https://g.example/a", Some("http://proxy:3128"), None, Some("Agent"));
        assert_eq!(
            a,
            [
                "--config-ignore",
                "--no-input",
                "--resolve-json",
                "-o",
                "output.jsonl=true",
                "--range",
                "1-1000",
                "--http-timeout",
                "20",
                "--user-agent",
                "Agent",
                "--proxy",
                "http://proxy:3128",
                "--",
                "https://g.example/a"
            ]
        );
        // The page comes after `--`, so it can never be taken for an option.
        assert_eq!(args("--help", None, None, None).iter().rev().take(2).collect::<Vec<_>>(), ["--help", "--"]);
    }

    const LINES: &str = concat!(
        r#"[2, {"category": "flickr", "album": {"id": "1", "title": "Holiday"}}]"#,
        "\n",
        r#"[3, "https://live.example/1_o.jpg", {"filename": "Beach", "extension": "jpg", "width": 4000, "height": "3000"}]"#,
        "\n",
        r#"[3, "ytdl:https://video.example/2", {"filename": "Clip", "extension": "mp4"}]"#,
        "\n",
        r#"[6, "https://elsewhere.example/x", {}]"#,
        "\n",
        r#"[3, "https://live.example/3_o.png", {"filename": "Sea", "extension": "png"}]"#,
        "\n"
    );

    #[test]
    fn json_lines_and_one_array_read_the_same() {
        let (g, error) = read_answer(LINES.as_bytes());
        assert_eq!(error, None);
        assert_eq!(g.title, "Holiday");
        let urls: Vec<&str> = g.pictures.iter().map(|p| p.url.as_str()).collect();
        assert_eq!(urls, ["https://live.example/1_o.jpg", "https://live.example/3_o.png"]);
        assert_eq!((g.pictures[0].width, g.pictures[0].height), (Some(4000), Some(3000)));
        assert_eq!(g.pictures[0].file_name(1), "Beach.jpg");
        let array = format!("[{}]", LINES.trim_end().replace('\n', ",\n"));
        assert_eq!(read_answer(array.as_bytes()).0, g);
        let failed = r#"[[-1, {"error": "HttpError", "message": "'404 Not Found' for 'https://x/'"}]]"#;
        assert_eq!(read_answer(failed.as_bytes()).1.as_deref(), Some("'404 Not Found' for 'https://x/'"));
    }

    #[test]
    fn complaints_lose_their_prefixes() {
        let err = "[gallery-dl][warning] slow\n[flickr][error] HttpError: '403 Forbidden'\n";
        assert_eq!(complaint(err), "HttpError: '403 Forbidden'");
        assert_eq!(complaint(""), "it failed without saying why.");
    }

    /// A stand-in gallery-dl: a script that prints canned lines, fails or
    /// sleeps.
    #[cfg(unix)]
    fn fake(name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("udm_fake_gallerydl_{}_{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gallery-dl");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn a_stand_in_gallery_dl_lists_fails_or_is_given_up() {
        let ok = fake("ok", &format!("[ \"$1\" = --config-ignore ] || exit 3\ncat <<'EOF'\n{LINES}EOF"));
        let g = probe(&ok, "https://g.example/album", None, None, None, Duration::from_secs(20)).unwrap();
        assert_eq!(g.pictures.len(), 2);

        let unknown = fake("unknown", "echo \"[gallery-dl][error] Unsupported URL '$*'\" >&2; exit 64");
        assert_eq!(
            probe(&unknown, "https://x/", None, None, None, Duration::from_secs(20)),
            Err(GalleryError::Unsupported)
        );

        let empty = fake("empty", "exit 0");
        assert_eq!(
            probe(&empty, "https://x/", None, None, None, Duration::from_secs(20)),
            Err(GalleryError::NothingFound)
        );

        let fails = fake("fails", "echo '[flickr][error] HttpError: 403' >&2; exit 4");
        assert_eq!(
            probe(&fails, "https://x/", None, None, None, Duration::from_secs(20)),
            Err(GalleryError::Failed("HttpError: 403".into()))
        );

        // Some pictures, then it hangs: what came is kept.
        let first = LINES.lines().nth(1).unwrap();
        let partial = fake("partial", &format!("cat <<'EOF'\n{first}\nEOF\nsleep 30"));
        let started = Instant::now();
        let g = probe(&partial, "https://x/", None, None, None, Duration::from_millis(800)).unwrap();
        assert_eq!(g.pictures.len(), 1);
        let slow = fake("slow", "sleep 30");
        assert_eq!(
            probe(&slow, "https://x/", None, None, None, Duration::from_millis(300)),
            Err(GalleryError::TimedOut(0))
        );
        assert!(started.elapsed() < Duration::from_secs(10));

        let missing = Path::new("/nonexistent/gallery-dl");
        assert!(matches!(
            probe(missing, "https://x/", None, None, None, Duration::from_secs(1)),
            Err(GalleryError::Failed(_))
        ));
    }

    /// More than `MAX_PICTURES` stops gallery-dl at the limit.
    #[cfg(unix)]
    #[test]
    fn a_huge_gallery_stops_at_the_limit() {
        let script = "i=0; while [ $i -lt 5000 ]; do echo \"[3, \\\"https://i.example/$i.jpg\\\", {}]\"; i=$((i+1)); done; sleep 30";
        let many = fake("many", script);
        let started = Instant::now();
        let g = probe(&many, "https://x/", None, None, None, Duration::from_secs(25)).unwrap();
        assert_eq!(g.pictures.len(), MAX_PICTURES);
        assert!(started.elapsed() < Duration::from_secs(20), "it was stopped, not waited for");
    }
}
