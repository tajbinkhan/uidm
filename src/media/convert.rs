//! Converting a finished video with ffmpeg: to an MP4, or its sound alone to
//! an MP3 or M4A. Each conversion first tries to copy the streams as they
//! are (fast, no loss), and encodes them only when the copy fails. ffmpeg
//! reports how far it got on a pipe (`-progress`), which becomes a fraction
//! of the input's length.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::{Deserialize, Serialize};


/// What a video becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Mp4,
    Mp3,
    M4a,
}

impl Target {
    pub fn extension(self) -> &'static str {
        match self {
            Target::Mp4 => "mp4",
            Target::Mp3 => "mp3",
            Target::M4a => "m4a",
        }
    }

    pub fn audio_only(self) -> bool {
        self != Target::Mp4
    }

    /// The ffmpeg arguments between the input and the output: a copy of
    /// the streams first, then an encode.
    fn attempts(self) -> [&'static [&'static str]; 2] {
        match self {
            Target::Mp4 => [
                &["-map", "0:v:0?", "-map", "0:a:0?", "-c", "copy", "-movflags", "+faststart"],
                &["-map", "0:v:0?", "-map", "0:a:0?", "-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-c:a", "aac", "-b:a", "160k", "-movflags", "+faststart"],
            ],
            Target::M4a => [&["-vn", "-map", "0:a:0", "-c:a", "copy"], &["-vn", "-map", "0:a:0", "-c:a", "aac", "-b:a", "192k"]],
            // MP3 is always encoded; the copy only works for MP3 sound.
            Target::Mp3 => [&["-vn", "-map", "0:a:0", "-c:a", "copy", "-f", "mp3"], &["-vn", "-map", "0:a:0", "-c:a", "libmp3lame", "-q:a", "2"]],
        }
    }
}

/// `HH:MM:SS.ss` as seconds.
fn clock_secs(s: &str) -> Option<f64> {
    let mut parts = s.trim().split(':');
    let (h, m, sec) = (parts.next()?, parts.next()?, parts.next()?);
    Some(h.parse::<f64>().ok()? * 3600.0 + m.parse::<f64>().ok()? * 60.0 + sec.parse::<f64>().ok()?)
}

/// The input's length from ffmpeg's `Duration: 00:03:21.48, start: ...`.
fn duration_in(line: &str) -> Option<f64> {
    let rest = line.split("Duration:").nth(1)?;
    clock_secs(rest.split(',').next()?)
}

/// How far a `-progress` line says ffmpeg got, in seconds.
fn progress_secs(line: &str) -> Option<f64> {
    let us = line.strip_prefix("out_time_us=").or_else(|| line.strip_prefix("out_time_ms="))?;
    // Despite its name, out_time_ms is in microseconds too.
    us.trim().parse::<f64>().ok().map(|v| v / 1_000_000.0)
}

