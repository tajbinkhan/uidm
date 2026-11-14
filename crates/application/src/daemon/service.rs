//! The daemon as a service: what it does with each client's command, its
//! scheduler pass, and its notification thread's work, through the ports
//! of `domain` and this crate. `udm-daemon` wires adapters in through
//! [`Wiring`], runs the server and the background threads, and hands each
//! command to [`Served`].
//!
//! Work that waits on a server or a program (a crawl, a probe, a scan, an
//! update check) runs on a thread of its own, and answers on the client's
//! [`Reply`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};
use udm_domain::address::Link;
use udm_domain::debrid::{DebridKey, DebridService};
use udm_domain::id::{DownloadId, PackageId, QueueId};
use udm_domain::package::{Packaging, Password, Unpack};
use udm_domain::seat::Seat;
use udm_domain::sync::LockExt;

use tracing::{info, warn};
use udm_domain::download::{
    Answer, CommandLine, Download, DownloadEntry, DuplicateStep, OnDone, Properties, Scan, Status, ZipEntry,
};
use udm_domain::grab::{self, GrabRequest, RenderedPage};
use udm_domain::ports::{
    Answers, Attention, Browser, Clock, Converter, Debrid, DebridKeys, DownloadControl, DownloadFiles, DownloadIntake,
    DownloadList, EarlyStarts, FileHashes, FileOpener, GrabDownloads, GrabProjects, GrabRuns, LimitLift, Notifications,
    PackageStore, PausedDownloads, PowerControl, Probes, ProxyFallback, QueueFinish, QueueRuns, QueueSounds,
    QueueStore, QuotaState, ReleaseSource, RemoteListener, SaveFolders, SettingsError, SettingsStore, SiteCrawler,
    SyncChecks, Timestamps, ToolFolder, ToolReleases, TransferEngine, Unpacker, UpdateChecks, UserIdle, VirusScanner,
};
use udm_domain::queue::{
    self, PowerRequest, PowerStep, Queue, ScheduleStep, StopReason, WhenDone, COUNTDOWN_SECS, MAIN_QUEUE,
};
use udm_domain::quota::QuotaLimit;
use udm_domain::settings::{Duplicate, Settings};

use super::{ClientId, ClientList, Command, Countdowns, DownloadFeed, Event, Failure, Reply, Requests};
use crate::debrid::{CheckAccount, ResolveLink, SetKey};
use crate::downloads::{
    AddLater, AddMany, AfterDownload, CancelDownload, CheckDownload, CheckOutcome, ConfirmAnswer, ConfirmWarning,
    Confirmed, ConvertDownload, Existing, Held, HoldForAnswer, KeepPackages, LiftLimitWhileAway, PauseAll, Probed,
    Redownload, RefreshLink, Refreshed, RemoveDownload, ResumeAll, RetryDownload, SetLimitRemember, SetOnDone,
    SetProperties, SetSpeedLimit, UngroupPackage, UnpackPackage, UnpackPackageError, UseExisting,
};
use crate::gallery::ReadGallery;
use crate::grab::RunGrabSchedules;
use crate::pairing::PairingDesk;
use crate::power::{CancelCountdown, CountdownNow, RunWhenDone};
use crate::queues::{
    DeleteQueue, MoveToQueue, QueueEvent, RemoveFromQueue, RunQueues, RunSchedules, SaveQueue, Scheduled, StartQueue,
    StopQueue,
};
use crate::quota::{CheckQuota, QuotaEvent, QuotaStatus};
use crate::tools::{InstallTool, ListTools, UpdateYtDlp};
use crate::update::{CheckForUpdate, ClaimWeeklyCheck, InstallUpdate};
use udm_domain::media::Playlist;
use udm_domain::pairing::{PairingError, PairingOffer, RemoteState};
use udm_domain::tools::{Platform, Tool, ToolError, ToolState};

