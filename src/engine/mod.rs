//! The download engine: probe a URL, split it into byte ranges, fetch the
//! ranges on parallel threads into one file, and persist progress so an
//! interrupted download resumes instead of restarting.
//!
//! Downloads run through a queue (`Engine`) that starts at most a set number
//! at once; the number can change while downloads run. Each download has an
//! id, a cancel token, and a segment thread per range. Segments retry transient
//! failures with backoff and resume from their own cursor. Every read passes
//! through one shared `RateLimiter`, so a speed limit covers all downloads.
//! Progress is reported through a cloneable `Reporter`.

mod http;
mod limit;
mod resume;
mod segments;
#[cfg(test)]
mod testserver;

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::settings::FileExists;
use limit::RateLimiter;
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
    /// A file an earlier attempt of this same download wrote (a retry or a
    /// resume from history). It is reused, and resumed when its sidecar still
    /// matches, instead of being treated as somebody else's file.
    pub reuse_path: Option<PathBuf>,
    /// What to do when the chosen name is taken by an unrelated file.
    pub file_exists: FileExists,
}

/// Progress events over the course of a download. `id` matches the `Ack`.
pub enum Progress {
    Started {
        id: u64,
        url: String,
        /// The file being written.
        path: PathBuf,
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
    /// The server answered with an error status.
    Status(u16),
    Http(String),
    Tls(String),
    Io(String),
    Protocol(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::Status(code) => write!(f, "server returned HTTP {code}"),
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

/// Which jobs wait and how many run.
struct Sched {
    queue: VecDeque<Job>,
    running: usize,
    limit: usize,
}

/// What every running download shares.
struct Shared {
    limiter: RateLimiter,
    /// Output files in use, so two downloads never pick the same file.
    claimed: Mutex<HashSet<PathBuf>>,
}

struct Inner {
    sched: Mutex<Sched>,
    registry: Mutex<HashMap<u64, CancelToken>>,
    shared: Arc<Shared>,
    reporter: Reporter,
}

/// A cancelable download queue. At most `concurrency` downloads run at once,
/// each on its own thread; the rest wait in order. `set_concurrency` changes
/// the limit live: raising it starts waiting downloads at once, lowering it
/// lets running ones finish and starts no more until under the new limit.
pub struct Engine {
    inner: Arc<Inner>,
    counter: AtomicU64,
}

impl Engine {
    pub fn new(cb: Arc<dyn Fn(Progress) + Send + Sync>, concurrency: usize) -> Self {
        Engine {
            inner: Arc::new(Inner {
                sched: Mutex::new(Sched {
                    queue: VecDeque::new(),
                    running: 0,
                    limit: concurrency.max(1),
                }),
                registry: Mutex::new(HashMap::new()),
                shared: Arc::new(Shared {
                    limiter: RateLimiter::default(),
                    claimed: Mutex::new(HashSet::new()),
                }),
                reporter: Reporter(cb),
            }),
            counter: AtomicU64::new(1),
        }
    }

    /// Queue `req` and return its id. It starts when a slot is free.
    pub fn enqueue(&self, req: DownloadRequest) -> u64 {
        let id = self.counter.fetch_add(1, Ordering::Relaxed);
        let token: CancelToken = Arc::new(AtomicBool::new(false));
        self.inner.registry.lock().unwrap().insert(id, Arc::clone(&token));
        self.inner.sched.lock().unwrap().queue.push_back(Job { id, token, req });
        pump(&self.inner);
        id
    }

    /// Make the next id at least `next`, so ids keep increasing across
    /// restarts and never match one saved in the history.
    pub fn set_next_id(&self, next: u64) {
        self.counter.fetch_max(next.max(1), Ordering::Relaxed);
    }

    /// The id the next download will get.
    pub fn next_id(&self) -> u64 {
        self.counter.load(Ordering::Relaxed)
    }

    /// Change how many downloads run at once. Running downloads are never
    /// interrupted.
    pub fn set_concurrency(&self, n: usize) {
        self.inner.sched.lock().unwrap().limit = n.max(1);
        pump(&self.inner);
    }

    /// Set the total speed limit in bytes per second, or `None` for no limit.
    pub fn set_speed_limit(&self, bytes_per_sec: Option<u64>) {
        self.inner.shared.limiter.set_rate(bytes_per_sec);
    }

    /// Downloads queued or running. Each leaves the registry when it finishes,
    /// fails or is canceled.
    pub fn active_count(&self) -> usize {
        self.inner.registry.lock().unwrap().len()
    }

    /// Stop a download. A queued one is dropped from the queue and reported
    /// canceled at once; a running one stops at its next read. Unknown ids are
    /// ignored.
    pub fn cancel(&self, id: u64) {
        let Some(token) = self.inner.registry.lock().unwrap().get(&id).cloned() else {
            return;
        };
        token.store(true, Ordering::Relaxed);
        let queued = {
            let mut sched = self.inner.sched.lock().unwrap();
            let pos = sched.queue.iter().position(|j| j.id == id);
            pos.and_then(|p| sched.queue.remove(p))
        };
        if let Some(job) = queued {
            self.inner.registry.lock().unwrap().remove(&id);
            self.inner.reporter.send(Progress::Canceled { id, url: job.req.url });
        }
    }
}

/// Start waiting jobs while there are free slots.
fn pump(inner: &Arc<Inner>) {
    loop {
        let job = {
            let mut sched = inner.sched.lock().unwrap();
            if sched.running >= sched.limit {
                return;
            }
            match sched.queue.pop_front() {
                Some(job) => {
                    sched.running += 1;
                    job
                }
                None => return,
            }
        };
        let inner = Arc::clone(inner);
        thread::spawn(move || {
            let id = job.id;
            run(job.id, job.token, job.req, inner.reporter.clone(), &inner.shared);
            inner.registry.lock().unwrap().remove(&id);
            inner.sched.lock().unwrap().running -= 1;
            pump(&inner);
        });
    }
}

/// Run a download to completion on the calling thread (CLI test mode).
pub fn run_blocking(req: DownloadRequest, cb: Arc<dyn Fn(Progress) + Send + Sync>) {
    let shared = Shared {
        limiter: RateLimiter::default(),
        claimed: Mutex::new(HashSet::new()),
    };
    run(0, Arc::new(AtomicBool::new(false)), req, Reporter(cb), &shared);
}

/// Statuses that, on a retried or resumed download, usually mean the link or
/// the sign-in it relied on has expired.
const EXPIRED_STATUSES: [u16; 4] = [401, 403, 404, 410];

/// Run one download to completion (blocking), reporting a terminal event.
fn run(id: u64, token: CancelToken, req: DownloadRequest, reporter: Reporter, shared: &Shared) {
    let url = req.url.clone();
    if token.load(Ordering::Relaxed) {
        reporter.send(Progress::Canceled { id, url });
        return;
    }
    let result = run_inner(id, &token, &req, &reporter, shared);
    if token.load(Ordering::Relaxed) {
        reporter.send(Progress::Canceled { id, url });
        return;
    }
    match result {
        Ok(path) => reporter.send(Progress::Finished { id, url, path }),
        Err(e) => {
            let mut error = e.to_string();
            if let EngineError::Status(code) = e {
                if req.reuse_path.is_some() && EXPIRED_STATUSES.contains(&code) {
                    error.push_str(
                        ". The link or its sign-in may have expired; start the download again from its web page.",
                    );
                }
            }
            reporter.send(Progress::Failed { id, url, error })
        }
    }
}

fn run_inner(
    id: u64,
    token: &CancelToken,
    req: &DownloadRequest,
    reporter: &Reporter,
    shared: &Shared,
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
    fs::create_dir_all(&req.output_dir)?;
    let claim = choose_output(&shared.claimed, &req.output_dir, &filename, req, &probe)?;
    let output = claim.path.clone();

    let segmented = probe.resumable && probe.total.map_or(false, |t| t > 0);
    let connections = if segmented {
        effective_connections(probe.total.unwrap(), req.connections)
    } else {
        1
    };

    reporter.send(Progress::Started {
        id,
        url: req.url.clone(),
        path: output.clone(),
        total: probe.total,
        resumable: probe.resumable,
        connections,
    });

    let limiter = &shared.limiter;
    let result = if segmented {
        run_segmented(id, token, &agent, req, output, &probe, connections, reporter, limiter)
    } else {
        run_single(id, token, &agent, req, output, &probe, reporter, limiter)
    };
    drop(claim);
    result
}

/// An output file reserved for one download; released when dropped.
struct Claim<'a> {
    set: &'a Mutex<HashSet<PathBuf>>,
    path: PathBuf,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.set.lock().unwrap().remove(&self.path);
    }
}

/// Most `name (n).ext` variants tried before giving up.
const MAX_RENAMES: u32 = 9999;

/// Pick the file this download writes, never touching an unrelated file:
///
/// - the request's `reuse_path`, written by an earlier attempt of this same
///   download, is taken as is;
/// - otherwise the first of `name`, `name (1)`, `name (2)`, ... that is not
///   in use by another download and either does not exist, holds a partial
///   copy whose sidecar matches what the server reports now (a resume), or
///   may be overwritten under the `Overwrite` setting.
///
/// A sidecar left without its file is deleted, so it can never mark bytes of a
/// fresh file as already downloaded.
fn choose_output<'a>(
    claimed: &'a Mutex<HashSet<PathBuf>>,
    dir: &Path,
    filename: &str,
    req: &DownloadRequest,
    probe: &http::Probe,
) -> Result<Claim<'a>, EngineError> {
    let mut set = claimed.lock().unwrap();
    let path = pick_free_path(&set, dir, filename, req, probe).ok_or_else(|| {
        EngineError::Io(format!("no free file name for {filename} in {}", dir.display()))
    })?;
    set.insert(path.clone());
    Ok(Claim { set: claimed, path })
}

