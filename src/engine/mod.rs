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

mod ftp;
mod http;
mod limit;
mod page;
mod resume;
mod segments;
mod stream;
#[cfg(test)]
mod testserver;

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::media::MediaChoice;
use crate::protocol::{SegmentInfo, SegmentState};
use crate::net::NetConfig;
use crate::settings::{FileExists, Folders};
use limit::{RateLimiter, Throttle};
use resume::Manifest;
use segments::split;

/// Smallest file worth splitting; below this, one connection is used.
const MIN_SEGMENT_BYTES: u64 = 256 * 1024;
/// A connection that finishes early splits the largest remaining range in
/// two, if both halves get at least this much. It is well above one read
/// (`BUF_SIZE`), so a read in flight never crosses into the new half.
const MIN_SPLIT_BYTES: u64 = 256 * 1024;
/// Hard cap on parallel connections per download.
const MAX_CONNECTIONS: usize = 16;
/// Read buffer per segment.
const BUF_SIZE: usize = 64 * 1024;
/// Report progress and flush the resume manifest at most this often, in bytes.
const REPORT_EVERY: u64 = 1024 * 1024;
/// Byte-driven reports carry each connection's progress at most this often.
const SEGMENTS_EVERY: Duration = Duration::from_millis(500);
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
    /// Where the file goes, chosen by its final name (its category).
    pub folders: Folders,
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
    /// The proxy and site logins to use.
    pub net: Arc<NetConfig>,
    /// A form body to send with POST instead of a GET. The response is saved
    /// as one stream, and the POST is sent once: it is not repeated on
    /// failure, since a POST may not be safe to repeat.
    pub post_body: Option<Vec<u8>>,
    /// Download `url` as a stream (an HLS playlist or DASH manifest) in this
    /// quality, joined into one video.
    pub media: Option<MediaChoice>,
    /// ffmpeg, for turning a stream into an .mp4. `None` saves what can be
    /// saved without it.
    pub ffmpeg: Option<PathBuf>,
    /// This download's own speed limit in bytes per second, on top of the
    /// total one.
    pub speed_limit: Option<u64>,
    /// Give the finished file the server's `Last-Modified` date.
    pub server_date: bool,
    /// Resume a partial file even if the server's `Last-Modified` changed,
    /// as long as its size (and ETag, when it sends one) did not.
    pub ignore_date: bool,
    /// Where partial files are written until they are complete; `None`
    /// writes them in place.
    pub temp_dir: Option<PathBuf>,
    /// Where an earlier attempt kept its partial file (or a stream's
    /// segments), when not at `reuse_path`: in a temporary folder, which may
    /// have changed since.
    pub reuse_partial: Option<PathBuf>,
    /// The most connections to this download's host, across every download
    /// to it; `None` for no limit beyond `connections`.
    pub host_limit: Option<usize>,
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
        /// Where the data goes until it is complete, when not at `path`.
        partial: Option<PathBuf>,
    },
    Advanced {
        id: u64,
        url: String,
        done: u64,
        total: Option<u64>,
        /// For a stream: segments fetched and segments in all.
        parts: Option<[u64; 2]>,
        /// For a download in byte ranges: each connection's range and state.
        segments: Option<Vec<SegmentInfo>>,
    },
    Finished {
        id: u64,
        url: String,
        path: PathBuf,
        /// Something the user should know about the result.
        note: Option<String>,
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
    /// The file name is taken and the request says to ask: nothing was
    /// written. `path` is the taken file.
    NameTaken {
        id: u64,
        url: String,
        path: PathBuf,
        total: Option<u64>,
    },
}

#[derive(Debug)]
pub enum EngineError {
    /// The server answered with an error status.
    Status(u16),
    /// The server asked for a login and none is set for this host; the
    /// realm it named, if any.
    NeedsLogin(String, Option<String>),
    /// The server refused the login set for this host.
    LoginRejected(String, Option<String>),
    Http(String),
    Tls(String),
    Io(String),
    Protocol(String),
    /// UIDM will not download this (DRM, a live stream); the message says
    /// why, for the user.
    Refused(String),
    /// The file name is taken and the request says to ask first.
    NameTaken { path: PathBuf, total: Option<u64> },
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::Status(code) => write!(f, "server returned HTTP {code}"),
            EngineError::NeedsLogin(host, realm) => write!(f, "{NEEDS_LOGIN}{host}{}", realm_suffix(realm)),
            EngineError::LoginRejected(host, realm) => {
                write!(f, "{LOGIN_FOR}{host}{LOGIN_REFUSED}{}", realm_suffix(realm))
            }
            EngineError::Http(s) => write!(f, "http error: {s}"),
            EngineError::Tls(s) => write!(f, "tls error: {s}"),
            EngineError::Io(s) => write!(f, "io error: {s}"),
            EngineError::Protocol(s) => write!(f, "protocol error: {s}"),
            EngineError::Refused(s) => f.write_str(s),
            EngineError::NameTaken { path, .. } => write!(f, "{} already exists", path.display()),
        }
    }
}

impl std::error::Error for EngineError {}

const NEEDS_LOGIN: &str = "This site needs a login: ";
const LOGIN_FOR: &str = "The login for ";
const LOGIN_REFUSED: &str = " was refused";
const REALM: &str = " (realm: ";

fn realm_suffix(realm: &Option<String>) -> String {
    realm.as_deref().map_or(String::new(), |r| format!("{REALM}{r})"))
}

/// A login a download failed for: the site that asked, the realm it named,
/// and whether a login was sent and refused.
#[derive(Debug, Clone, PartialEq)]
pub struct LoginWanted {
    pub host: String,
    pub realm: Option<String>,
    pub refused: bool,
}

/// The login a failed download's `error` says it needs, if that is why it
/// failed.
pub fn login_wanted(error: &str) -> Option<LoginWanted> {
    let (rest, refused) = match error.strip_prefix(NEEDS_LOGIN) {
        Some(rest) => (rest.to_string(), false),
        None => {
            let rest = error.strip_prefix(LOGIN_FOR)?;
            let (host, after) = rest.split_once(LOGIN_REFUSED)?;
            (format!("{host}{after}"), true)
        }
    };
    let (host, realm) = match rest.split_once(REALM) {
        Some((host, realm)) => (host.to_string(), realm.strip_suffix(')').map(str::to_string)),
        None => (rest, None),
    };
    (!host.is_empty() && !host.contains(char::is_whitespace)).then_some(LoginWanted { host, realm, refused })
}

impl EngineError {
    /// Errors that trying again cannot fix.
    fn is_permanent(&self) -> bool {
        matches!(
            self,
            EngineError::NeedsLogin(..)
                | EngineError::LoginRejected(..)
                | EngineError::Refused(_)
                | EngineError::NameTaken { .. }
        )
    }
}

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
    /// Each running download's own limit, by id.
    own_limits: Mutex<HashMap<u64, Arc<RateLimiter>>>,
    /// Connections in use to hosts with a limit.
    host_slots: HostSlots,
    /// Whether a connection that finishes early takes over half of the
    /// largest remaining range. Only tests turn it off, to compare.
    resplit: AtomicBool,
    /// Output files in use, so two downloads never pick the same file.
    claimed: Mutex<HashSet<PathBuf>>,
}

/// How often a connection waiting for its host to have room looks again.
const GATE_POLL: Duration = Duration::from_millis(200);

/// Connections in use per host, for hosts with a limit.
#[derive(Default)]
struct HostSlots(Mutex<HashMap<String, usize>>);

/// One connection to a host, given back when dropped.
struct Slot<'a> {
    gate: &'a Gate<'a>,
    limited: bool,
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        if !self.limited {
            return;
        }
        let mut used = self.gate.slots.0.lock().unwrap();
        if let Some(n) = used.get_mut(&self.gate.host) {
            *n -= 1;
            if *n == 0 {
                used.remove(&self.gate.host);
            }
        }
    }
}

/// The connection limit of one download's host, shared with every other
/// download to that host.
struct Gate<'a> {
    slots: &'a HostSlots,
    host: String,
    max: Option<usize>,
}

impl<'a> Gate<'a> {
    fn new(shared: &'a Shared, req: &DownloadRequest) -> Self {
        let host = url::Url::parse(&req.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_default();
        Gate {
            slots: &shared.host_slots,
            host,
            max: req.host_limit.filter(|m| *m > 0),
        }
    }

    /// A connection, if the host has room for one.
    fn take(&self) -> Option<Slot<'_>> {
        let Some(max) = self.max else {
            return Some(Slot { gate: self, limited: false });
        };
        let mut used = self.slots.0.lock().unwrap();
        let n = used.entry(self.host.clone()).or_insert(0);
        if *n >= max {
            return None;
        }
        *n += 1;
        Some(Slot { gate: self, limited: true })
    }

    /// Wait for a connection; `None` if the download is canceled first.
    fn wait(&self, token: &CancelToken) -> Option<Slot<'_>> {
        loop {
            if token.load(Ordering::Relaxed) {
                return None;
            }
            if let Some(slot) = self.take() {
                return Some(slot);
            }
            thread::sleep(GATE_POLL);
        }
    }
}

