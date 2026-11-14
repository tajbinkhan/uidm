//! The ports the application's use cases work through. Adapters implement
//! them: today the daemon binary does, over the state it keeps in memory
//! (see AGENTS.md section 16).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::address::{Address, Link};
use crate::bytes::ByteCount;
use crate::checksum::Algorithm;
use crate::download::{Download, DownloadEntry, Scan, Status, ZipEntry};
use crate::filename::FileName;
use crate::gallery::{Gallery, GalleryError};
use crate::grab::{GrabRequest, Outcome, Project, RenderedPage};
use crate::id::{DownloadId, QueueId};
use crate::media::{Conversion, MediaInfo};
use crate::package::{ArchiveKind, Family, Package, Password, UnpackError};
use crate::queue::{Moment, PowerRequest, Queue, RunEnd, WhenDone};
use crate::quota::QuotaNotice;
use crate::settings::Settings;
use crate::tools::{Forge, InstalledTool, Packing, Tool, ToolError};
use crate::update::{Release, SetupAsset, UpdateError};

/// The download queues, the main queue first.
pub trait QueueStore: Send + Sync {
    /// Run `f` on the queues while nothing else changes them.
    fn edit<T>(&self, f: impl FnOnce(&mut Vec<Queue>) -> T) -> T;

    /// The queues changed: save them, tell every client, and let the
    /// scheduler look at them again.
    fn queues_changed(&self);
}

/// The downloads in the list.
pub trait DownloadList: Send + Sync {
    /// The state of download `id`, or `None` when there is no such download.
    fn status(&self, id: DownloadId) -> Option<Status>;

    /// Download `id` as the list shows it.
    fn entry(&self, id: DownloadId) -> Option<DownloadEntry>;

    /// The request download `id` was made with, if it was kept, forgetting
    /// it. It may hold the browser's cookies.
    fn take_request(&self, id: DownloadId) -> Option<Download>;

    /// Take download `id`'s row out of the list and tell every client.
    fn drop_row(&self, id: DownloadId);

    /// Change download `id`'s row with `f` and tell every client. `f`
    /// returns false to leave the row as it was. Returns `None` when there
    /// is no such download, otherwise what `f` returned.
    fn update(&self, id: DownloadId, f: impl FnOnce(&mut DownloadEntry) -> bool) -> Option<bool>;

    /// The request download `id` was made with, if it was kept.
    fn request(&self, id: DownloadId) -> Option<Download>;

    /// Keep `download` as the request download `id` starts from.
    fn keep_request(&self, id: DownloadId, download: Download);

    /// Change download `id`'s kept request with `f`, if one was kept.
    fn edit_request(&self, id: DownloadId, f: impl FnOnce(&mut Download));

    /// The ids of the finished downloads.
    fn finished(&self) -> HashSet<DownloadId>;

    /// Make download `id` wait to run again. Returns false when there is no
    /// such download.
    fn set_waiting(&self, id: DownloadId) -> bool;

    /// The last download in the list for which `keep` holds.
    fn find_last(&self, keep: impl Fn(&DownloadEntry) -> bool) -> Option<DownloadEntry>;

    /// The ids of the downloads for which `keep(id, status)` holds.
    fn ids_where(&self, keep: impl Fn(DownloadId, Status) -> bool) -> Vec<DownloadId>;

    /// Whether download `id` is running: queued in the engine or downloading.
    fn is_running(&self, id: DownloadId) -> bool {
        matches!(self.status(id), Some(Status::Queued | Status::Downloading))
    }
}

/// Why a download could not be started again. The message is shown to
/// the user or logged.
///
/// The variant names are part of the wire format (the daemon's
/// `error_kind`): renaming one changes what clients read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[error("{0}")]
pub struct ControlError(pub String);

/// Taking new downloads into the list.
pub trait DownloadIntake: Send + Sync {
    /// Whether UDM can send `download`'s request: a form body must be
    /// readable.
    fn sendable(&self, download: &Download) -> bool;