fn pick_free_path(
    claimed: &HashSet<PathBuf>,
    dir: &Path,
    filename: &str,
    req: &DownloadRequest,
    probe: &http::Probe,
) -> Option<PathBuf> {
    if let Some(path) = &req.reuse_path {
        if !claimed.contains(path) {
            return Some(path.clone());
        }
    }

    for n in 0..=MAX_RENAMES {
        let path = dir.join(numbered_name(filename, n));
        if claimed.contains(&path) {
            continue;
        }
        let sidecar = Manifest::sidecar_path(&path);
        if !path.exists() {
            let _ = fs::remove_file(&sidecar);
            return Some(path);
        }
        if sidecar_matches(&sidecar, probe) {
            return Some(path);
        }
        if req.file_exists == FileExists::Overwrite && path.is_file() {
            let _ = fs::remove_file(&sidecar);
            return Some(path);
        }
    }
    None
}

/// `report.pdf` for 0, then `report (1).pdf`, `report (2).pdf`, ...
fn numbered_name(filename: &str, n: u32) -> String {
    if n == 0 {
        return filename.to_string();
    }
    let path = Path::new(filename);
    let stem = path.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default();
    match path.extension() {
        Some(ext) => format!("{stem} ({n}).{}", ext.to_string_lossy()),
        None => format!("{stem} ({n})"),
    }
}

