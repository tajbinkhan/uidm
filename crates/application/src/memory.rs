//! In-memory adapters for the use cases' tests. Their own helpers take and
//! return ids as plain numbers, so tests stay short; the ports they
//! implement take the id types.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use udm_domain::address::Link;
use udm_domain::bytes::ByteCount;
use udm_domain::checksum::Algorithm;
use udm_domain::download::{CommandLine, Download, DownloadEntry, Status};
use udm_domain::filename::FileName;
use udm_domain::id::{DownloadId, QueueId};
use udm_domain::media::Conversion;
use udm_domain::package::{Package, Password, UnpackError};
use udm_domain::ports::{
    Answers, Attention, Clock, ControlError, Converter, DownloadControl, DownloadFiles, DownloadIntake, DownloadList,
    EarlyStarts, FileHashes, FilesError, HashError, LimitLift, PackageStore, PausedDownloads, PowerControl,
    ProblemReports, QueueFinish, QueueRuns, QueueSounds, QueueStore, QuotaState, ReleaseSource, SettingsError,
    SettingsStore, SyncChecks, UnpackJob, Unpacked, Unpacker, UpdateChecks, UserIdle,
};
use udm_domain::queue::{Moment, PowerRequest, Queue, RunEnd, WhenDone, MAIN_QUEUE, MAIN_QUEUE_NAME};
use udm_domain::quota::QuotaNotice;
use udm_domain::settings::Settings;
use udm_domain::update::{Release, SetupAsset, UpdateError};

use crate::daemon::{Command, DaemonClient, DaemonError, Event, Lost};

pub struct Queues {
    list: Mutex<Vec<Queue>>,
    changes: AtomicUsize,
}

impl Queues {
    /// Just the main queue, empty.
    pub fn main() -> Self {
        Queues { list: Mutex::new(vec![Queue::new(MAIN_QUEUE, MAIN_QUEUE_NAME)]), changes: AtomicUsize::new(0) }
    }

    pub fn get(&self, id: impl Into<QueueId>) -> Queue {
        let id = id.into();
        self.edit(|qs| qs.iter().find(|q| q.id == id).cloned()).expect("the queue exists")
    }

    /// How many times the queues were announced as changed.
    pub fn changes(&self) -> usize {
        self.changes.load(Ordering::SeqCst)
    }
}

impl QueueStore for Queues {
    fn edit<T>(&self, f: impl FnOnce(&mut Vec<Queue>) -> T) -> T {
        f(&mut self.list.lock().unwrap())
    }

    fn queues_changed(&self) {
        self.changes.fetch_add(1, Ordering::SeqCst);
    }
}

/// The download list, with the requests kept for retries.
#[derive(Default)]
pub struct Downloads {
    rows: Mutex<HashMap<DownloadId, DownloadEntry>>,
    requests: Mutex<HashMap<DownloadId, Download>>,
}

/// A row for download `id` in state `status`.
pub fn row(id: impl Into<DownloadId>, status: Status) -> DownloadEntry {
    let id = id.into();
    let json = serde_json::json!({
        "id": id, "url": format!("https://dl.example/{id}.zip"), "status": status,
        "connections": 0, "done": 0, "resumable": false,
    });
    serde_json::from_value(json).expect("a valid row")
}

impl Downloads {
    pub fn with(entries: &[(u64, Status)]) -> Self {
        Self::rows(entries.iter().map(|(id, s)| row(*id, *s)))
    }

    pub fn rows(rows: impl IntoIterator<Item = DownloadEntry>) -> Self {
        Downloads { rows: Mutex::new(rows.into_iter().map(|e| (e.id, e)).collect()), ..Self::default() }
    }

    /// Put download `id` in state `status`. Returns false when there is no
    /// such download.
    pub fn set(&self, id: impl Into<DownloadId>, status: Status) -> bool {
        self.rows.lock().unwrap().get_mut(&id.into()).map(|e| e.status = status).is_some()
    }

    pub fn has_row(&self, id: impl Into<DownloadId>) -> bool {
        self.rows.lock().unwrap().contains_key(&id.into())
    }
}

impl DownloadList for Downloads {
    fn status(&self, id: DownloadId) -> Option<Status> {
        self.rows.lock().unwrap().get(&id).map(|e| e.status)
    }

    fn entry(&self, id: DownloadId) -> Option<DownloadEntry> {
        self.rows.lock().unwrap().get(&id).cloned()
    }

    fn take_request(&self, id: DownloadId) -> Option<Download> {
        self.requests.lock().unwrap().remove(&id)
    }

    fn drop_row(&self, id: DownloadId) {
        self.rows.lock().unwrap().remove(&id);
    }

    fn update(&self, id: DownloadId, f: impl FnOnce(&mut DownloadEntry) -> bool) -> Option<bool> {
        let mut rows = self.rows.lock().unwrap();
        let row = rows.get_mut(&id)?;
        let mut changed = row.clone();
        let ok = f(&mut changed);
        if ok {
            *row = changed;
        }
        Some(ok)
    }

    fn request(&self, id: DownloadId) -> Option<Download> {
        self.requests.lock().unwrap().get(&id).cloned()
    }

    fn keep_request(&self, id: DownloadId, download: Download) {
        self.requests.lock().unwrap().insert(id, download);
    }

    fn edit_request(&self, id: DownloadId, f: impl FnOnce(&mut Download)) {
        if let Some(d) = self.requests.lock().unwrap().get_mut(&id) {
            f(d);
        }
    }

    fn finished(&self) -> HashSet<DownloadId> {
        self.ids_where(|_, s| s == Status::Done).into_iter().collect()
    }

    fn set_waiting(&self, id: DownloadId) -> bool {
        self.set(id, Status::Waiting)
    }

    fn find_last(&self, keep: impl Fn(&DownloadEntry) -> bool) -> Option<DownloadEntry> {
        let rows = self.rows.lock().unwrap();
        let mut found: Vec<&DownloadEntry> = rows.values().filter(|e| keep(e)).collect();
        found.sort_by_key(|e| e.id);
        found.last().map(|e| (*e).clone())
    }

    fn ids_where(&self, keep: impl Fn(DownloadId, Status) -> bool) -> Vec<DownloadId> {
        let mut ids: Vec<DownloadId> =
            self.rows.lock().unwrap().values().filter(|e| keep(e.id, e.status)).map(|e| e.id).collect();
        ids.sort_unstable();
        ids
    }
}

