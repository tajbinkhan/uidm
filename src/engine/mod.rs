//! The download engine: probe a URL, split it into byte ranges, fetch the
//! ranges on parallel threads into one file, and persist progress so an
//! interrupted download resumes instead of restarting.
//!
//! Downloads run through a bounded worker pool (`Engine`). Each download has an
//! id, a cancel token, and a segment thread per range. Segments retry transient
//! failures with backoff and resume from their own cursor. Progress is reported
//! through a cloneable `Reporter` so the host can forward it to the browser.
//!
//! Note on lifetime: running the engine inside the native-messaging host means
//! downloads stop when the browser closes the port. A production build would
//! move this engine into a long-lived desktop process and keep the host as a
//! thin relay. The engine is a self-contained module precisely so it can move.

mod http;
mod resume;
mod segments;

use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::fs::FileExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use resume::Manifest;
use segments::split;

/// Smallest file worth splitting; below this, one connection is used.
const MIN_SEGMENT_BYTES: u64 = 256 * 1024;
/// Hard cap on parallel connections per download.
const MAX_CONNECTIONS: usize = 16;
/// Read buffer per segment.
const BUF_SIZE: usize = 64 * 1024;
/// Report progress and flush the resume manifest at most this often, in bytes.
const REPORT_EVERY: u64 = 1024 * 1024;
/// Transient-failure retries per segment before giving up.
const MAX_RETRIES: u32 = 5;
/// Backoff base and ceiling for retries.
const RETRY_BASE_MS: u64 = 400;
const RETRY_CAP_MS: u64 = 10_000;

/// A cancel flag shared between the engine and a download's threads.
type CancelToken = Arc<AtomicBool>;

/// A download to perform.
pub struct DownloadRequest {
    pub url: String,
    pub output_dir: PathBuf,
    /// Suggested filename; derived from the server or URL when absent.
    pub filename: Option<String>,
    /// Extra request headers (user agent, cookies, referrer, explicit headers).
    pub headers: Vec<(String, String)>,
    /// Desired parallel connections; clamped to file size and `MAX_CONNECTIONS`.
    pub connections: usize,
}

/// Progress events over the course of a download. `id` matches the `Ack`.
pub enum Progress {
    Started {
        id: u64,
        url: String,
        total: Option<u64>,
        resumable: bool,
        connections: usize,
    },
    Advanced {
        id: u64,
        url: String,
        done: u64,
        total: Option<u64>,
    },
    Finished {
        id: u64,
        url: String,
        path: PathBuf,
    },
    Failed {
        id: u64,
        url: String,
        error: String,
    },
    Canceled {
        id: u64,
        url: String,
    },
}