    /// Add `download` to the list as waiting, in no queue yet, without
    /// opening a connection. Returns its id and address.
    fn stage(&self, download: Download) -> (DownloadId, Link);
}

/// Stops and restarts downloads.
pub trait DownloadControl: Send + Sync {
    /// Stop download `id` so that it waits in its queue again, with its
    /// progress kept.
    fn hold(&self, id: DownloadId);

    /// Stop download `id`, keeping its progress. It shows as canceled.
    fn stop(&self, id: DownloadId);

    /// Set running download `id`'s own speed limit, in bytes per second;
    /// `None` lifts it.
    fn set_limit(&self, id: DownloadId, bytes_per_sec: Option<u64>);

    /// Start download `id` again under its own id from `download`.
    fn start_again(&self, id: DownloadId, download: Download);

    /// Hand waiting download `id` to the engine.
    fn dispatch(&self, id: DownloadId) -> Result<(), ControlError>;

    /// Cancel download `id`, which waits for its save-as dialog or in a
    /// queue: anything it started early is thrown away, and every client
    /// hears it was canceled.
    fn cancel_waiting(&self, id: DownloadId);

    /// Make download `id` the one waiting for the next link the browser
    /// hands over, or none, and tell every client.
    fn wait_for_link(&self, id: Option<DownloadId>);

    /// The download waiting for a link from the browser, if any.
    fn waiting_for_link(&self) -> Option<DownloadId>;

    /// Ask the server at `download`'s address whether it still has the file
    /// the partial copy at `partial` was taken from. Returns why not, or
    /// `None` when it does.
    fn check_partial(
        &self,
        download: &Download,
        partial: &str,
    ) -> Result<Option<crate::download::Mismatch>, ControlError>;

    /// Remove download `id` from the list for good: its row, its kept
    /// request, and any wait for a new address for it.
    fn forget(&self, id: DownloadId);

    /// Start `download` as a new row, resuming into `path` and `partial`
    /// when given. Without `remember_limit` its speed limit lasts only until
    /// it stops. Returns the new row's id and its address.
    fn restart(
        &self,
        download: Download,
        path: Option<PathBuf>,
        partial: Option<PathBuf>,
        remember_limit: bool,
    ) -> (DownloadId, Link);
}

/// Why a download's files could not be removed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct FilesError(pub String);

/// The files a download leaves on disk.
pub trait DownloadFiles: Send + Sync {
    /// Send the file at `path` and the partial copy at `partial` (each with
    /// the files the engine keeps beside it) to the Recycle Bin or Trash.
    /// `download` names the download they belong to, so only its own
    /// working files go with them; `None` recycles the files alone.
    fn recycle(
        &self,
        download: Option<DownloadId>,
        path: Option<&str>,
        partial: Option<&str>,
    ) -> Result<(), FilesError>;

    /// Delete the resume records kept beside `paths`, so a file left there
    /// counts as a taken name rather than something to resume.
    fn forget_resume(&self, paths: &[&str]);

    /// Move a finished file to `to`, which must not exist yet.
    fn move_finished(&self, from: &Path, to: &Path) -> Result<(), FilesError>;

    /// Move an unfinished download's file and its resume record to `to`.
    /// Either may be missing.
    fn move_partial(&self, from: &Path, to: &Path) -> Result<(), FilesError>;

    /// Whether anything is at `path`.
    fn exists(&self, path: &Path) -> bool;

    /// Whether a file (not a folder) is at `path`.
    fn is_file(&self, path: &Path) -> bool;

    /// Whether UDM may write files into `dir` without administrator rights.
    fn can_write(&self, dir: &Path) -> bool;
}

/// What "pause all" stopped, for "resume all" to start again.
pub trait PausedDownloads: Send + Sync {
    /// Add `queues` and `downloads` to what was paused.
    fn remember(&self, queues: &[QueueId], downloads: &[DownloadId]);