/// One ffmpeg run. Returns the last thing it complained about on failure.
fn run_once(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    args: &[&str],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64),
) -> Result<(), String> {
    let mut child = crate::platform::no_window(&mut Command::new(ffmpeg))
        .args(["-hide_banner", "-nostats", "-y", "-i"])
        .arg(input)
        .args(args)
        .args(["-progress", "pipe:1"])
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run ffmpeg ({}): {e}", ffmpeg.display()))?;
    // stderr has the input's length and any complaint; read it aside.
    let duration = Arc::new(Mutex::new(None::<f64>));
    let last_error = Arc::new(Mutex::new(String::new()));
    let stderr = child.stderr.take().expect("piped");
    let (d, e) = (Arc::clone(&duration), Arc::clone(&last_error));
    let reader = thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Some(secs) = duration_in(&line) {
                d.lock().unwrap().get_or_insert(secs);
            } else if !line.trim().is_empty() {
                *e.lock().unwrap() = line.trim().to_string();
            }
        }
    });
    let stdout = child.stdout.take().expect("piped");
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            break;
        }
        if let (Some(at), Some(total)) = (progress_secs(&line), *duration.lock().unwrap()) {
            if total > 0.0 {
                progress((at / total).clamp(0.0, 1.0));
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let _ = reader.join();
    if cancel.load(Ordering::Relaxed) {
        let _ = std::fs::remove_file(output);
        return Err("canceled".into());
    }
    if status.success() {
        return Ok(());
    }
    let _ = std::fs::remove_file(output);
    let why = last_error.lock().unwrap().clone();
    Err(if why.is_empty() { "ffmpeg failed".into() } else { why })
}

/// Convert `input` to `output` as `target`, reporting progress from 0 to 1.
/// A copy of the streams is tried first; an encode follows if it fails.
pub fn convert(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    target: Target,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f64),
) -> Result<(), String> {
    let [copy, encode] = target.attempts();
    match run_once(ffmpeg, input, output, copy, cancel, &mut progress) {
        Ok(()) => Ok(()),
        Err(e) if e == "canceled" => Err(e),
        Err(_) => run_once(ffmpeg, input, output, encode, cancel, &mut progress)
            .map_err(|e| format!("ffmpeg could not convert it: {e}")),
    }
}

/// Whether a file name looks like a video ffmpeg can convert.
pub fn is_video(name: &str) -> bool {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "mp4" | "mkv" | "webm" | "ts" | "avi" | "mov" | "flv" | "m4v" | "wmv" | "mpg" | "mpeg" | "3gp")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ffmpegs_length_and_progress() {
        assert_eq!(duration_in("  Duration: 00:03:21.50, start: 0.000000, bitrate: 800 kb/s"), Some(201.5));
        assert_eq!(duration_in("Stream #0:0: Video: h264"), None);
        assert_eq!(progress_secs("out_time_us=50000000"), Some(50.0));
        assert_eq!(progress_secs("out_time_ms=1500000"), Some(1.5));
        assert_eq!(progress_secs("progress=continue"), None);
    }

    #[test]
    fn targets_and_names() {
        assert_eq!(Target::Mp3.extension(), "mp3");
        assert!(Target::M4a.audio_only() && !Target::Mp4.audio_only());
        assert!(is_video("Movie.MKV") && is_video("clip.ts") && !is_video("song.mp3") && !is_video("noext"));
    }

    #[test]
    fn a_missing_ffmpeg_is_reported() {
        let err = convert(
            Path::new(r"C:\no\such\ffmpeg.exe"),
            Path::new("in.mp4"),
            Path::new("out.mp3"),
            Target::Mp3,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap_err();
        assert!(err.contains("ffmpeg"), "{err}");
    }

    /// Makes a two-second clip with ffmpeg and converts it every way. Skipped
    /// where ffmpeg is not installed.
    #[test]
    fn converts_a_real_clip_when_ffmpeg_is_there() {
        let Some(ffmpeg) = crate::settings::Settings::default().ffmpeg() else {
            eprintln!("no ffmpeg; skipped");
            return;
        };
        let dir = std::env::temp_dir().join(format!("uidm_convert_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("clip.mkv");
        let made = crate::platform::no_window(&mut Command::new(&ffmpeg))
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=duration=2:size=160x120:rate=10"])
            .args(["-f", "lavfi", "-i", "sine=duration=2", "-c:v", "libx264", "-c:a", "aac", "-shortest"])
            .arg(&clip)
            .status()
            .unwrap();
        assert!(made.success());
        for target in [Target::Mp4, Target::M4a, Target::Mp3] {
            let out = dir.join(format!("out.{}", target.extension()));
            let mut seen = Vec::new();
            convert(&ffmpeg, &clip, &out, target, &AtomicBool::new(false), |f| seen.push(f)).unwrap();
            assert!(std::fs::metadata(&out).unwrap().len() > 0, "{target:?}");
            assert!(seen.iter().all(|f| (0.0..=1.0).contains(f)));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}