#[derive(Debug)]
pub enum EngineError {
    Http(String),
    Tls(String),
    Io(String),
    Protocol(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::Http(s) => write!(f, "http error: {s}"),
            EngineError::Tls(s) => write!(f, "tls error: {s}"),
            EngineError::Io(s) => write!(f, "io error: {s}"),
            EngineError::Protocol(s) => write!(f, "protocol error: {s}"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<std::io::Error> for EngineError {
    fn from(e: std::io::Error) -> Self {
        EngineError::Io(e.to_string())
    }
}

/// A cloneable progress sink. `&dyn Fn` is callable, so calling through the
/// `Arc` is done via `as_ref()`.
#[derive(Clone)]
pub struct Reporter(Arc<dyn Fn(Progress) + Send + Sync>);

impl Reporter {
    fn send(&self, p: Progress) {
        (self.0.as_ref())(p)
    }
}

/// One queued unit of work.
struct Job {
    id: u64,
    token: CancelToken,
    req: DownloadRequest,
}

/// A bounded, cancelable download queue. `new` starts `concurrency` workers
/// that pull jobs off a shared channel, so at most that many downloads run at
/// once. Each download gets an id; `cancel(id)` stops it. Dropping the engine
/// closes the channel and the workers exit.
pub struct Engine {
    jobs: Sender<Job>,
    counter: AtomicU64,
    registry: Arc<Mutex<HashMap<u64, CancelToken>>>,
}

impl Engine {
    pub fn new(cb: Arc<dyn Fn(Progress) + Send + Sync>, concurrency: usize) -> Self {
        let (jobs_tx, jobs_rx) = channel::<Job>();
        let jobs_rx = Arc::new(Mutex::new(jobs_rx));
        let registry: Arc<Mutex<HashMap<u64, CancelToken>>> = Arc::new(Mutex::new(HashMap::new()));
        let reporter = Reporter(cb);

        for _ in 0..concurrency.max(1) {
            let jobs_rx = Arc::clone(&jobs_rx);
            let reporter = reporter.clone();
            let registry = Arc::clone(&registry);
            thread::spawn(move || loop {
                let job = {
                    let rx = jobs_rx.lock().unwrap();
                    rx.recv()
                };
                match job {
                    Ok(job) => {
                        let id = job.id;
                        run(job.id, job.token, job.req, reporter.clone());
                        registry.lock().unwrap().remove(&id);
                    }
                    Err(_) => break, // channel closed: engine dropped.
                }
            });
        }

        Engine {
            jobs: jobs_tx,
            counter: AtomicU64::new(1),
            registry,
        }
    }

    /// Queue `req` and return its id. It starts when a worker is free.
    pub fn enqueue(&self, req: DownloadRequest) -> u64 {
        let id = self.counter.fetch_add(1, Ordering::Relaxed);
        let token: CancelToken = Arc::new(AtomicBool::new(false));
        self.registry.lock().unwrap().insert(id, Arc::clone(&token));
        let _ = self.jobs.send(Job { id, token, req });
        id
    }

    /// Signal a download to stop. Unknown ids are ignored.
    pub fn cancel(&self, id: u64) {
        if let Some(token) = self.registry.lock().unwrap().get(&id) {
            token.store(true, Ordering::Relaxed);
        }
    }
}

/// Run a download to completion on the calling thread (CLI test mode).
pub fn run_blocking(req: DownloadRequest, cb: Arc<dyn Fn(Progress) + Send + Sync>) {
    run(0, Arc::new(AtomicBool::new(false)), req, Reporter(cb));
}

/// Run one download to completion (blocking), reporting a terminal event.
fn run(id: u64, token: CancelToken, req: DownloadRequest, reporter: Reporter) {
    let url = req.url.clone();
    let result = run_inner(id, &token, &req, &reporter);
    if token.load(Ordering::Relaxed) {
        reporter.send(Progress::Canceled { id, url });
        return;
    }
    match result {
        Ok(path) => reporter.send(Progress::Finished { id, url, path }),
        Err(e) => reporter.send(Progress::Failed {
            id,
            url,
            error: e.to_string(),
        }),
    }
}

fn run_inner(
    id: u64,
    token: &CancelToken,
    req: &DownloadRequest,
    reporter: &Reporter,
) -> Result<PathBuf, EngineError> {
    let agent = http::build_agent()?;
    let probe = http::probe(&agent, &req.url, &req.headers)?;

    // Filename priority: explicit request, then Content-Disposition, then the
    // URL path, then a generic fallback.
    let filename = req
        .filename
        .as_deref()
        .map(sanitize_filename)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            probe
                .suggested_filename
                .as_deref()
                .map(sanitize_filename)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| derive_filename(&probe.final_url));
    let output = req.output_dir.join(filename);
    fs::create_dir_all(&req.output_dir)?;

    let segmented = probe.resumable && probe.total.map_or(false, |t| t > 0);
    let connections = if segmented {
        effective_connections(probe.total.unwrap(), req.connections)
    } else {
        1
    };

    reporter.send(Progress::Started {
        id,
        url: req.url.clone(),
        total: probe.total,
        resumable: probe.resumable,
        connections,
    });

    if segmented {
        run_segmented(id, token, &agent, req, output, &probe, connections, reporter)
    } else {
        run_single(id, token, &agent, req, output, &probe, reporter)
    }
}

/// Parallel, resumable path for a server that honors byte ranges.
#[allow(clippy::too_many_arguments)]
fn run_segmented(
    id: u64,
    token: &CancelToken,
    agent: &ureq::Agent,
    req: &DownloadRequest,
    output: PathBuf,
    probe: &http::Probe,
    connections: usize,
    reporter: &Reporter,
) -> Result<PathBuf, EngineError> {
    let total = probe.total.expect("segmented path requires a known total");
    let url = probe.final_url.clone();
    let sidecar = Manifest::sidecar_path(&output);

    let manifest = match Manifest::load(&sidecar) {
        Ok(m)
            if !m.segments.is_empty()
                && m.matches(total, probe.etag.as_deref(), probe.last_modified.as_deref()) =>
        {
            m
        }
        _ => Manifest::new(
            url.clone(),
            total,
            probe.etag.clone(),
            probe.last_modified.clone(),
            &split(total, connections),
        ),
    };

    ensure_file_len(&output, total)?;
    manifest.save(&sidecar).ok();

    let segment_count = manifest.segments.len();
    let resumed = manifest.bytes_done();
    let shared = Arc::new(Mutex::new(manifest));
    let global_done = Arc::new(AtomicU64::new(resumed));
    let last_report = Arc::new(AtomicU64::new(resumed));
    let first_error: Arc<Mutex<Option<EngineError>>> = Arc::new(Mutex::new(None));

    let mut handles = Vec::new();
    for idx in 0..segment_count {
        if shared.lock().unwrap().segments[idx].is_complete() {
            continue;
        }
        let agent = agent.clone();
        let url = url.clone();
        let headers = req.headers.clone();
        let output = output.clone();
        let sidecar = sidecar.clone();
        let shared = Arc::clone(&shared);
        let global_done = Arc::clone(&global_done);
        let last_report = Arc::clone(&last_report);
        let first_error = Arc::clone(&first_error);
        let reporter = reporter.clone();
        let token = Arc::clone(token);

        handles.push(thread::spawn(move || {
            let ctx = SegmentCtx {
                id,
                token: &token,
                agent: &agent,
                url: &url,
                headers: &headers,
                output: &output,
                sidecar: &sidecar,
                shared: &shared,
                global_done: &global_done,
                last_report: &last_report,
                reporter: &reporter,
                total,
            };
            if let Err(e) = download_segment(&ctx, idx) {
                let mut slot = first_error.lock().unwrap();
                if slot.is_none() {
                    *slot = Some(e);
                }
            }
        }));
    }

    for h in handles {
        let _ = h.join();
    }

    if token.load(Ordering::Relaxed) {
        // Keep the sidecar and partial file so a later request resumes.
        return Ok(output);
    }
    if let Some(err) = first_error.lock().unwrap().take() {
        return Err(err);
    }

    let done = shared.lock().unwrap().bytes_done();
    reporter.send(Progress::Advanced {
        id,
        url: url.clone(),
        done,
        total: Some(total),
    });
    let _ = fs::remove_file(&sidecar);
    Ok(output)
}

/// Shared references a segment thread needs; grouped to keep signatures short.
struct SegmentCtx<'a> {
    id: u64,
    token: &'a CancelToken,
    agent: &'a ureq::Agent,
    url: &'a str,
    headers: &'a [(String, String)],
    output: &'a std::path::Path,
    sidecar: &'a std::path::Path,
    shared: &'a Arc<Mutex<Manifest>>,
    global_done: &'a AtomicU64,
    last_report: &'a AtomicU64,
    reporter: &'a Reporter,
    total: u64,
}

/// Fetch one segment, retrying transient failures with backoff. Each attempt
/// resumes from the segment's current cursor, so a retry refetches only what is
/// still missing. Stops early if the download is canceled.
fn download_segment(ctx: &SegmentCtx, idx: usize) -> Result<(), EngineError> {
    let mut attempt = 0u32;
    loop {
        if ctx.token.load(Ordering::Relaxed) {
            return Ok(());
        }
        match download_segment_once(ctx, idx) {
            Ok(()) => return Ok(()),
            Err(e) => {
                if ctx.token.load(Ordering::Relaxed) {
                    return Ok(());
                }
                attempt += 1;
                if attempt > MAX_RETRIES {
                    return Err(e);
                }
                thread::sleep(backoff(attempt));
            }
        }
    }
}

fn download_segment_once(ctx: &SegmentCtx, idx: usize) -> Result<(), EngineError> {
    let (start, end, mut cursor) = {
        let m = ctx.shared.lock().unwrap();
        let s = &m.segments[idx];
        (s.start, s.end, s.cursor())
    };
    if cursor > end {
        return Ok(());
    }

    let mut reader = http::fetch_range(ctx.agent, ctx.url, ctx.headers, cursor, end)?;
    // A private handle per thread gives each its own file pointer, so the
    // positioned writes below never race one another.
    let file = OpenOptions::new().write(true).open(ctx.output)?;
    let mut buf = vec![0u8; BUF_SIZE];

    loop {
        if ctx.token.load(Ordering::Relaxed) {
            return Ok(());
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        // Never write past the segment end, even if the server over-sends.
        let room = (end - cursor + 1) as usize;
        let n = n.min(room);
        write_at(&file, &buf[..n], cursor)?;
        cursor += n as u64;

        {
            let mut m = ctx.shared.lock().unwrap();
            m.segments[idx].done = cursor - start;
        }
        let g = ctx.global_done.fetch_add(n as u64, Ordering::Relaxed) + n as u64;
        maybe_report(ctx, g);

        if cursor > end {
            break;
        }
    }
    Ok(())
}

/// Emit an Advanced event and flush the manifest when global progress has moved
/// at least `REPORT_EVERY` bytes since the last report.
fn maybe_report(ctx: &SegmentCtx, global: u64) {
    let last = ctx.last_report.load(Ordering::Relaxed);
    if global.saturating_sub(last) < REPORT_EVERY {
        return;
    }
    if ctx
        .last_report
        .compare_exchange(last, global, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return; // another thread reported; skip to avoid a duplicate.
    }
    ctx.reporter.send(Progress::Advanced {
        id: ctx.id,
        url: ctx.url.to_string(),
        done: global,
        total: Some(ctx.total),
    });
    if let Ok(m) = ctx.shared.lock() {
        m.save(ctx.sidecar).ok();
    }
}

/// Single-stream path for a server without range support. Not resumable, so a
/// transient failure restarts from the beginning, up to `MAX_RETRIES`.
#[allow(clippy::too_many_arguments)]
fn run_single(
    id: u64,
    token: &CancelToken,
    agent: &ureq::Agent,
    req: &DownloadRequest,
    output: PathBuf,
    probe: &http::Probe,
    reporter: &Reporter,
) -> Result<PathBuf, EngineError> {
    let mut attempt = 0u32;
    loop {
        if token.load(Ordering::Relaxed) {
            return Ok(output);
        }
        match single_once(id, token, agent, req, &output, probe, reporter) {
            Ok(()) => return Ok(output),
            Err(e) => {
                if token.load(Ordering::Relaxed) {
                    return Ok(output);
                }
                attempt += 1;
                if attempt > MAX_RETRIES {
                    return Err(e);
                }
                thread::sleep(backoff(attempt));
            }
        }
    }
}

fn single_once(
    id: u64,
    token: &CancelToken,
    agent: &ureq::Agent,
    req: &DownloadRequest,
    output: &std::path::Path,
    probe: &http::Probe,
    reporter: &Reporter,
) -> Result<(), EngineError> {
    let mut reader = http::fetch_whole(agent, &probe.final_url, &req.headers)?;
    let mut file = File::create(output)?;
    let mut buf = vec![0u8; BUF_SIZE];
    let mut done = 0u64;
    let mut last = 0u64;

    loop {
        if token.load(Ordering::Relaxed) {
            return Ok(());
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        if done - last >= REPORT_EVERY {
            last = done;
            reporter.send(Progress::Advanced {
                id,
                url: probe.final_url.clone(),
                done,
                total: probe.total,
            });
        }
    }
    Ok(())
}

/// Positioned write that does not disturb a shared cursor. Loops because a
/// single call may write fewer bytes than requested.
fn write_at(file: &File, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
    while !buf.is_empty() {
        let n = file.seek_write(buf, offset)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "seek_write wrote nothing",
            ));
        }
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

/// Create the output file if needed and size it to `total` without truncating
/// bytes an earlier run already wrote.
fn ensure_file_len(output: &std::path::Path, total: u64) -> std::io::Result<()> {
    let file = OpenOptions::new().create(true).write(true).open(output)?;
    if file.metadata()?.len() != total {
        file.set_len(total)?;
    }
    Ok(())
}

/// Backoff for retry `attempt` (1-based): 400ms, 800ms, 1.6s, ... capped at 10s.
fn backoff(attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(6);
    let ms = RETRY_BASE_MS.saturating_mul(1u64 << shift).min(RETRY_CAP_MS);
    Duration::from_millis(ms)
}

/// Clamp requested connections to [1, MAX_CONNECTIONS] and to roughly one per
/// `MIN_SEGMENT_BYTES` of file.
fn effective_connections(total: u64, requested: usize) -> usize {
    let by_size = (total / MIN_SEGMENT_BYTES).max(1) as usize;
    requested.clamp(1, MAX_CONNECTIONS).min(by_size)
}

/// Pull a filename out of a URL's path. Falls back to `download`.
fn derive_filename(url: &str) -> String {
    let no_query = url.split(['?', '#']).next().unwrap_or(url);
    let tail = no_query.rsplit('/').next().unwrap_or("");
    let name = sanitize_filename(tail);
    if name.is_empty() {
        "download".to_string()
    } else {
        name
    }
}

/// Strip characters Windows forbids in filenames.
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .trim_matches('.')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_connections_bounds() {
        assert_eq!(effective_connections(1000, 8), 1);
        assert_eq!(effective_connections(100 * 1024 * 1024, 8), 8);
        assert_eq!(effective_connections(100 * 1024 * 1024, 999), MAX_CONNECTIONS);
        assert_eq!(effective_connections(100 * 1024 * 1024, 0), 1);
    }

    #[test]
    fn derive_filename_from_url() {
        assert_eq!(derive_filename("https://x.com/a/b/movie.mp4"), "movie.mp4");
        assert_eq!(derive_filename("https://x.com/file.zip?token=1"), "file.zip");
        assert_eq!(derive_filename("https://x.com/"), "download");
    }

    #[test]
    fn sanitize_strips_bad_chars() {
        assert_eq!(sanitize_filename("a:b/c*?.txt"), "a_b_c__.txt");
    }

    #[test]
    fn backoff_grows_then_caps() {
        assert_eq!(backoff(1), Duration::from_millis(400));
        assert_eq!(backoff(2), Duration::from_millis(800));
        assert_eq!(backoff(3), Duration::from_millis(1600));
        // Caps at 10s regardless of attempt.
        assert_eq!(backoff(20), Duration::from_millis(RETRY_CAP_MS));
    }

    #[test]
    fn cancel_unknown_id_is_safe() {
        // No jobs are enqueued, so no download runs; cancel on an absent id
        // must simply do nothing.
        let engine = Engine::new(Arc::new(|_p: Progress| {}), 1);
        engine.cancel(999);
        assert!(engine.registry.lock().unwrap().is_empty());
    }
}