    /// What was paused, forgetting it.
    fn take(&self) -> (HashSet<QueueId>, HashSet<DownloadId>);
}

/// What the scheduler remembers about each queue's current run.
pub trait QueueRuns: Send + Sync {
    /// Queue `queue` started a run with nothing started in it yet.
    fn begin(&self, queue: QueueId);

    /// Queue `queue`'s run ended (or the queue is gone).
    fn end(&self, queue: QueueId);

    /// Whether queue `queue`'s current run started any download.
    fn started_any(&self, queue: QueueId) -> bool {
        !self.started_in_run(queue).is_empty()
    }

    /// The downloads queue `queue`'s current run started (or checked).
    fn started_in_run(&self, queue: QueueId) -> HashSet<DownloadId>;

    /// Queue `queue`'s run started (or checked) download `id`.
    fn note_started(&self, queue: QueueId, id: DownloadId);

    /// Count one more retry of download `id`, if it has had fewer than
    /// `max`. Returns its new count, or `None` when it has had them all.
    fn count_retry(&self, id: DownloadId, max: u32) -> Option<u32>;

    /// Forget the retries counted for `ids`.
    fn forget_retries(&self, ids: &HashSet<DownloadId>);
}

/// The background checks a synchronizing queue runs on its finished
/// downloads: has the server's copy changed since?
pub trait SyncChecks: Send + Sync {
    /// The downloads being checked now.
    fn checking(&self) -> HashSet<DownloadId>;

    /// Start checking finished download `id`. When its copy changed, it
    /// waits to be downloaded again; the scheduler hears when the check ends.
    fn check(&self, id: DownloadId);
}

/// What happens when a queue's run ends having started something.
pub trait QueueFinish: Send + Sync {
    /// Queue `queue`'s run ended as `end` asks: play the sound the user
    /// chose, open the file, and sleep, shut down or exit if asked.
    fn run_ended(&self, queue: &str, end: &RunEnd);
}

/// The sounds the user may have chosen to hear.
pub trait QueueSounds: Send + Sync {
    /// A queue that was not running started.
    fn queue_started(&self);
}

/// The download quota's count and what it has stopped.
pub trait QuotaState: Send + Sync {
    /// Bytes received in the `window` minutes up to minute `now`.
    fn used(&self, now: u64, window: u64) -> ByteCount;

    /// The first minute after `now` at which the window holds less than
    /// `limit` bytes again, if no more arrive.
    fn frees_at(&self, now: u64, window: u64, limit: ByteCount) -> u64;

    /// Whether downloads are paused for the quota.
    fn paused(&self) -> bool;

    /// Downloads are paused for the quota: add `queues` and `downloads` to
    /// what it stopped.
    fn hold(&self, queues: &[QueueId], downloads: &[DownloadId]);

    /// The quota allows downloads again: what it stopped, forgetting it.
    fn release(&self) -> (Vec<QueueId>, HashSet<DownloadId>);

    /// Whether the nearly-reached warning was given in this window.
    fn warned(&self) -> bool;

    fn set_warned(&self, warned: bool);

    /// Tell every client the quota's state.
    fn announce(&self, notice: QuotaNotice);
}

/// Closing UDM, and sleeping, hibernating or shutting down the computer.
pub trait PowerControl: Send + Sync {
    /// Close UDM: every window quits, and the daemon ends once its other
    /// downloads are done.
    fn exit(&self);

    /// Count down `seconds` before carrying out `request`, replacing any
    /// countdown running. The windows show it and may stop it.
    fn start_countdown(&self, request: &PowerRequest, seconds: u32);

    /// Stop the countdown running, if any, and return what it was for.
    fn take_countdown(&self) -> Option<PowerRequest>;

    /// Tell the windows the countdown stopped.
    fn countdown_canceled(&self);

    /// Sleep, hibernate or shut down now.
    fn power_off(&self, action: WhenDone, force_close: bool);
}