impl Shared {
    /// Download `id`'s own limiter; an unlimited one if it has none.
    fn own_limiter(&self, id: u64) -> Arc<RateLimiter> {
        self.own_limits.lock().unwrap().get(&id).cloned().unwrap_or_default()
    }
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
                    own_limits: Mutex::new(HashMap::new()),
                    host_slots: HostSlots::default(),
                    resplit: AtomicBool::new(true),
                    claimed: Mutex::new(HashSet::new()),
                }),
                reporter: Reporter(cb),
            }),
            counter: AtomicU64::new(1),
        }
    }

    /// Queue `req` and return its id. It starts when a slot is free.
    pub fn enqueue(&self, req: DownloadRequest) -> u64 {
        let id = self.reserve_id();
        self.enqueue_as(id, req);
        id
    }

    /// Take the next id without queuing anything, for a download that waits
    /// (for the user to confirm it, say) before it is queued with `enqueue_as`.
    pub fn reserve_id(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::Relaxed)
    }

    /// Queue `req` under an id from `reserve_id`.
    pub fn enqueue_as(&self, id: u64, req: DownloadRequest) {
        let token: CancelToken = Arc::new(AtomicBool::new(false));
        self.inner.registry.lock().unwrap().insert(id, Arc::clone(&token));
        self.inner.sched.lock().unwrap().queue.push_back(Job { id, token, req });
        pump(&self.inner);
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

    #[cfg(test)]
    fn set_resplit(&self, on: bool) {
        self.inner.shared.resplit.store(on, Ordering::Relaxed);
    }

    /// Set the total speed limit in bytes per second, or `None` for no limit.
    pub fn set_speed_limit(&self, bytes_per_sec: Option<u64>) {
        self.inner.shared.limiter.set_rate(bytes_per_sec);
    }

    /// Set one download's own speed limit, or `None` to clear it. A running
    /// download slows down or speeds up at once; a queued one starts with it.
    pub fn set_download_limit(&self, id: u64, bytes_per_sec: Option<u64>) {
        if let Some(own) = self.inner.shared.own_limits.lock().unwrap().get(&id) {
            own.set_rate(bytes_per_sec);
        }
        let mut sched = self.inner.sched.lock().unwrap();
        for job in sched.queue.iter_mut().filter(|j| j.id == id) {
            job.req.speed_limit = bytes_per_sec;
        }
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
            let token = Arc::clone(&job.token);
            run(job.id, job.token, job.req, inner.reporter.clone(), &inner.shared);
            // The same id may already be queued again (a paused download put
            // back in its queue); leave that run's token alone.
            let mut registry = inner.registry.lock().unwrap();
            if registry.get(&id).is_some_and(|t| Arc::ptr_eq(t, &token)) {
                registry.remove(&id);
            }
            drop(registry);
            inner.sched.lock().unwrap().running -= 1;
            pump(&inner);
        });
    }
}

/// Run a download to completion on the calling thread (CLI test mode).
pub fn run_blocking(req: DownloadRequest, cb: Arc<dyn Fn(Progress) + Send + Sync>) {
    let shared = Shared {
        limiter: RateLimiter::default(),
        own_limits: Mutex::new(HashMap::new()),
        host_slots: HostSlots::default(),
        resplit: AtomicBool::new(true),
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
    let own = Arc::new(RateLimiter::new(req.speed_limit));
    shared.own_limits.lock().unwrap().insert(id, Arc::clone(&own));
    let result = run_inner(id, &token, &req, &reporter, shared);
    {
        let mut own_limits = shared.own_limits.lock().unwrap();
        if own_limits.get(&id).is_some_and(|o| Arc::ptr_eq(o, &own)) {
            own_limits.remove(&id);
        }
    }
    if token.load(Ordering::Relaxed) {
        reporter.send(Progress::Canceled { id, url });
        return;
    }
    match result {
        Ok(Saved { path, note }) => reporter.send(Progress::Finished { id, url, path, note }),
        Err(EngineError::NameTaken { path, total }) => reporter.send(Progress::NameTaken { id, url, path, total }),
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

/// Where a finished download was saved, and anything to say about it.
pub(crate) struct Saved {
    pub path: PathBuf,
    pub note: Option<String>,
}

impl From<PathBuf> for Saved {
    fn from(path: PathBuf) -> Self {
        Saved { path, note: None }
    }
}

fn run_inner(
    id: u64,
    token: &CancelToken,
    req: &DownloadRequest,
    reporter: &Reporter,
    shared: &Shared,
) -> Result<Saved, EngineError> {
    let agent = http::Client::new(&req.net, &req.url)?;
    if let Some(choice) = req.media.as_ref().filter(|c| c.kind == Some(crate::media::MediaKind::Page)) {
        return page::run(id, token, req, choice, reporter, shared);
    }
    if let Some(choice) = &req.media {
        return stream::run(id, token, &agent, req, choice, reporter, shared);
    }
    if let Some(body) = &req.post_body {
        return run_post(id, token, &agent, req, body, reporter, shared).map(Saved::from);
    }
    let probe = http::probe(&agent, &req.url, &req.headers)?;
    let filename = pick_filename(req.filename.as_deref(), &probe);
    let dir = req.folders.for_name(&filename);
    fs::create_dir_all(&dir)?;
    let claim = choose_output(&shared.claimed, &dir, &filename, req, &probe)?;
    let output = claim.path.clone();
    let work = work_file(req, &output);
    if let Some(dir) = work.parent() {
        fs::create_dir_all(dir)?;
    }

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
        partial: (work != output).then(|| work.clone()),
    });

    let own = shared.own_limiter(id);
    let limiter = &Throttle::new(&shared.limiter, &own);
    let resplit = shared.resplit.load(Ordering::Relaxed);
    let gate = Gate::new(shared, req);
    let result = if segmented {
        let plan = Plan {
            connections,
            resplit,
        };
        run_segmented(id, token, &agent, req, work.clone(), &probe, plan, reporter, limiter, &gate)
    } else {
        run_single(id, token, &agent, req, work.clone(), &probe, reporter, limiter, &gate)
    };
    // A finished file written in the temporary folder moves into place.
    let result = match result {
        Ok(_) if work != output && !token.load(Ordering::Relaxed) => move_into_place(&work, &output).map(|()| output.clone()),
        other => other,
    };
    drop(claim);
    // A canceled download also returns its path; only a finished one gets
    // the server's date.
    if let Ok(path) = &result {
        if req.server_date && !token.load(Ordering::Relaxed) {
            if let Some(date) = probe.last_modified.as_deref().and_then(http_date) {
                let _ = OpenOptions::new().write(true).open(path).and_then(|f| f.set_modified(date));
            }
        }
    }
    result.map(Saved::from)
}

/// An HTTP date (`Sun, 06 Nov 1994 08:49:37 GMT`, or the older
/// `Sunday, 06-Nov-94 08:49:37 GMT` and `Sun Nov  6 08:49:37 1994`) as a
/// time. `None` for anything else.
pub fn http_date(s: &str) -> Option<std::time::SystemTime> {
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let month = |m: &str| MONTHS.iter().position(|x| m.eq_ignore_ascii_case(x)).map(|i| i as u32 + 1);
    let words: Vec<&str> = s.split_whitespace().collect();
    let (day, mon, year, time) = match words.as_slice() {
        // IMF-fixdate: the weekday ends in a comma.
        [_, d, m, y, t, "GMT"] => (d.parse::<u32>().ok()?, month(m)?, y.parse::<i64>().ok()?, *t),
        // RFC 850: 06-Nov-94.
        [_, dmy, t, "GMT"] => {
            let mut parts = dmy.split('-');
            let d = parts.next()?.parse::<u32>().ok()?;
            let m = month(parts.next()?)?;
            let y = parts.next()?.parse::<i64>().ok()?;
            (d, m, if y < 70 { 2000 + y } else if y < 100 { 1900 + y } else { y }, *t)
        }
        // asctime: Sun Nov  6 08:49:37 1994.
        [_, m, d, t, y] => (d.parse::<u32>().ok()?, month(m)?, y.parse::<i64>().ok()?, *t),
        _ => return None,
    };
    let mut hms = time.split(':').map(|x| x.parse::<u64>().ok());
    let (h, mi, sec) = (hms.next()??, hms.next()??, hms.next()??);
    if !(1..=31).contains(&day) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let days = crate::queue::days_from_civil(year, mon, day);
    let secs = u64::try_from(days).ok()? * 86_400 + h * 3600 + mi * 60 + sec;
    Some(std::time::UNIX_EPOCH + Duration::from_secs(secs))
}

/// A download sent as a POST: one request, its response saved as one stream.
fn run_post(
    id: u64,
    token: &CancelToken,
    agent: &http::Client,
    req: &DownloadRequest,
    body: &[u8],
    reporter: &Reporter,
    shared: &Shared,
) -> Result<PathBuf, EngineError> {
    let (probe, mut reader) = http::post(agent, &req.url, &req.headers, body)?;
    let filename = pick_filename(req.filename.as_deref(), &probe);
    let dir = req.folders.for_name(&filename);
    fs::create_dir_all(&dir)?;
    let claim = choose_output(&shared.claimed, &dir, &filename, req, &probe)?;
    let output = claim.path.clone();
    reporter.send(Progress::Started {
        id,
        url: req.url.clone(),
        path: output.clone(),
        total: probe.total,
        resumable: false,
        connections: 1,
        partial: None,
    });
    let mut file = File::create(&output)?;
    let mut buf = vec![0u8; BUF_SIZE];
    let (mut done, mut last) = (0u64, 0u64);
    let own = shared.own_limiter(id);
    let limiter = &Throttle::new(&shared.limiter, &own);
    loop {
        if token.load(Ordering::Relaxed) {
            return Ok(output);
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
                url: req.url.clone(),
                done,
                total: probe.total,
                parts: None,
                segments: None,
            });
        }
    }
    if let Some(total) = probe.total {
        if done < total {
            return Err(EngineError::Http(format!(
                "the response ended after {done} of {total} bytes"
            )));
        }
    }
    drop(claim);
    Ok(output)
}