/// Whether `sidecar` describes a partial copy of the file the probe found.
fn sidecar_matches(sidecar: &Path, probe: &http::Probe) -> bool {
    let (true, Some(total)) = (probe.resumable, probe.total) else {
        return false;
    };
    match Manifest::load(sidecar) {
        Ok(m) => {
            !m.segments.is_empty()
                && m.matches(total, probe.etag.as_deref(), probe.last_modified.as_deref())
        }
        Err(_) => false,
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
    limiter: &RateLimiter,
) -> Result<PathBuf, EngineError> {
    let total = probe.total.expect("segmented path requires a known total");
    let url = probe.final_url.clone();
    let sidecar = Manifest::sidecar_path(&output);

    // A sidecar only counts while its partial file is still there.
    let manifest = match Manifest::load(&sidecar) {
        Ok(m)
            if output.is_file()
                && !m.segments.is_empty()
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

    // Scoped threads may borrow the shared limiter.
    thread::scope(|scope| {
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

            scope.spawn(move || {
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
                    limiter,
                    total,
                };
                if let Err(e) = download_segment(&ctx, idx) {
                    let mut slot = first_error.lock().unwrap();
                    if slot.is_none() {
                        *slot = Some(e);
                    }
                }
            });
        }
    });

    if token.load(Ordering::Relaxed) {
        // Keep the sidecar and partial file so a later request resumes, saved
        // with the latest progress rather than the last periodic flush.
        shared.lock().unwrap().save(&sidecar).ok();
        return Ok(output);
    }
    if let Some(err) = first_error.lock().unwrap().take() {
        shared.lock().unwrap().save(&sidecar).ok();
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
    limiter: &'a RateLimiter,
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
        let want = ctx.limiter.chunk_size(buf.len());
        let n = reader.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        ctx.limiter.consume(n, ctx.token);
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
    limiter: &RateLimiter,
) -> Result<PathBuf, EngineError> {
    let mut attempt = 0u32;
    loop {
        if token.load(Ordering::Relaxed) {
            return Ok(output);
        }
        match single_once(id, token, agent, req, &output, probe, reporter, limiter) {
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

#[allow(clippy::too_many_arguments)]
fn single_once(
    id: u64,
    token: &CancelToken,
    agent: &ureq::Agent,
    req: &DownloadRequest,
    output: &std::path::Path,
    probe: &http::Probe,
    reporter: &Reporter,
    limiter: &RateLimiter,
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
        let want = limiter.chunk_size(buf.len());
        let n = reader.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        limiter.consume(n, token);
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
        assert!(engine.inner.registry.lock().unwrap().is_empty());
    }

    #[test]
    fn numbered_names() {
        assert_eq!(numbered_name("report.pdf", 0), "report.pdf");
        assert_eq!(numbered_name("report.pdf", 2), "report (2).pdf");
        assert_eq!(numbered_name("archive.tar.gz", 1), "archive.tar (1).gz");
        assert_eq!(numbered_name("README", 1), "README (1)");
        assert_eq!(numbered_name(".bashrc", 1), ".bashrc (1)");
    }

    /// End-to-end checks against a local server.
    mod live {
        use super::super::testserver::{body, TestServer};
        use super::*;
        use std::sync::mpsc;
        use std::time::Instant;

        /// A fresh, empty folder for one test.
        fn temp_dir(name: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!("uidm_engine_{}_{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn request(url: &str, dir: &Path, file_exists: FileExists) -> DownloadRequest {
            DownloadRequest {
                url: url.to_string(),
                output_dir: dir.to_path_buf(),
                filename: Some("report.pdf".into()),
                headers: Vec::new(),
                connections: 4,
                reuse_path: None,
                file_exists,
            }
        }

        /// Run one download through an engine and return where it was saved.
        fn download(engine_setup: impl FnOnce(&Engine), req: DownloadRequest) -> PathBuf {
            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            let engine = Engine::new(
                Arc::new(move |p: Progress| {
                    let _ = tx.lock().unwrap().send(p);
                }),
                1,
            );
            engine_setup(&engine);
            engine.enqueue(req);
            loop {
                match rx.recv_timeout(Duration::from_secs(30)).expect("download timed out") {
                    Progress::Finished { path, .. } => return path,
                    Progress::Failed { error, .. } => panic!("download failed: {error}"),
                    Progress::Canceled { .. } => panic!("download canceled"),
                    _ => {}
                }
            }
        }

        const LEN: usize = 2 * 1024 * 1024;

        #[test]
        fn an_existing_file_is_never_overwritten() {
            let server = TestServer::start(body(LEN));
            let dir = temp_dir("existing");
            fs::write(dir.join("report.pdf"), b"someone else's report").unwrap();
            fs::write(dir.join("report (1).pdf"), b"another one").unwrap();

            let path = download(|_| {}, request(&server.url, &dir, FileExists::Rename));
            assert_eq!(path, dir.join("report (2).pdf"));
            assert_eq!(fs::read(&path).unwrap(), body(LEN));
            assert_eq!(fs::read(dir.join("report.pdf")).unwrap(), b"someone else's report");
            assert_eq!(fs::read(dir.join("report (1).pdf")).unwrap(), b"another one");
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn a_stale_sidecar_does_not_claim_an_existing_file() {
            let server = TestServer::start(body(LEN));
            let dir = temp_dir("stale");
            let existing = dir.join("report.pdf");
            fs::write(&existing, b"old").unwrap();
            // A sidecar for a different file: wrong size.
            Manifest::new(server.url.clone(), 999, None, None, &split(999, 2))
                .save(&Manifest::sidecar_path(&existing))
                .unwrap();

            let path = download(|_| {}, request(&server.url, &dir, FileExists::Rename));
            assert_eq!(path, dir.join("report (1).pdf"));
            assert_eq!(fs::read(&path).unwrap(), body(LEN));
            assert_eq!(fs::read(&existing).unwrap(), b"old");
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn a_matching_sidecar_still_resumes() {
            let data = body(LEN);
            let server = TestServer::start(data.clone());
            let dir = temp_dir("resume");
            let partial = dir.join("report.pdf");

            // The first half was fetched by an earlier run.
            let half = LEN / 2;
            let mut on_disk = data[..half].to_vec();
            on_disk.resize(LEN, 0);
            fs::write(&partial, &on_disk).unwrap();
            let mut m = Manifest::new(
                server.url.clone(),
                LEN as u64,
                Some("\"v1\"".into()),
                None,
                &split(LEN as u64, 2),
            );
            m.segments[0].done = m.segments[0].len();
            m.save(&Manifest::sidecar_path(&partial)).unwrap();

            let path = download(|_| {}, request(&server.url, &dir, FileExists::Rename));
            assert_eq!(path, partial);
            assert_eq!(fs::read(&path).unwrap(), data);
            assert!(!Manifest::sidecar_path(&partial).exists());
            // Only the probe byte and the missing half came over the wire.
            assert_eq!(server.served.load(Ordering::Relaxed), 1 + (LEN - half) as u64);
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn overwrite_setting_replaces_the_file() {
            let server = TestServer::start(body(LEN));
            let dir = temp_dir("overwrite");
            let existing = dir.join("report.pdf");
            fs::write(&existing, vec![7u8; LEN + 1000]).unwrap();

            let path = download(|_| {}, request(&server.url, &dir, FileExists::Overwrite));
            assert_eq!(path, existing);
            assert_eq!(fs::read(&path).unwrap(), body(LEN));
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn speed_limit_holds_a_download_within_ten_percent() {
            // 2 MiB at 1 MiB/s over 4 connections: about 2 seconds.
            let server = TestServer::start(body(LEN));
            let dir = temp_dir("limit");
            let start = Instant::now();
            let path = download(
                |e| e.set_speed_limit(Some(1024 * 1024)),
                request(&server.url, &dir, FileExists::Rename),
            );
            let secs = start.elapsed().as_secs_f64();
            assert_eq!(fs::read(&path).unwrap(), body(LEN));
            assert!((secs - 2.0).abs() / 2.0 < 0.10, "took {secs:.2}s, expected about 2s");
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn raising_concurrency_starts_waiting_downloads() {
            let server = TestServer::start(body(LEN));
            let dir = temp_dir("concurrency");
            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            let engine = Engine::new(
                Arc::new(move |p: Progress| {
                    let _ = tx.lock().unwrap().send(p);
                }),
                1,
            );
            // Slow enough that the first download is still running.
            engine.set_speed_limit(Some(256 * 1024));
            let a = engine.enqueue(request(&server.url, &dir, FileExists::Rename));
            let b = engine.enqueue(request(&server.url, &dir, FileExists::Rename));
            let started = |rx: &mpsc::Receiver<Progress>| loop {
                if let Progress::Started { id, .. } = rx.recv_timeout(Duration::from_secs(10)).unwrap() {
                    return id;
                }
            };
            assert_eq!(started(&rx), a);
            thread::sleep(Duration::from_millis(300));
            assert!(rx.try_iter().all(|p| !matches!(p, Progress::Started { .. })));
            engine.set_concurrency(2);
            assert_eq!(started(&rx), b);
            engine.cancel(a);
            engine.cancel(b);
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn canceling_a_queued_download_reports_at_once() {
            let server = TestServer::start(body(LEN));
            let dir = temp_dir("queued");
            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            let engine = Engine::new(
                Arc::new(move |p: Progress| {
                    let _ = tx.lock().unwrap().send(p);
                }),
                1,
            );
            // The first download runs slowly; the second waits behind it.
            engine.set_speed_limit(Some(256 * 1024));
            let a = engine.enqueue(request(&server.url, &dir, FileExists::Rename));
            let b = engine.enqueue(request(&server.url, &dir, FileExists::Rename));
            engine.cancel(b);
            let canceled = rx
                .iter()
                .find_map(|p| match p {
                    Progress::Canceled { id, .. } => Some(id),
                    Progress::Finished { id, .. } => panic!("download {id} finished first"),
                    _ => None,
                })
                .unwrap();
            assert_eq!(canceled, b);
            assert_eq!(engine.active_count(), 1);
            engine.cancel(a);
            fs::remove_dir_all(&dir).ok();
        }
    }
}