/// Where newer UDM releases come from.
pub trait ReleaseSource: Send + Sync {
    /// The latest release, with the file that installs it only where this
    /// copy can install one (see `udm_domain::update::Install`).
    fn latest(&self) -> Result<Release, UpdateError>;

    /// Download `setup`, check it against the release's signed checksums,
    /// and save it. Returns where.
    fn fetch_setup(&self, setup: &SetupAsset) -> Result<PathBuf, UpdateError>;

    /// Install `setup`, saved at `path`: start Windows' Setup, telling it to
    /// close UDM without asking, or put the new copy in place of this one
    /// and start it once UDM has closed.
    fn run_setup(&self, setup: &SetupAsset, path: &Path) -> Result<(), UpdateError>;
}

/// A tool's latest release on its forge (GitHub or Codeberg).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRelease {
    /// Its tag, or its date for a release whose tag never changes.
    pub version: String,
    pub files: Vec<ReleaseFile>,
}

/// One file of a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseFile {
    pub name: String,
    pub url: String,
    /// The SHA-256 the forge lists for it, when it lists one (GitHub does;
    /// Codeberg does not).
    pub sha256: Option<String>,
}

/// Where yt-dlp's, ffmpeg's and gallery-dl's official releases come from.
pub trait ToolReleases: Send + Sync {
    /// The latest release of `repository` (`owner/name`) on `forge`.
    fn latest(&self, forge: Forge, repository: &str) -> Result<ToolRelease, ToolError>;

    /// A small text file of a release: its checksums.
    fn text(&self, file: &ReleaseFile) -> Result<String, ToolError>;

    /// Download `file`, up to `limit` bytes, to a file of its own that
    /// nothing runs. Returns where.
    fn download(&self, file: &ReleaseFile, limit: ByteCount) -> Result<PathBuf, ToolError>;
}

/// The folder UDM installs tools into.
pub trait ToolFolder: Send + Sync {
    /// The tool UDM installed there, if it did.
    fn installed(&self, tool: Tool) -> Option<InstalledTool>;

    /// Put the checked download at `file` in place as `tool` (unpacking an
    /// archive), let it run, and note `version` and `now` as its release
    /// and last check. Returns the program's path. `file` is gone after.
    fn install(&self, tool: Tool, file: &Path, packing: Packing, version: &str, now: u64)
        -> Result<PathBuf, ToolError>;

    /// Remove a download that did not pass its checks.
    fn discard(&self, file: &Path);

    /// Note that UDM looked for a newer release of `tool` at `now`.
    fn note_checked(&self, tool: Tool, now: u64);

    /// The program UDM would use for `tool`, wherever it is.
    fn found(&self, tool: Tool) -> Option<PathBuf>;
}

/// When UDM last looked for a newer release.
pub trait UpdateChecks: Send + Sync {
    /// Whether the weekly check is on, and when the last check was (seconds
    /// since 1970).
    fn weekly(&self) -> (bool, u64);

    /// A check happened at `at`; with `save`, keep that in the settings
    /// file too.
    fn note_checked(&self, at: u64, save: bool);
}

/// Converting finished downloads with ffmpeg.
pub trait Converter: Send + Sync {
    /// Whether ffmpeg can be found: the one set in Options, or one on `PATH`.
    fn available(&self) -> bool;

    /// Add the conversion's row and run it in the background, reporting
    /// its progress, its end and ffmpeg's complaint if it fails.
    fn start(&self, conversion: Conversion);

    /// Stop the conversion running as row `id`. Returns false when no
    /// conversion runs as that row.
    fn stop(&self, id: DownloadId) -> bool;

    /// Whether any conversion is running.
    fn any_running(&self) -> bool;
}

/// The packages: downloads kept together under a name and a folder.
pub trait PackageStore: Send + Sync {
    /// Run `f` on the packages under their lock.
    fn edit_packages<T>(&self, f: impl FnOnce(&mut Vec<Package>) -> T) -> T;