/// Filename priority: explicit request, then Content-Disposition, then the URL
/// path, then a generic fallback.
fn pick_filename(requested: Option<&str>, probe: &http::Probe) -> String {
    requested
        .map(sanitize_filename)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            probe
                .suggested_filename
                .as_deref()
                .map(sanitize_filename)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| derive_filename(&probe.final_url))
}

/// A fetched page or small file.
pub struct Fetched {
    /// The address after redirects.
    pub url: String,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

/// GET `url` with the download's headers, proxy and logins, reading at most
/// `limit` bytes of the body (the rest is dropped).
pub fn fetch(url: &str, headers: &[(String, String)], net: &NetConfig, limit: u64) -> Result<Fetched, EngineError> {
    let agent = http::Client::new(net, url)?;
    fetch_with(&agent, url, headers, limit)
}

/// The bytes `start` through `end` (inclusive) of `url`, for a preview.
/// Fails when the server does not send ranges.
pub fn fetch_range_bytes(
    url: &str,
    headers: &[(String, String)],
    net: &NetConfig,
    start: u64,
    end: u64,
) -> Result<Vec<u8>, EngineError> {
    let agent = http::Client::new(net, url)?;
    let mut body = Vec::new();
    http::fetch_range(&agent, url, headers, start, end)?
        .take(end + 1 - start)
        .read_to_end(&mut body)?;
    Ok(body)
}

fn fetch_with(agent: &http::Client, url: &str, headers: &[(String, String)], limit: u64) -> Result<Fetched, EngineError> {
    let (url, content_type, reader) = http::get(agent, url, headers)?;
    let mut body = Vec::new();
    reader.take(limit).read_to_end(&mut body)?;
    Ok(Fetched {
        url,
        content_type,
        body,
    })
}

/// What a stream (an HLS playlist or DASH manifest at `url`) or a video page
/// yt-dlp reads offers, or why it cannot be downloaded. `ffmpeg` is where
/// to look for yt-dlp besides `PATH`.
pub fn probe_media(
    url: &str,
    headers: &[(String, String)],
    net: &NetConfig,
    ffmpeg: Option<&Path>,
) -> crate::media::MediaInfo {
    use crate::media::{ytdlp, MediaInfo, MediaKind};
    if !ytdlp::supports(url) {
        return stream::probe(url, headers, net);
    }
    match ytdlp::find(ffmpeg) {
        Some(yt_dlp) => ytdlp::probe(&yt_dlp, url, net.proxy.as_ref().and_then(|p| p.proxy_for(url)).as_deref()),
        None => MediaInfo::failed(Some(MediaKind::Page), ytdlp::MISSING.into()),
    }
}

/// What a download will be before it starts, for a save-as dialog.
#[derive(Debug, Clone)]
pub struct Inspection {
    /// The name the file would be saved under.
    pub filename: String,
    pub total: Option<u64>,
    pub resumable: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

impl Inspection {
    /// What can be said without asking the server: the name from the request
    /// or the URL, and no size.
    pub fn unprobed(url: &str, filename: Option<&str>) -> Self {
        Inspection {
            filename: filename
                .map(sanitize_filename)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| derive_filename(url)),
            total: None,
            resumable: false,
            etag: None,
            last_modified: None,
        }
    }
}

/// Probe `url` the way a download would and report its name and size,
/// without writing anything.
pub fn inspect(
    url: &str,
    filename: Option<&str>,
    headers: &[(String, String)],
    net: &NetConfig,
) -> Result<Inspection, EngineError> {
    let agent = http::Client::new(net, url)?;
    let probe = http::probe(&agent, url, headers)?;
    Ok(Inspection {
        filename: pick_filename(filename, &probe),
        total: probe.total,
        resumable: probe.resumable,
        etag: probe.etag,
        last_modified: probe.last_modified,
    })
}

/// The files a download writing to `output` may leave: the file itself, its
/// resume sidecar, and a stream's folder of segments.
pub fn files_of(output: &Path) -> Vec<PathBuf> {
    let mut parts = output.file_name().unwrap_or_default().to_os_string();
    parts.push(".uidmparts");
    vec![output.to_path_buf(), Manifest::sidecar_path(output), output.with_file_name(parts)]
}

/// The resume sidecar of a partial file at `partial`.
pub fn sidecar_path(partial: &Path) -> PathBuf {
    Manifest::sidecar_path(partial)
}

/// The files of a partial download kept in a temporary folder: the file (or
/// a stream's segment folder) and its resume sidecar.
pub fn partial_files(partial: &Path) -> Vec<PathBuf> {
    vec![partial.to_path_buf(), Manifest::sidecar_path(partial)]
}

/// Whether the partial file at `output` can be continued from the file
/// `found` describes: the same size, and the same ETag or Last-Modified when
/// both sides have one (`ignore_date` skips the Last-Modified check). A
/// missing partial file or sidecar has nothing to keep, so anything fits. The
/// error says why not, for the user.
pub fn partial_matches(output: &Path, found: &Inspection, ignore_date: bool) -> Result<(), String> {
    let Ok(m) = Manifest::load(&Manifest::sidecar_path(output)) else {
        return Ok(());
    };
    if !output.is_file() {
        return Ok(());
    }
    let mb = |n: u64| format!("{:.1} MB", n as f64 / (1024.0 * 1024.0));
    match found.total {
        None => Err("The new link does not say how big its file is, so it cannot be matched to the partial download.".into()),
        Some(total) if total != m.total => Err(format!(
            "The new link is for a {} file; the partial download is of a {} file.",
            mb(total),
            mb(m.total)
        )),
        Some(total) if !m.matches(total, found.etag.as_deref(), found.last_modified.as_deref(), ignore_date) => Err(
            "The file at the new link has changed since the download started (its ETag or date differs).".into(),
        ),
        Some(_) if !found.resumable => {
            Err("The new link's server cannot resume downloads, so it must start over.".into())
        }
        Some(_) => Ok(()),
    }
}

/// Make `name` safe to use as a Windows file name, as the engine will.
pub fn clean_filename(name: &str) -> String {
    sanitize_filename(name)
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
/// Under `Ask`, a name taken by an unrelated file is a `NameTaken` error
/// instead, before anything is written. A POST does not ask: answering would
/// send the form again, so it gets a new name.
///
/// A sidecar left without its file is deleted, so it can never mark bytes of a
/// fresh file as already downloaded.
/// Where a download whose finished file is `output` writes its data: the
/// earlier attempt's partial file if it is still there, else a file in the
/// temporary folder named after `output`, else `output` itself.
fn work_file(req: &DownloadRequest, output: &Path) -> PathBuf {
    if let Some(p) = req.reuse_partial.as_ref().filter(|p| p.is_file()) {
        return p.clone();
    }
    match &req.temp_dir {
        Some(dir) => temp_name(dir, output, ""),
        None => output.to_path_buf(),
    }
}

/// `<dir>\<file name>.<hash of the full path><suffix>`: the same for the same
/// finished file on every run, so a resume finds it, and different for two
/// files of the same name in different folders.
pub fn temp_name(dir: &Path, output: &Path, suffix: &str) -> PathBuf {
    // FNV-1a, which unlike the standard hasher never changes between builds.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in output.to_string_lossy().to_lowercase().bytes() {
        hash = (hash ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    let name = output.file_name().unwrap_or_default().to_string_lossy();
    dir.join(format!("{name}.{hash:016x}{suffix}"))
}

/// Move a finished file from the temporary folder to `to`: a rename on the
/// same drive, a copy and delete across drives. Its resume sidecar goes.
fn move_into_place(from: &Path, to: &Path) -> Result<(), EngineError> {
    let _ = fs::remove_file(Manifest::sidecar_path(from));
    if fs::rename(from, to).is_err() {
        fs::copy(from, to)?;
        fs::remove_file(from)?;
    }
    Ok(())
}

fn choose_output<'a>(
    claimed: &'a Mutex<HashSet<PathBuf>>,
    dir: &Path,
    filename: &str,
    req: &DownloadRequest,
    probe: &http::Probe,
) -> Result<Claim<'a>, EngineError> {
    let mut set = claimed.lock().unwrap();
    let path = pick_free_path(&set, dir, filename, req, probe)?;
    set.insert(path.clone());
    Ok(Claim { set: claimed, path })
}

fn pick_free_path(
    claimed: &HashSet<PathBuf>,
    dir: &Path,
    filename: &str,
    req: &DownloadRequest,
    probe: &http::Probe,
) -> Result<PathBuf, EngineError> {
    if let Some(path) = &req.reuse_path {
        if !claimed.contains(path) {
            return Ok(path.clone());
        }
    }
    let ask = req.file_exists == FileExists::Ask && req.post_body.is_none();

    for n in 0..=MAX_RENAMES {
        let path = dir.join(numbered_name(filename, n));
        if claimed.contains(&path) {
            continue;
        }
        let sidecar = Manifest::sidecar_path(&path);
        if !path.exists() {
            // A partial copy in the temporary folder resumes; a stale one
            // starts over.
            let work = work_file(req, &path);
            if work != path && !(work.is_file() && sidecar_matches(&Manifest::sidecar_path(&work), probe, req.ignore_date)) {
                let _ = fs::remove_file(Manifest::sidecar_path(&work));
            }
            let _ = fs::remove_file(&sidecar);
            return Ok(path);
        }
        if sidecar_matches(&sidecar, probe, req.ignore_date) {
            return Ok(path);
        }
        if req.file_exists == FileExists::Overwrite && path.is_file() {
            let _ = fs::remove_file(&sidecar);
            return Ok(path);
        }
        if ask && path.is_file() {
            return Err(EngineError::NameTaken {
                path,
                total: probe.total,
            });
        }
    }
    Err(EngineError::Io(format!(
        "no free file name for {filename} in {}",
        dir.display()
    )))
}

/// `report.pdf` for 0, then `report (1).pdf`, `report (2).pdf`, ...
pub fn numbered_name(filename: &str, n: u32) -> String {
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
fn sidecar_matches(sidecar: &Path, probe: &http::Probe, ignore_date: bool) -> bool {
    let (true, Some(total)) = (probe.resumable, probe.total) else {
        return false;
    };
    match Manifest::load(sidecar) {
        Ok(m) => {
            !m.segments.is_empty()
                && m.matches(total, probe.etag.as_deref(), probe.last_modified.as_deref(), ignore_date)
        }
        Err(_) => false,
    }
}

/// How a segmented download is split.
#[derive(Clone, Copy)]
struct Plan {
    /// Connections at once.
    connections: usize,
    /// Whether a connection that finishes early splits the largest remaining
    /// range and takes its second half.
    resplit: bool,
}

/// The manifest plus which segments a connection is working on. The busy
/// flags only live while the download runs.
struct Work {
    manifest: Manifest,
    busy: Vec<bool>,
    /// What each segment's connection is doing, for showing progress.
    states: Vec<SegmentState>,
    /// When progress last carried the segments. Byte-driven reports carry
    /// them at most every `SEGMENTS_EVERY`, so a fast download does not
    /// flood clients with them.
    segments_at: Option<Instant>,
}

impl Work {
    /// The next segment for a free connection: an unstarted one, or else (when
    /// `resplit`) the second half of the busy segment with the most left. The
    /// original connection stops at the new end of its first half without a
    /// new request.
    fn next(&mut self, resplit: bool) -> Option<usize> {
        let segments = &self.manifest.segments;
        if let Some(i) = (0..segments.len()).find(|&i| !self.busy[i] && !segments[i].is_complete()) {
            self.busy[i] = true;
            self.states[i] = SegmentState::Connecting;
            return Some(i);
        }
        if !resplit {
            return None;
        }
        let (i, left) = (0..segments.len())
            .filter(|&i| self.busy[i])
            .map(|i| (i, segments[i].end + 1 - segments[i].cursor().min(segments[i].end + 1)))
            .max_by_key(|(_, left)| *left)?;
        if left < 2 * MIN_SPLIT_BYTES {
            return None;
        }
        let seg = &mut self.manifest.segments[i];
        let mid = seg.cursor() + left / 2;
        let new = resume::SegmentProgress {
            start: mid,
            end: seg.end,
            done: 0,
        };
        seg.end = mid - 1;
        self.manifest.segments.push(new);
        self.busy.push(true);
        self.states.push(SegmentState::Connecting);
        Some(self.manifest.segments.len() - 1)
    }

    /// Each segment's range, progress and state.
    fn snapshot(&self) -> Vec<SegmentInfo> {
        self.manifest
            .segments
            .iter()
            .zip(&self.states)
            .map(|(s, state)| SegmentInfo {
                start: s.start,
                end: s.end,
                done: s.done,
                state: if s.is_complete() { SegmentState::Done } else { *state },
            })
            .collect()
    }
}

/// Parallel, resumable path for a server that honors byte ranges.
#[allow(clippy::too_many_arguments)]
fn run_segmented(
    id: u64,
    token: &CancelToken,
    agent: &http::Client,
    req: &DownloadRequest,
    output: PathBuf,
    probe: &http::Probe,
    plan: Plan,
    reporter: &Reporter,
    limiter: &Throttle,
    gate: &Gate,
) -> Result<PathBuf, EngineError> {
    let total = probe.total.expect("segmented path requires a known total");
    let url = probe.final_url.clone();
    let sidecar = Manifest::sidecar_path(&output);

    // A sidecar only counts while its partial file is still there.
    let manifest = match Manifest::load(&sidecar) {
        Ok(m)
            if output.is_file()
                && !m.segments.is_empty()
                && m.matches(total, probe.etag.as_deref(), probe.last_modified.as_deref(), req.ignore_date) =>
        {
            m
        }
        _ => Manifest::new(
            url.clone(),
            total,
            probe.etag.clone(),
            probe.last_modified.clone(),
            &split(total, plan.connections),
        ),
    };

    ensure_file_len(&output, total)?;
    manifest.save(&sidecar).ok();

    let unfinished = manifest.segments.iter().filter(|s| !s.is_complete()).count();
    let resumed = manifest.bytes_done();
    let busy = vec![false; manifest.segments.len()];
    let states = vec![SegmentState::Waiting; manifest.segments.len()];
    let shared = Arc::new(Mutex::new(Work {
        manifest,
        busy,
        states,
        segments_at: None,
    }));
    let global_done = Arc::new(AtomicU64::new(resumed));
    let last_report = Arc::new(AtomicU64::new(resumed));
    let first_error: Arc<Mutex<Option<EngineError>>> = Arc::new(Mutex::new(None));

    // Each connection takes segments until none is left to take or split.
    // Scoped threads may borrow the shared limiter.
    let workers = plan.connections.min(unfinished).max(1);
    thread::scope(|scope| {
        for _ in 0..workers {
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
                loop {
                    if token.load(Ordering::Relaxed) || first_error.lock().unwrap().is_some() {
                        return;
                    }
                    // A host at its limit: wait for a connection to free up,
                    // as long as some range is waiting for one.
                    let Some(slot) = gate.take() else {
                        let waiting = {
                            let w = shared.lock().unwrap();
                            w.manifest.segments.iter().zip(&w.busy).any(|(s, b)| !*b && !s.is_complete())
                        };
                        if !waiting {
                            return;
                        }
                        thread::sleep(GATE_POLL);
                        continue;
                    };
                    let Some(idx) = shared.lock().unwrap().next(plan.resplit) else {
                        return;
                    };
                    let result = download_segment(&ctx, idx);
                    drop(slot);
                    {
                        let mut w = shared.lock().unwrap();
                        w.busy[idx] = false;
                        w.states[idx] = SegmentState::Waiting;
                    }
                    if let Err(e) = result {
                        let mut slot = first_error.lock().unwrap();
                        if slot.is_none() {
                            *slot = Some(e);
                        }
                        return;
                    }
                }
            });
        }
    });