/// The queues that have a run going, with what each run started, and the
/// retries counted per download.
#[derive(Default)]
pub struct Runs(Mutex<HashMap<QueueId, HashSet<DownloadId>>>, Mutex<HashMap<DownloadId, u32>>);

impl Runs {
    pub fn running(&self, queue: impl Into<QueueId>) -> bool {
        self.0.lock().unwrap().contains_key(&queue.into())
    }

    /// Queue `queue`'s run started download `id`.
    pub fn started(&self, queue: impl Into<QueueId>, id: impl Into<DownloadId>) {
        self.0.lock().unwrap().entry(queue.into()).or_default().insert(id.into());
    }
}

impl QueueRuns for Runs {
    fn begin(&self, queue: QueueId) {
        self.0.lock().unwrap().insert(queue, HashSet::new());
    }

    fn end(&self, queue: QueueId) {
        self.0.lock().unwrap().remove(&queue);
    }

    fn started_in_run(&self, queue: QueueId) -> HashSet<DownloadId> {
        self.0.lock().unwrap().get(&queue).cloned().unwrap_or_default()
    }

    fn note_started(&self, queue: QueueId, id: DownloadId) {
        self.started(queue, id);
    }

    fn count_retry(&self, id: DownloadId, max: u32) -> Option<u32> {
        let mut retries = self.1.lock().unwrap();
        let n = retries.entry(id).or_insert(0);
        (*n < max).then(|| {
            *n += 1;
            *n
        })
    }

    fn forget_retries(&self, ids: &HashSet<DownloadId>) {
        self.1.lock().unwrap().retain(|id, _| !ids.contains(id));
    }
}

/// Counts the sounds played.
#[derive(Default)]
pub struct Sounds(AtomicUsize);