    /// Tell every client the packages changed, and save them.
    fn packages_changed(&self);
}

/// One archive to unpack: its parts in order, the first the one opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnpackJob {
    /// The first part's file name, for messages.
    pub archive: String,
    pub kind: ArchiveKind,
    pub family: Family,
    pub parts: Vec<PathBuf>,
    /// The folder its files go into, made when missing.
    pub into: PathBuf,
}

/// What unpacking one archive did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Unpacked {
    pub files: u64,
    pub bytes: ByteCount,
}

/// Unpacking archives: zip, tar, gz and xz itself, 7z and RAR through an
/// installed 7-Zip.
pub trait Unpacker: Send + Sync {
    /// Unpack `job`, with `password` when it is encrypted. Entries that
    /// would land outside `job.into` refuse the whole archive, files already
    /// there are never replaced (the new one gets a numbered name), and an
    /// archive that would leave too little room on the disk or holds too
    /// many files is refused. `settings` say where 7-Zip is.
    fn unpack(
        &self,
        job: &UnpackJob,
        password: Option<&Password>,
        settings: &Settings,
    ) -> Result<Unpacked, UnpackError>;
}

/// Why the settings could not be saved. The message is shown to the user
/// or logged.
///
/// The variant names are part of the wire format (the daemon's
/// `error_kind`): renaming one changes what clients read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, serde::Serialize, serde::Deserialize)]
pub enum SettingsError {
    /// A value breaks a rule of [`crate::settings::Settings::validate`].
    #[error(transparent)]
    Invalid(#[from] crate::settings::InvalidSettings),
    /// A check that looks at the disk failed, or the file could not be
    /// written.
    #[error("{0}")]
    Refused(String),
}

/// The user's settings, as the daemon keeps them.
pub trait SettingsStore: Send + Sync {
    fn settings(&self) -> Settings;

    /// Check `settings`, keep them, save them and tell every client.
    fn save_settings(&self, settings: Settings) -> Result<(), SettingsError>;
}

/// Downloads started while their save-as dialog is open, so no time is lost
/// while the user decides.
pub trait EarlyStarts: Send + Sync {
    /// Start download `id` from `download` while its dialog is open, into a
    /// folder of its own and as `filename`, where the clients do not see it.
    fn start(&self, id: DownloadId, download: Download, filename: &FileName);

    /// If download `id` started early, stop that copy and move what it
    /// fetched to the file chosen, then start the download (`start`) or put
    /// it in `queue`. Returns false when it did not start early, leaving
    /// that to the caller.
    fn hand_over(&self, id: DownloadId, start: bool, queue: Option<QueueId>) -> bool;
}

/// Waiting for a window to answer a save-as dialog.
pub trait Answers: Send + Sync {
    /// Wait for a window to answer download `id`'s dialog, opening one if
    /// none is open. If none answers in time, the download starts as
    /// proposed.
    fn await_answer(&self, id: DownloadId);

    /// Download `id` stopped because its file name is taken: wait for a
    /// window to answer its dialog as `await_answer` does, and if none
    /// answers in time, give it a new name.
    fn await_new_name(&self, id: DownloadId);
}

/// Bringing things in front of the user.
pub trait Attention: Send + Sync {
    /// Show finished download `id`'s completion dialog again, or without a
    /// window, a notification standing in for it.
    fn show_done(&self, id: DownloadId);

    /// Bring the main window up.
    fn show_window(&self);
}

/// The site grabber's saved projects.
pub trait GrabProjects: Send + Sync {
    /// The projects' names, the most recently used first.
    fn names(&self) -> Vec<String>;

    /// The project named `name`, or `None` when it cannot be read.
    fn load(&self, name: &str) -> Option<Project>;

    fn save(&self, project: &Project) -> Result<(), FilesError>;
}

/// The site grabber's scheduled runs.
pub trait GrabRuns: Send + Sync {
    /// Claim the run of the project named `name`. Returns false when one is
    /// going on already.
    fn claim(&self, name: &str) -> bool;