    if token.load(Ordering::Relaxed) {
        // Keep the sidecar and partial file so a later request resumes, saved
        // with the latest progress rather than the last periodic flush.
        shared.lock().unwrap().manifest.save(&sidecar).ok();
        return Ok(output);
    }
    if let Some(err) = first_error.lock().unwrap().take() {
        shared.lock().unwrap().manifest.save(&sidecar).ok();
        return Err(err);
    }

    let (done, segments) = {
        let w = shared.lock().unwrap();
        (w.manifest.bytes_done(), w.snapshot())
    };
    reporter.send(Progress::Advanced {
        id,
        url: url.clone(),
        done,
        total: Some(total),
        parts: None,
        segments: Some(segments),
    });
    let _ = fs::remove_file(&sidecar);
    Ok(output)
}

/// Shared references a segment thread needs; grouped to keep signatures short.
struct SegmentCtx<'a> {
    id: u64,
    token: &'a CancelToken,
    agent: &'a http::Client,
    url: &'a str,
    headers: &'a [(String, String)],
    output: &'a std::path::Path,
    sidecar: &'a std::path::Path,
    shared: &'a Arc<Mutex<Work>>,
    global_done: &'a AtomicU64,
    last_report: &'a AtomicU64,
    reporter: &'a Reporter,
    limiter: &'a Throttle<'a>,
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
            Err(e) if e.is_permanent() => return Err(e),
            Err(e) => {
                if ctx.token.load(Ordering::Relaxed) {
                    return Ok(());
                }
                attempt += 1;
                if attempt > MAX_RETRIES {
                    return Err(e);
                }
                set_state(ctx, idx, SegmentState::Retrying);
                thread::sleep(backoff(attempt));
                set_state(ctx, idx, SegmentState::Connecting);
            }
        }
    }
}

