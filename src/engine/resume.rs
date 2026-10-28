//! The resume store: a JSON sidecar next to the partial download that records
//! how far each segment has progressed, so an interrupted download can pick up
//! where it left off instead of starting over.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::segments::Segment;

/// Per-segment progress. `done` counts bytes already written within the
/// segment, so the next byte to fetch is `start + done`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SegmentProgress {
    pub start: u64,
    pub end: u64,
    pub done: u64,
}

impl SegmentProgress {
    /// Byte offset of the next unfetched byte in this segment.
    pub fn cursor(&self) -> u64 {
        self.start + self.done
    }

    /// Total bytes this segment must fetch.
    pub fn len(&self) -> u64 {
        self.end - self.start + 1
    }

    pub fn is_complete(&self) -> bool {
        self.done >= self.len()
    }
}

/// The full state needed to resume a download.
///
/// `etag` and `last_modified` are kept so a resume can refuse to stitch new
/// bytes onto an old partial file when the remote resource has changed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub url: String,
    pub total: u64,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub segments: Vec<SegmentProgress>,
}

impl Manifest {
    pub fn new(
        url: String,
        total: u64,
        etag: Option<String>,
        last_modified: Option<String>,
        segments: &[Segment],
    ) -> Self {
        Manifest {
            url,
            total,
            etag,
            last_modified,
            segments: segments
                .iter()
                .map(|s| SegmentProgress {
                    start: s.start,
                    end: s.end,
                    done: 0,
                })
                .collect(),
        }
    }

    /// Bytes fetched across all segments.
    pub fn bytes_done(&self) -> u64 {
        self.segments.iter().map(|s| s.done).sum()
    }

    /// Whether every segment is finished. The engine tracks completion through
    /// its error slot instead, so this is here for callers and tests.
    #[allow(dead_code)]
    pub fn is_complete(&self) -> bool {
        self.segments.iter().all(SegmentProgress::is_complete)
    }

    /// Decide whether a loaded manifest still describes the same remote file
    /// probed now. A changed total, ETag, or Last-Modified means the partial
    /// bytes are stale and the download must restart. With `ignore_date`, a
    /// changed Last-Modified alone is not taken as a new file, for servers
    /// that report a new date on every request.
    pub fn matches(&self, total: u64, etag: Option<&str>, last_modified: Option<&str>, ignore_date: bool) -> bool {
        if self.total != total {
            return false;
        }
        match (self.etag.as_deref(), etag) {
            (Some(a), Some(b)) => return a == b,
            _ => {}
        }
        if ignore_date {
            return true;
        }
        match (self.last_modified.as_deref(), last_modified) {
            (Some(a), Some(b)) => a == b,
            // No validators to compare: fall back to trusting the size match.
            _ => true,
        }
    }

    /// The sidecar path for a given output file: `<output>.uidmpart`.
    pub fn sidecar_path(output: &Path) -> PathBuf {
        let mut name = output.file_name().unwrap_or_default().to_os_string();
        name.push(".uidmpart");
        output.with_file_name(name)
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Write the sidecar atomically: serialize to a temp file in the same
    /// directory, then rename over the target so a crash never leaves a
    /// half-written manifest.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let tmp = path.with_extension("uidmpart.tmp");
        let bytes = serde_json::to_vec(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(&tmp, &bytes)?;
        // rename is atomic on the same volume; fall back to remove+rename on
        // Windows where rename onto an existing file can fail.
        match fs::rename(&tmp, path) {
            Ok(()) => Ok(()),
            Err(_) => {
                let _ = fs::remove_file(path);
                fs::rename(&tmp, path)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        let segs = super::super::segments::split(1000, 4);
        Manifest::new("https://example.com/x".into(), 1000, Some("\"abc\"".into()), None, &segs)
    }

    #[test]
    fn cursor_and_completion() {
        let mut m = sample();
        assert_eq!(m.bytes_done(), 0);
        assert!(!m.is_complete());
        assert_eq!(m.segments[0].cursor(), 0);

        m.segments[0].done = 100;
        assert_eq!(m.segments[0].cursor(), 100);

        for s in &mut m.segments {
            s.done = s.len();
        }
        assert!(m.is_complete());
        assert_eq!(m.bytes_done(), 1000);
    }

    #[test]
    fn matches_rejects_changed_size_or_etag() {
        let m = sample();
        assert!(m.matches(1000, Some("\"abc\""), None, false));
        assert!(!m.matches(2000, Some("\"abc\""), None, false));
        assert!(!m.matches(1000, Some("\"zzz\""), None, false));
        // No etag to compare, same size: trusted.
        let no_etag = Manifest::new("u".into(), 1000, None, None, &[]);
        assert!(no_etag.matches(1000, None, None, false));
    }

    #[test]
    fn ignoring_the_date_still_checks_the_size_and_etag() {
        let dated = Manifest::new("u".into(), 1000, None, Some("Mon, 01 Jan 2024 00:00:00 GMT".into()), &[]);
        let later = Some("Tue, 02 Jan 2024 00:00:00 GMT");
        assert!(!dated.matches(1000, None, later, false));
        assert!(dated.matches(1000, None, later, true));
        assert!(!dated.matches(2000, None, later, true));
        let tagged = sample();
        assert!(!tagged.matches(1000, Some("\"zzz\""), later, true));
    }

    #[test]
    fn save_load_roundtrip() {
        let mut path = std::env::temp_dir();
        path.push(format!("uidm_test_{}.uidmpart", std::process::id()));
        let m = sample();
        m.save(&path).unwrap();
        let loaded = Manifest::load(&path).unwrap();
        assert_eq!(m, loaded);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn sidecar_naming() {
        let p = Path::new("C:\\dl\\movie.mp4");
        assert_eq!(
            Manifest::sidecar_path(p).file_name().unwrap(),
            "movie.mp4.uidmpart"
        );
    }
}