    /// Run `project`'s scheduled run of `day` in the background: crawl,
    /// keep what it found, and add and start downloads as it asks. The claim
    /// is released when it ends.
    fn run(&self, project: Project, day: i64);
}

/// What a server said about a link, asked the way a download of it would
/// ask (its cookies, referrer, proxy and login).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LinkFacts {
    pub total: Option<ByteCount>,
    pub content_type: Option<String>,
}

/// What the save-as dialog for a download proposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFacts {
    pub filename: FileName,
    /// The folder for its category, then the name.
    pub path: PathBuf,
    pub total: Option<ByteCount>,
    /// Whether the server sends the file in ranges, so a download can resume.
    pub resumable: bool,
}

/// Why a server could not be asked, in words for the user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ProbeError(pub String);

/// Asking servers about downloads before, or instead of, downloading them.
/// Each call waits for the server.
pub trait Probes: Send + Sync {
    /// The size and type of `download`'s file.
    fn link(&self, download: &Download) -> Result<LinkFacts, ProbeError>;

    /// The file the save-as dialog for `download` proposes. A form post is
    /// not asked, which would send it again: its name and size come from
    /// the request alone.
    fn file(&self, download: &Download) -> Result<FileFacts, ProbeError>;

    /// What the stream at `download` offers: its qualities and tracks.
    fn media(&self, download: &Download) -> MediaInfo;

    /// The files inside the zip archive at `download`, reading only its
    /// end. `total` is its size when known already.
    fn zip(&self, download: &Download, total: Option<ByteCount>) -> Result<Vec<ZipEntry>, ProbeError>;

    /// The pictures of the gallery at `page`, as gallery-dl lists them
    /// (cleaned), read with the settings' proxy and the browser's
    /// `user_agent`. Given up after a timeout.
    fn gallery(&self, page: &Address, user_agent: Option<&str>) -> Result<Gallery, GalleryError>;
}

/// The browser extension, for what only the browser knows. Asking waits a
/// few seconds for its answer, which comes back through `answered_*`.
pub trait Browser: Send + Sync {
    /// Whether the extension is connected.
    fn connected(&self) -> bool;

    /// The browser's cookies for `url`, or `None` without the extension,
    /// an answer or cookies.
    fn cookies(&self, url: &str) -> Option<String>;

    /// `url` as the browser shows it, or `None` without the extension or
    /// an answer in time.
    fn render(&self, url: &str) -> Option<RenderedPage>;

    /// The browser answered cookie request `request`.
    fn answered_cookies(&self, request: u64, cookies: Option<String>);

    /// The browser answered render request `request`.
    fn answered_render(&self, request: u64, page: Option<RenderedPage>);
}

/// Crawling a site for the site grabber.
pub trait SiteCrawler: Send + Sync {
    /// Crawl for `req` on this thread with the settings' proxy and logins,
    /// sending `login` (the login page site's cookies) to that site's
    /// pages, until done or `stop` is set. `progress` hears how it goes.
    fn crawl(
        &self,
        req: &GrabRequest,
        login: Option<&str>,
        stop: &AtomicBool,
        progress: &mut dyn FnMut(&Outcome),
    ) -> Outcome;
}

/// The download engine's own state, and starting a download with it.
pub trait TransferEngine: Send + Sync {
    /// A fresh download id.
    fn reserve_id(&self) -> DownloadId;

    /// How many downloads the engine is running.
    fn active_count(&self) -> usize;

    /// Queue `download` now, remember its request for retries, and announce
    /// it. Returns its id and address.
    fn start(&self, download: Download) -> (DownloadId, Link);

    /// Save the download list and the requests kept for retries.
    fn save_history(&self);

    /// Whether finished download `id`'s file changed on its server since it
    /// was downloaded. False when that cannot be told.
    fn has_changed(&self, id: DownloadId) -> bool;
}