/// Why the daemon refused a client's command. Clients hear it as a
/// [`Failure`], kind and values, so the window can translate it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServiceError {
    #[error(transparent)]
    Queue(#[from] udm_domain::queue::QueueError),
    #[error(transparent)]
    Download(#[from] udm_domain::download::DownloadError),
    #[error(transparent)]
    Confirm(#[from] crate::downloads::ConfirmError),
    #[error(transparent)]
    Update(#[from] udm_domain::update::UpdateError),
    #[error(transparent)]
    Grab(#[from] grab::InvalidGrab),
    #[error(transparent)]
    Tool(#[from] udm_domain::tools::ToolError),
    #[error(transparent)]
    Gallery(#[from] udm_domain::gallery::GalleryError),
    #[error("A site crawl is already running.")]
    CrawlRunning,
    /// The address of a download whose form body cannot be sent.
    #[error("the form body of {0} is not valid base64")]
    BadBody(String),
    /// An address that is not http, https or ftp, such as `javascript:`.
    #[error("{0} is not an http, https or ftp address")]
    NotDownloadAddress(String),
    /// Pairing a phone or removing one failed.
    #[error(transparent)]
    Pairing(#[from] udm_domain::pairing::PairingError),
    #[error("There is no package {0}.")]
    NoPackage(PackageId),
    #[error("The package is being unpacked already.")]
    PackageBusy,
}

impl From<UnpackPackageError> for ServiceError {
    fn from(e: UnpackPackageError) -> Self {
        match e {
            UnpackPackageError::NoPackage(id) => ServiceError::NoPackage(id),
            UnpackPackageError::Busy => ServiceError::PackageBusy,
        }
    }
}

/// Links the link picker's probes ask about at once.
const LINK_PROBES: usize = 4;

/// A failure is announced after this long, so a download the browser
/// extension takes back (and removes) right away is not announced at all.
const FAILED_GRACE: Duration = Duration::from_secs(1);
/// The `client` a desktop window names in its `hello`.
const WINDOW_CLIENT: &str = "window";

/// How long a command line waits for the window its `udm-gui` opens. One
/// older than this belonged to a window that never connected.
pub const COMMAND_LINE_WAIT: Duration = Duration::from_secs(60);

/// Work for the notification thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Note {
    Finished(DownloadId),
    Failed(DownloadId),
    /// A finished download matched the checksum it came with.
    Verified(DownloadId),
    /// A finished download's virus scan is done.
    Scanned(DownloadId),
    /// A download stopped to ask about its taken file name.
    NameTaken(DownloadId),
}

/// Lock `m`. Every mutex here holds a map or set changed by single
/// inserts and removals, or a plain value, so one a panicking thread left
/// behind still holds a usable value.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock_or_recover()
}

/// A clock stopped at one moment, for a scheduler pass that looks at
/// "now" more than once.
struct StoppedAt(queue::Moment);

impl Clock for StoppedAt {
    fn now(&self) -> queue::Moment {
        self.0
    }
}

/// A scheduled grabber project's run: the project and the day it is for.
pub type RunProject = Arc<dyn Fn(grab::Project, i64) + Send + Sync>;

/// The adapters the service runs on, by the ports each must implement.
/// One type may stand for several, as the download store does.
pub trait Wiring: 'static {
    /// The download list, the kept requests and the queues.
    type Store: QueueStore + DownloadList + DownloadFeed + PackageStore + 'static;
    /// New downloads going into the list.
    type Intake: DownloadIntake + 'static;
    /// Starts, stops and checks downloads, keeps the settings, and lifts the
    /// speed limit while nobody uses the computer.
    type Transfers: DownloadControl + SettingsStore + TransferEngine + LimitLift + 'static;
    /// The settings' record of update checks.
    type Settings: UpdateChecks + 'static;
    /// Each started queue's current run, and its retries.
    type Runs: QueueRuns + 'static;
    /// What `PauseAll` stopped.
    type Paused: PausedDownloads + 'static;
    type Clock: Clock + Timestamps + 'static;
    type Chimes: QueueSounds + 'static;
    type Sync: SyncChecks + 'static;
    type RunEnds: QueueFinish + 'static;
    type Quota: QuotaState + 'static;
    /// The time since the last keyboard or mouse input.
    type Idle: UserIdle + 'static;
    type Power: PowerControl + Countdowns + 'static;
    /// The downloads' files and the folders they are saved in.
    type Files: DownloadFiles + SaveFolders + 'static;
    type Front: Attention + 'static;
    type Early: EarlyStarts + 'static;
    type Answers: Answers + 'static;
    type Converter: Converter + 'static;
    type Releases: ReleaseSource + 'static;
    type Projects: GrabProjects + 'static;
    type GrabRuns: GrabRuns + 'static;
    type Notifier: Notifications + 'static;
    type Retries: ProxyFallback + 'static;
    type Found: GrabDownloads + 'static;
}

/// Makes the update checker for a releases address.
pub type ReleasesAt<R> = Box<dyn Fn(&str) -> R + Send + Sync>;

/// Makes the grabber's scheduled runs from the set of projects whose run is
/// going on and what one run does.
pub type GrabRunsFor<G> = Box<dyn Fn(Arc<Mutex<HashSet<String>>>, RunProject) -> G + Send + Sync>;

/// The adapters, as the wiring builds them.
pub struct Parts<W: Wiring> {
    pub store: Arc<W::Store>,
    pub intake: W::Intake,
    pub clients: Arc<dyn ClientList>,
    pub clock: W::Clock,
    /// The grabber's saved projects, when their folder can be found.
    pub projects: Option<Arc<W::Projects>>,
    pub files: Arc<W::Files>,
    pub scanner: Arc<dyn VirusScanner>,
    /// Hashes finished files that came with a checksum.
    pub hashes: Arc<dyn FileHashes>,
    /// Unpacks packages' archives.
    pub unpacker: Arc<dyn Unpacker>,
    pub opener: Arc<dyn FileOpener>,
    pub settings: Arc<W::Settings>,
    pub transfers: Arc<W::Transfers>,
    pub notifier: Arc<W::Notifier>,
    pub front: Arc<W::Front>,
    pub answers: W::Answers,
    pub early: Arc<W::Early>,
    pub conversions: W::Converter,
    pub sync: W::Sync,
    pub run_ends: W::RunEnds,
    pub browser: Arc<dyn Browser>,
    pub probes: Arc<dyn Probes>,
    pub crawler: Arc<dyn SiteCrawler>,
    /// Where yt-dlp's and ffmpeg's releases come from.
    pub tool_releases: Arc<dyn ToolReleases>,
    /// UDM's folder of installed tools.
    pub tool_folder: Arc<dyn ToolFolder>,
    /// The system the tools are installed for.
    pub platform: Platform,
    pub found: W::Found,
    pub browser_proxy_retries: Arc<W::Retries>,
    pub run_book: W::Runs,
    pub pause_book: W::Paused,
    pub quota: W::Quota,
    pub idle: W::Idle,
    pub power: Arc<W::Power>,
    pub chimes: W::Chimes,
    pub releases: ReleasesAt<W::Releases>,
    pub release_url: String,
    pub grab_runs_for: GrabRunsFor<W::GrabRuns>,
    /// Finished and failed downloads, for the notification thread.
    pub notes: Sender<Note>,
    /// Wakes the scheduler when a download ends or a queue changes.
    pub wake: Sender<()>,
    /// This copy's version.
    pub version: &'static str,
    /// The paired phones and the pairing code shown.
    pub pairing: Arc<PairingDesk>,
    /// The remote-control page's listener.
    pub listener: Arc<dyn RemoteListener>,
    /// Where the daemon runs, which decides whether it exits when idle.
    pub seat: Seat,
    /// The debrid account's service API, its stored key, and the links
    /// going through it (experimental).
    pub debrid: Arc<dyn Debrid>,
    pub debrid_keys: Arc<dyn DebridKeys>,
    pub links: Arc<ResolveLink>,
}

/// The daemon's work. Its fields are the adapters it was wired with, and
/// what it keeps between requests.
pub struct DaemonService<W: Wiring> {
    pub store: Arc<W::Store>,
    pub intake: W::Intake,
    pub clients: Arc<dyn ClientList>,
    pub clock: W::Clock,
    pub projects: Option<Arc<W::Projects>>,
    pub files: Arc<W::Files>,
    pub scanner: Arc<dyn VirusScanner>,
    pub hashes: Arc<dyn FileHashes>,
    pub unpacker: Arc<dyn Unpacker>,
    pub opener: Arc<dyn FileOpener>,
    pub settings: Arc<W::Settings>,
    pub transfers: Arc<W::Transfers>,
    pub notifier: Arc<W::Notifier>,
    pub front: Arc<W::Front>,
    pub answers: W::Answers,
    pub early: Arc<W::Early>,
    pub conversions: W::Converter,
    pub sync: W::Sync,
    pub run_ends: W::RunEnds,
    pub browser: Arc<dyn Browser>,
    pub probes: Arc<dyn Probes>,
    pub crawler: Arc<dyn SiteCrawler>,
    /// Where yt-dlp's and ffmpeg's releases come from.
    pub tool_releases: Arc<dyn ToolReleases>,
    /// UDM's folder of installed tools.
    pub tool_folder: Arc<dyn ToolFolder>,
    /// The system the tools are installed for.
    pub platform: Platform,
    pub found: W::Found,
    pub browser_proxy_retries: Arc<W::Retries>,
    pub run_book: W::Runs,
    pub pause_book: W::Paused,
    pub quota: W::Quota,
    pub idle: W::Idle,
    pub power: Arc<W::Power>,
    pub chimes: W::Chimes,
    releases: ReleasesAt<W::Releases>,
    pub release_url: String,
    grab_runs_for: GrabRunsFor<W::GrabRuns>,
    pub notes: Sender<Note>,
    pub wake: Sender<()>,
    version: &'static str,
    pub pairing: Arc<PairingDesk>,
    pub listener: Arc<dyn RemoteListener>,
    /// Where the daemon runs.
    seat: Seat,
    pub debrid: Arc<dyn Debrid>,
    pub debrid_keys: Arc<dyn DebridKeys>,
    pub links: Arc<ResolveLink>,
    /// Keeps two scheduler passes from running at once.
    tick_lock: Mutex<()>,
    /// Each client's running site crawl, by its stop flag.
    pub grabs: Mutex<HashMap<ClientId, Arc<AtomicBool>>>,
    /// Each client's link picker probes, by their stop flag.
    pub link_probes: Mutex<HashMap<ClientId, Arc<AtomicBool>>>,
    /// Links from a page, held for the window that is being opened.
    pub picked_links: Mutex<Option<Event>>,
    /// A command line held for the window its `udm-gui` is opening, and
    /// when it came.
    pub command_line: Mutex<Option<(CommandLine, Instant)>>,
    /// Grabber projects whose scheduled run is going on, by name.
    pub grab_runs: Arc<Mutex<HashSet<String>>>,
    /// When the projects' schedules were last looked at.
    grab_checked: Mutex<Option<Instant>>,
    /// The tools being installed now.
    tool_installs: Mutex<HashSet<Tool>>,
    /// When "Keep yt-dlp up to date" was last looked at.
    tools_checked: Mutex<Option<Instant>>,
    /// The galleries being read, and the pages lately read that held none.
    gallery_reads: Arc<Mutex<udm_domain::gallery::GalleryReads>>,
}

impl<W: Wiring> DaemonService<W> {
    pub fn new(p: Parts<W>) -> Self {
        DaemonService {
            store: p.store,
            intake: p.intake,
            clients: p.clients,
            clock: p.clock,
            projects: p.projects,
            files: p.files,
            scanner: p.scanner,
            hashes: p.hashes,
            unpacker: p.unpacker,
            opener: p.opener,
            settings: p.settings,
            transfers: p.transfers,
            notifier: p.notifier,
            front: p.front,
            answers: p.answers,
            early: p.early,
            conversions: p.conversions,
            sync: p.sync,
            run_ends: p.run_ends,
            browser: p.browser,
            probes: p.probes,
            crawler: p.crawler,
            tool_releases: p.tool_releases,
            tool_folder: p.tool_folder,
            platform: p.platform,
            found: p.found,
            browser_proxy_retries: p.browser_proxy_retries,
            run_book: p.run_book,
            pause_book: p.pause_book,
            quota: p.quota,
            idle: p.idle,
            power: p.power,
            chimes: p.chimes,
            releases: p.releases,
            release_url: p.release_url,
            grab_runs_for: p.grab_runs_for,
            notes: p.notes,
            wake: p.wake,
            version: p.version,
            pairing: p.pairing,
            listener: p.listener,
            seat: p.seat,
            debrid: p.debrid,
            debrid_keys: p.debrid_keys,
            links: p.links,
            tick_lock: Mutex::new(()),
            grabs: Mutex::default(),
            link_probes: Mutex::default(),
            picked_links: Mutex::new(None),
            command_line: Mutex::new(None),
            grab_runs: Arc::default(),
            grab_checked: Mutex::new(None),
            tool_installs: Mutex::default(),
            tools_checked: Mutex::new(None),
            gallery_reads: Arc::default(),
        }
    }

    /// The queues, the main queue first.
    pub fn queues(&self) -> Vec<Queue> {
        self.store.edit(|queues| queues.clone())
    }

    /// Change queue `id` with `f`, then save the queues and tell the
    /// clients.
    pub fn update_queue<T>(
        &self,
        id: QueueId,
        f: impl FnOnce(&mut Queue) -> T,
    ) -> Result<T, udm_domain::queue::QueueError> {
        let result = self.store.edit(|queues| {
            queues.iter_mut().find(|q| q.id == id).map(f).ok_or(udm_domain::queue::QueueError::NoQueue(id))
        })?;
        self.store.queues_changed();
        Ok(result)
    }

    /// Whether UDM can send `download`'s request: its address must be
    /// http, https or ftp, as the command line's must, and a form body must
    /// be readable.
    fn check_download(&self, download: &Download) -> Result<(), ServiceError> {
        if !download.url.is_address() {
            Err(ServiceError::NotDownloadAddress(download.url.to_string()))
        } else if self.intake.sendable(download) {
            Ok(())
        } else {
            Err(ServiceError::BadBody(download.url.to_string()))
        }
    }

    /// The settings as they are now.
    fn settings_now(&self) -> Settings {
        self.transfers.settings()
    }

    /// What happens once at start: the queues set to start with UDM
    /// start, and with "resume at startup", every interrupted download
    /// goes again.
    pub fn start_up(&self) {
        let launch: Vec<QueueId> = self.queues().iter().filter(|q| q.start_on_launch).map(|q| q.id).collect();
        for id in launch {
            if let Err(e) = self.start_queue(id, None) {
                warn!(queue = %id, error = %e, "could not start the queue");
            }
        }
        if self.settings_now().resume_at_startup {
            self.resume_interrupted();
        }
    }

    /// One scheduler pass: queues and their schedules, then the grabber's
    /// schedules. Returns when the computer should wake for the next
    /// scheduled start.
    pub fn scheduler_pass(self: &Arc<Self>) -> Option<queue::Moment> {
        self.tick();
        self.check_grab_schedules();
        self.keep_yt_dlp_current();
        self.next_wake(self.clock.now())
    }

    /// Whether the computer should stay awake: something is working and
    /// the "keep awake" option is on.
    pub fn wants_awake(&self) -> bool {
        self.settings_now().keep_awake && self.is_working()
    }

    /// Whether the daemon has nothing to do: no clients, no downloads, no
    /// queue that needs it, and remote control off (a phone may ask for
    /// something at any time while it is on).
    pub fn idle(&self) -> bool {
        self.clients.is_empty()
            && self.transfers.active_count() == 0
            && !self.queues_need_daemon()
            && !self.settings_now().remote_enabled
    }

    /// How remote control stands, with the pairing code shown, if any.
    pub fn remote_state(&self) -> RemoteState {
        let listener = self.listener.state();
        let pairing = self.pairing.pending().and_then(|p| {
            let link = self.listener.link(&p.code)?;
            Some(PairingOffer { link, code: p.code, expires_at: p.expires_at })
        });
        RemoteState { listener, devices: self.pairing.devices(), pairing }
    }

    /// Show a new pairing code, while the page is served.
    pub fn start_pairing(&self) -> Result<RemoteState, ServiceError> {
        if self.listener.state().page.is_none() {
            return Err(PairingError::NotListening.into());
        }
        self.pairing.start()?;
        Ok(self.remote_state())
    }

    /// Whether the daemon may exit now for having nothing to do: it is idle
    /// and runs on a desktop. A server's daemon runs until it is stopped
    /// (see `Seat`).
    pub fn may_exit(&self) -> bool {
        self.seat.exits_when_idle() && self.idle()
    }

    /// Put download `id` in queue `queue` at `position` (or at the end). A
    /// download that is not running waits there.
    pub fn move_to_queue(&self, id: DownloadId, queue: QueueId, position: Option<usize>) -> Result<(), ServiceError> {
        MoveToQueue::new(&*self.store, &*self.store).run(id, queue, position).map_err(ServiceError::from)
    }

    /// Take download `id` out of its queue, keeping it in the list (see
    /// `RemoveFromQueue`).
    pub fn remove_from_queue(&self, id: DownloadId) -> Result<(), ServiceError> {
        RemoveFromQueue::new(&*self.store, &*self.store).run(id).map_err(ServiceError::from)
    }

    /// Create a queue (id 0) or change one.
    pub fn save_queue(&self, edited: Queue) -> Result<QueueId, ServiceError> {
        SaveQueue::new(&*self.store, &*self.store).run(edited).map_err(ServiceError::from)
    }

    /// Delete a queue. Its downloads move to the main queue; the running ones
    /// keep running.
    pub fn delete_queue(&self, id: QueueId) -> Result<(), ServiceError> {
        DeleteQueue::new(&*self.store, &*self.store, &self.run_book).run(id).map_err(ServiceError::from)
    }

    /// Start a queue: its waiting downloads run in order. `scheduled_day` is
    /// the day whose scheduled run this is, or `None` when started by hand.
    pub fn start_queue(&self, id: QueueId, scheduled_day: Option<i64>) -> Result<(), ServiceError> {
        StartQueue::new(&*self.store, &self.run_book, &self.clock, &self.chimes)
            .run(id, scheduled_day)
            .map_err(ServiceError::from)
    }

    /// Stop a queue. Its running downloads stop and wait in it again, with
    /// their progress kept.
    pub fn stop_queue(&self, id: QueueId) -> Result<(), ServiceError> {
        StopQueue::new(&*self.store, &*self.store, &self.run_book, &*self.transfers).run(id).map_err(ServiceError::from)
    }

    /// One pass of the scheduler: start and stop queues on their schedules,
    /// start waiting downloads in started queues, retry failed ones, and end
    /// the runs that are done.
    pub fn tick(self: &Arc<Self>) {
        let _guard = lock(&self.tick_lock);
        self.check_quota();
        self.lift_limit_while_away();
        let scheduled =
            RunSchedules::new(&*self.store, &*self.store, &self.run_book, &*self.transfers, &self.clock, &self.chimes)
                .run();
        for Scheduled { queue, step } in scheduled {
            let why = match step {
                ScheduleStep::Start { .. } => "schedule starts",
                ScheduleStep::Stop(StopReason::ScheduleEnded) => "schedule stops",
                ScheduleStep::Stop(StopReason::GrabberStopTime) => "the grabber project's stop time stops",
            };
            info!(queue = %queue, why, "schedule step");
        }

        let events =
            RunQueues::new(&*self.store, &*self.store, &self.run_book, &*self.transfers, &self.sync, &self.run_ends)
                .run();
        for event in events {
            match event {
                QueueEvent::Retried { queue, id, attempt, of } => {
                    info!(queue = %queue, download_id = id.get(), attempt, of, "queue retries a download")
                }
                QueueEvent::Finished { queue } => info!(queue = %queue, "queue is done"),
            }
        }
    }

    /// Check the finished downloads at `urls` for changes, and put those
    /// whose server copy changed in `queue` to be downloaded again, starting
    /// it. Returns how many changed, and the addresses it found finished in
    /// the list (which need no new row).
    pub fn resync(&self, urls: &[String], queue: QueueId) -> (usize, HashSet<String>) {
        let urls: HashSet<&str> = urls.iter().map(String::as_str).collect();
        let (ids, listed): (Vec<DownloadId>, HashSet<String>) = self
            .store
            .ids_where(|_, status| status == Status::Done)
            .into_iter()
            .filter_map(|id| self.store.entry(id))
            .filter(|e| urls.contains(e.url.as_str()))
            .map(|e| (e.id, e.url.into_string()))
            .unzip();
        let changed = ids
            .into_iter()
            .filter(|id| self.transfers.has_changed(*id) && self.move_to_queue(*id, queue, None).is_ok())
            .count();
        if changed > 0 && !self.queues().iter().any(|q| q.id == queue && q.running) {
            if let Err(e) = self.start_queue(queue, None) {
                warn!(queue = %queue, error = %e, "could not start the queue");
            }
        }
        (changed, listed)
    }

    /// Do what a finished queue, or with `file` one finished download, asks
    /// (see `RunWhenDone`).
    pub fn when_done(&self, action: WhenDone, queue: &str, file: Option<&str>, force_close: bool) {
        when_done(&*self.power, action, queue, file, force_close);
    }

    /// Run what download `id` asked to happen once it finished, then clear
    /// it so downloading it again does not repeat it.
    pub fn after_download(self: &Arc<Self>, id: DownloadId) {
        AfterDownload::new(&*self.store, &*self.power).run(id);
        let settings = self.settings_now();
        let due =
            UnpackPackage::new(&*self.store, &*self.store, &*self.unpacker, &*self.files).due_after(id, &settings);
        if let Some(package) = due {
            info!(package = package.get(), "unpacking a complete package");
            if let Err(e) = self.unpack_package(package, None) {
                warn!(package = package.get(), error = %e, "could not unpack");
            }
        }
    }

    /// Unpack package `id`'s archives on a thread of its own, with
    /// `password` for encrypted ones. Its state reaches the clients through
    /// the packages.
    pub fn unpack_package(self: &Arc<Self>, id: PackageId, password: Option<Password>) -> Result<(), ServiceError> {
        UnpackPackage::new(&*self.store, &*self.store, &*self.unpacker, &*self.files).begin(id)?;
        let daemon = Arc::clone(self);
        thread::spawn(move || {
            let settings = daemon.settings_now();
            let outcome = UnpackPackage::new(&*daemon.store, &*daemon.store, &*daemon.unpacker, &*daemon.files).run(
                id,
                password.as_ref(),
                &settings,
            );
            for (path, e) in &outcome.not_recycled {
                warn!(package = id.get(), path = %path.display(), error = %e, "could not recycle an archive");
            }
            match outcome.state {
                Unpack::Done { folder, files } => info!(package = id.get(), folder = %folder, files, "unpacked"),
                Unpack::NeedsPassword { wrong } => info!(package = id.get(), wrong, "the archives need a password"),
                Unpack::Failed { reason } => warn!(package = id.get(), error = %reason, "could not unpack"),
                Unpack::Waiting | Unpack::Unpacking => {}
            }
        });
        Ok(())
    }

    /// Stop keeping package `id`; its downloads stay.
    pub fn ungroup_package(&self, id: PackageId) -> Result<(), ServiceError> {
        UngroupPackage::new(&*self.store).run(id).map_err(ServiceError::from)
    }

    /// Do what `udm-gui`'s command line asked. `/q` counts only when that
    /// call opened the window (`started`), as in IDM, where it works only
    /// for the first copy. Problems go to the windows, since the caller may
    /// be gone.
    pub fn run_command_line(self: &Arc<Self>, cmd: CommandLine, started: bool) {
        if let Some(mut download) = cmd.download {
            download.limit_browser_request();
            let (id, url) = self.add(download);
            info!(download_id = id.get(), url = %url, "add from the command line");
            if cmd.quit_after && started {
                let exit = OnDone { then: WhenDone::Exit, ..OnDone::default() };
                if let Err(e) = self.set_on_done(id, Some(exit)) {
                    warn!(download_id = id.get(), error = %e, "could not set /q");
                }
            }
        }
        if cmd.start_queue {
            match self.start_queue(MAIN_QUEUE, None) {
                Ok(()) => {
                    info!("start the main queue from the command line");
                    let _ = self.wake.send(());
                }
                Err(e) => self.clients.to_windows(Event::Error { error: e.into() }),
            }
        }
    }

    /// Set or clear what download `id` does once it finishes.
    pub fn set_on_done(&self, id: DownloadId, on_done: Option<OnDone>) -> Result<(), ServiceError> {
        SetOnDone::new(&*self.store).run(id, on_done).map_err(ServiceError::from)
    }

    /// End the countdown early and carry out its action now.
    pub fn countdown_now(&self) {
        CountdownNow::new(&*self.power).run();
    }

    pub fn cancel_countdown(&self) {
        if CancelCountdown::new(&*self.power).run() {
            info!("countdown canceled");
        }
    }

    /// The countdown still running, as the message a window joining now needs.
    pub fn countdown_message(&self) -> Option<Event> {
        self.power.countdown()
    }

    /// The next scheduled start, of a queue or a grabber project, whose
    /// schedule asks to wake the computer.
    pub fn next_wake(&self, now: queue::Moment) -> Option<queue::Moment> {
        let queues = self.queues();
        let from_queues = queues.iter().filter(|q| !q.running).filter_map(|q| {
            q.schedule.as_ref().filter(|s| s.wake).and_then(|s| s.next_run(now, q.last_scheduled_start))
        });
        let projects: Vec<queue::Moment> = self
            .saved_projects()
            .iter()
            .filter_map(|p| {
                p.schedule.as_ref().filter(|s| s.wake).and_then(|s| s.next_run(now, p.last_scheduled_start))
            })
            .collect();
        from_queues.chain(projects).filter(|m| *m > now).min()
    }

    /// Whether work is running that sleep would interrupt: a download, a
    /// site grab or a conversion.
    pub fn is_working(&self) -> bool {
        self.transfers.active_count() > 0
            || !lock(&self.grabs).is_empty()
            || !lock(&self.grab_runs).is_empty()
            || self.conversions.any_running()
    }

    /// Whether the daemon must stay up for a queue: one is started, one has a
    /// schedule ahead, or a countdown runs.
    pub fn queues_need_daemon(&self) -> bool {
        let now = self.clock.now();
        let projects = || {
            !lock(&self.grab_runs).is_empty()
                || self
                    .saved_projects()
                    .into_iter()
                    .filter_map(|p| p.schedule)
                    .any(|s| s.next_start(now).is_some() || s.open_run(now).is_some())
        };
        self.countdown_message().is_some()
            || projects()
            || self.queues().iter().any(|q| {
                q.running
                    || q.schedule.as_ref().is_some_and(|s| s.next_start(now).is_some() || s.open_run(now).is_some())
            })
    }

    /// Every saved grabber project that can be read.
    pub fn saved_projects(&self) -> Vec<grab::Project> {
        let Some(projects) = &self.projects else {
            return Vec::new();
        };
        projects.names().iter().filter_map(|name| projects.load(name)).collect()
    }

    pub fn entry(&self, id: DownloadId) -> Option<DownloadEntry> {
        self.store.entry(id)
    }

    /// Bring the window forward: ask an open one to show itself, or start one.
    pub fn show_window(&self) {
        self.front.show_window();
    }

    /// Stop every queued or running download, remembering them for
    /// `resume_all`.
    pub fn pause_all(&self) {
        PauseAll::new(&*self.store, &*self.store, &self.run_book, &*self.transfers, &self.pause_book).run();
    }

    /// The quota the settings ask for, if it is on.
    pub fn quota_limit(&self) -> Option<QuotaLimit> {
        let s = self.settings_now();
        s.quota().map(|(bytes, window)| QuotaLimit { bytes, window, warn: s.quota_warn })
    }

    /// The quota's state for a window that just connected, if downloads are
    /// paused for it or it is nearly reached.
    pub fn quota_message(&self) -> Option<Event> {
        let now = self.clock.unix_now() / 60;
        QuotaStatus::new(&self.quota).run(self.quota_limit(), now).map(Event::from)
    }

    /// Pause everything when the download quota is reached, warn shortly
    /// before, and resume what it stopped once the window allows (see
    /// `CheckQuota`). Runs with the scheduler.
    pub fn check_quota(&self) {
        let now = self.clock.unix_now() / 60;
        let check = CheckQuota::new(
            &*self.store,
            &*self.store,
            &self.run_book,
            &*self.transfers,
            &self.clock,
            &self.chimes,
            &self.quota,
        );
        match check.run(self.quota_limit(), now) {
            Some(QuotaEvent::Reached { used, limit }) => {
                info!(used = used.get(), limit = limit.get(), "quota reached")
            }
            Some(QuotaEvent::Resumed { failed }) => {
                info!("quota allows downloads again");
                for (id, e) in failed {
                    warn!(download_id = id.get(), error = %e, "could not resume");
                }
            }
            None => {}
        }
    }

    /// Lift the speed limit while nobody uses the computer, when the
    /// settings ask for that, and apply it again once someone does (see
    /// `LiftLimitWhileAway`). Runs with the scheduler.
    pub fn lift_limit_while_away(&self) {
        if let Some(lifted) = LiftLimitWhileAway::new(&*self.transfers, &self.idle, &*self.transfers).run() {
            info!(lifted, "speed limit while away");
        }
    }

    /// Whether the speed limit is lifted, for a window that just connected,
    /// when it is.
    pub fn limit_lift_message(&self) -> Option<Event> {
        self.transfers.lifted().then_some(Event::LimitLifted { lifted: true })
    }

    /// Start again what `pause_all` stopped, and every interrupted download.
    pub fn resume_all(&self) {
        let resumed = ResumeAll::new(
            &*self.store,
            &*self.store,
            &self.run_book,
            &*self.transfers,
            &self.pause_book,
            &self.clock,
            &self.chimes,
        )
        .run();
        for (id, e) in resumed.failed {
            warn!(download_id = id.get(), error = %e, "could not resume");
        }
    }

    /// Handle one note: check a finished download's checksum first when it
    /// came with one, then scan it when asked, then play the sound and notify.
    pub fn on_note(self: &Arc<Self>, note: Note) {
        let settings = self.settings_now();
        match note {
            Note::Finished(id) if CheckDownload::new(&*self.store, &*self.hashes).wanted(id) => {
                let daemon = Arc::clone(self);
                thread::spawn(move || match CheckDownload::new(&*daemon.store, &*daemon.hashes).run(id) {
                    CheckOutcome::Failed(error) => {
                        warn!(download_id = id.get(), error = %error, "checksum check failed");
                        daemon.notifier.notify(id, true, &settings);
                    }
                    CheckOutcome::Matches | CheckOutcome::Unchecked => {
                        info!(download_id = id.get(), "checksum matches");
                        let _ = daemon.notes.send(Note::Verified(id));
                    }
                });
            }
            Note::Finished(id) | Note::Verified(id) if settings.scan_after_download => {
                let scanning = self.store.update(id, |e| {
                    if e.status != Status::Done || e.path.is_none() {
                        return false;
                    }
                    e.scan = Some(Scan::Scanning);
                    true
                });
                let Some(entry) = scanning.filter(|ok| *ok).and_then(|_| self.store.entry(id)) else {
                    return;
                };
                let daemon = Arc::clone(self);
                thread::spawn(move || {
                    let path = PathBuf::from(entry.path.unwrap_or_default());
                    let result = daemon.scanner.scan(&settings, &path);
                    info!(download_id = id.get(), result = ?result, "scanned");
                    let _ = daemon.store.update(id, |e| {
                        e.scan = Some(result);
                        true
                    });
                    let _ = daemon.notes.send(Note::Scanned(id));
                });
            }
            Note::Finished(id) | Note::Verified(id) | Note::Scanned(id) => {
                self.notifier.notify(id, false, &settings);
                self.after_download(id);
            }
            Note::Failed(id) => {
                thread::sleep(FAILED_GRACE);
                if self.entry(id).is_some_and(|e| e.status == Status::Failed)
                    && !self.retry_through_browser_proxy(id, &settings)
                {
                    self.notifier.notify(id, true, &settings);
                }
            }
            // Without a window to ask, a taken name gets a new one.
            Note::NameTaken(id) => self.answers.await_new_name(id),
        }
    }

    /// Take a new download: ask where to save it when the settings say so
    /// (unless its type starts without asking), otherwise queue it at once,
    /// or only add it to the Download later queue when the settings say
    /// that. If its address is already in the list, the duplicate setting
    /// decides first, asking in the save-as dialog under `Ask` even when
    /// that dialog is off.
    pub fn add(self: &Arc<Self>, mut download: Download) -> (DownloadId, Link) {
        if let Some(old) = self.duplicate_of(&download) {
            let choice = download.duplicate.unwrap_or(self.settings_now().if_duplicate);
            info!(url = %download.url, download_id = old.id.get(), choice = ?choice, "already in the list");
            if choice == Duplicate::Ask {
                return self.ask(download, Some(old.id));
            }
            if let Some(kept) = self.settle_duplicate(&mut download, &old, choice) {
                return kept;
            }
        }
        let settings = self.settings_now();
        if download.start == Some(false) || (download.start.is_none() && settings.add_only) {
            return self.add_later(download);
        }
        let name = udm_domain::filename::proposed_name(download.url.as_str(), download.filename.as_deref());
        // A folder UDM may not write to asks first, so the dialog can offer
        // administrator rights or another folder.
        let folder = match download.folder.as_deref().map(Path::new).filter(|f| f.is_absolute()) {
            Some(f) => f.to_path_buf(),
            None => settings.folders_for(download.url.as_str(), &self.files.downloads_dir()).for_name(name.as_str()),
        };
        let denied = !download.elevate && !self.files.can_write(&folder);
        if denied
            || download.ask.unwrap_or(settings.ask_where_to_save && !settings.starts_without_asking(name.as_str()))
        {
            self.ask(download, None)
        } else {
            self.start(download)
        }
    }

    /// The newest download in the list with `download`'s address, unless it
    /// cannot be a duplicate (a form post or a stream).
    pub fn duplicate_of(&self, download: &Download) -> Option<DownloadEntry> {
        if !download.may_be_duplicate() {
            return None;
        }
        self.store.find_last(|e| udm_domain::download::same_address(&e.url, &download.url))
    }

    /// Apply `choice` to `download`, whose address download `old` already
    /// has. Returns the download to report when nothing new should be added;
    /// otherwise `download` is ready to add.
    pub fn settle_duplicate(
        self: &Arc<Self>,
        download: &mut Download,
        old: &DownloadEntry,
        choice: Duplicate,
    ) -> Option<(DownloadId, Link)> {
        match download.settle_duplicate(old, choice) {
            DuplicateStep::Add => None,
            DuplicateStep::UseExisting => self.use_existing(old),
            DuplicateStep::Replace => {
                if let Err(e) = self.remove(old.id, false) {
                    warn!(download_id = old.id.get(), error = %e, "could not remove");
                }
                None
            }
        }
    }

    /// Point the user at download `old` instead of adding its address again
    /// (see `UseExisting`). `None` when the finished file is gone, so the new
    /// download should be added after all.
    pub fn use_existing(self: &Arc<Self>, old: &DownloadEntry) -> Option<(DownloadId, Link)> {
        match UseExisting::new(&*self.store, &*self.store, &*self.transfers, &*self.files, &*self.front).run(old) {
            Existing::Gone => None,
            Existing::Shown { id, url } | Existing::Resumed { id, url } => Some((id, url)),
            Existing::NotResumed { id, url, error } => {
                warn!(download_id = id.get(), error = %error, "could not resume");
                Some((id, url))
            }
        }
    }

    /// Add `download` to a queue (the one it names, else the one Download
    /// later goes to, else the main one) without starting it or opening a
    /// connection.
    pub fn add_later(&self, download: Download) -> (DownloadId, Link) {
        let later = self.settings_now().later_queue;
        AddLater::new(&*self.store, &self.intake).run(download, later)
    }

    /// Add `items` to wait in `queue` (see `AddMany`). Returns how many were
    /// added and how many were skipped as duplicates.
    pub fn add_many(self: &Arc<Self>, items: Vec<Download>, queue: Option<QueueId>) -> (usize, usize) {
        let (ids, skipped) = self.add_many_ids(items, queue);
        (ids.len(), skipped)
    }

    /// `add_many`, answering with the new downloads' ids.
    pub fn add_many_ids(self: &Arc<Self>, items: Vec<Download>, queue: Option<QueueId>) -> (Vec<DownloadId>, usize) {
        self.add_packaged(items, queue, &Packaging::None)
    }

    /// `add_many_ids`, keeping the new downloads as packages as `packaging`
    /// says (see `KeepPackages`).
    pub fn add_packaged(
        self: &Arc<Self>,
        mut items: Vec<Download>,
        queue: Option<QueueId>,
        packaging: &Packaging,
    ) -> (Vec<DownloadId>, usize) {
        let keep = KeepPackages::new(&*self.store);
        keep.prepare(&mut items, packaging);
        let names: Vec<String> = items
            .iter()
            .map(|d| udm_domain::filename::proposed_name(d.url.as_str(), d.filename.as_deref()).into_string())
            .collect();
        let choice = self.settings_now().if_duplicate;
        let added = AddMany::new(&*self.store, &*self.store, &*self.transfers, &*self.files, &self.intake)
            .run(items, queue, choice);
        for (id, e) in added.not_replaced {
            warn!(download_id = id.get(), error = %e, "could not remove");
        }
        let kept: Vec<(DownloadId, String)> = added
            .ids
            .iter()
            .zip(&added.positions)
            .map(|(id, &i)| (*id, names.get(i).cloned().unwrap_or_default()))
            .collect();
        for package in keep.keep(&kept, packaging) {
            info!(package = package.get(), "kept as a package");
        }
        (added.ids, added.skipped)
    }

    /// Read a stream's playlist on another thread and answer `reply` with
    /// what it offers.
    pub fn probe_media(&self, request: u64, download: Download, reply: Reply) {
        let probes = Arc::clone(&self.probes);
        thread::spawn(move || {
            reply.send(Event::MediaInfo { request, info: probes.media(&download) });
        });
    }

    /// Crawl a site for `client` on another thread, sending it progress and
    /// then what was found. With `download_found`, each file found goes to
    /// that queue at once, and the queue starts.
    pub fn grab(self: &Arc<Self>, client: ClientId, req: GrabRequest, reply: Reply) -> Result<(), ServiceError> {
        req.validate()?;
        let stop = Arc::new(AtomicBool::new(false));
        {
            let mut grabs = lock(&self.grabs);
            if grabs.contains_key(&client) {
                return Err(ServiceError::CrawlRunning);
            }
            grabs.insert(client, Arc::clone(&stop));
        }
        let daemon = Arc::clone(self);
        thread::spawn(move || {
            let send = |event: Event| {
                reply.send(event);
            };
            info!(client = client.get(), url = %req.start, "crawl");
            let mut handed = 0;
            let mut added = Vec::new();
            let login = daemon.login_cookies(&req);
            let out = daemon.crawler.crawl(&req, login.as_deref(), &stop, &mut |o| {
                if let Some(queue) = req.download_found {
                    let fresh = &o.files[handed..];
                    if !fresh.is_empty() {
                        added.extend(daemon.download_in_queue(&req, login.as_deref(), fresh, queue, handed == 0));
                    }
                    handed = o.files.len();
                }
                send(Event::GrabProgress {
                    pages: o.pages,
                    bytes: o.bytes,
                    found: o.files.len(),
                    explored: o.explored,
                    added: added.clone(),
                })
            });
            lock(&daemon.grabs).remove(&client);
            info!(client = client.get(), pages = out.pages, files = out.files.len(), "crawl done");
            send(Event::GrabDone {
                files: out.files,
                pages: out.pages,
                explored: out.explored,
                stopped: out.stopped,
                login_cookies: login,
            });
        });
        Ok(())
    }

    /// Add `files` a crawl for `req` found to `queue` and start it; with
    /// `first`, also set how many it downloads at once to the request's.
    /// Files on the login page's site carry `login` (its cookies).
    /// Returns the new downloads' ids.
    pub fn download_in_queue(
        self: &Arc<Self>,
        req: &GrabRequest,
        login: Option<&str>,
        files: &[grab::GrabFile],
        queue: QueueId,
        first: bool,
    ) -> Vec<DownloadId> {
        let settings = self.settings_now();
        let items = files.iter().filter_map(|f| self.found.download_for(req, f, &settings, login)).collect();
        let (added, _) = self.add_many_ids(items, Some(queue));
        if first {
            let at_once = req.download_at_once as usize;
            let _ = self.update_queue(queue, |q| {
                q.max_concurrent = at_once;
            });
        }
        if !added.is_empty() && !self.queues().iter().any(|q| q.id == queue && q.running) {
            if let Err(e) = self.start_queue(queue, None) {
                warn!(error = %e, "could not start the grabber's queue");
            }
        }
        added
    }

    /// Where yt-dlp and ffmpeg are, for Options.
    pub fn tools(&self) -> Vec<ToolState> {
        ListTools::new(&*self.tool_folder, self.platform).run()
    }

    /// Install `tool` (see `InstallTool`) on another thread and answer
    /// `reply` with `Tools`. One install of a tool runs at a time.
    pub fn install_tool(self: &Arc<Self>, tool: Tool, reply: Reply) {
        if !lock(&self.tool_installs).insert(tool) {
            let error = Failure::from(ToolError::Busy(tool.name().to_string()));
            reply.send(Event::Tools { tools: self.tools(), done: Some(tool), error: Some(error) });
            return;
        }
        let daemon = Arc::clone(self);
        thread::spawn(move || {
            let installed =
                InstallTool::new(&*daemon.tool_releases, &*daemon.tool_folder, &*daemon.hashes, daemon.platform).run(
                    tool,
                    false,
                    daemon.clock.unix_now(),
                );
            lock(&daemon.tool_installs).remove(&tool);
            let error = installed.err().map(|e| {
                warn!(tool = tool.name(), error = %e, "could not install");
                Failure::from(e)
            });
            reply.send(Event::Tools { tools: daemon.tools(), done: Some(tool), error });
        });
    }

    /// Check the debrid account (see `CheckAccount`) on another thread and
    /// answer `reply` with `Debrid`.
    pub fn check_debrid(self: &Arc<Self>, service: DebridService, key: Option<DebridKey>, reply: Reply) {
        let daemon = Arc::clone(self);
        thread::spawn(move || {
            let check = CheckAccount::new(&*daemon.debrid, &*daemon.debrid_keys).run(service, key);
            reply.send(Event::Debrid { check });
        });
    }

    /// Store or remove the debrid account's key (see `SetKey`); the links
    /// and hosts kept for the old key are forgotten.
    pub fn set_debrid_key(&self, key: Option<DebridKey>) -> Result<(), SettingsError> {
        let done = SetKey::new(&*self.debrid_keys, &*self.transfers).run(key);
        self.links.forget();
        done
    }

    /// List `page`'s pictures with gallery-dl on another thread (see
    /// `ReadGallery`) and answer `reply` with `Gallery`.
    pub fn read_gallery(self: &Arc<Self>, request: u64, page: String, user_agent: Option<String>, reply: Reply) {
        let daemon = Arc::clone(self);
        thread::spawn(move || {
            let found = ReadGallery::new(&*daemon.probes, &daemon.clock, &daemon.gallery_reads)
                .run(&page, user_agent.as_deref());
            let page: String = page.trim().chars().take(2000).collect();
            let event = match found {
                Ok(gallery) => Event::Gallery { request, page, gallery: Some(gallery), error: None },
                Err(e) => Event::Gallery { request, page, gallery: None, error: Some(e.into()) },
            };
            reply.send(event);
        });
    }

    /// "Keep yt-dlp up to date": at most once an hour, see whether a day
    /// passed since the last look, and if so update yt-dlp on another
    /// thread (see `UpdateYtDlp`).
    pub fn keep_yt_dlp_current(self: &Arc<Self>) {
        {
            let mut last = lock(&self.tools_checked);
            if last.is_some_and(|t| t.elapsed() < Duration::from_secs(60 * 60)) {
                return;
            }
            *last = Some(Instant::now());
        }
        let auto = self.settings_now().ytdlp_auto_update;
        if !auto || !lock(&self.tool_installs).insert(Tool::YtDlp) {
            return;
        }
        let daemon = Arc::clone(self);
        thread::spawn(move || {
            let updated =
                UpdateYtDlp::new(&*daemon.tool_releases, &*daemon.tool_folder, &*daemon.hashes, daemon.platform)
                    .run(auto, daemon.clock.unix_now());
            lock(&daemon.tool_installs).remove(&Tool::YtDlp);
            match updated {
                Ok(Some((version, _))) => {
                    info!(version = %version, "updated yt-dlp");
                    daemon.clients.to_windows(Event::Tools { tools: daemon.tools(), done: None, error: None });
                }
                Ok(None) => {}
                Err(e) => warn!(error = %e, "could not update yt-dlp"),
            }
        });
    }

    /// Look for due grabber projects at most every half minute: the
    /// scheduler wakes on every download event, and this reads files.
    pub fn check_grab_schedules(self: &Arc<Self>) {
        {
            let mut last = lock(&self.grab_checked);
            if last.is_some_and(|t| t.elapsed() < Duration::from_secs(30)) {
                return;
            }
            *last = Some(Instant::now());
        }
        self.run_grab_schedules(self.clock.now());
    }

    /// Start the saved grabber projects whose scheduled run is due, each on
    /// its own thread. Runs with the scheduler.
    pub fn run_grab_schedules(self: &Arc<Self>, now: queue::Moment) {
        let Some(projects) = self.projects.clone() else {
            return;
        };
        let runs = {
            let (daemon, files) = (Arc::clone(self), Arc::clone(&projects));
            (self.grab_runs_for)(
                Arc::clone(&self.grab_runs),
                Arc::new(move |project, day| daemon.run_scheduled_grab(&*files, project, day)),
            )
        };
        for started in RunGrabSchedules::new(&*projects, &runs, &StoppedAt(now)).run() {
            if let Some(e) = started.not_saved {
                warn!(project = %started.project, error = %e, "could not save the grabber project");
            }
        }
    }

    /// One scheduled run of a grabber project, as its `run` says: crawl
    /// until done or until the schedule's stop time and keep what it found,
    /// then maybe add the new finds to the project's queue and start it; or
    /// only add the ticked files and start the queue. The stop time ends the
    /// queue's run too, and what the project asks when done (with its own
    /// force-close choice) happens after the crawl, or once the queue is done.
    pub fn run_scheduled_grab(self: &Arc<Self>, projects: &dyn GrabProjects, mut project: grab::Project, day: i64) {
        info!(project = %project.name, run = ?project.run, "schedule runs a grabber project");
        let end = project.schedule.as_ref().filter(|s| s.stop.is_some()).and_then(|s| s.end_of_run(day));
        let resync;
        let login = self.login_cookies(&project.request);
        let to_download = match project.run {
            grab::RunAction::DownloadChecked => {
                resync = project.to_resync(&[]);
                project.checked_files()
            }
            run => {
                let stop = AtomicBool::new(false);
                let out = self.crawler.crawl(&project.request, login.as_deref(), &stop, &mut |_| {
                    if end.is_some_and(|end| self.clock.now() >= end) {
                        stop.store(true, Ordering::Relaxed);
                    }
                });
                resync = project.to_resync(&out.files);
                let new = project.record_run(out.files);
                info!(project = %project.name, files = new.len(), "grabber project found new files");
                if run == grab::RunAction::ExploreAndDownload {
                    new
                } else {
                    Vec::new()
                }
            }
        };
        if let Err(e) = projects.save(&project) {
            warn!(project = %project.name, error = %e, "could not save the grabber project");
        }
        // Set before the queue starts, so a run that ends at once still
        // does it. A queue already running is a run someone else started,
        // such as the user: the project's files join it, but its stop time
        // and when-done action stay that run's own.
        let mut owns_run = false;
        let _ = self.update_queue(project.queue, |q| {
            if q.running {
                return;
            }
            owns_run = true;
            q.when_done_once = Some(project.when_done);
            q.open_when_done_once = project.open_when_done.clone();
            q.force_close_once = Some(project.force_close);
            q.stop_at_once = end.map(queue::Moment::total);
        });
        // A synchronizing project downloads again what earlier runs got and
        // the server has since changed.
        let (changed, listed) = self.resync(&resync, project.queue);
        let to_download: Vec<grab::GrabFile> = to_download.into_iter().filter(|f| !listed.contains(&f.url)).collect();
        // Through the usual path, which skips addresses already in the list.
        let added = if to_download.is_empty() {
            0
        } else {
            self.download_in_queue(&project.request, login.as_deref(), &to_download, project.queue, true).len()
        };
        info!(project = %project.name, added, changed, "grabber project queued files");
        let added = added + changed;
        if added == 0 {
            // Nothing to wait for: done now.
            if owns_run {
                let _ = self.update_queue(project.queue, |q| {
                    q.when_done_once = None;
                    q.open_when_done_once = None;
                    q.force_close_once = None;
                    q.stop_at_once = None;
                });
            }
            if let Some(file) = &project.open_when_done {
                self.opener.open(file);
            }
            self.when_done(project.when_done, &project.name, None, project.force_close);
        }
    }
    /// Convert finished download `id` on another thread (see
    /// `ConvertDownload`). The new file is a row of its own: added and
    /// started now, with ffmpeg's progress, done with a note, or failed with
    /// ffmpeg's complaint.
    pub fn convert(
        self: &Arc<Self>,
        id: DownloadId,
        to: udm_domain::media::Target,
        delete_original: bool,
    ) -> Result<(), ServiceError> {
        let output =
            ConvertDownload::new(&*self.store, &*self.files, &self.conversions).run(id, to, delete_original)?;
        info!(download_id = id.get(), output = %output.display(), "converting");
        Ok(())
    }

    /// Ask GitHub (at `url`) for the latest release, through the settings'
    /// proxy, and note when that happened.
    pub fn check_update(&self, url: &str) -> Event {
        let github = (self.releases)(url);
        match CheckForUpdate::new(&github, &*self.settings, self.version).run(self.clock.unix_now()) {
            Ok(newer) => Event::Update { newer, error: None },
            Err(e) => Event::Update { newer: None, error: Some(e.into()) },
        }
    }

    /// Update now (see `InstallUpdate`), then stop the downloads, save the
    /// list and quit with the windows, so Setup can replace the files.
    /// Setup closes whatever is left.
    pub fn install_update(&self) -> Result<(), ServiceError> {
        let github = (self.releases)(&self.release_url);
        let path = InstallUpdate::new(&github, &*self.settings, self.version).run(self.clock.unix_now())?;
        info!(path = %path.display(), "started the update; quitting for it");
        self.pause_all();
        self.transfers.save_history();
        self.clients.broadcast(Event::Quit);
        Ok(())
    }

    /// Once a week, when a window connects, look for a newer release on
    /// another thread and tell the windows if there is one.
    pub fn weekly_update_check(self: &Arc<Self>) {
        if !ClaimWeeklyCheck::new(&*self.settings).run(self.clock.unix_now()) {
            return;
        }
        let daemon = Arc::clone(self);
        thread::spawn(move || {
            if let msg @ Event::Update { newer: Some(_), .. } = daemon.check_update(&daemon.release_url) {
                daemon.clients.broadcast(msg);
            }
        });
    }

    /// List the files inside zip download `id` on another thread, reading
    /// the end of the archive with its own request (cookies and all), and
    /// answer `reply` with `zip_preview`.
    pub fn preview_zip(&self, id: DownloadId, reply: Reply) {
        let entry = self.entry(id);
        let download = self.store.request(id);
        let answer = move |entries: Vec<ZipEntry>, error: Option<Failure>| {
            reply.send(Event::ZipPreview { id, entries, error });
        };
        let Some(entry) = entry else {
            return answer(Vec::new(), Some(udm_domain::download::DownloadError::NoDownload(id).into()));
        };
        let download = download.unwrap_or_else(|| Download { url: entry.url.clone(), ..Download::default() });
        let probes = Arc::clone(&self.probes);
        thread::spawn(move || match probes.zip(&download, entry.total) {
            Ok(entries) => answer(entries, None),
            Err(e) => answer(Vec::new(), Some(e.into())),
        });
    }

    /// The cookies for `req`'s login page's site: the browser's, where the
    /// user signed in after Open, else the ones the user pasted. `None`
    /// without a login page.
    pub fn login_cookies(&self, req: &GrabRequest) -> Option<String> {
        let login = req.login_url.as_deref()?;
        self.browser.cookies(login).or_else(|| req.login_cookies.as_deref().and_then(grab::clean_cookies))
    }

    /// Stop `client`'s crawl, if it has one.
    pub fn stop_grab(&self, client: ClientId) {
        if let Some(stop) = lock(&self.grabs).get(&client) {
            stop.store(true, Ordering::Relaxed);
        }
    }

    /// Ask the server of each of `links` for its size and type, the way a
    /// download of it would (its cookies, referrer, proxy and login), for
    /// `client`'s link picker. `LINK_PROBES` links are asked at a time, so a
    /// long list neither waits on one slow server nor floods the sites, and
    /// each answer goes out as it comes. Any earlier probing for `client`
    /// stops.
    pub fn probe_links(&self, client: ClientId, request: u64, links: Vec<Download>, reply: Reply) {
        let stop = Arc::new(AtomicBool::new(false));
        if let Some(old) = lock(&self.link_probes).insert(client, Arc::clone(&stop)) {
            old.store(true, Ordering::Relaxed);
        }
        let links = Arc::new(links);
        let next = Arc::new(AtomicUsize::new(0));
        for _ in 0..LINK_PROBES.min(links.len()) {
            let (links, next, stop, reply) = (Arc::clone(&links), Arc::clone(&next), Arc::clone(&stop), reply.clone());
            let probes = Arc::clone(&self.probes);
            thread::spawn(move || loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(link) = links.get(index).filter(|_| !stop.load(Ordering::Relaxed)) else {
                    return;
                };
                let found = probes.link(link);
                // Stopped while asking: the picker is gone, or asked anew.
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let msg = match found {
                    Ok(i) => {
                        Event::LinkInfo { request, index, total: i.total, content_type: i.content_type, error: None }
                    }
                    Err(e) => {
                        Event::LinkInfo { request, index, total: None, content_type: None, error: Some(e.into()) }
                    }
                };
                // A send that fails means the client went.
                if !reply.send(msg) {
                    return;
                }
            });
        }
    }

    /// Stop `client`'s link probes, if it has any.
    pub fn stop_probe_links(&self, client: ClientId) {
        if let Some(stop) = lock(&self.link_probes).remove(&client) {
            stop.store(true, Ordering::Relaxed);
        }
    }

    /// Announce `download` as queued, probe it on another thread, then hold it
    /// as `confirming` with the proposed file until a window answers.
    /// `duplicate_of` is another download with the same address, for the
    /// dialog to ask about.
    pub fn ask(self: &Arc<Self>, download: Download, duplicate_of: Option<DownloadId>) -> (DownloadId, Link) {
        let url = download.url.clone();
        let id = self.transfers.reserve_id();
        self.store.keep_request(id, download.clone());
        self.store.announce(Event::Added { id, url: url.clone() });

        let daemon = Arc::clone(self);
        thread::spawn(move || {
            let found = match daemon.probes.file(&download) {
                Ok(found) => found,
                Err(e) => {
                    // Same as a download that fails to start, so the browser
                    // can take it back.
                    daemon.store.announce(Event::Failed { id, url: download.url, error: e.to_string() });
                    return;
                }
            };
            let probed =
                Probed { filename: found.filename, path: found.path, total: found.total, resumable: found.resumable };
            let hold = HoldForAnswer::new(
                &*daemon.store,
                &*daemon.transfers,
                &*daemon.files,
                &*daemon.transfers,
                &*daemon.early,
                &daemon.answers,
            );
            if let Held::Started(Err(e)) = hold.run(id, &download, probed, duplicate_of) {
                warn!(download_id = id.get(), error = %e, "could not start");
            }
        });
        (id, url)
    }

    /// Answer the save-as dialog of a `confirming` download, or start a
    /// `waiting` one. Missing names and folders keep the proposed ones. With
    /// `start` false it waits in `queue` (the main queue by default).
    /// `file_exists`, when given, decides what happens if the name is taken,
    /// now and on later starts. For a download asked about as a duplicate,
    /// `duplicate` decides what happens to the other download first (a new
    /// name when missing). A category and description, when given, are kept
    /// with it. The dialog's "remember" boxes change the settings.
    pub fn confirm(&self, answer: Answer) -> Result<(), ServiceError> {
        confirm_answer::<W>(&self.store, &self.transfers, &self.files, &self.front, &self.early, answer)
    }

    /// Cancel a download, whether it waits for its dialog or is in the engine.
    pub fn cancel(self: &Arc<Self>, id: DownloadId) {
        CancelDownload::new(&*self.store, &*self.transfers, &self.conversions).run(id);
    }

    /// Queue `download`, remember it for retries, and announce it.
    /// `reuse_path` is a partial file an earlier attempt left behind.
    pub fn start(&self, download: Download) -> (DownloadId, Link) {
        self.transfers.start(download)
    }

    /// Re-queue a failed, canceled or interrupted download from its stored
    /// request, or from its URL alone when the request was not kept (see
    /// `RetryDownload`).
    pub fn retry(&self, id: DownloadId) -> Result<(DownloadId, Link), ServiceError> {
        RetryDownload::new(&*self.store, &*self.store, &*self.transfers).run(id).map_err(ServiceError::from)
    }

    /// Set or clear download `id`'s own speed limit (see `SetSpeedLimit`).
    pub fn set_download_limit(&self, id: DownloadId, bytes_per_sec: Option<u64>) -> Result<(), ServiceError> {
        SetSpeedLimit::new(&*self.store, &*self.transfers).run(id, bytes_per_sec).map_err(ServiceError::from)
    }

    /// Set whether download `id` keeps its own speed limit when it is
    /// stopped and resumed.
    pub fn set_limit_remember(&self, id: DownloadId, remember: bool) -> Result<(), ServiceError> {
        SetLimitRemember::new(&*self.store).run(id, remember).map_err(ServiceError::from)
    }

    /// Start download `id` again from the first byte, under the same id (see
    /// `Redownload`).
    pub fn redownload(&self, id: DownloadId) -> Result<(), ServiceError> {
        let url = Redownload::new(&*self.store, &*self.transfers, &*self.files).run(id)?;
        info!(download_id = id.get(), url = %url, "downloading again");
        Ok(())
    }

    /// Give download `id` a new link, or wait for the browser's next one (see
    /// `RefreshLink`). Returns why the new link was refused, if it was.
    pub fn refresh_link(
        &self,
        id: DownloadId,
        download: Option<Download>,
        restart: bool,
    ) -> Result<Option<udm_domain::download::Mismatch>, ServiceError> {
        match RefreshLink::new(&*self.store, &*self.store, &*self.transfers).run(id, download, restart)? {
            Refreshed::Waiting => Ok(None),
            Refreshed::Mismatch(reason) => Ok(Some(reason)),
            Refreshed::NewLink(url) => {
                info!(download_id = id.get(), url = %url, "new link");
                Ok(None)
            }
        }
    }

    /// Change a stopped or finished download's details (see `SetProperties`).
    pub fn set_properties(&self, id: DownloadId, p: Properties) -> Result<(), ServiceError> {
        let moved = SetProperties::new(&*self.store, &*self.files).run(id, p)?;
        if let Some(to) = moved {
            info!(download_id = id.get(), to = %to.display(), "moved");
        }
        Ok(())
    }

    /// Under "the browser's proxy only after download errors", retry failed
    /// download `id` once, through the proxy its browser reported. Returns
    /// whether it did.
    pub fn retry_through_browser_proxy(&self, id: DownloadId, s: &Settings) -> bool {
        if !(s.use_browser_proxy && s.browser_proxy_after_errors) {
            return false;
        }
        let url = self.store.request(id).filter(|d| d.proxy.is_some() || d.browser_proxy.is_some()).map(|d| d.url);
        let Some(url) = url else {
            return false;
        };
        if !self.browser_proxy_retries.claim_retry(url) {
            return false;
        }
        match self.retry(id) {
            Ok((new_id, _)) => {
                info!(download_id = id.get(), new_id = new_id.get(), "failed; retrying through the browser's proxy");
                true
            }
            Err(e) => {
                warn!(download_id = id.get(), error = %e, "could not retry through the browser's proxy");
                false
            }
        }
    }

    /// The settings as they are, with only the video button's fields taken
    /// from `asked`. The browser extension changes nothing else, and only
    /// the window changes settings at large: a browser connection, from an
    /// extension that is not UDM's or a bug in it, must not point the virus
    /// scanner, ffmpeg, folders or proxy somewhere else.
    pub fn with_video_button(&self, asked: &Settings) -> Settings {
        let mut settings = self.settings_now();
        settings.video_button = asked.video_button;
        settings.video_button_excluded_sites = asked.video_button_excluded_sites.clone();
        settings
    }

    /// Queue every interrupted download again (the "resume at startup"
    /// option).
    pub fn resume_interrupted(&self) {
        let ids = self.store.ids_where(|_, status| status == Status::Interrupted);
        for id in ids {
            match self.retry(id) {
                Ok((new_id, url)) => info!(download_id = id.get(), new_id = new_id.get(), url = %url, "resuming"),
                Err(e) => warn!(download_id = id.get(), error = %e, "could not resume"),
            }
        }
    }

    /// Forget a download that is no longer running. Its files stay on disk
    /// unless `delete_files`, which sends them to the Recycle Bin.
    pub fn remove(&self, id: DownloadId, delete_files: bool) -> Result<(), ServiceError> {
        if delete_files {
            info!(download_id = id.get(), "removing with its files");
        }
        RemoveDownload::new(&*self.store, &*self.store, &*self.transfers, &*self.files)
            .run(id, delete_files)
            .map_err(ServiceError::from)
    }

    /// Send the whole list to one client, under the table lock so no event can
    /// slip between the snapshot and the client's next frame.
    pub fn send_list(&self, reply: &Reply) {
        self.store.snapshot(|items, queues| {
            let refresh = Event::RefreshWaiting { id: self.transfers.waiting_for_link() };
            let packages = Event::Packages { items: self.store.edit_packages(|list| list.clone()) };
            for msg in [Event::Downloads { items }, Event::Queues { items: queues }, packages, refresh] {
                reply.send(msg);
            }
        })
    }
}

/// Do what a finished queue, or with `file` one finished download, asks
/// (see `RunWhenDone`).
pub fn when_done(power: &impl PowerControl, action: WhenDone, queue: &str, file: Option<&str>, force_close: bool) {
    let request = PowerRequest { action, queue: queue.to_string(), file: file.map(str::to_string), force_close };
    match RunWhenDone::new(power).run(&request) {
        PowerStep::None => {}
        PowerStep::Exit => info!(subject = %request.subject(), "done: exiting"),
        PowerStep::Countdown => {
            info!(subject = %request.subject(), action = action.label(), seconds = COUNTDOWN_SECS, "done: countdown")
        }
    }
}

/// Answer a save-as dialog (see `ConfirmAnswer`), logging what could not
/// be done alongside.
pub fn confirm_answer<W: Wiring>(
    store: &W::Store,
    transfers: &W::Transfers,
    files: &W::Files,
    front: &W::Front,
    early: &W::Early,
    answer: Answer,
) -> Result<(), ServiceError> {
    let id = answer.id;
    let confirm = ConfirmAnswer::new(store, store, transfers, files, front, transfers, early);
    let (done, warnings) = confirm.run(answer)?;
    for w in warnings {
        match w {
            ConfirmWarning::DuplicateNotRemembered(e) => {
                warn!(error = %e, "could not remember the duplicate choice")
            }
            ConfirmWarning::ChoicesNotRemembered(e) => {
                warn!(error = %e, "could not remember the dialog's choices")
            }
            ConfirmWarning::OldNotRemoved(old, e) => warn!(download_id = old.get(), error = %e, "could not remove"),
            ConfirmWarning::OldNotResumed(old, e) => warn!(download_id = old.get(), error = %e, "could not resume"),
        }
    }
    if let Confirmed::UsedExisting { old, kept } = done {
        info!(download_id = id.get(), duplicates = old.get(), kept = kept.get(), "duplicate");
    }
    Ok(())
}

/// The daemon's answers to its clients' requests: the `Requests` port its
/// server drives.
pub struct Served<W: Wiring>(pub Arc<DaemonService<W>>);

impl<W: Wiring> Requests for Served<W> {
    fn handle(&self, client: ClientId, command: Command, reply: &Reply) {
        dispatch(command, &self.0, client, reply);
    }

    fn gone(&self, client: ClientId) {
        self.0.stop_grab(client);
        self.0.stop_probe_links(client);
    }
}

/// Handle request `incoming` from client `client`, sending any direct reply
/// to `reply`.
pub fn dispatch<W: Wiring>(incoming: Command, daemon: &Arc<DaemonService<W>>, client: ClientId, reply: &Reply) {
    let send = |event: Event| {
        reply.send(event);
    };
    match incoming {
        Command::Hello { client: kind, .. } => {
            if kind.is_none() {
                daemon.clients.browser_connected(client);
            }
            if kind.as_deref() == Some(WINDOW_CLIENT) {
                daemon.clients.add_window(client);
                send(daemon.clients.browser_link());
                // A window that opens during a countdown shows it too.
                if let Some(countdown) = daemon.countdown_message() {
                    send(countdown);
                }
                if let Some(quota) = daemon.quota_message() {
                    send(quota);
                }
                if let Some(lift) = daemon.limit_lift_message() {
                    send(lift);
                }
                daemon.weekly_update_check();
                // Links picked from a page while no window was open.
                if let Some(links) = lock(&daemon.picked_links).take() {
                    send(links);
                }
                // The command line of the `udm-gui` that opened this window.
                let held = lock(&daemon.command_line).take();
                if let Some((cmd, _)) = held.filter(|(_, at)| at.elapsed() < COMMAND_LINE_WAIT) {
                    daemon.run_command_line(cmd, true);
                }
            }
            send(Event::Hello {
                name: "udm-daemon".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                api: Some(super::AUTOMATION_API),
            })
        }
        Command::Ping => send(Event::Pong),
        Command::CommandLine(cmd) => {
            let window_open = daemon.clients.window_open();
            if window_open && cmd.is_empty() {
                // Starting UDM again brings the open window forward, as in
                // IDM, instead of opening a second one.
                daemon.clients.to_windows(Event::Show);
            } else if window_open {
                daemon.run_command_line(cmd, false);
            } else if !cmd.is_empty() {
                *lock(&daemon.command_line) = Some((cmd, Instant::now()));
            }
            send(Event::CommandLine { window_open });
        }
        Command::Add(mut download) => {
            download.limit_browser_request();
            if let Err(message) = daemon.check_download(&download) {
                warn!(url = %download.url, error = %message, "refused a download");
                send(Event::Rejected { url: download.url, error: message.into() });
                return;
            }
            let (id, url) = daemon.add(download);
            info!(download_id = id.get(), url = %url, "add");
            send(Event::Ack { id, url, status: "accepted".to_string() });
        }
        Command::AddMany { items, queue, packaging } => {
            let (ids, skipped) = daemon.add_packaged(items, queue, &packaging);
            info!(count = ids.len(), skipped, "added downloads to wait");
            send(Event::AddedMany { count: ids.len(), skipped });
        }
        Command::UnpackPackage { id, password } => {
            info!(package = id.get(), "unpack");
            if let Err(message) = daemon.unpack_package(id, password) {
                send(Event::Error { error: message.into() });
            }
        }
        Command::UngroupPackage { id } => {
            if let Err(message) = daemon.ungroup_package(id) {
                send(Event::Error { error: message.into() });
            }
        }
        Command::ProbeMedia { request, mut download } => {
            download.limit_browser_request();
            if let Err(message) = daemon.check_download(&download) {
                send(Event::Error { error: message.into() });
                return;
            }
            daemon.probe_media(request, download, reply.clone());
        }
        Command::Grab(req) => {
            if let Err(message) = daemon.grab(client, req, reply.clone()) {
                send(Event::Error { error: message.into() });
            }
        }
        Command::CancelGrab => daemon.stop_grab(client),
        Command::ProbeLinks { request, links } => {
            if let Some(message) = links.iter().find_map(|d| daemon.check_download(d).err()) {
                send(Event::Error { error: message.into() });
                return;
            }
            daemon.probe_links(client, request, links, reply.clone());
        }
        Command::StopProbeLinks => daemon.stop_probe_links(client),
        Command::Cancel { id } => {
            info!(download_id = id.get(), "cancel");
            daemon.cancel(id);
        }
        Command::Confirm(answer) => {
            let (id, start) = (answer.id, answer.start);
            match daemon.confirm(answer) {
                Ok(()) => info!(download_id = id.get(), start, "confirm"),
                Err(message) => send(Event::Error { error: message.into() }),
            }
        }
        Command::Retry { id } => match daemon.retry(id) {
            Ok((new_id, url)) => {
                info!(download_id = id.get(), new_id = new_id.get(), "retry");
                send(Event::Ack { id: new_id, url, status: "accepted".to_string() });
            }
            Err(message) => send(Event::Error { error: message.into() }),
        },
        Command::Remove { id, delete_files } => {
            if let Err(message) = daemon.remove(id, delete_files) {
                send(Event::Error { error: message.into() });
            }
        }
        Command::List => daemon.send_list(reply),
        Command::SaveQueue { queue } => match daemon.save_queue(queue) {
            Ok(id) => info!(queue = %id, "queue saved"),
            Err(message) => send(Event::Error { error: message.into() }),
        },
        Command::DeleteQueue { id } => {
            if let Err(message) = daemon.delete_queue(id) {
                send(Event::Error { error: message.into() });
            }
        }
        Command::StartQueue { id } => match daemon.start_queue(id, None) {
            Ok(()) => {
                info!(queue = %id, "start queue");
                let _ = daemon.wake.send(());
            }
            Err(message) => send(Event::Error { error: message.into() }),
        },
        Command::StopQueue { id } => match daemon.stop_queue(id) {
            Ok(()) => info!(queue = %id, "stop queue"),
            Err(message) => send(Event::Error { error: message.into() }),
        },
        Command::SetQueueOnce { id, when_done } => {
            if let Err(message) = daemon.update_queue(id, |q| {
                q.when_done_once = when_done;
            }) {
                send(Event::Error { error: message.into() });
            }
        }
        Command::MoveToQueue { id, queue, position } => {
            if let Err(message) = daemon.move_to_queue(id, queue, position) {
                send(Event::Error { error: message.into() });
            }
        }
        Command::RemoveFromQueue { id } => {
            if let Err(message) = daemon.remove_from_queue(id) {
                send(Event::Error { error: message.into() });
            }
        }
        Command::CancelCountdown => daemon.cancel_countdown(),
        Command::CountdownNow => daemon.countdown_now(),
        Command::RefreshLink { id, mut download, restart } => {
            if let Some(d) = &mut download {
                d.limit_browser_request();
            }
            let url = download.as_ref().map(|d| d.url.clone()).unwrap_or_default();
            if let Some(d) = &download {
                if let Err(message) = daemon.check_download(d) {
                    send(Event::Error { error: message.into() });
                    return;
                }
            }
            match daemon.refresh_link(id, download, restart) {
                Ok(Some(reason)) => send(Event::RefreshMismatch { id, url, reason: reason.into() }),
                Ok(None) => {}
                Err(message) => send(Event::Error { error: message.into() }),
            }
        }
        Command::CancelRefresh => daemon.transfers.wait_for_link(None),
        Command::PickLinks { page, mut links, user_agent } => {
            links.retain(|l| udm_domain::batch::is_web_url(&l.url));
            links.truncate(udm_domain::batch::MAX_BATCH);
            info!(links = links.len(), page = %page, "links to pick from");
            let msg = Event::PickLinks { page, links, user_agent };
            if daemon.clients.window_open() {
                daemon.clients.broadcast(msg);
                daemon.clients.broadcast(Event::Show);
            } else {
                *lock(&daemon.picked_links) = Some(msg);
                daemon.show_window();
            }
        }
        Command::PickVideos { page, playlist, user_agent } => {
            let playlist = Playlist { title: playlist.title.chars().take(500).collect(), ..playlist.cleaned() };
            info!(videos = playlist.entries.len(), page = %page, "videos to pick from");
            if playlist.entries.is_empty() {
                return;
            }
            let msg = Event::PickVideos { page, playlist, user_agent };
            if daemon.clients.window_open() {
                daemon.clients.broadcast(msg);
                daemon.clients.broadcast(Event::Show);
            } else {
                *lock(&daemon.picked_links) = Some(msg);
                daemon.show_window();
            }
        }
        Command::PickGallery { page, user_agent } => {
            let page: String = page.trim().chars().take(2000).collect();
            if !udm_domain::batch::is_web_url(&page) {
                return;
            }
            info!(page = %page, "gallery to pick from");
            let msg = Event::PickGallery { page, user_agent };
            if daemon.clients.window_open() {
                daemon.clients.broadcast(msg);
                daemon.clients.broadcast(Event::Show);
            } else {
                *lock(&daemon.picked_links) = Some(msg);
                daemon.show_window();
            }
        }
        Command::ReadGallery { request, page, user_agent } => {
            daemon.read_gallery(request, page, user_agent, reply.clone());
        }
        Command::InstallTool { tool } => daemon.install_tool(tool, reply.clone()),
        Command::ListTools => send(Event::Tools { tools: daemon.tools(), done: None, error: None }),
        Command::PreviewZip { id } => daemon.preview_zip(id, reply.clone()),
        Command::Convert { id, to, delete_original } => {
            if let Err(message) = daemon.convert(id, to, delete_original) {
                send(Event::Error { error: message.into() });
            }
        }
        Command::InstallUpdate => {
            let (daemon, reply) = (Arc::clone(daemon), reply.clone());
            thread::spawn(move || {
                if let Err(error) = daemon.install_update() {
                    reply.send(Event::UpdateFailed { error: error.into() });
                }
            });
        }
        Command::CheckUpdate => {
            let (daemon, reply) = (Arc::clone(daemon), reply.clone());
            thread::spawn(move || {
                reply.send(daemon.check_update(&daemon.release_url));
            });
        }
        Command::Rendered { request, html, url, popups } => {
            let page = html.zip(url).map(|(html, url)| RenderedPage { html, url, popups });
            daemon.browser.answered_render(request, page);
        }
        Command::Cookies { request, cookies } => daemon.browser.answered_cookies(request, cookies),
        Command::SetSpeedLimit { id, bytes_per_sec, remember } => {
            let set = daemon
                .set_download_limit(id, bytes_per_sec)
                .and_then(|()| remember.map_or(Ok(()), |r| daemon.set_limit_remember(id, r)));
            if let Err(message) = set {
                send(Event::Error { error: message.into() });
            }
        }
        Command::SetOnDone { id, on_done } => {
            if let Err(message) = daemon.set_on_done(id, on_done) {
                send(Event::Error { error: message.into() });
            }
        }
        Command::Redownload { id } => {
            if let Err(message) = daemon.redownload(id) {
                send(Event::Error { error: message.into() });
            }
        }
        Command::SetProperties { id, changes } => {
            if let Err(message) = daemon.set_properties(id, changes) {
                send(Event::Error { error: message.into() });
            }
        }
        Command::PauseAll => {
            info!("pause all");
            daemon.pause_all();
        }
        Command::ResumeAll => {
            info!("resume all");
            daemon.resume_all();
        }
        Command::GetSettings => send(Event::Settings { settings: daemon.settings_now() }),
        Command::RemoteStatus => send(Event::Remote { state: daemon.remote_state() }),
        Command::StartPairing => match daemon.start_pairing() {
            Ok(state) => send(Event::Remote { state }),
            Err(message) => send(Event::Error { error: message.into() }),
        },
        Command::CancelPairing => {
            daemon.pairing.cancel();
            send(Event::Remote { state: daemon.remote_state() });
        }
        Command::RemoveDevice { id } => {
            if let Err(message) = daemon.pairing.remove(id) {
                send(Event::Error { error: ServiceError::from(message).into() });
            }
            send(Event::Remote { state: daemon.remote_state() });
        }
        Command::CheckDebrid { service, key } => daemon.check_debrid(service, key, reply.clone()),
        Command::SetDebridKey { key } => {
            if let Err(e) = daemon.set_debrid_key(key) {
                warn!(error = %e, "could not store the debrid key");
                send(Event::Error { error: e.into() });
            }
        }
        Command::SetSettings { settings } => {
            let settings =
                if daemon.clients.is_window(client) { settings } else { daemon.with_video_button(&settings) };
            match daemon.transfers.save_settings(settings) {
                Ok(()) => info!("settings changed"),
                Err(message) => send(Event::Error { error: message.into() }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use udm_domain::download::DownloadError;
    use udm_domain::queue::QueueError;

    /// Clients hear the same words as before the errors were typed.
    #[test]
    fn messages_read_as_before() {
        assert_eq!(ServiceError::CrawlRunning.to_string(), "A site crawl is already running.");
        assert_eq!(
            ServiceError::BadBody("http://a/x".into()).to_string(),
            "the form body of http://a/x is not valid base64"
        );
        assert_eq!(ServiceError::from(QueueError::NoQueue(QueueId::new(7))).to_string(), "no queue with id 7");
        assert_eq!(
            ServiceError::from(DownloadError::NoDownload(DownloadId::new(3))).to_string(),
            DownloadError::NoDownload(DownloadId::new(3)).to_string()
        );
    }
}