impl Sounds {
    pub fn started(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

impl QueueSounds for Sounds {
    fn queue_started(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Records the downloads held, stopped and restarted. Downloads in
/// `refuse` cannot be started.
#[derive(Default)]
pub struct Control {
    held: Mutex<Vec<u64>>,
    stopped: Mutex<Vec<u64>>,
    forgotten: Mutex<Vec<u64>>,
    restarted: Mutex<Vec<Restart>>,
    limits: Mutex<Vec<(u64, Option<u64>)>>,
    started_again: Mutex<Vec<(u64, Download)>>,
    dispatched: Mutex<Vec<u64>>,
    canceled_waiting: Mutex<Vec<u64>>,
    waiting_for_link: Mutex<Option<DownloadId>>,
    refuse: HashSet<DownloadId>,
    /// What [`DownloadControl::check_partial`] answers.
    partial_check: Option<Result<Option<udm_domain::download::Mismatch>, ControlError>>,
}

/// One call to [`DownloadControl::restart`].
#[derive(Debug, Clone)]
pub struct Restart {
    pub download: Download,
    pub path: Option<PathBuf>,
    pub partial: Option<PathBuf>,
    pub remember_limit: bool,
}

/// The id [`Control`] gives the first restarted download.
pub const RESTARTED_ID: DownloadId = DownloadId::new(100);

impl Control {
    /// A control that cannot start the downloads in `refuse`.
    pub fn refusing(refuse: &[u64]) -> Self {
        Control { refuse: refuse.iter().copied().map(DownloadId::new).collect(), ..Control::default() }
    }

    pub fn held(&self) -> Vec<u64> {
        self.held.lock().unwrap().clone()
    }

    pub fn stopped(&self) -> Vec<u64> {
        self.stopped.lock().unwrap().clone()
    }

    pub fn forgotten(&self) -> Vec<u64> {
        self.forgotten.lock().unwrap().clone()
    }

    pub fn restarted(&self) -> Vec<Restart> {
        self.restarted.lock().unwrap().clone()
    }

    pub fn limits(&self) -> Vec<(u64, Option<u64>)> {
        self.limits.lock().unwrap().clone()
    }

    pub fn started_again(&self) -> Vec<(u64, Download)> {
        self.started_again.lock().unwrap().clone()
    }

    pub fn dispatched(&self) -> Vec<u64> {
        self.dispatched.lock().unwrap().clone()
    }

    pub fn canceled_waiting(&self) -> Vec<u64> {
        self.canceled_waiting.lock().unwrap().clone()
    }

    /// A control whose partial-file checks answer `answer`.
    pub fn checking(answer: Result<Option<udm_domain::download::Mismatch>, ControlError>) -> Self {
        Control { partial_check: Some(answer), ..Control::default() }
    }
}

impl DownloadControl for Control {
    fn hold(&self, id: DownloadId) {
        self.held.lock().unwrap().push(id.get());
    }

    fn stop(&self, id: DownloadId) {
        self.stopped.lock().unwrap().push(id.get());
    }

    fn forget(&self, id: DownloadId) {
        self.forgotten.lock().unwrap().push(id.get());
    }

    fn set_limit(&self, id: DownloadId, bytes_per_sec: Option<u64>) {
        self.limits.lock().unwrap().push((id.get(), bytes_per_sec));
    }

    fn start_again(&self, id: DownloadId, download: Download) {
        self.started_again.lock().unwrap().push((id.get(), download));
    }

    fn dispatch(&self, id: DownloadId) -> Result<(), ControlError> {
        if self.refuse.contains(&id) {
            return Err(ControlError(format!("download {id} cannot start")));
        }
        self.dispatched.lock().unwrap().push(id.get());
        Ok(())
    }

    fn cancel_waiting(&self, id: DownloadId) {
        self.canceled_waiting.lock().unwrap().push(id.get());
    }

    fn wait_for_link(&self, id: Option<DownloadId>) {
        *self.waiting_for_link.lock().unwrap() = id;
    }

    fn waiting_for_link(&self) -> Option<DownloadId> {
        *self.waiting_for_link.lock().unwrap()
    }

    fn check_partial(
        &self,
        _download: &Download,
        _partial: &str,
    ) -> Result<Option<udm_domain::download::Mismatch>, ControlError> {
        self.partial_check.clone().expect("the test sets what the check answers")
    }

    fn restart(
        &self,
        download: Download,
        path: Option<PathBuf>,
        partial: Option<PathBuf>,
        remember_limit: bool,
    ) -> (DownloadId, Link) {
        let mut restarted = self.restarted.lock().unwrap();
        let url = download.url.clone();
        restarted.push(Restart { download, path, partial, remember_limit });
        (DownloadId::new(RESTARTED_ID.get() + restarted.len() as u64 - 1), url)
    }
}

/// Records what it was asked to recycle.
#[derive(Default)]
pub struct Files {
    fail: bool,
    recycled: Mutex<Vec<(Option<String>, Option<String>)>>,
    forgot_resume: Mutex<Vec<String>>,
    moved: Mutex<Vec<(PathBuf, PathBuf, bool)>>,
    existing: HashSet<PathBuf>,
    read_only: HashSet<PathBuf>,
}

impl Files {
    /// Files that never go: every call fails.
    pub fn failing() -> Self {
        Files { fail: true, ..Files::default() }
    }

    pub fn recycled(&self) -> Vec<(Option<String>, Option<String>)> {
        self.recycled.lock().unwrap().clone()
    }

    pub fn forgot_resume(&self) -> Vec<String> {
        self.forgot_resume.lock().unwrap().clone()
    }

    /// Files where something already is at each of `paths`.
    pub fn with_existing(paths: &[&str]) -> Self {
        Files { existing: paths.iter().map(PathBuf::from).collect(), ..Files::default() }
    }

    /// Files where UDM may not write into `dir`.
    pub fn read_only(dir: &str) -> Self {
        Files { read_only: [PathBuf::from(dir)].into(), ..Files::default() }
    }

    /// The moves made: from, to, and whether the file was finished.
    pub fn moved(&self) -> Vec<(PathBuf, PathBuf, bool)> {
        self.moved.lock().unwrap().clone()
    }
}

impl DownloadFiles for Files {
    fn recycle(
        &self,
        _download: Option<DownloadId>,
        path: Option<&str>,
        partial: Option<&str>,
    ) -> Result<(), FilesError> {
        if self.fail {
            return Err(FilesError("the file is open".into()));
        }
        self.recycled.lock().unwrap().push((path.map(String::from), partial.map(String::from)));
        Ok(())
    }

    fn forget_resume(&self, paths: &[&str]) {
        self.forgot_resume.lock().unwrap().extend(paths.iter().map(|p| p.to_string()));
    }

    fn move_finished(&self, from: &Path, to: &Path) -> Result<(), FilesError> {
        if self.fail {
            return Err(FilesError("the file is open".into()));
        }
        self.moved.lock().unwrap().push((from.to_path_buf(), to.to_path_buf(), true));
        Ok(())
    }

    fn move_partial(&self, from: &Path, to: &Path) -> Result<(), FilesError> {
        self.moved.lock().unwrap().push((from.to_path_buf(), to.to_path_buf(), false));
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        self.existing.contains(path)
    }

    fn is_file(&self, path: &Path) -> bool {
        self.existing.contains(path)
    }

    fn can_write(&self, dir: &Path) -> bool {
        !self.read_only.contains(dir)
    }
}

/// What was paused.
#[derive(Default)]
pub struct Paused(Mutex<(HashSet<QueueId>, HashSet<DownloadId>)>);

impl PausedDownloads for Paused {
    fn remember(&self, queues: &[QueueId], downloads: &[DownloadId]) {
        let mut p = self.0.lock().unwrap();
        p.0.extend(queues);
        p.1.extend(downloads);
    }

    fn take(&self) -> (HashSet<QueueId>, HashSet<DownloadId>) {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

/// A clock stopped at Wed 2026-10-28 09:30.
pub struct FixedClock(pub Moment);

pub const FIXED_NOW: FixedClock = FixedClock(Moment { day: 20754, minute: 9 * 60 + 30 });

impl Clock for FixedClock {
    fn now(&self) -> Moment {
        self.0
    }
}

/// Records the checks started; `running` are the ones going on.
#[derive(Default)]
pub struct Checks {
    running: HashSet<DownloadId>,
    started: Mutex<Vec<u64>>,
}

impl Checks {
    /// Checks where the downloads in `running` are being checked now.
    pub fn going_on(running: &[u64]) -> Self {
        Checks { running: running.iter().copied().map(DownloadId::new).collect(), ..Checks::default() }
    }

    pub fn started(&self) -> Vec<u64> {
        self.started.lock().unwrap().clone()
    }
}

impl SyncChecks for Checks {
    fn checking(&self) -> HashSet<DownloadId> {
        self.running.clone()
    }

    fn check(&self, id: DownloadId) {
        self.started.lock().unwrap().push(id.get());
    }
}

/// Records the runs that ended.
#[derive(Default)]
pub struct Finish(Mutex<Vec<(String, RunEnd)>>);

impl Finish {
    pub fn ended(&self) -> Vec<(String, RunEnd)> {
        self.0.lock().unwrap().clone()
    }
}

impl QueueFinish for Finish {
    fn run_ended(&self, queue: &str, end: &RunEnd) {
        self.0.lock().unwrap().push((queue.to_string(), end.clone()));
    }
}

/// A quota count fixed at `used` bytes, wherever the window lies.
#[derive(Default)]
pub struct Quota {
    used: Mutex<ByteCount>,
    frees_at: u64,
    held: Mutex<Option<(Vec<QueueId>, HashSet<DownloadId>)>>,
    warned: Mutex<bool>,
    announced: Mutex<Vec<QuotaNotice>>,
}

impl Quota {
    /// `used` bytes in the window, which frees up at minute `frees_at`.
    pub fn at(used: ByteCount, frees_at: u64) -> Self {
        Quota { used: Mutex::new(used), frees_at, ..Quota::default() }
    }

    pub fn set_used(&self, used: ByteCount) {
        *self.used.lock().unwrap() = used;
    }

    pub fn announced(&self) -> Vec<QuotaNotice> {
        self.announced.lock().unwrap().clone()
    }
}

impl QuotaState for Quota {
    fn used(&self, _now: u64, _window: u64) -> ByteCount {
        *self.used.lock().unwrap()
    }

    fn frees_at(&self, _now: u64, _window: u64, _limit: ByteCount) -> u64 {
        self.frees_at
    }

    fn paused(&self) -> bool {
        self.held.lock().unwrap().is_some()
    }

    fn hold(&self, queues: &[QueueId], downloads: &[DownloadId]) {
        let mut held = self.held.lock().unwrap();
        let (q, d) = held.get_or_insert_with(Default::default);
        q.extend(queues);
        d.extend(downloads);
    }

    fn release(&self) -> (Vec<QueueId>, HashSet<DownloadId>) {
        self.held.lock().unwrap().take().unwrap_or_default()
    }

    fn warned(&self) -> bool {
        *self.warned.lock().unwrap()
    }

    fn set_warned(&self, warned: bool) {
        *self.warned.lock().unwrap() = warned;
    }

    fn announce(&self, notice: QuotaNotice) {
        self.announced.lock().unwrap().push(notice);
    }
}

/// Records what it was asked to do; never powers anything off.
#[derive(Default)]
pub struct Power {
    exits: AtomicUsize,
    countdown: Mutex<Option<PowerRequest>>,
    started: Mutex<Vec<(PowerRequest, u32)>>,
    canceled: AtomicUsize,
    powered_off: Mutex<Vec<(WhenDone, bool)>>,
}

impl Power {
    pub fn exits(&self) -> usize {
        self.exits.load(Ordering::SeqCst)
    }

    pub fn started(&self) -> Vec<(PowerRequest, u32)> {
        self.started.lock().unwrap().clone()
    }

    pub fn canceled(&self) -> usize {
        self.canceled.load(Ordering::SeqCst)
    }

    pub fn powered_off(&self) -> Vec<(WhenDone, bool)> {
        self.powered_off.lock().unwrap().clone()
    }
}

impl PowerControl for Power {
    fn exit(&self) {
        self.exits.fetch_add(1, Ordering::SeqCst);
    }

    fn start_countdown(&self, request: &PowerRequest, seconds: u32) {
        *self.countdown.lock().unwrap() = Some(request.clone());
        self.started.lock().unwrap().push((request.clone(), seconds));
    }

    fn take_countdown(&self) -> Option<PowerRequest> {
        self.countdown.lock().unwrap().take()
    }

    fn countdown_canceled(&self) {
        self.canceled.fetch_add(1, Ordering::SeqCst);
    }

    fn power_off(&self, action: WhenDone, force_close: bool) {
        self.powered_off.lock().unwrap().push((action, force_close));
    }
}

/// A release source that offers `latest` and records what it was asked.
pub struct Releases {
    latest: Result<Release, UpdateError>,
    fetched: Mutex<Vec<SetupAsset>>,
    started: Mutex<Vec<PathBuf>>,
}

impl Releases {
    pub fn offering(latest: Result<Release, UpdateError>) -> Self {
        Releases { latest, fetched: Mutex::default(), started: Mutex::default() }
    }

    pub fn fetched(&self) -> Vec<SetupAsset> {
        self.fetched.lock().unwrap().clone()
    }

    pub fn started(&self) -> Vec<PathBuf> {
        self.started.lock().unwrap().clone()
    }
}

impl ReleaseSource for Releases {
    fn latest(&self) -> Result<Release, UpdateError> {
        self.latest.clone()
    }

    fn fetch_setup(&self, setup: &SetupAsset) -> Result<PathBuf, UpdateError> {
        self.fetched.lock().unwrap().push(setup.clone());
        Ok(PathBuf::from("/tmp").join(&setup.name))
    }

    fn run_setup(&self, _setup: &SetupAsset, path: &Path) -> Result<(), UpdateError> {
        self.started.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }
}

/// The weekly check's setting and the checks noted.
#[derive(Default)]
pub struct Checked {
    on: bool,
    last: Mutex<u64>,
    noted: Mutex<Vec<(u64, bool)>>,
}

impl Checked {
    pub fn weekly(on: bool, last: u64) -> Self {
        Checked { on, last: Mutex::new(last), ..Checked::default() }
    }

    pub fn noted(&self) -> Vec<(u64, bool)> {
        self.noted.lock().unwrap().clone()
    }
}

impl UpdateChecks for Checked {
    fn weekly(&self) -> (bool, u64) {
        (self.on, *self.last.lock().unwrap())
    }

    fn note_checked(&self, at: u64, save: bool) {
        *self.last.lock().unwrap() = at;
        self.noted.lock().unwrap().push((at, save));
    }
}

/// Records the conversions started; `ffmpeg` says whether it is there.
#[derive(Default)]
pub struct Convert {
    ffmpeg: bool,
    started: Mutex<Vec<Conversion>>,
    running: HashSet<DownloadId>,
    stopped: Mutex<Vec<u64>>,
}

impl Convert {
    /// A converter that finds ffmpeg.
    pub fn with_ffmpeg() -> Self {
        Convert { ffmpeg: true, ..Convert::default() }
    }

    pub fn started(&self) -> Vec<Conversion> {
        self.started.lock().unwrap().clone()
    }

    /// A converter running a conversion as each of the rows `running`.
    pub fn running(running: &[u64]) -> Self {
        Convert { running: running.iter().copied().map(DownloadId::new).collect(), ..Convert::default() }
    }

    pub fn stopped(&self) -> Vec<u64> {
        self.stopped.lock().unwrap().clone()
    }
}

impl Converter for Convert {
    fn available(&self) -> bool {
        self.ffmpeg
    }

    fn start(&self, conversion: Conversion) {
        self.started.lock().unwrap().push(conversion);
    }

    fn stop(&self, id: DownloadId) -> bool {
        if !self.running.contains(&id) {
            return false;
        }
        self.stopped.lock().unwrap().push(id.get());
        true
    }

    fn any_running(&self) -> bool {
        !self.started.lock().unwrap().is_empty()
    }
}

/// Stages downloads as rows in `list`, numbering them from 1000. Downloads
/// whose address ends in `.bad` are not sendable.
pub struct Intake<'a> {
    list: &'a Downloads,
    next: AtomicUsize,
}

impl<'a> Intake<'a> {
    pub fn into(list: &'a Downloads) -> Self {
        Intake { list, next: AtomicUsize::new(1000) }
    }
}

impl DownloadIntake for Intake<'_> {
    fn sendable(&self, download: &Download) -> bool {
        !download.url.as_str().ends_with(".bad")
    }

    fn stage(&self, download: Download) -> (DownloadId, Link) {
        let n = self.next.fetch_add(1, Ordering::SeqCst) as u64;
        let id = DownloadId::new(n);
        let mut e = row(id, Status::Waiting);
        e.url = download.url.clone();
        self.list.rows.lock().unwrap().insert(id, e);
        self.list.keep_request(id, download.clone());
        (id, download.url)
    }
}

/// Records what was brought in front of the user.
#[derive(Default)]
pub struct Shown {
    done: Mutex<Vec<u64>>,
    window: AtomicUsize,
}

impl Shown {
    pub fn done(&self) -> Vec<u64> {
        self.done.lock().unwrap().clone()
    }

    pub fn windows(&self) -> usize {
        self.window.load(Ordering::SeqCst)
    }
}

impl Attention for Shown {
    fn show_done(&self, id: DownloadId) {
        self.done.lock().unwrap().push(id.get());
    }

    fn show_window(&self) {
        self.window.fetch_add(1, Ordering::SeqCst);
    }
}

/// Settings kept in memory; `refuse` makes every save fail.
#[derive(Default)]
pub struct Prefs {
    settings: Mutex<Settings>,
    saves: AtomicUsize,
    refuse: bool,
}

impl Prefs {
    pub fn with(settings: Settings) -> Self {
        Prefs { settings: Mutex::new(settings), ..Prefs::default() }
    }

    pub fn saves(&self) -> usize {
        self.saves.load(Ordering::SeqCst)
    }
}

impl SettingsStore for Prefs {
    fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    fn save_settings(&self, settings: Settings) -> Result<(), SettingsError> {
        if self.refuse {
            return Err(SettingsError::Refused("the settings file is read-only".into()));
        }
        *self.settings.lock().unwrap() = settings;
        self.saves.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Early starts: downloads in `started` started early and are handed over.
#[derive(Default)]
pub struct Early {
    started: HashSet<DownloadId>,
    handed: Mutex<Vec<(u64, bool, Option<u64>)>>,
    began: Mutex<Vec<(u64, String)>>,
}

impl Early {
    pub fn started(ids: &[u64]) -> Self {
        Early { started: ids.iter().copied().map(DownloadId::new).collect(), ..Early::default() }
    }

    pub fn handed(&self) -> Vec<(u64, bool, Option<u64>)> {
        self.handed.lock().unwrap().clone()
    }

    /// The early starts begun, with the file name each was given.
    pub fn began(&self) -> Vec<(u64, String)> {
        self.began.lock().unwrap().clone()
    }
}

impl EarlyStarts for Early {
    fn start(&self, id: DownloadId, _download: Download, filename: &FileName) {
        self.began.lock().unwrap().push((id.get(), filename.to_string()));
    }

    fn hand_over(&self, id: DownloadId, start: bool, queue: Option<QueueId>) -> bool {
        if !self.started.contains(&id) {
            return false;
        }
        self.handed.lock().unwrap().push((id.get(), start, queue.map(QueueId::get)));
        true
    }
}

/// Records the dialogs waited for.
#[derive(Default)]
pub struct Waiting(Mutex<Vec<u64>>);

impl Waiting {
    pub fn awaited(&self) -> Vec<u64> {
        self.0.lock().unwrap().clone()
    }
}

impl Answers for Waiting {
    fn await_answer(&self, id: DownloadId) {
        self.0.lock().unwrap().push(id.get());
    }

    fn await_new_name(&self, id: DownloadId) {
        self.0.lock().unwrap().push(id.get());
    }
}

/// The packages, counting the changes announced.
#[derive(Default)]
pub struct Packages {
    list: Mutex<Vec<Package>>,
    changes: AtomicUsize,
}

impl Packages {
    pub fn with(list: Vec<Package>) -> Self {
        Packages { list: Mutex::new(list), ..Packages::default() }
    }

    pub fn list(&self) -> Vec<Package> {
        self.list.lock().unwrap().clone()
    }

    pub fn get(&self, id: u64) -> Option<Package> {
        self.list().into_iter().find(|p| p.id.get() == id)
    }

    pub fn changes(&self) -> usize {
        self.changes.load(Ordering::SeqCst)
    }
}

impl PackageStore for Packages {
    fn edit_packages<T>(&self, f: impl FnOnce(&mut Vec<Package>) -> T) -> T {
        f(&mut self.list.lock().unwrap())
    }

    fn packages_changed(&self) {
        self.changes.fetch_add(1, Ordering::SeqCst);
    }
}

/// An unpacker that records each job and the password it came with, and
/// answers with what a test set: by default every archive unpacks one file.
#[derive(Default)]
pub struct Unpacks {
    jobs: Mutex<Vec<(UnpackJob, Option<String>)>>,
    answer: Mutex<Option<Result<Unpacked, UnpackError>>>,
    /// The password that opens every archive, when they are encrypted.
    password: Option<String>,
}

impl Unpacks {
    /// Archives that need `password`.
    pub fn locked(password: &str) -> Self {
        Unpacks { password: Some(password.into()), ..Unpacks::default() }
    }

    /// Archives that fail as `answer` says.
    pub fn answering(answer: Result<Unpacked, UnpackError>) -> Self {
        Unpacks { answer: Mutex::new(Some(answer)), ..Unpacks::default() }
    }

    pub fn jobs(&self) -> Vec<(UnpackJob, Option<String>)> {
        self.jobs.lock().unwrap().clone()
    }
}

impl Unpacker for Unpacks {
    fn unpack(&self, job: &UnpackJob, password: Option<&Password>, _: &Settings) -> Result<Unpacked, UnpackError> {
        let given = password.map(|p| p.expose().to_string());
        self.jobs.lock().unwrap().push((job.clone(), given.clone()));
        match (&self.password, given) {
            (Some(_), None) => return Err(UnpackError::NeedsPassword),
            (Some(want), Some(got)) if *want != got => return Err(UnpackError::WrongPassword),
            _ => {}
        }
        self.answer.lock().unwrap().clone().unwrap_or(Ok(Unpacked { files: 1, bytes: ByteCount::new(10) }))
    }
}

/// File digests by path; a path it was not given cannot be read.
#[derive(Default)]
pub struct Hashes(HashMap<PathBuf, Vec<u8>>);

impl Hashes {
    pub fn with(path: &str, digest: Vec<u8>) -> Self {
        Hashes(HashMap::from([(PathBuf::from(path), digest)]))
    }
}

impl FileHashes for Hashes {
    fn digest(&self, path: &Path, _algorithm: Algorithm) -> Result<Vec<u8>, HashError> {
        self.0.get(path).cloned().ok_or_else(|| HashError(format!("Could not read {}", path.display())))
    }
}

/// The time since the last input, as a test sets it.
#[derive(Default)]
pub struct Idle {
    idle: Mutex<Option<Duration>>,
    asked: AtomicUsize,
}

impl Idle {
    pub fn set(&self, idle: Option<Duration>) {
        *self.idle.lock().unwrap() = idle;
    }

    /// How many times the time was asked for.
    pub fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }
}

impl UserIdle for Idle {
    fn idle_for(&self) -> Option<Duration> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        *self.idle.lock().unwrap()
    }
}

/// Whether the speed limit is lifted, with each change.
#[derive(Default)]
pub struct Lift {
    changes: Mutex<Vec<bool>>,
}

impl Lift {
    pub fn changes(&self) -> Vec<bool> {
        self.changes.lock().unwrap().clone()
    }
}

impl LimitLift for Lift {
    fn lifted(&self) -> bool {
        self.changes.lock().unwrap().last().copied().unwrap_or(false)
    }

    fn set_lifted(&self, lifted: bool) {
        self.changes.lock().unwrap().push(lifted);
    }
}

type Answerer = dyn Fn(&Command) -> Vec<Event> + Send + Sync;
type Sink = Box<dyn FnMut(Event) + Send>;

/// A pretend daemon for the remote use cases: it answers each command with
/// the events `answer` gives, then `Pong` for a `Ping`, as the daemon
/// answers a client's requests in order.
pub struct FakeDaemon {
    answer: Box<Answerer>,
    /// Why connecting fails, when it does.
    unreachable: Option<String>,
    sink: Mutex<Option<Sink>>,
    sent: Mutex<Vec<Command>>,
    connections: Mutex<usize>,
}

impl FakeDaemon {
    pub fn new(answer: impl Fn(&Command) -> Vec<Event> + Send + Sync + 'static) -> Self {
        FakeDaemon {
            answer: Box::new(answer),
            unreachable: None,
            sink: Mutex::new(None),
            sent: Mutex::new(Vec::new()),
            connections: Mutex::new(0),
        }
    }

    /// A daemon that lists `downloads` and the main queue, and answers
    /// nothing else.
    pub fn with_list(downloads: Vec<DownloadEntry>) -> Self {
        Self::listing(downloads, vec![Queue::main()], |_| Vec::new())
    }

    /// A daemon that lists `downloads` and `queues`, and answers the rest
    /// with `answer`.
    pub fn listing(
        downloads: Vec<DownloadEntry>,
        queues: Vec<Queue>,
        answer: impl Fn(&Command) -> Vec<Event> + Send + Sync + 'static,
    ) -> Self {
        Self::new(move |c| match c {
            Command::List => vec![
                Event::Downloads { items: downloads.clone() },
                Event::Queues { items: queues.clone() },
                Event::RefreshWaiting { id: None },
            ],
            other => answer(other),
        })
    }

    pub fn unreachable(why: &str) -> Self {
        FakeDaemon { unreachable: Some(why.to_string()), ..Self::new(|_| Vec::new()) }
    }

    /// The commands sent so far.
    pub fn sent(&self) -> Vec<Command> {
        self.sent.lock().unwrap().clone()
    }

    /// Send `event` unasked, as the daemon tells its clients of a change or
    /// that it is going.
    pub fn tell(&self, event: Event) {
        if let Some(sink) = self.sink.lock().unwrap().as_mut() {
            sink(event);
        }
    }

    /// How many times a client connected.
    pub fn connections(&self) -> usize {
        *self.connections.lock().unwrap()
    }
}

impl DaemonClient for FakeDaemon {
    fn connect(&self, _restart: bool, mut on_event: Box<dyn FnMut(Event) + Send>) {
        if let Some(why) = &self.unreachable {
            return on_event(Event::Lost(Lost::Failed(why.clone())));
        }
        on_event(Event::Connected);
        *self.connections.lock().unwrap() += 1;
        *self.sink.lock().unwrap() = Some(on_event);
    }

    fn send(&self, command: Command) -> bool {
        let mut events = (self.answer)(&command);
        if matches!(command, Command::Ping) {
            events.push(Event::Pong);
        }
        self.sent.lock().unwrap().push(command);
        let mut sink = self.sink.lock().unwrap();
        let Some(sink) = sink.as_mut() else {
            return false;
        };
        for event in events {
            sink(event);
        }
        true
    }

    fn run_command_line(&self, _command: CommandLine) -> Result<bool, DaemonError> {
        Err(DaemonError::Closed)
    }
}

/// A reports folder in memory: `add` puts a report in it, and `save`
/// writes one unless `refuse`. This copy is version 9.9.9, and each
/// program's log is `<program>.log` with one line.
#[derive(Default)]
pub struct Reports {
    pub refuse: bool,
    files: Mutex<Vec<(udm_domain::report::Report, bool)>>,
}

impl Reports {
    /// A folder that refuses every write.
    pub fn refusing() -> Self {
        Reports { refuse: true, ..Reports::default() }
    }

    /// Put a report from `program` about `kind` at `at_ms` in the folder,
    /// not yet offered.
    pub fn add(&self, program: udm_domain::report::Program, kind: udm_domain::report::Kind, at_ms: u64) {
        let report = udm_domain::report::Report {
            program,
            kind,
            copy: self.this_copy(),
            at_ms,
            thread: None,
            what: format!("{} {}", program.name(), kind.name()),
            location: None,
            backtrace: None,
            log_name: String::new(),
            log_tail: Vec::new(),
        };
        self.files.lock().unwrap().push((report, false));
    }

    /// The reports in the folder.
    pub fn written(&self) -> Vec<udm_domain::report::Report> {
        self.files.lock().unwrap().iter().map(|(r, _)| r.clone()).collect()
    }

    fn saved(r: &udm_domain::report::Report) -> udm_domain::report::SavedReport {
        let name = r.file_name();
        let summary = udm_domain::report::Summary::read(&name, &r.to_text()).unwrap();
        udm_domain::report::SavedReport { path: PathBuf::from("/reports").join(name), summary }
    }
}

impl ProblemReports for Reports {
    fn this_copy(&self) -> udm_domain::report::ThisCopy {
        udm_domain::report::ThisCopy { version: "9.9.9".into(), ..Default::default() }
    }

    fn log_tail(&self, program: udm_domain::report::Program) -> (String, Vec<String>) {
        (format!("{}.log", program.name()), vec![format!("last {} line", program.name())])
    }

    fn save(&self, report: &udm_domain::report::Report) -> Result<udm_domain::report::SavedReport, FilesError> {
        if self.refuse {
            return Err(FilesError("the disk is full".into()));
        }
        self.files.lock().unwrap().push((report.clone(), false));
        Ok(Self::saved(report))
    }

    fn delete(&self, report: &udm_domain::report::SavedReport) -> Result<(), FilesError> {
        self.files.lock().unwrap().retain(|(r, _)| r.file_name() != report.file_name());
        Ok(())
    }

    fn unoffered(&self) -> Vec<udm_domain::report::SavedReport> {
        let files = self.files.lock().unwrap();
        let mut found: Vec<_> = files.iter().filter(|(_, offered)| !offered).map(|(r, _)| Self::saved(r)).collect();
        found.sort_by_key(|r| r.summary.at_ms);
        found
    }

    fn mark_offered(&self, reports: &[udm_domain::report::SavedReport]) {
        for (r, offered) in self.files.lock().unwrap().iter_mut() {
            if reports.iter().any(|s| s.file_name() == r.file_name()) {
                *offered = true;
            }
        }
    }
}

/// A themes folder holding these files.
pub struct Shelf(pub Vec<crate::window::ThemeFile>);

impl crate::window::ThemeFiles for Shelf {
    fn open_folder(&self) -> Result<PathBuf, FilesError> {
        Ok(PathBuf::from("themes"))
    }

    fn read(&self) -> Vec<crate::window::ThemeFile> {
        self.0.clone()
    }
}

/// A system whose high-contrast setting gives these colors (or is off), and
/// that is asked nothing else.
pub struct Contrast(pub Option<udm_domain::theme::SystemContrast>);

impl crate::window::SystemServices for Contrast {
    fn fixed_proxy(&self) -> Option<crate::window::SystemProxy> {
        None
    }

    fn wake_timers_allowed(&self) -> Option<(bool, bool)> {
        None
    }

    fn can_elevate(&self) -> bool {
        false
    }

    fn can_set_creation_time(&self) -> bool {
        false
    }

    fn is_portable(&self) -> bool {
        false
    }

    fn file_type_name(&self, _ext: &str) -> Option<String> {
        None
    }

    fn play(&self, _sound: udm_domain::settings::Sound, _file: Option<&Path>) {}

    fn save_credential(&self, _target: &str, _username: &str, _password: &str) -> Result<(), FilesError> {
        Ok(())
    }

    fn contrast(&self) -> Option<udm_domain::theme::SystemContrast> {
        self.0
    }
}

/// A tool's latest release as a test sets it: its files (with GitHub's
/// digest, if any) and the checksum file's text. Downloads land in `/dl`.
pub struct ToolSite {
    release: udm_domain::ports::ToolRelease,
    sums: String,
    asked: Mutex<Vec<String>>,
    downloaded: Mutex<Vec<String>>,
}

impl ToolSite {
    pub fn release(version: &str, files: &[(&str, Option<&str>)], sums: &str) -> Self {
        let files = files
            .iter()
            .map(|(name, sha)| udm_domain::ports::ReleaseFile {
                name: name.to_string(),
                url: format!("https://github.test/{name}"),
                sha256: sha.map(String::from),
            })
            .collect();
        ToolSite {
            release: udm_domain::ports::ToolRelease { version: version.into(), files },
            sums: sums.into(),
            asked: Mutex::default(),
            downloaded: Mutex::default(),
        }
    }

    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }

    pub fn downloaded(&self) -> Vec<String> {
        self.downloaded.lock().unwrap().clone()
    }
}

impl udm_domain::ports::ToolReleases for ToolSite {
    fn latest(
        &self,
        forge: udm_domain::tools::Forge,
        repository: &str,
    ) -> Result<udm_domain::ports::ToolRelease, udm_domain::tools::ToolError> {
        self.asked.lock().unwrap().push(format!("{forge:?} {repository}"));
        Ok(self.release.clone())
    }

    fn text(&self, _: &udm_domain::ports::ReleaseFile) -> Result<String, udm_domain::tools::ToolError> {
        Ok(self.sums.clone())
    }

    fn download(
        &self,
        file: &udm_domain::ports::ReleaseFile,
        _: ByteCount,
    ) -> Result<PathBuf, udm_domain::tools::ToolError> {
        self.downloaded.lock().unwrap().push(file.name.clone());
        Ok(PathBuf::from("/dl").join(&file.name))
    }
}

/// UDM's tools folder in memory: what it installed, at `/tools`.
#[derive(Default)]
pub struct ToolShelf {
    tools: Mutex<HashMap<udm_domain::tools::Tool, udm_domain::tools::InstalledTool>>,
    installs: Mutex<Vec<(udm_domain::tools::Tool, String, u64)>>,
    discarded: Mutex<Vec<PathBuf>>,
}

impl ToolShelf {
    pub fn with(tool: udm_domain::tools::Tool, installed: udm_domain::tools::InstalledTool) -> Self {
        let shelf = ToolShelf::default();
        shelf.tools.lock().unwrap().insert(tool, installed);
        shelf
    }

    pub fn installs(&self) -> Vec<(udm_domain::tools::Tool, String, u64)> {
        self.installs.lock().unwrap().clone()
    }

    pub fn discarded(&self) -> Vec<PathBuf> {
        self.discarded.lock().unwrap().clone()
    }
}

impl udm_domain::ports::ToolFolder for ToolShelf {
    fn installed(&self, tool: udm_domain::tools::Tool) -> Option<udm_domain::tools::InstalledTool> {
        self.tools.lock().unwrap().get(&tool).cloned()
    }

    fn install(
        &self,
        tool: udm_domain::tools::Tool,
        _file: &Path,
        _packing: udm_domain::tools::Packing,
        version: &str,
        now: u64,
    ) -> Result<PathBuf, udm_domain::tools::ToolError> {
        self.installs.lock().unwrap().push((tool, version.to_string(), now));
        let installed = udm_domain::tools::InstalledTool { version: version.into(), checked: now };
        self.tools.lock().unwrap().insert(tool, installed);
        Ok(PathBuf::from("/tools").join(tool.name()))
    }

    fn discard(&self, file: &Path) {
        self.discarded.lock().unwrap().push(file.to_path_buf());
    }

    fn note_checked(&self, tool: udm_domain::tools::Tool, now: u64) {
        if let Some(t) = self.tools.lock().unwrap().get_mut(&tool) {
            t.checked = now;
        }
    }

    fn found(&self, tool: udm_domain::tools::Tool) -> Option<PathBuf> {
        self.tools.lock().unwrap().contains_key(&tool).then(|| PathBuf::from("/tools").join(tool.name()))
    }
}

/// Gallery pages gallery-dl would read, by address: what each lists, or
/// why not. Other pages are `Unsupported`; the other probes are not asked.
#[derive(Default)]
pub struct GalleryPages {
    pages: HashMap<String, Result<udm_domain::gallery::Gallery, udm_domain::gallery::GalleryError>>,
    asked: Mutex<Vec<(String, Option<String>)>>,
}

impl GalleryPages {
    pub fn with(page: &str, found: Result<udm_domain::gallery::Gallery, udm_domain::gallery::GalleryError>) -> Self {
        GalleryPages { pages: HashMap::from([(page.to_string(), found)]), asked: Mutex::default() }
    }

    /// The pages read, with the user agent each was read with.
    pub fn asked(&self) -> Vec<(String, Option<String>)> {
        self.asked.lock().unwrap().clone()
    }
}

impl udm_domain::ports::Probes for GalleryPages {
    fn link(&self, _: &Download) -> Result<udm_domain::ports::LinkFacts, udm_domain::ports::ProbeError> {
        Err(udm_domain::ports::ProbeError("not asked".into()))
    }

    fn file(&self, _: &Download) -> Result<udm_domain::ports::FileFacts, udm_domain::ports::ProbeError> {
        Err(udm_domain::ports::ProbeError("not asked".into()))
    }

    fn media(&self, _: &Download) -> udm_domain::media::MediaInfo {
        udm_domain::media::MediaInfo::default()
    }

    fn zip(
        &self,
        _: &Download,
        _: Option<ByteCount>,
    ) -> Result<Vec<udm_domain::download::ZipEntry>, udm_domain::ports::ProbeError> {
        Err(udm_domain::ports::ProbeError("not asked".into()))
    }

    fn gallery(
        &self,
        page: &udm_domain::address::Address,
        user_agent: Option<&str>,
    ) -> Result<udm_domain::gallery::Gallery, udm_domain::gallery::GalleryError> {
        self.asked.lock().unwrap().push((page.as_str().to_string(), user_agent.map(String::from)));
        self.pages.get(page.as_str()).cloned().unwrap_or(Err(udm_domain::gallery::GalleryError::Unsupported))
    }
}

/// Seconds since 1970 that a test moves on by hand.
pub struct Seconds(std::sync::atomic::AtomicU64);

impl Seconds {
    pub fn at(now: u64) -> Self {
        Seconds(std::sync::atomic::AtomicU64::new(now))
    }

    pub fn set(&self, now: u64) {
        self.0.store(now, Ordering::SeqCst);
    }
}

impl udm_domain::ports::Timestamps for Seconds {
    fn unix_now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A debrid service in memory: the hosts it supports, what it makes of each
/// link, its account, and what it was asked, with the key it was asked with.
pub struct DebridDesk {
    pub hosts: Mutex<Result<Vec<String>, udm_domain::debrid::DebridError>>,
    pub links: HashMap<String, Result<String, udm_domain::debrid::DebridError>>,
    pub account: Option<udm_domain::debrid::DebridAccount>,
    /// The calls made: `hosts`, `unrestrict <link>` or `account`, each with
    /// the service and the key.
    pub calls: Mutex<Vec<String>>,
}

impl Default for DebridDesk {
    fn default() -> Self {
        DebridDesk {
            hosts: Mutex::new(Ok(Vec::new())),
            links: HashMap::new(),
            account: None,
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl DebridDesk {
    /// A service supporting `hosts`, unlocking each `(link, direct)`.
    pub fn new(hosts: &[&str], links: &[(&str, &str)]) -> Self {
        DebridDesk {
            hosts: Mutex::new(Ok(hosts.iter().map(|h| h.to_string()).collect())),
            links: links.iter().map(|(l, d)| (l.to_string(), Ok(d.to_string()))).collect(),
            ..DebridDesk::default()
        }
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn note(&self, what: String, service: udm_domain::debrid::DebridService, key: &udm_domain::debrid::DebridKey) {
        self.calls.lock().unwrap().push(format!("{what} {service} {}", key.expose()));
    }
}

impl udm_domain::ports::Debrid for DebridDesk {
    fn hosts(
        &self,
        service: udm_domain::debrid::DebridService,
        key: &udm_domain::debrid::DebridKey,
    ) -> Result<udm_domain::debrid::SupportedHosts, udm_domain::debrid::DebridError> {
        self.note("hosts".into(), service, key);
        self.hosts.lock().unwrap().clone().map(udm_domain::debrid::SupportedHosts::new)
    }

    fn unrestrict(
        &self,
        service: udm_domain::debrid::DebridService,
        key: &udm_domain::debrid::DebridKey,
        link: &udm_domain::address::Address,
    ) -> Result<udm_domain::debrid::Unrestricted, udm_domain::debrid::DebridError> {
        self.note(format!("unrestrict {}", link.as_str()), service, key);
        let direct =
            self.links.get(link.as_str()).cloned().unwrap_or(Err(udm_domain::debrid::DebridError::HostNotSupported))?;
        Ok(udm_domain::debrid::Unrestricted {
            address: udm_domain::address::Address::parse(&direct).unwrap(),
            filename: Some("file.zip".into()),
            size: Some(ByteCount::new(1000)),
        })
    }

    fn account(
        &self,
        service: udm_domain::debrid::DebridService,
        key: &udm_domain::debrid::DebridKey,
    ) -> Result<udm_domain::debrid::DebridAccount, udm_domain::debrid::DebridError> {
        self.note("account".into(), service, key);
        self.account.clone().ok_or(udm_domain::debrid::DebridError::BadKey)
    }
}

/// The debrid key kept in memory.
#[derive(Default)]
pub struct Keyring(pub Mutex<Option<udm_domain::debrid::DebridKey>>);

impl Keyring {
    pub fn holding(key: &str) -> Self {
        Keyring(Mutex::new(udm_domain::debrid::DebridKey::new(key)))
    }
}

impl udm_domain::ports::DebridKeys for Keyring {
    fn key(&self) -> Option<udm_domain::debrid::DebridKey> {
        self.0.lock().unwrap().clone()
    }

    fn set_key(&self, key: Option<&udm_domain::debrid::DebridKey>) -> Result<(), SettingsError> {
        *self.0.lock().unwrap() = key.cloned();
        Ok(())
    }
}