/// How long the computer has gone without keyboard or mouse input.
pub trait UserIdle: Send + Sync {
    /// The time since the last input, or `None` when the system does not
    /// say.
    fn idle_for(&self) -> Option<Duration>;
}

/// Whether the speed limit is lifted because nobody is using the computer
/// (`Settings::speed_limit_only_while_active`).
pub trait LimitLift: Send + Sync {
    fn lifted(&self) -> bool;

    /// Lift the speed limit, or apply it again, and tell every window.
    fn set_lifted(&self, lifted: bool);
}

/// The sound and notification for a finished or failed download.
pub trait Notifications: Send + Sync {
    /// Play the sound `settings` choose for download `id` finishing (or
    /// with `failed`, failing), and show a notification when they ask.
    fn notify(&self, id: DownloadId, failed: bool, settings: &Settings);
}

/// Addresses retried through the browser's proxy after failing through
/// UDM's own proxy setting.
pub trait ProxyFallback: Send + Sync {
    /// Note that `url` is retried through the browser's proxy. False when
    /// it already was: each address gets one such retry.
    fn claim_retry(&self, url: Link) -> bool;
}

/// The downloads a crawl's finds become.
pub trait GrabDownloads: Send + Sync {
    /// The download for `file`, which a crawl for `req` found, saved where
    /// the request and `settings` say, carrying `login` (the login page
    /// site's cookies) when it is on that site. `None` when the mirror
    /// skips files already there and this one is.
    fn download_for(
        &self,
        req: &GrabRequest,
        file: &crate::grab::GrabFile,
        settings: &Settings,
        login: Option<&str>,
    ) -> Option<Download>;
}

/// The computer's local date and time.
pub trait Clock: Send + Sync {
    fn now(&self) -> Moment;
}

/// The time as seconds since 1970, for what is kept across restarts (the
/// weekly update check, the download quota's minutes).
pub trait Timestamps: Send + Sync {
    fn unix_now(&self) -> u64;
}

/// Where downloads are saved.
pub trait SaveFolders: Send + Sync {
    /// The system's Downloads folder, which the settings' folders start
    /// from.
    fn downloads_dir(&self) -> PathBuf;
}

/// The virus scanner the settings name.
pub trait VirusScanner: Send + Sync {
    /// Scan the file at `path` with `settings`' scanner, waiting for it.
    fn scan(&self, settings: &Settings, path: &Path) -> Scan;
}

/// Why a file's digest could not be made: it could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct HashError(pub String);

/// Hashing files, to compare them with a published checksum.
pub trait FileHashes: Send + Sync {
    /// The digest of the file at `path`, made with `algorithm`. It reads
    /// the whole file, so callers run it off the window's thread.
    fn digest(&self, path: &Path, algorithm: Algorithm) -> Result<Vec<u8>, HashError>;
}

/// Opening files as the desktop would on a double-click.
pub trait FileOpener: Send + Sync {
    /// Open `file`, a program or a document. Failures are logged.
    fn open(&self, file: &str);

    /// Show `path` selected in its folder, in the file manager. Failures
    /// are logged.
    fn reveal(&self, path: &str);

    /// Whether this system has an Open with picker for [`FileOpener::open_with`].
    fn can_open_with(&self) -> bool {
        false
    }

    /// Show the system's Open with picker for `file`, where it has one.
    /// It returns once the picker closes.
    fn open_with(&self, _file: &str) {}
}

/// The problem reports UDM keeps in its data folder (see `report`),
/// for the window to offer and to write when it sees a freeze. Nothing
/// here sends anything anywhere.
pub trait ProblemReports: Send + Sync {
    /// This copy's version, the system and how it was installed. It looks
    /// at the disk, so callers run it off the window's thread.
    fn this_copy(&self) -> crate::report::ThisCopy;

    /// The name of `program`'s log and its last [`crate::report::LOG_LINES`]
    /// lines, scrubbed of secrets.
    fn log_tail(&self, program: crate::report::Program) -> (String, Vec<String>);