/// Record what segment `idx`'s connection is doing and report it. These
/// changes are rare next to the byte-driven reports, so each one is sent.
fn set_state(ctx: &SegmentCtx, idx: usize, state: SegmentState) {
    let segments = {
        let mut w = ctx.shared.lock().unwrap();
        if w.states[idx] == state {
            return;
        }
        w.states[idx] = state;
        w.snapshot()
    };
    ctx.reporter.send(Progress::Advanced {
        id: ctx.id,
        url: ctx.url.to_string(),
        done: ctx.global_done.load(Ordering::Relaxed),
        total: Some(ctx.total),
        parts: None,
        segments: Some(segments),
    });
}

fn download_segment_once(ctx: &SegmentCtx, idx: usize) -> Result<(), EngineError> {
    let (start, end, mut cursor) = {
        let w = ctx.shared.lock().unwrap();
        let s = &w.manifest.segments[idx];
        (s.start, s.end, s.cursor())
    };
    if cursor > end {
        return Ok(());
    }

    let mut reader = http::fetch_range(ctx.agent, ctx.url, ctx.headers, cursor, end)?;
    set_state(ctx, idx, SegmentState::Receiving);
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
        // The end can move closer while this runs: another connection may
        // have taken the second half of this range. Never write past it, even
        // if the server over-sends.
        let end = ctx.shared.lock().unwrap().manifest.segments[idx].end;
        if cursor > end {
            break;
        }
        let room = (end - cursor + 1) as usize;
        let n = n.min(room);
        write_at(&file, &buf[..n], cursor)?;
        cursor += n as u64;

        {
            let mut w = ctx.shared.lock().unwrap();
            w.manifest.segments[idx].done = cursor - start;
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
    let segments = match ctx.shared.lock() {
        Ok(mut w) => {
            w.manifest.save(ctx.sidecar).ok();
            let due = w.segments_at.is_none_or(|at| at.elapsed() >= SEGMENTS_EVERY);
            if due {
                w.segments_at = Some(Instant::now());
            }
            due.then(|| w.snapshot())
        }
        Err(_) => None,
    };
    ctx.reporter.send(Progress::Advanced {
        id: ctx.id,
        url: ctx.url.to_string(),
        done: global,
        total: Some(ctx.total),
        parts: None,
        segments,
    });
}

/// Single-stream path for a server without range support. Not resumable, so a
/// transient failure restarts from the beginning, up to `MAX_RETRIES`.
#[allow(clippy::too_many_arguments)]
fn run_single(
    id: u64,
    token: &CancelToken,
    agent: &http::Client,
    req: &DownloadRequest,
    output: PathBuf,
    probe: &http::Probe,
    reporter: &Reporter,
    limiter: &Throttle,
    gate: &Gate,
) -> Result<PathBuf, EngineError> {
    let Some(_slot) = gate.wait(token) else {
        return Ok(output);
    };
    let mut attempt = 0u32;
    loop {
        if token.load(Ordering::Relaxed) {
            return Ok(output);
        }
        match single_once(id, token, agent, req, &output, probe, reporter, limiter) {
            Ok(()) => return Ok(output),
            Err(e) if e.is_permanent() => return Err(e),
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
    agent: &http::Client,
    req: &DownloadRequest,
    output: &std::path::Path,
    probe: &http::Probe,
    reporter: &Reporter,
    limiter: &Throttle,
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
                parts: None,
                segments: None,
            });
        }
    }
    Ok(())
}

/// Positioned write that does not disturb a shared cursor. Loops because a
/// single call may write fewer bytes than requested.
fn write_at(file: &File, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
    while !buf.is_empty() {
        let n = crate::platform::write_at(file, buf, offset)?;
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
        use super::super::testserver::{body, Behavior, TestServer};
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
                folders: Folders::fixed(dir.to_path_buf()),
                filename: Some("report.pdf".into()),
                headers: Vec::new(),
                connections: 4,
                reuse_path: None,
                file_exists,
                post_body: None,
                net: Arc::new(NetConfig::default()),
                media: None,
                ffmpeg: None,
                speed_limit: None,
                server_date: false,
                ignore_date: false,
                temp_dir: None,
                reuse_partial: None,
                host_limit: None,
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
        fn a_new_server_date_restarts_unless_told_to_ignore_it() {
            let data = body(LEN);
            let half = LEN / 2;
            for ignore_date in [false, true] {
                let server = TestServer::start_with(
                    data.clone(),
                    Behavior {
                        last_modified: Some("Tue, 02 Jan 2024 00:00:00 GMT".into()),
                        ..Behavior::default()
                    },
                );
                let dir = temp_dir(if ignore_date { "date_ignored" } else { "date_checked" });
                let partial = dir.join("report.pdf");
                let mut on_disk = data[..half].to_vec();
                on_disk.resize(LEN, 0);
                fs::write(&partial, &on_disk).unwrap();
                // Saved by a run that saw no ETag and an older date.
                let mut m = Manifest::new(
                    server.url.clone(),
                    LEN as u64,
                    None,
                    Some("Mon, 01 Jan 2024 00:00:00 GMT".into()),
                    &split(LEN as u64, 2),
                );
                m.segments[0].done = m.segments[0].len();
                m.save(&Manifest::sidecar_path(&partial)).unwrap();

                let mut req = request(&server.url, &dir, FileExists::Overwrite);
                req.ignore_date = ignore_date;
                // No splitting ranges midway, which can fetch a few bytes
                // twice and throw the count off.
                assert_eq!(download(|e| e.set_resplit(false), req), partial);
                assert_eq!(fs::read(&partial).unwrap(), data);
                let sent = if ignore_date { 1 + (LEN - half) } else { 1 + LEN };
                assert_eq!(server.served.load(Ordering::Relaxed), sent as u64, "ignore_date {ignore_date}");
                fs::remove_dir_all(&dir).ok();
            }
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

        /// Run one download that should stop at a taken name; the taken file
        /// and the size it reported.
        fn name_taken(req: DownloadRequest) -> (PathBuf, Option<u64>) {
            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            let engine = Engine::new(
                Arc::new(move |p: Progress| {
                    let _ = tx.lock().unwrap().send(p);
                }),
                1,
            );
            engine.enqueue(req);
            loop {
                match rx.recv_timeout(Duration::from_secs(30)).expect("download timed out") {
                    Progress::NameTaken { path, total, .. } => return (path, total),
                    Progress::Started { .. } => panic!("the download started"),
                    Progress::Finished { .. } => panic!("the download finished"),
                    Progress::Failed { error, .. } => panic!("download failed: {error}"),
                    _ => {}
                }
            }
        }

        #[test]
        fn ask_setting_stops_before_writing_a_taken_name() {
            let server = TestServer::start(body(LEN));
            let dir = temp_dir("ask");
            let existing = dir.join("report.pdf");
            fs::write(&existing, b"someone else's report").unwrap();

            let (path, total) = name_taken(request(&server.url, &dir, FileExists::Ask));
            assert_eq!(path, existing);
            assert_eq!(total, Some(LEN as u64));
            assert_eq!(fs::read(&existing).unwrap(), b"someone else's report");
            assert!(!Manifest::sidecar_path(&existing).exists(), "nothing written");
            assert!(!dir.join("report (1).pdf").exists());

            // A stale sidecar does not make the file ours: it still asks.
            Manifest::new(server.url.clone(), 999, None, None, &split(999, 2))
                .save(&Manifest::sidecar_path(&existing))
                .unwrap();
            assert_eq!(name_taken(request(&server.url, &dir, FileExists::Ask)).0, existing);
            assert_eq!(fs::read(&existing).unwrap(), b"someone else's report");

            // The answers, as the save-as dialog sends them.
            let renamed = download(|_| {}, request(&server.url, &dir, FileExists::Rename));
            assert_eq!(renamed, dir.join("report (1).pdf"));
            assert_eq!(fs::read(&existing).unwrap(), b"someone else's report");
            let replaced = download(|_| {}, request(&server.url, &dir, FileExists::Overwrite));
            assert_eq!(replaced, existing);
            assert_eq!(fs::read(&existing).unwrap(), body(LEN));
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn ask_setting_lets_a_free_name_and_a_resume_through() {
            let data = body(LEN);
            let server = TestServer::start(data.clone());
            let dir = temp_dir("ask_free");
            let path = download(|_| {}, request(&server.url, &dir, FileExists::Ask));
            assert_eq!(path, dir.join("report.pdf"));

            // A partial copy with a matching sidecar is this download's own.
            let partial = dir.join("partial.pdf");
            let half = LEN / 2;
            let mut on_disk = data[..half].to_vec();
            on_disk.resize(LEN, 0);
            fs::write(&partial, &on_disk).unwrap();
            let mut m = Manifest::new(server.url.clone(), LEN as u64, Some("\"v1\"".into()), None, &split(LEN as u64, 2));
            m.segments[0].done = m.segments[0].len();
            m.save(&Manifest::sidecar_path(&partial)).unwrap();
            let mut req = request(&server.url, &dir, FileExists::Ask);
            req.filename = Some("partial.pdf".into());
            assert_eq!(download(|_| {}, req), partial);
            assert_eq!(fs::read(&partial).unwrap(), data);
            fs::remove_dir_all(&dir).ok();
        }

        /// Asking would mean sending the form again, so a POST is renamed.
        #[test]
        fn a_post_under_ask_gets_a_new_name() {
            let data = body(LEN);
            let form = b"file=report".to_vec();
            let behavior = Behavior {
                post_only_body: Some(form.clone()),
                ..Behavior::default()
            };
            let server = TestServer::start_with(data.clone(), behavior);
            let dir = temp_dir("ask_post");
            fs::write(dir.join("report.pdf"), b"old").unwrap();
            let mut req = request(&server.url, &dir, FileExists::Ask);
            req.filename = None;
            req.post_body = Some(form);
            let path = download(|_| {}, req);
            assert_eq!(path, dir.join("report (1).pdf"));
            assert_eq!(fs::read(&path).unwrap(), data);
            assert_eq!(fs::read(dir.join("report.pdf")).unwrap(), b"old");
            fs::remove_dir_all(&dir).ok();
        }

        /// One connection is slow. With re-splitting, the free connections
        /// take over halves of its range, so the download ends far sooner
        /// than with fixed ranges, and the file still matches.
        #[test]
        fn a_slow_range_is_taken_over_by_free_connections() {
            const LEN: usize = 4 * 1024 * 1024;
            let data = body(LEN);
            // 4 connections: 1 MiB each; the last range starts at 3 MiB and is
            // sent at 128 KiB/s, so fixed ranges need about 8 s.
            let behavior = Behavior {
                slow_range_start: Some(3 * 1024 * 1024),
                slow_bytes_per_sec: 128 * 1024,
                ..Behavior::default()
            };
            let run = |resplit: bool| {
                let server = TestServer::start_with(data.clone(), behavior.clone());
                let dir = temp_dir(if resplit { "resplit" } else { "fixed" });
                let start = Instant::now();
                let path = download(|e| e.set_resplit(resplit), request(&server.url, &dir, FileExists::Rename));
                let secs = start.elapsed().as_secs_f64();
                assert_eq!(fs::read(&path).unwrap(), data, "resplit {resplit}: file matches");
                assert!(!Manifest::sidecar_path(&path).exists());
                fs::remove_dir_all(&dir).ok();
                secs
            };
            let fixed = run(false);
            let resplit = run(true);
            eprintln!("slow range: fixed ranges {fixed:.1}s, re-split {resplit:.1}s");
            assert!(fixed > 7.0, "fixed ranges wait for the slow one: {fixed:.1}s");
            assert!(resplit < fixed * 0.7, "re-split {resplit:.1}s vs fixed {fixed:.1}s");
        }

        #[test]
        fn a_download_canceled_after_splits_resumes_byte_identical() {
            const LEN: usize = 4 * 1024 * 1024;
            let data = body(LEN);
            let behavior = Behavior {
                slow_range_start: Some(3 * 1024 * 1024),
                slow_bytes_per_sec: 128 * 1024,
                ..Behavior::default()
            };
            let server = TestServer::start_with(data.clone(), behavior);
            let dir = temp_dir("split-resume");
            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            let engine = Engine::new(
                Arc::new(move |p: Progress| {
                    let _ = tx.lock().unwrap().send(p);
                }),
                1,
            );
            let id = engine.enqueue(request(&server.url, &dir, FileExists::Rename));
            let path = loop {
                if let Progress::Started { path, .. } = rx.recv_timeout(Duration::from_secs(10)).unwrap() {
                    break path;
                }
            };
            // The fast ranges finish at once; give the free connections time
            // to split the slow one, then stop.
            thread::sleep(Duration::from_millis(1500));
            engine.cancel(id);
            while !matches!(rx.recv_timeout(Duration::from_secs(10)).unwrap(), Progress::Canceled { .. }) {}
            let m = Manifest::load(&Manifest::sidecar_path(&path)).unwrap();
            assert!(m.segments.len() > 4, "the split ranges are saved: {}", m.segments.len());
            assert!(m.bytes_done() < LEN as u64);

            let resumed = download(|_| {}, request(&server.url, &dir, FileExists::Rename));
            assert_eq!(resumed, path, "resumes into the same file");
            assert_eq!(fs::read(&path).unwrap(), data);
            fs::remove_dir_all(&dir).ok();
        }

        /// Start a slow download, stop it partway, and return where its data
        /// went and the reported partial path.
        fn stop_partway(server: &TestServer, req: DownloadRequest) -> (PathBuf, Option<PathBuf>) {
            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            let engine = Engine::new(
                Arc::new(move |p: Progress| {
                    let _ = tx.lock().unwrap().send(p);
                }),
                1,
            );
            let id = engine.enqueue(req);
            let started = loop {
                if let Progress::Started { path, partial, .. } = rx.recv_timeout(Duration::from_secs(10)).unwrap() {
                    break (path, partial);
                }
            };
            while server.served.load(Ordering::Relaxed) < 512 * 1024 {
                thread::sleep(Duration::from_millis(20));
            }
            engine.cancel(id);
            while !matches!(rx.recv_timeout(Duration::from_secs(10)).unwrap(), Progress::Canceled { .. }) {}
            started
        }

        #[test]
        fn a_temporary_folder_holds_the_partial_file_until_it_is_done() {
            const LEN: usize = 2 * 1024 * 1024;
            let data = body(LEN);
            let behavior = Behavior {
                slow_range_start: Some(0),
                slow_bytes_per_sec: 512 * 1024,
                ..Behavior::default()
            };
            let server = TestServer::start_with(data.clone(), behavior);
            let dir = temp_dir("temp_folder");
            let temp = dir.join("temp");
            let req = || {
                let mut r = request(&server.url, &dir.join("out"), FileExists::Rename);
                r.temp_dir = Some(temp.clone());
                r
            };
            let (path, partial) = stop_partway(&server, req());
            let partial = partial.expect("the partial file is in the temporary folder");
            assert_eq!(partial.parent(), Some(temp.as_path()));
            assert!(partial.is_file() && Manifest::sidecar_path(&partial).is_file());
            assert!(!path.exists(), "nothing unfinished in the download folder");

            // A plain resume finds it in the temporary folder by its name.
            let done = download(|_| {}, req());
            assert_eq!(done, path);
            assert_eq!(fs::read(&path).unwrap(), data);
            assert!(!partial.exists() && !Manifest::sidecar_path(&partial).exists());
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn a_partial_file_resumes_after_the_temporary_folder_changes() {
            const LEN: usize = 2 * 1024 * 1024;
            let data = body(LEN);
            let behavior = Behavior {
                slow_range_start: Some(0),
                slow_bytes_per_sec: 512 * 1024,
                ..Behavior::default()
            };
            let server = TestServer::start_with(data.clone(), behavior);
            let dir = temp_dir("temp_moved");
            let mut req = request(&server.url, &dir.join("out"), FileExists::Rename);
            req.temp_dir = Some(dir.join("old"));
            let (path, partial) = stop_partway(&server, req);
            let mut req = request(&server.url, &dir.join("out"), FileExists::Rename);
            let served = server.served.load(Ordering::Relaxed);

            // Retried as the daemon does: its own file, and where its data is.
            req.temp_dir = Some(dir.join("new"));
            req.reuse_path = Some(path.clone());
            req.reuse_partial = partial.clone();
            let done = download(|_| {}, req);
            assert_eq!(done, path);
            assert_eq!(fs::read(&path).unwrap(), data);
            let again = server.served.load(Ordering::Relaxed) - served;
            assert!(again < LEN as u64, "only the rest was fetched: {again} bytes");
            assert!(!partial.unwrap().exists());
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn a_preview_reads_just_the_range_it_asks_for() {
            let data = body(LEN);
            let server = TestServer::start(data.clone());
            let tail = fetch_range_bytes(&server.url, &[], &NetConfig::default(), LEN as u64 - 100, LEN as u64 - 1).unwrap();
            assert_eq!(tail, &data[LEN - 100..]);
            assert!(server.served.load(Ordering::Relaxed) <= 100, "only the range was sent");
        }

        #[test]
        fn pages_for_the_grabber_answer_a_login_request() {
            let behavior = Behavior {
                basic_auth: Some("ann:secret".into()),
                ..Behavior::default()
            };
            let server = TestServer::start_with(b"<a href=x.pdf>".to_vec(), behavior);
            assert!(fetch(&server.url, &[], &NetConfig::default(), 1024).is_err());
            let net = |plain| NetConfig {
                logins: vec![crate::net::Login {
                    site: "127.0.0.1".into(),
                    username: "ann".into(),
                    password: "secret".into(),
                    plain,
                }],
                ..NetConfig::default()
            };
            // Basic over plain http: a saved login goes only when allowed
            // there; otherwise the user is asked.
            assert!(matches!(
                fetch(&server.url, &[], &net(false), 1024),
                Err(EngineError::NeedsLogin(..))
            ));
            assert_eq!(fetch(&server.url, &[], &net(true), 1024).unwrap().body, b"<a href=x.pdf>");
        }

        #[test]
        fn ftp_downloads_in_parallel_ranges() {
            let data = body(LEN);
            let server = crate::engine::ftp::server::start(data.clone(), None);
            let dir = temp_dir("ftp");
            let mut req = request(&server.url, &dir, FileExists::Rename);
            req.filename = None;
            let path = download(|_| {}, req);
            assert_eq!(path, dir.join("file.bin"));
            assert_eq!(fs::read(&path).unwrap(), data);
            assert!(server.transfers.load(Ordering::Relaxed) >= 2, "more than one connection");
            assert!(!Manifest::sidecar_path(&path).exists());
            // The server's MDTM date is the file's date.
            let mut req = request(&server.url, &dir, FileExists::Overwrite);
            req.filename = None;
            req.server_date = true;
            let path = download(|_| {}, req);
            let modified = fs::metadata(&path).unwrap().modified().unwrap();
            assert_eq!(modified, http_date("Wed, 21 Oct 2015 07:28:00 GMT").unwrap());
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn a_host_gate_hands_out_at_most_its_limit() {
            let shared = Shared {
                limiter: RateLimiter::default(),
                own_limits: Mutex::new(HashMap::new()),
                host_slots: HostSlots::default(),
                resplit: AtomicBool::new(true),
                claimed: Mutex::new(HashSet::new()),
            };
            let mut req = request("https://Files.Example.com/a", Path::new("."), FileExists::Rename);
            req.host_limit = Some(2);
            let a = Gate::new(&shared, &req);
            let b = Gate::new(&shared, &req);
            let one = a.take().unwrap();
            let _two = b.take().unwrap();
            assert!(a.take().is_none() && b.take().is_none(), "both downloads share the limit");
            drop(one);
            assert!(b.take().is_some());
            req.host_limit = None;
            let free = Gate::new(&shared, &req);
            assert!((0..10).all(|_| free.take().is_some()), "no limit, no counting");
        }

        #[test]
        fn downloads_to_one_host_share_its_connection_limit() {
            const LEN: usize = 2 * 1024 * 1024;
            let data = body(LEN);
            let behavior = Behavior {
                slow_range_start: Some(0),
                slow_bytes_per_sec: 1024 * 1024,
                ..Behavior::default()
            };
            let server = TestServer::start_with(data.clone(), behavior);
            let dir = temp_dir("host_limit");
            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            let engine = Engine::new(
                Arc::new(move |p: Progress| {
                    let _ = tx.lock().unwrap().send(p);
                }),
                2,
            );
            for name in ["a.bin", "b.bin"] {
                let mut req = request(&server.url, &dir, FileExists::Rename);
                req.filename = Some(name.into());
                req.host_limit = Some(2);
                engine.enqueue(req);
            }
            let (mut most, mut finished) = (0, Vec::new());
            while finished.len() < 2 {
                let used: usize = engine.inner.shared.host_slots.0.lock().unwrap().values().sum();
                most = most.max(used);
                match rx.recv_timeout(Duration::from_millis(5)) {
                    Ok(Progress::Finished { path, .. }) => finished.push(path),
                    Ok(Progress::Failed { error, .. }) => panic!("download failed: {error}"),
                    _ => {}
                }
            }
            assert!(most <= 2, "{most} connections at once");
            for path in finished {
                assert_eq!(fs::read(&path).unwrap(), data);
            }
            assert!(engine.inner.shared.host_slots.0.lock().unwrap().is_empty(), "all given back");
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn a_free_connection_takes_the_second_half() {
            // A plan with one busy segment: a free connection takes its second
            // half; tiny remainders are left alone.
            let mut w = Work {
                manifest: Manifest::new("u".into(), 4 << 20, None, None, &split(4 << 20, 1)),
                busy: vec![true],
                states: vec![SegmentState::Receiving],
                segments_at: None,
            };
            w.manifest.segments[0].done = 1 << 20;
            let i = w.next(true).unwrap();
            let (a, b) = (w.manifest.segments[0].clone(), w.manifest.segments[i].clone());
            assert_eq!(a.end + 1, b.start, "the halves meet");
            assert_eq!(b.end, (4 << 20) - 1);
            assert_eq!(b.start - a.cursor(), (3 << 20) / 2, "split at the middle of what is left");
            assert!(w.next(false).is_none(), "no splitting when it is off");
            w.manifest.segments[0].done = a.len() - 1000;
            w.manifest.segments[i].done = b.len() - 1000;
            assert!(w.next(true).is_none(), "too little left to split");
        }

        #[test]
        fn progress_shows_each_connection() {
            let server = TestServer::start(body(LEN));
            let dir = temp_dir("connections");
            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            let engine = Engine::new(
                Arc::new(move |p: Progress| {
                    let _ = tx.lock().unwrap().send(p);
                }),
                1,
            );
            engine.enqueue(request(&server.url, &dir, FileExists::Rename));
            let mut states = HashSet::new();
            let last = loop {
                match rx.recv_timeout(Duration::from_secs(30)).expect("download timed out") {
                    Progress::Advanced { segments: Some(s), .. } => {
                        states.extend(s.iter().map(|x| x.state));
                        if s.iter().all(|x| x.state == SegmentState::Done) {
                            break s;
                        }
                    }
                    Progress::Failed { error, .. } => panic!("download failed: {error}"),
                    _ => {}
                }
            };
            assert!(last.len() >= 4, "one range per connection at least");
            assert_eq!(last.iter().map(|s| s.done).sum::<u64>(), LEN as u64);
            assert!(states.contains(&SegmentState::Connecting) && states.contains(&SegmentState::Receiving));
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn http_dates_in_all_three_forms() {
            let want = std::time::UNIX_EPOCH + Duration::from_secs(784_111_777);
            assert_eq!(http_date("Sun, 06 Nov 1994 08:49:37 GMT"), Some(want));
            assert_eq!(http_date("Sunday, 06-Nov-94 08:49:37 GMT"), Some(want));
            assert_eq!(http_date("Sun Nov  6 08:49:37 1994"), Some(want));
            assert_eq!(http_date("yesterday"), None);
            assert_eq!(http_date("Sun, 06 Nov 1994 25:49:37 GMT"), None);
        }

        #[test]
        fn the_file_gets_the_servers_date() {
            let date = "Wed, 21 Oct 2015 07:28:00 GMT";
            let behavior = Behavior {
                last_modified: Some(date.into()),
                ..Behavior::default()
            };
            let server = TestServer::start_with(body(LEN), behavior);
            let dir = temp_dir("server_date");
            let mut req = request(&server.url, &dir, FileExists::Rename);
            req.server_date = true;
            let path = download(|_| {}, req);
            let modified = fs::metadata(&path).unwrap().modified().unwrap();
            assert_eq!(modified, http_date(date).unwrap());

            // Without the header (or with the option off) it keeps the time
            // it was written.
            let plain = TestServer::start(body(LEN));
            let mut req = request(&plain.url, &dir, FileExists::Rename);
            req.server_date = true;
            let path = download(|_| {}, req);
            let age = fs::metadata(&path).unwrap().modified().unwrap().elapsed().unwrap();
            assert!(age < Duration::from_secs(60));
            fs::remove_dir_all(&dir).ok();
        }

        /// A download a form started: the server sends the file only to the
        /// POST with the right body, and an HTML page to a GET.
        #[test]
        fn a_form_post_is_sent_once_and_saved() {
            let data = body(LEN);
            let form = b"session=abc&file=report".to_vec();
            let behavior = Behavior {
                post_only_body: Some(form.clone()),
                ..Behavior::default()
            };
            let server = TestServer::start_with(data.clone(), behavior);
            let dir = temp_dir("post");
            let mut req = request(&server.url, &dir, FileExists::Rename);
            req.filename = None;
            req.post_body = Some(form);
            let path = download(|_| {}, req);
            assert_eq!(path, dir.join("report.pdf"), "named by Content-Disposition");
            assert_eq!(fs::read(&path).unwrap(), data);
            assert_eq!(server.served.load(Ordering::Relaxed), LEN as u64, "one request, no probe");

            // The same URL as a GET only gets the error page.
            let mut get = request(&server.url, &dir, FileExists::Rename);
            get.filename = Some("as-get.html".into());
            let html = fs::read(download(|_| {}, get)).unwrap();
            assert!(html.starts_with(b"<html>"));
            fs::remove_dir_all(&dir).ok();
        }

        fn login(site: &str, username: &str, password: &str) -> crate::net::Login {
            crate::net::Login {
                site: site.into(),
                username: username.into(),
                password: password.into(),
                plain: true,
            }
        }

        /// Run one download and return its error.
        fn download_err(req: DownloadRequest) -> String {
            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            let engine = Engine::new(
                Arc::new(move |p: Progress| {
                    let _ = tx.lock().unwrap().send(p);
                }),
                1,
            );
            engine.enqueue(req);
            loop {
                match rx.recv_timeout(Duration::from_secs(30)).expect("download timed out") {
                    Progress::Failed { error, .. } => return error,
                    Progress::Finished { .. } => panic!("download should have failed"),
                    _ => {}
                }
            }
        }

        #[test]
        fn a_site_login_answers_a_401() {
            let data = body(LEN);
            let behavior = Behavior {
                basic_auth: Some("ann:pa ss".into()),
                ..Behavior::default()
            };
            let server = TestServer::start_with(data.clone(), behavior);
            let dir = temp_dir("login");

            // No login for this host: a clear error, at once (no retries).
            let start = Instant::now();
            let err = download_err(request(&server.url, &dir, FileExists::Rename));
            assert_eq!(err, "This site needs a login: 127.0.0.1 (realm: files)");
            assert_eq!(
                login_wanted(&err),
                Some(LoginWanted {
                    host: "127.0.0.1".into(),
                    realm: Some("files".into()),
                    refused: false
                })
            );
            assert!(start.elapsed() < Duration::from_secs(3), "not retried");

            // The wrong password is refused.
            let mut req = request(&server.url, &dir, FileExists::Rename);
            req.net = Arc::new(NetConfig {
                ftp_active: false,
                ftp_proxy: None,
                ftp_proxy_login: Default::default(),
                proxy: None,
                logins: vec![login("127.0.0.1", "ann", "wrong")],
            });
            let err = download_err(req);
            assert_eq!(err, "The login for 127.0.0.1 was refused (realm: files)");
            assert!(login_wanted(&err).is_some_and(|w| w.refused && w.host == "127.0.0.1"));
            assert_eq!(login_wanted("server returned HTTP 404"), None);
            assert_eq!(login_wanted("This site needs a login: ftp.example.com").map(|w| w.realm), Some(None));

            // The right one downloads every range, each answering its own 401.
            let mut req = request(&server.url, &dir, FileExists::Rename);
            req.net = Arc::new(NetConfig {
                ftp_active: false,
                ftp_proxy: None,
                ftp_proxy_login: Default::default(),
                proxy: None,
                logins: vec![login("example.com", "x", "y"), login("127.0.0.1", "ann", "pa ss")],
            });
            let path = download(|_| {}, req);
            assert_eq!(fs::read(&path).unwrap(), data);
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn a_login_never_follows_a_redirect_to_another_host() {
            let data = body(LEN);
            // B is "localhost", a different host name from A's 127.0.0.1.
            let b = TestServer::start(data.clone());
            let b_url = b.url.replace("127.0.0.1", "localhost");
            let a = TestServer::start_with(
                data.clone(),
                Behavior {
                    basic_auth: Some("ann:pw".into()),
                    redirect_to: Some(b_url),
                    ..Behavior::default()
                },
            );
            let dir = temp_dir("redirect");
            let mut req = request(&a.url, &dir, FileExists::Rename);
            req.net = Arc::new(NetConfig {
                ftp_active: false,
                ftp_proxy: None,
                ftp_proxy_login: Default::default(),
                proxy: None,
                logins: vec![login("127.0.0.1", "ann", "pw")],
            });
            let path = download(|_| {}, req);
            assert_eq!(fs::read(&path).unwrap(), data);
            assert!(!a.auth_seen.lock().unwrap().is_empty(), "A got the login");
            assert!(b.auth_seen.lock().unwrap().is_empty(), "B never did");
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn downloads_go_through_the_proxy() {
            // A minimal HTTP proxy: it records the requests and forwards them.
            let data = body(LEN);
            let origin = TestServer::start(data.clone());
            let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let proxy_addr = proxy.local_addr().unwrap().to_string();
            let through = Arc::new(Mutex::new(Vec::<String>::new()));
            let log = Arc::clone(&through);
            thread::spawn(move || {
                for client in proxy.incoming().flatten() {
                    let log = Arc::clone(&log);
                    thread::spawn(move || {
                        use std::io::{BufRead, BufReader};
                        let mut reader = BufReader::new(client.try_clone().unwrap());
                        let mut head = String::new();
                        let mut first = String::new();
                        reader.read_line(&mut first).unwrap();
                        log.lock().unwrap().push(first.trim().to_string());
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                                break;
                            }
                            if line.to_ascii_lowercase().starts_with("proxy-authorization:") {
                                log.lock().unwrap().push(format!("auth {}", line.trim()));
                            } else if !line.to_ascii_lowercase().starts_with("proxy-") {
                                head.push_str(&line);
                            }
                        }
                        // "GET http://host:port/path HTTP/1.1" -> origin.
                        let target = first.split_whitespace().nth(1).unwrap().to_string();
                        let rest = target.trim_start_matches("http://");
                        let (authority, path) = rest.split_at(rest.find('/').unwrap());
                        let mut up = std::net::TcpStream::connect(authority).unwrap();
                        write!(up, "GET {path} HTTP/1.1\r\n{head}\r\n").unwrap();
                        let mut client = client;
                        std::io::copy(&mut up, &mut client).ok();
                    });
                }
            });
            let dir = temp_dir("proxy");
            let mut req = request(&origin.url, &dir, FileExists::Rename);
            req.net = Arc::new(NetConfig {
                ftp_active: false,
                ftp_proxy: None,
                ftp_proxy_login: Default::default(),
                proxy: Some(crate::net::ProxyRules {
                    http: Some(proxy_addr),
                    http_login: crate::net::ProxyLogin::new("pu", "pp"),
                    ..Default::default()
                }),
                logins: Vec::new(),
            });

            let path = download(|_| {}, req);
            assert_eq!(fs::read(&path).unwrap(), data);
            let seen = through.lock().unwrap();
            let requests: Vec<_> = seen.iter().filter(|l| l.starts_with("GET ")).collect();
            let auths: Vec<_> = seen.iter().filter(|l| l.starts_with("auth ")).collect();
            assert!(requests.len() >= 2, "the probe and the ranges went through it: {seen:?}");
            assert!(requests.iter().all(|l| l.starts_with(&format!("GET {}", origin.url))));
            // "pu:pp" in base64, on every request.
            assert_eq!(auths.len(), requests.len(), "{seen:?}");
            assert!(auths.iter().all(|l| l.ends_with("Basic cHU6cHA=")), "{seen:?}");
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn downloads_go_to_their_category_folder() {
            let server = TestServer::start(body(LEN));
            let dir = temp_dir("categories");
            let settings = crate::settings::Settings {
                download_dir: Some(dir.display().to_string()),
                ..crate::settings::Settings::default()
            };
            let mut req = request(&server.url, &dir, FileExists::Rename);
            req.folders = settings.folders();
            req.filename = Some("Report.PDF".into());
            assert_eq!(download(|_| {}, req), dir.join("Documents").join("Report.PDF"));

            let mut req = request(&server.url, &dir, FileExists::Rename);
            req.folders = settings.folders();
            req.filename = Some("photo.jpg".into());
            assert_eq!(download(|_| {}, req), dir.join("photo.jpg"));
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn a_refreshed_link_keeps_the_partial_file_only_for_the_same_file() {
            let data = body(LEN);
            let server = TestServer::start(data.clone());
            let dir = temp_dir("refresh");
            let partial = dir.join("file.bin");
            fs::write(&partial, vec![0u8; LEN]).unwrap();
            let mut m = Manifest::new("https://old/link".into(), LEN as u64, Some("\"v1\"".into()), None, &split(LEN as u64, 2));
            m.segments[0].done = 1000;
            m.save(&Manifest::sidecar_path(&partial)).unwrap();

            let found = inspect(&server.url, None, &[], &NetConfig::default()).unwrap();
            assert_eq!(found.etag.as_deref(), Some("\"v1\""));
            assert_eq!(partial_matches(&partial, &found, false), Ok(()));

            let other_size = Inspection { total: Some(LEN as u64 + 1), ..found.clone() };
            assert!(partial_matches(&partial, &other_size, false).unwrap_err().contains("2.0 MB file"));
            let changed = Inspection { etag: Some("\"v2\"".into()), ..found.clone() };
            assert!(partial_matches(&partial, &changed, false).unwrap_err().contains("changed"));
            let unknown = Inspection { total: None, ..found.clone() };
            assert!(partial_matches(&partial, &unknown, false).is_err());

            // Nothing on disk: nothing to keep, any link fits.
            assert_eq!(partial_matches(&dir.join("none.bin"), &other_size, false), Ok(()));
            fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn inspect_reports_name_and_size_without_writing() {
            let server = TestServer::start(body(LEN));
            let found = inspect(&server.url, None, &[], &NetConfig::default()).unwrap();
            assert_eq!(found.filename, "file.bin");
            assert_eq!(found.total, Some(LEN as u64));
            assert!(found.resumable);
            let named = inspect(&server.url, Some("a/b:c.txt"), &[], &NetConfig::default()).unwrap();
            assert_eq!(named.filename, "a_b_c.txt");
        }

        #[test]
        fn speed_limit_holds_a_download_to_its_limit() {
            // 2 MiB at 1 MiB/s over 4 connections: about 2 seconds. The limit
            // must never let it finish early. A busy machine running other
            // tests at once can only make it later, so that side has more
            // room.
            let server = TestServer::start(body(LEN));
            let dir = temp_dir("limit");
            let start = Instant::now();
            let path = download(
                |e| e.set_speed_limit(Some(1024 * 1024)),
                request(&server.url, &dir, FileExists::Rename),
            );
            let secs = start.elapsed().as_secs_f64();
            assert_eq!(fs::read(&path).unwrap(), body(LEN));
            assert!((1.8..3.0).contains(&secs), "took {secs:.2}s, expected about 2s");
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