    /// Write `report` to the reports folder, scrubbing its texts first.
    fn save(&self, report: &crate::report::Report) -> Result<crate::report::SavedReport, FilesError>;

    /// Delete a report the user did not want kept.
    fn delete(&self, report: &crate::report::SavedReport) -> Result<(), FilesError>;

    /// The reports not yet offered to the user, oldest first.
    fn unoffered(&self) -> Vec<crate::report::SavedReport>;

    /// Remember that these reports were offered, so they are not again.
    fn mark_offered(&self, reports: &[crate::report::SavedReport]);
}

/// Why the paired devices could not be read or saved, or no random bytes
/// could be had, in the adapter's words.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct DevicesError(pub String);

/// The devices paired with the remote-control page, kept between runs,
/// sealed for the user as the saved requests are.
pub trait PairedDevices: Send + Sync {
    /// The devices kept; none when nothing was saved yet.
    fn load(&self) -> Result<Vec<crate::pairing::Device>, DevicesError>;

    /// Keep `devices` in place of those kept before.
    fn save(&self, devices: &[crate::pairing::Device]) -> Result<(), DevicesError>;
}

/// What pairing needs from the system: random bytes and a digest.
pub trait PairingKeys: Send + Sync {
    /// Fresh bytes from the operating system's random source.
    fn random(&self) -> Result<[u8; crate::pairing::RANDOM_BYTES], DevicesError>;

    /// The SHA-256 digest of `secret`, which is all the daemon keeps of it.
    fn verifier(&self, secret: &crate::pairing::DeviceSecret) -> crate::pairing::Verifier;
}

/// The remote-control page's listener, as far as the window needs it.
pub trait RemoteListener: Send + Sync {
    /// Whether and where it listens, or why not.
    fn state(&self) -> crate::pairing::ListenerState;

    /// The page's address carrying `code`, for the QR code, while it
    /// listens.
    fn link(&self, code: &crate::pairing::PairingCode) -> Option<String>;
}

/// A debrid service's API, reached with an account's key: which file hosts
/// it supports, a host's link unlocked into a direct one, and the account.
pub trait Debrid: Send + Sync {
    /// The file hosts `service` supports for `key`'s account.
    fn hosts(
        &self,
        service: crate::debrid::DebridService,
        key: &crate::debrid::DebridKey,
    ) -> Result<crate::debrid::SupportedHosts, crate::debrid::DebridError>;

    /// `link` unlocked by `service` into a direct link.
    fn unrestrict(
        &self,
        service: crate::debrid::DebridService,
        key: &crate::debrid::DebridKey,
        link: &Address,
    ) -> Result<crate::debrid::Unrestricted, crate::debrid::DebridError>;

    /// What `service` says of `key`'s account.
    fn account(
        &self,
        service: crate::debrid::DebridService,
        key: &crate::debrid::DebridKey,
    ) -> Result<crate::debrid::DebridAccount, crate::debrid::DebridError>;
}

/// Where the debrid account's API key is kept: sealed, beside the settings,
/// together with the service it is for, so it only ever goes to that one.
pub trait DebridKeys: Send + Sync {
    /// The service a key is stored for, if any.
    fn stored(&self) -> Option<crate::debrid::DebridService>;

    /// The key stored for `service`; `None` when none is, or the stored key
    /// is another service's.
    fn key(&self, service: crate::debrid::DebridService) -> Option<crate::debrid::DebridKey>;

    /// Store `key` for `service`, in place of any stored key. A key that
    /// cannot be stored fails as `SettingsError::Refused`, with why.
    fn set_key(
        &self,
        service: crate::debrid::DebridService,
        key: &crate::debrid::DebridKey,
    ) -> Result<(), SettingsError>;

    /// Remove the stored key; none stored is not an error.
    fn remove_key(&self) -> Result<(), SettingsError>;
}
