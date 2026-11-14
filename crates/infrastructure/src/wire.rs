//! Mapping between the wire messages (`protocol::Incoming` and `Outgoing`)
//! and the application's `Command` and `Event`. Both sides of a
//! connection use it: the window's `daemon_client` sends commands and reads
//! events, and the daemon's `server` reads commands and sends events.

use serde::{Deserialize, Serialize};
use udm_application::daemon::service::ServiceError;
use udm_application::daemon::{Command, Event, Failure};
use udm_application::downloads::{ConfirmError, StartOrWaitError};
use udm_domain::debrid::DebridKey;
use udm_domain::download::{Download, DownloadError, Mismatch};
use udm_domain::grab::InvalidGrab;
use udm_domain::pairing::PairingError;
use udm_domain::ports::{ControlError, SettingsError};
use udm_domain::queue::QueueError;
use udm_domain::update::UpdateError;

use crate::protocol::{Incoming, NewPackage, Outgoing, Secret, WireKey};
use udm_domain::id::PackageId;
use udm_domain::package::{Packaging, Password};

/// The wire message for `command`.
pub fn incoming(command: Command) -> Incoming {
    match command {
        Command::Hello { version, client } => Incoming::Hello { version, client },
        Command::Ping => Incoming::Ping,
        Command::CommandLine(c) => Incoming::CommandLine(c),
        Command::ProbeMedia { request, download } => Incoming::ProbeMedia { request, download },
        Command::PickLinks { page, links, user_agent } => Incoming::PickLinks { page, links, user_agent },
        Command::PickVideos { page, playlist, user_agent } => Incoming::PickVideos { page, playlist, user_agent },
        Command::PickGallery { page, user_agent } => Incoming::PickGallery { page, user_agent },
        Command::ReadGallery { request, page, user_agent } => Incoming::ReadGallery { request, page, user_agent },
        Command::InstallTool { tool } => Incoming::InstallTool { tool },
        Command::ListTools => Incoming::ListTools,
        Command::Rendered { request, html, url, popups } => Incoming::Rendered { request, html, url, popups },
        Command::Cookies { request, cookies } => Incoming::Cookies { request, cookies },
        Command::Add(d) => Incoming::Add(d),
        Command::AddTorrentFile { metainfo, download } => {
            use base64::Engine as _;
            let file = base64::engine::general_purpose::STANDARD.encode(metainfo);
            Incoming::Add(Download { torrent_file: Some(file), ..download })
        }
        Command::SelectTorrentFiles { id, files } => Incoming::SelectTorrentFiles { id, files },
        Command::SetSeedLimits { id, limits } => Incoming::SetSeedLimits { id, limits },
        Command::StopSeeding { id } => Incoming::StopSeeding { id },
        Command::TorrentFiles { id } => Incoming::TorrentFiles { id },
        Command::AddMany { items, queue, packaging } => {
            let (package, group_archives) = match packaging {
                Packaging::None => (None, false),
                Packaging::Archives => (None, true),
                Packaging::One { name, folder } => (Some(NewPackage { name, folder }), false),
            };
            Incoming::AddMany { items, queue, package, group_archives }
        }
        Command::UnpackPackage { id, password } => {
            Incoming::UnpackPackage { id, password: password.map(|p| Secret(p.expose().to_string())) }
        }
        Command::UngroupPackage { id } => Incoming::UngroupPackage { id },
        Command::Grab(r) => Incoming::Grab(r),
        Command::CancelGrab => Incoming::CancelGrab,
        Command::ProbeLinks { request, links } => Incoming::ProbeLinks { request, links },
        Command::StopProbeLinks => Incoming::StopProbeLinks,
        Command::Cancel { id } => Incoming::Cancel { id },
        Command::Retry { id } => Incoming::Retry { id },
        Command::Redownload { id } => Incoming::Redownload { id },
        Command::Remove { id, delete_files } => Incoming::Remove { id, delete_files },
        Command::List => Incoming::List,
        Command::PauseAll => Incoming::PauseAll,
        Command::ResumeAll => Incoming::ResumeAll,
        Command::Confirm(a) => Incoming::Confirm(a),
        Command::SaveQueue { queue } => Incoming::SaveQueue { queue },
        Command::DeleteQueue { id } => Incoming::DeleteQueue { id },
        Command::StartQueue { id } => Incoming::StartQueue { id },
        Command::StopQueue { id } => Incoming::StopQueue { id },
        Command::SetQueueOnce { id, when_done } => Incoming::SetQueueOnce { id, when_done },
        Command::MoveToQueue { id, queue, position } => Incoming::MoveToQueue { id, queue, position },
        Command::RemoveFromQueue { id } => Incoming::RemoveFromQueue { id },
        Command::CancelCountdown => Incoming::CancelCountdown,
        Command::RefreshLink { id, download, restart } => Incoming::RefreshLink { id, download, restart },
        Command::CancelRefresh => Incoming::CancelRefresh,
        Command::CheckUpdate => Incoming::CheckUpdate,
        Command::InstallUpdate => Incoming::InstallUpdate,
        Command::Convert { id, to, delete_original } => Incoming::Convert { id, to, delete_original },
        Command::PreviewZip { id } => Incoming::PreviewZip { id },
        Command::SetProperties { id, changes } => Incoming::SetProperties { id, changes },
        Command::SetSpeedLimit { id, bytes_per_sec, remember } => {
            Incoming::SetSpeedLimit { id, bytes_per_sec, remember }
        }
        Command::CountdownNow => Incoming::CountdownNow,
        Command::SetOnDone { id, on_done } => Incoming::SetOnDone { id, on_done },
        Command::GetSettings => Incoming::GetSettings,
        Command::SetSettings { settings } => Incoming::SetSettings { settings },
        Command::RemoteStatus => Incoming::RemoteStatus,
        Command::StartPairing => Incoming::StartPairing,
        Command::CancelPairing => Incoming::CancelPairing,
        Command::RemoveDevice { id } => Incoming::RemoveDevice { id },
        Command::CheckDebrid { service, key } => {
            Incoming::CheckDebrid { service, key: key.map(|k| Secret(k.expose().to_string())) }
        }
        Command::SetDebridKey { service, key } => Incoming::SetDebridKey { service, key: WireKey(key) },
        Command::RemoveDebridKey => Incoming::RemoveDebridKey,
    }
}

/// The command wire message `msg` carries.
pub fn command(msg: Incoming) -> Command {
    match msg {
        Incoming::Hello { version, client } => Command::Hello { version, client },
        Incoming::Ping => Command::Ping,
        Incoming::CommandLine(c) => Command::CommandLine(c),
        Incoming::ProbeMedia { request, download } => Command::ProbeMedia { request, download },
        Incoming::PickLinks { page, links, user_agent } => Command::PickLinks { page, links, user_agent },
        Incoming::PickVideos { page, playlist, user_agent } => Command::PickVideos { page, playlist, user_agent },
        Incoming::PickGallery { page, user_agent } => Command::PickGallery { page, user_agent },
        Incoming::ReadGallery { request, page, user_agent } => Command::ReadGallery { request, page, user_agent },
        Incoming::InstallTool { tool } => Command::InstallTool { tool },
        Incoming::ListTools => Command::ListTools,
        Incoming::Rendered { request, html, url, popups } => Command::Rendered { request, html, url, popups },
        Incoming::Cookies { request, cookies } => Command::Cookies { request, cookies },
        Incoming::Add(mut d) => match d.torrent_file.take() {
            Some(file) => {
                use base64::Engine as _;
                // A file that is not base64 is no torrent: adding it says so.
                let metainfo = base64::engine::general_purpose::STANDARD.decode(file.trim()).unwrap_or_default();
                Command::AddTorrentFile { metainfo, download: d }
            }
            None => Command::Add(d),
        },
        Incoming::SelectTorrentFiles { id, files } => Command::SelectTorrentFiles { id, files },
        Incoming::SetSeedLimits { id, limits } => Command::SetSeedLimits { id, limits },
        Incoming::StopSeeding { id } => Command::StopSeeding { id },
        Incoming::TorrentFiles { id } => Command::TorrentFiles { id },
        Incoming::AddMany { items, queue, package, group_archives } => {
            let packaging = match package {
                Some(NewPackage { name, folder }) => Packaging::One { name, folder },
                None if group_archives => Packaging::Archives,
                None => Packaging::None,
            };
            Command::AddMany { items, queue, packaging }
        }
        Incoming::UnpackPackage { id, password } => {
            Command::UnpackPackage { id, password: password.and_then(|Secret(p)| Password::new(p)) }
        }
        Incoming::UngroupPackage { id } => Command::UngroupPackage { id },
        Incoming::Grab(r) => Command::Grab(r),
        Incoming::CancelGrab => Command::CancelGrab,
        Incoming::ProbeLinks { request, links } => Command::ProbeLinks { request, links },
        Incoming::StopProbeLinks => Command::StopProbeLinks,
        Incoming::Cancel { id } => Command::Cancel { id },
        Incoming::Retry { id } => Command::Retry { id },
        Incoming::Redownload { id } => Command::Redownload { id },
        Incoming::Remove { id, delete_files } => Command::Remove { id, delete_files },
        Incoming::List => Command::List,
        Incoming::PauseAll => Command::PauseAll,
        Incoming::ResumeAll => Command::ResumeAll,
        Incoming::Confirm(a) => Command::Confirm(a),
        Incoming::SaveQueue { queue } => Command::SaveQueue { queue },
        Incoming::DeleteQueue { id } => Command::DeleteQueue { id },
        Incoming::StartQueue { id } => Command::StartQueue { id },
        Incoming::StopQueue { id } => Command::StopQueue { id },
        Incoming::SetQueueOnce { id, when_done } => Command::SetQueueOnce { id, when_done },
        Incoming::MoveToQueue { id, queue, position } => Command::MoveToQueue { id, queue, position },
        Incoming::RemoveFromQueue { id } => Command::RemoveFromQueue { id },
        Incoming::CancelCountdown => Command::CancelCountdown,
        Incoming::RefreshLink { id, download, restart } => Command::RefreshLink { id, download, restart },
        Incoming::CancelRefresh => Command::CancelRefresh,
        Incoming::CheckUpdate => Command::CheckUpdate,
        Incoming::InstallUpdate => Command::InstallUpdate,
        Incoming::Convert { id, to, delete_original } => Command::Convert { id, to, delete_original },
        Incoming::PreviewZip { id } => Command::PreviewZip { id },
        Incoming::SetProperties { id, changes } => Command::SetProperties { id, changes },
        Incoming::SetSpeedLimit { id, bytes_per_sec, remember } => {
            Command::SetSpeedLimit { id, bytes_per_sec, remember }
        }
        Incoming::CountdownNow => Command::CountdownNow,
        Incoming::SetOnDone { id, on_done } => Command::SetOnDone { id, on_done },
        Incoming::GetSettings => Command::GetSettings,
        Incoming::SetSettings { settings } => Command::SetSettings { settings },
        Incoming::RemoteStatus => Command::RemoteStatus,
        Incoming::StartPairing => Command::StartPairing,
        Incoming::CancelPairing => Command::CancelPairing,
        Incoming::RemoveDevice { id } => Command::RemoveDevice { id },
        Incoming::CheckDebrid { service, key } => {
            Command::CheckDebrid { service, key: key.and_then(|Secret(k)| DebridKey::new(&k)) }
        }
        Incoming::SetDebridKey { service, key: WireKey(key) } => Command::SetDebridKey { service, key },
        Incoming::RemoveDebridKey => Command::RemoveDebridKey,
    }
}

/// The event wire message `msg` carries.
pub fn event(msg: Outgoing) -> Event {
    match msg {
        Outgoing::Hello { name, version, api } => Event::Hello { name, version, api },
        Outgoing::Pong => Event::Pong,
        Outgoing::Ack { id, url, status } => Event::Ack { id, url, status },
        Outgoing::MediaInfo { request, info } => Event::MediaInfo { request, info },
        Outgoing::CookiesWanted { request, url } => Event::CookiesWanted { request, url },
        Outgoing::RenderPage { request, url } => Event::RenderPage { request, url },
        Outgoing::CommandLine { window_open } => Event::CommandLine { window_open },
        Outgoing::Added { id, url } => Event::Added { id, url },
        Outgoing::Started { id, url, path, total, resumable, connections, partial, validator, country } => {
            Event::Started { id, url, path, total, resumable, connections, partial, validator, country }
        }
        Outgoing::Progress { id, url, done, total, parts, segments } => {
            Event::Progress { id, url, done, total, parts, segments }
        }
        Outgoing::Done { id, url, path, note } => Event::Done { id, url, path, note },
        Outgoing::Failed { id, url, error } => Event::Failed { id, url, error },
        Outgoing::Canceled { id, url } => Event::Canceled { id, url },
        Outgoing::Removed { id } => Event::Removed { id },
        Outgoing::TorrentInfo { id, url, info } => Event::TorrentInfo { id, url, info },
        Outgoing::TorrentStats { id, url, stats } => Event::TorrentStats { id, url, stats },
        Outgoing::Updated { item } => Event::Updated { item },
        Outgoing::Downloads { items } => Event::Downloads { items },
        Outgoing::Settings { settings } => Event::Settings { settings },
        Outgoing::Show => Event::Show,
        Outgoing::ShowDone { id } => Event::ShowDone { id },
        Outgoing::Queues { items } => Event::Queues { items },
        Outgoing::Packages { items } => Event::Packages { items },
        Outgoing::Countdown { action, seconds, queue, file } => Event::Countdown { action, seconds, queue, file },
        Outgoing::CountdownCanceled => Event::CountdownCanceled,
        Outgoing::Quit => Event::Quit,
        Outgoing::RefreshWaiting { id } => Event::RefreshWaiting { id },
        Outgoing::NameTaken { id, url, path, total } => Event::NameTaken { id, url, path, total },
        Outgoing::RefreshMismatch { id, url, reason, error_kind } => {
            Event::RefreshMismatch { id, url, reason: failure(reason, error_kind) }
        }
        Outgoing::AddedMany { count, skipped } => Event::AddedMany { count, skipped },
        Outgoing::LinkInfo { request, index, total, content_type, error, error_kind } => {
            Event::LinkInfo { request, index, total, content_type, error: error.map(|e| failure(e, error_kind)) }
        }
        Outgoing::PickLinks { page, links, user_agent } => Event::PickLinks { page, links, user_agent },
        Outgoing::PickVideos { page, playlist, user_agent } => Event::PickVideos { page, playlist, user_agent },
        Outgoing::PickGallery { page, user_agent } => Event::PickGallery { page, user_agent },
        Outgoing::Gallery { request, page, gallery, error, error_kind } => {
            Event::Gallery { request, page, gallery, error: error.map(|e| failure(e, error_kind)) }
        }
        Outgoing::Tools { tools, done, error, error_kind } => {
            Event::Tools { tools, done, error: error.map(|e| failure(e, error_kind)) }
        }
        Outgoing::Quota { used, limit, paused, resumes_at } => Event::Quota { used, limit, paused, resumes_at },
        Outgoing::LimitLifted { lifted } => Event::LimitLifted { lifted },
        Outgoing::BrowserLink { connected, last_seen } => Event::BrowserLink { connected, last_seen },
        Outgoing::Update { newer, error, error_kind } => {
            Event::Update { newer, error: error.map(|e| failure(e, error_kind)) }
        }
        Outgoing::UpdateFailed { error, error_kind } => Event::UpdateFailed { error: failure(error, error_kind) },
        Outgoing::ZipPreview { id, entries, error, error_kind } => {
            Event::ZipPreview { id, entries, error: error.map(|e| failure(e, error_kind)) }
        }
        Outgoing::GrabProgress { pages, bytes, found, explored, added } => {
            Event::GrabProgress { pages, bytes, found, explored, added }
        }
        Outgoing::GrabDone { files, pages, explored, stopped, login_cookies } => {
            Event::GrabDone { files, pages, explored, stopped, login_cookies }
        }
        Outgoing::Error { message, error_kind } => Event::Error { error: failure(message, error_kind) },
        Outgoing::Rejected { url, message, error_kind } => Event::Rejected { url, error: failure(message, error_kind) },
        Outgoing::Remote { state } => Event::Remote { state },
        Outgoing::Debrid { check } => Event::Debrid { check },
        Outgoing::DebridRefused { refused } => Event::DebridRefused { refused },
    }
}

/// The wire message for `event`. `None` for `Connected` and `Lost`, which
/// a client's own connection reports and nothing sends.
pub fn outgoing(event: Event) -> Option<Outgoing> {
    Some(match event {
        Event::Connected | Event::Lost(_) => return None,
        Event::Hello { name, version, api } => Outgoing::Hello { name, version, api },
        Event::Pong => Outgoing::Pong,
        Event::Ack { id, url, status } => Outgoing::Ack { id, url, status },
        Event::MediaInfo { request, info } => Outgoing::MediaInfo { request, info },
        Event::CookiesWanted { request, url } => Outgoing::CookiesWanted { request, url },
        Event::RenderPage { request, url } => Outgoing::RenderPage { request, url },
        Event::CommandLine { window_open } => Outgoing::CommandLine { window_open },
        Event::Added { id, url } => Outgoing::Added { id, url },
        Event::Started { id, url, path, total, resumable, connections, partial, validator, country } => {
            Outgoing::Started { id, url, path, total, resumable, connections, partial, validator, country }
        }
        Event::Progress { id, url, done, total, parts, segments } => {
            Outgoing::Progress { id, url, done, total, parts, segments }
        }
        Event::Done { id, url, path, note } => Outgoing::Done { id, url, path, note },
        Event::Failed { id, url, error } => Outgoing::Failed { id, url, error },
        Event::Canceled { id, url } => Outgoing::Canceled { id, url },
        Event::Removed { id } => Outgoing::Removed { id },
        Event::TorrentInfo { id, url, info } => Outgoing::TorrentInfo { id, url, info },
        Event::TorrentStats { id, url, stats } => Outgoing::TorrentStats { id, url, stats },
        Event::Updated { item } => Outgoing::Updated { item },
        Event::Downloads { items } => Outgoing::Downloads { items },
        Event::Settings { settings } => Outgoing::Settings { settings },
        Event::Show => Outgoing::Show,
        Event::ShowDone { id } => Outgoing::ShowDone { id },
        Event::Queues { items } => Outgoing::Queues { items },
        Event::Packages { items } => Outgoing::Packages { items },
        Event::Countdown { action, seconds, queue, file } => Outgoing::Countdown { action, seconds, queue, file },
        Event::CountdownCanceled => Outgoing::CountdownCanceled,
        Event::Quit => Outgoing::Quit,
        Event::RefreshWaiting { id } => Outgoing::RefreshWaiting { id },
        Event::NameTaken { id, url, path, total } => Outgoing::NameTaken { id, url, path, total },
        Event::RefreshMismatch { id, url, reason } => {
            let (reason, error_kind) = text_and_kind(reason);
            Outgoing::RefreshMismatch { id, url, reason, error_kind }
        }
        Event::AddedMany { count, skipped } => Outgoing::AddedMany { count, skipped },
        Event::LinkInfo { request, index, total, content_type, error } => {
            let (error, error_kind) = optional(error);
            Outgoing::LinkInfo { request, index, total, content_type, error, error_kind }
        }
        Event::PickLinks { page, links, user_agent } => Outgoing::PickLinks { page, links, user_agent },
        Event::PickVideos { page, playlist, user_agent } => Outgoing::PickVideos { page, playlist, user_agent },
        Event::PickGallery { page, user_agent } => Outgoing::PickGallery { page, user_agent },
        Event::Gallery { request, page, gallery, error } => {
            let (error, error_kind) = optional(error);
            Outgoing::Gallery { request, page, gallery, error, error_kind }
        }
        Event::Tools { tools, done, error } => {
            let (error, error_kind) = optional(error);
            Outgoing::Tools { tools, done, error, error_kind }
        }
        Event::Quota { used, limit, paused, resumes_at } => Outgoing::Quota { used, limit, paused, resumes_at },
        Event::LimitLifted { lifted } => Outgoing::LimitLifted { lifted },
        Event::BrowserLink { connected, last_seen } => Outgoing::BrowserLink { connected, last_seen },
        Event::Update { newer, error } => {
            let (error, error_kind) = optional(error);
            Outgoing::Update { newer, error, error_kind }
        }
        Event::UpdateFailed { error } => {
            let (error, error_kind) = text_and_kind(error);
            Outgoing::UpdateFailed { error, error_kind }
        }
        Event::ZipPreview { id, entries, error } => {
            let (error, error_kind) = optional(error);
            Outgoing::ZipPreview { id, entries, error, error_kind }
        }
        Event::GrabProgress { pages, bytes, found, explored, added } => {
            Outgoing::GrabProgress { pages, bytes, found, explored, added }
        }
        Event::GrabDone { files, pages, explored, stopped, login_cookies } => {
            Outgoing::GrabDone { files, pages, explored, stopped, login_cookies }
        }
        Event::Error { error } => {
            let (message, error_kind) = text_and_kind(error);
            Outgoing::Error { message, error_kind }
        }
        Event::Rejected { url, error } => {
            let (message, error_kind) = text_and_kind(error);
            Outgoing::Rejected { url, message, error_kind }
        }
        Event::Remote { state } => Outgoing::Remote { state },
        Event::Debrid { check } => Outgoing::Debrid { check },
        Event::DebridRefused { refused } => Outgoing::DebridRefused { refused },
    })
}

/// A failure as the wire carries it: its English text, which clients that
/// do not translate show (the browser extension, `udm-host send`), and its
/// kind and values as `error_kind`. A failure known only by its text has no
/// kind.
pub fn text_and_kind(failure: Failure) -> (String, Option<serde_json::Value>) {
    let text = failure.to_string();
    let kind = WireFailure::of(failure).and_then(|w| serde_json::to_value(w).ok());
    (text, kind)
}

fn optional(failure: Option<Failure>) -> (Option<String>, Option<serde_json::Value>) {
    match failure.map(text_and_kind) {
        Some((text, kind)) => (Some(text), kind),
        None => (None, None),
    }
}

/// The failure that `text` and `kind` describe. Without a kind this
/// client can read (a daemon from before kinds, or a kind newer than this
/// client), it is the text alone; an older daemon's "invalid message: ..."
/// is still known for what it is.
pub fn failure(text: String, kind: Option<serde_json::Value>) -> Failure {
    if let Some(w) = kind.and_then(|k| serde_json::from_value::<WireFailure>(k).ok()) {
        return w.into();
    }
    match text.strip_prefix(INVALID_MESSAGE) {
        Some(why) => Failure::InvalidMessage(why.to_string()),
        None => Failure::Other(text),
    }
}

/// How `Failure::InvalidMessage` reads, which daemons sent as text alone.
const INVALID_MESSAGE: &str = "invalid message: ";

/// `Failure`'s shape on the wire: the application's enums, which do not know
/// the wire, mirrored here around the domain's, which serialize themselves.
/// These variant names, and the domain enums', are the wire format: renaming
/// one changes what clients read (the golden tests below pin them).
#[derive(Serialize, Deserialize)]
enum WireFailure {
    Service(WireService),
    Settings(SettingsError),
    Mismatch(Mismatch),
    InvalidMessage(String),
    Probe(String),
}

#[derive(Serialize, Deserialize)]
enum WireService {
    Queue(QueueError),
    Download(DownloadError),
    Confirm(WireConfirm),
    Update(UpdateError),
    Grab(InvalidGrab),
    CrawlRunning,
    BadBody(String),
    NotDownloadAddress(String),
    Tool(udm_domain::tools::ToolError),
    Gallery(udm_domain::gallery::GalleryError),
    Pairing(PairingError),
    NoPackage(PackageId),
    PackageBusy,
}

#[derive(Serialize, Deserialize)]
enum WireConfirm {
    Download(DownloadError),
    StartControl(ControlError),
    StartQueue(QueueError),
}

impl WireFailure {
    /// `None` for `Failure::Other`, which is its text alone.
    fn of(f: Failure) -> Option<WireFailure> {
        Some(match f {
            Failure::Service(e) => WireFailure::Service(WireService::of(e)),
            Failure::Settings(e) => WireFailure::Settings(e),
            Failure::Mismatch(e) => WireFailure::Mismatch(e),
            Failure::InvalidMessage(why) => WireFailure::InvalidMessage(why),
            Failure::Probe(why) => WireFailure::Probe(why),
            Failure::Other(_) => return None,
        })
    }
}

impl From<WireFailure> for Failure {
    fn from(w: WireFailure) -> Self {
        match w {
            WireFailure::Service(e) => Failure::Service(e.into()),
            WireFailure::Settings(e) => Failure::Settings(e),
            WireFailure::Mismatch(e) => Failure::Mismatch(e),
            WireFailure::InvalidMessage(why) => Failure::InvalidMessage(why),
            WireFailure::Probe(why) => Failure::Probe(why),
        }
    }
}

impl WireService {
    fn of(e: ServiceError) -> WireService {
        match e {
            ServiceError::Queue(e) => WireService::Queue(e),
            ServiceError::Download(e) => WireService::Download(e),
            ServiceError::Confirm(ConfirmError::Download(e)) => WireService::Confirm(WireConfirm::Download(e)),
            ServiceError::Confirm(ConfirmError::Start(StartOrWaitError::Start(e))) => {
                WireService::Confirm(WireConfirm::StartControl(e))
            }
            ServiceError::Confirm(ConfirmError::Start(StartOrWaitError::Queue(e))) => {
                WireService::Confirm(WireConfirm::StartQueue(e))
            }
            ServiceError::Update(e) => WireService::Update(e),
            ServiceError::Grab(e) => WireService::Grab(e),
            ServiceError::CrawlRunning => WireService::CrawlRunning,
            ServiceError::BadBody(url) => WireService::BadBody(url),
            ServiceError::NotDownloadAddress(url) => WireService::NotDownloadAddress(url),
            ServiceError::Tool(e) => WireService::Tool(e),
            ServiceError::Gallery(e) => WireService::Gallery(e),
            ServiceError::Pairing(e) => WireService::Pairing(e),
            ServiceError::NoPackage(id) => WireService::NoPackage(id),
            ServiceError::PackageBusy => WireService::PackageBusy,
        }
    }
}

impl From<WireService> for ServiceError {
    fn from(w: WireService) -> Self {
        match w {
            WireService::Queue(e) => ServiceError::Queue(e),
            WireService::Download(e) => ServiceError::Download(e),
            WireService::Confirm(WireConfirm::Download(e)) => ServiceError::Confirm(ConfirmError::Download(e)),
            WireService::Confirm(WireConfirm::StartControl(e)) => {
                ServiceError::Confirm(ConfirmError::Start(StartOrWaitError::Start(e)))
            }
            WireService::Confirm(WireConfirm::StartQueue(e)) => {
                ServiceError::Confirm(ConfirmError::Start(StartOrWaitError::Queue(e)))
            }
            WireService::Update(e) => ServiceError::Update(e),
            WireService::Grab(e) => ServiceError::Grab(e),
            WireService::CrawlRunning => ServiceError::CrawlRunning,
            WireService::BadBody(url) => ServiceError::BadBody(url),
            WireService::NotDownloadAddress(url) => ServiceError::NotDownloadAddress(url),
            WireService::Tool(e) => ServiceError::Tool(e),
            WireService::Gallery(e) => ServiceError::Gallery(e),
            WireService::Pairing(e) => ServiceError::Pairing(e),
            WireService::NoPackage(id) => ServiceError::NoPackage(id),
            WireService::PackageBusy => ServiceError::PackageBusy,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use udm_domain::bytes::ByteCount;
    use udm_domain::download::Download;
    use udm_domain::id::{DownloadId, QueueId};

    /// `msg` as JSON, after a trip through `f` and back.
    fn json<T: serde::Serialize>(msg: &T) -> serde_json::Value {
        serde_json::to_value(msg).unwrap()
    }

    #[test]
    fn commands_keep_their_fields_on_the_wire() {
        let d = Download { url: "http://x/f".into(), ..Download::default() };
        let msg =
            incoming(Command::AddMany { items: vec![d], queue: Some(QueueId::new(3)), packaging: Packaging::None });
        let value = json(&msg);
        assert_eq!(value["op"], "add_many");
        assert_eq!(value["queue"], 3);
        assert_eq!(value["items"][0]["url"], "http://x/f");
        assert!(value.get("package").is_none() && value.get("group_archives").is_none(), "{value}");
        assert_eq!(json(&incoming(command(msg))), value, "back and forth is the same message");
        let msg = incoming(Command::Remove { id: DownloadId::new(2), delete_files: true });
        assert_eq!(json(&msg), serde_json::json!({"op": "remove", "id": 2, "delete_files": true}));
    }

    #[test]
    fn packages_go_as_their_ops_and_the_password_stays_out_of_logs() {
        use udm_domain::package::{Package, PackageName, Unpack, UnpackFailure};
        let name = PackageName::parse("Trip").unwrap();
        let one = Packaging::One { name: name.clone(), folder: Some("/dl/Trip".into()) };
        for (packaging, extra) in [
            (one, serde_json::json!({"package": {"name": "Trip", "folder": "/dl/Trip"}})),
            (Packaging::Archives, serde_json::json!({"group_archives": true})),
        ] {
            let msg = incoming(Command::AddMany { items: Vec::new(), queue: None, packaging: packaging.clone() });
            let mut want = serde_json::json!({"op": "add_many", "items": [], "queue": null});
            want.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            assert_eq!(json(&msg), want);
            assert!(matches!(command(msg), Command::AddMany { packaging: p, .. } if p == packaging));
        }
        let old: Incoming = serde_json::from_str(r#"{"op":"add_many","items":[]}"#).unwrap();
        assert!(matches!(command(old), Command::AddMany { packaging: Packaging::None, .. }), "an older client's");
        let bad = serde_json::from_str::<Incoming>(r#"{"op":"add_many","items":[],"package":{"name":" "}}"#);
        assert!(bad.is_err(), "a package needs a name");

        let unpack = incoming(Command::UnpackPackage { id: PackageId::new(4), password: Password::new("pw".into()) });
        assert_eq!(json(&unpack), serde_json::json!({"op": "unpack_package", "id": 4, "password": "pw"}));
        assert!(!format!("{unpack:?}").contains("pw"), "{unpack:?}");
        assert!(matches!(command(unpack), Command::UnpackPackage { password: Some(p), .. } if p.expose() == "pw"));
        let raw: Incoming = serde_json::from_str(r#"{"op":"unpack_package","id":4,"password":""}"#).unwrap();
        assert!(matches!(command(raw), Command::UnpackPackage { password: None, .. }), "an empty password is none");
        let ungroup = incoming(Command::UngroupPackage { id: PackageId::new(4) });
        assert_eq!(json(&ungroup), serde_json::json!({"op": "ungroup_package", "id": 4}));

        let mut p = Package::new(PackageId::new(4), name, None, vec![DownloadId::new(7)]);
        p.unpack = Unpack::Failed { reason: UnpackFailure::NeedsSevenZip { archive: "a.rar".into() } };
        let (value, back) = round_trip(Event::Packages { items: vec![p.clone()] });
        assert_eq!(
            value,
            serde_json::json!({"op": "packages", "items": [{"id": 4, "name": "Trip", "downloads": [7],
                "unpack": {"state": "failed", "reason": {"kind": "needs_seven_zip", "archive": "a.rar"}}}]})
        );
        assert_eq!(back, Event::Packages { items: vec![p] });
    }

    #[test]
    fn every_request_the_host_sends_reads_as_a_command() {
        for raw in [
            r#"{"op":"hello","version":"1"}"#,
            r#"{"op":"ping"}"#,
            r#"{"op":"cookies","request":4,"cookies":"a=b"}"#,
            r#"{"op":"rendered","request":5}"#,
            r#"{"op":"pick_links","page":"https://p/","links":[]}"#,
        ] {
            let msg: Incoming = serde_json::from_str(raw).unwrap();
            let value = json(&msg);
            assert_eq!(json(&incoming(command(msg))), value, "{raw}");
        }
    }

    #[test]
    fn events_go_out_as_they_came_in() {
        for msg in [
            Outgoing::Pong,
            Outgoing::Ack { id: DownloadId::new(1), url: "u".into(), status: "accepted".into() },
            Outgoing::CommandLine { window_open: true },
            Outgoing::RenderPage { request: 2, url: "u".into() },
            Outgoing::Error { message: "no".into(), error_kind: None },
        ] {
            let value = json(&msg);
            assert_eq!(json(&outgoing(event(msg)).unwrap()), value);
        }
        assert!(outgoing(Event::Connected).is_none());
    }

    /// `event` as the JSON a client reads, and that JSON read back.
    fn round_trip(event: Event) -> (serde_json::Value, Event) {
        let value = json(&outgoing(event).unwrap());
        let back: Outgoing = serde_json::from_value(value.clone()).unwrap();
        (value, super::event(back))
    }

    #[test]
    fn a_large_torrent_fits_in_a_message() {
        use udm_domain::torrent::{TorrentFiles, TorrentInfo, TorrentStats, MAX_FILE_NAME_BYTES, MAX_TORRENT_FILES};
        // The most files, with names as long as the list allows, each named
        // with characters JSON escapes.
        let per = MAX_FILE_NAME_BYTES / MAX_TORRENT_FILES - 1;
        let raw = (0..MAX_TORRENT_FILES)
            .map(|i| (vec![format!("{i:05}{}", "\u{00e9}".repeat((per - 5) / 2))], u64::MAX / 2, false))
            .collect();
        let files = TorrentFiles::parse(raw).unwrap();
        let hash = udm_domain::torrent::InfoHash::from_bytes([1; 20]);
        let info = TorrentInfo {
            info_hash: hash,
            name: "Big".into(),
            files: files.clone(),
            piece_size: udm_domain::bytes::ByteCount::new(1 << 24),
            pieces: u32::MAX,
            private: false,
        };
        let id = DownloadId::new(1);
        let url: udm_domain::address::Link = format!("magnet:?xt=urn:btih:{hash}").into();
        let frame = serde_json::to_vec(&outgoing(Event::TorrentInfo { id, url: url.clone(), info }).unwrap()).unwrap();
        assert!(frame.len() < crate::app::MAX_MESSAGE_BYTES as usize, "{} bytes", frame.len());
        // Its row carries no file list, and its stats no file progress at
        // this size.
        let mut row: udm_domain::download::DownloadEntry = serde_json::from_value(serde_json::json!({
            "id": 1, "url": url, "status": "downloading", "connections": 1, "done": 0, "resumable": true,
        }))
        .unwrap();
        let mut torrent = udm_domain::torrent::TorrentEntry::new(hash);
        torrent.files = files;
        row.torrent = Some(torrent);
        let frame = serde_json::to_vec(&outgoing(Event::Updated { item: row }).unwrap()).unwrap();
        assert!(frame.len() < 1024, "{} bytes", frame.len());
        let stats = TorrentStats::default();
        let frame = serde_json::to_vec(&outgoing(Event::TorrentStats { id, url, stats }).unwrap()).unwrap();
        assert!(frame.len() < 1024);
    }

    #[test]
    fn torrent_messages_have_their_wire_shapes() {
        use udm_domain::torrent::{FileSelection, Ratio, SeedLimits, TorrentPhase, TorrentStats};
        // A `.torrent` file read from disk travels base64-encoded in `add`.
        let download = Download {
            url: "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a".into(),
            ..Download::default()
        };
        let add = incoming(Command::AddTorrentFile { metainfo: b"d4:infoe".to_vec(), download: download.clone() });
        let value = json(&add);
        assert_eq!((value["op"].as_str(), value["torrent_file"].as_str()), (Some("add"), Some("ZDQ6aW5mb2U=")));
        let Command::AddTorrentFile { metainfo, download: back } = command(add) else { panic!("a torrent file") };
        assert_eq!((metainfo.as_slice(), back.url, back.torrent_file), (&b"d4:infoe"[..], download.url.clone(), None));
        let garbled: Incoming =
            serde_json::from_value(serde_json::json!({"op": "add", "url": "x", "torrent_file": "!"})).unwrap();
        assert!(matches!(command(garbled), Command::AddTorrentFile { metainfo, .. } if metainfo.is_empty()));
        assert!(matches!(command(incoming(Command::Add(download))), Command::Add(_)), "a plain add stays one");

        let id = DownloadId::new(4);
        let select = incoming(Command::SelectTorrentFiles { id, files: FileSelection::parse(&[2, 0], 3).unwrap() });
        assert_eq!(json(&select), serde_json::json!({"op": "select_torrent_files", "id": 4, "files": [0, 2]}));
        let limits = SeedLimits { ratio: Some(Ratio::new(1.5).unwrap()), minutes: None };
        let seed = incoming(Command::SetSeedLimits { id, limits: Some(limits) });
        assert_eq!(json(&seed), serde_json::json!({"op": "set_seed_limits", "id": 4, "limits": {"ratio": 1.5}}));
        assert!(matches!(command(seed), Command::SetSeedLimits { limits: Some(l), .. } if l == limits));
        assert_eq!(json(&incoming(Command::StopSeeding { id })), serde_json::json!({"op": "stop_seeding", "id": 4}));
        assert_eq!(json(&incoming(Command::TorrentFiles { id })), serde_json::json!({"op": "torrent_files", "id": 4}));
        let empty = serde_json::json!({"op": "select_torrent_files", "id": 4, "files": []});
        assert!(serde_json::from_value::<Incoming>(empty).is_err(), "no file chosen is no message");

        let stats = TorrentStats { phase: TorrentPhase::Seeding, down_speed: 5, ..TorrentStats::default() };
        let (value, back) = round_trip(Event::TorrentStats { id, url: "magnet:?x".into(), stats: stats.clone() });
        assert_eq!((value["op"].as_str(), value["stats"]["phase"].as_str()), (Some("torrent_stats"), Some("seeding")));
        assert_eq!(back, Event::TorrentStats { id, url: "magnet:?x".into(), stats });
    }

    #[test]
    fn debrid_messages_carry_the_key_only_to_the_daemon() {
        use udm_domain::debrid::{DebridAccount, DebridCheck, DebridError, DebridService};
        let key = DebridKey::new("KEY123");
        let check = incoming(Command::CheckDebrid { service: DebridService::RealDebrid, key: key.clone() });
        assert_eq!(json(&check), serde_json::json!({"op": "check_debrid", "service": "real_debrid", "key": "KEY123"}));
        assert!(!format!("{check:?}").contains("KEY123"), "{check:?}");
        assert!(matches!(command(check), Command::CheckDebrid { key: Some(k), .. } if k.expose() == "KEY123"));
        let stored = incoming(Command::CheckDebrid { service: DebridService::Premiumize, key: None });
        assert_eq!(json(&stored), serde_json::json!({"op": "check_debrid", "service": "premiumize"}));
        let set = incoming(Command::SetDebridKey { service: DebridService::AllDebrid, key: key.unwrap() });
        assert_eq!(json(&set), serde_json::json!({"op": "set_debrid_key", "service": "all_debrid", "key": "KEY123"}));
        assert!(!format!("{set:?}").contains("KEY123"), "{set:?}");
        assert!(
            matches!(command(set), Command::SetDebridKey { service: DebridService::AllDebrid, key } if key.expose() == "KEY123")
        );
        let remove = incoming(Command::RemoveDebridKey);
        assert_eq!(json(&remove), serde_json::json!({"op": "remove_debrid_key"}));
        assert!(matches!(command(remove), Command::RemoveDebridKey));
        let blank = r#"{"op":"set_debrid_key","service":"real_debrid","key":"  "}"#;
        assert!(serde_json::from_str::<Incoming>(blank).is_err(), "a blank key never means remove");
        let missing = r#"{"op":"set_debrid_key","service":"real_debrid"}"#;
        assert!(serde_json::from_str::<Incoming>(missing).is_err());

        for outcome in [
            Ok(DebridAccount { username: Some("ann".into()), premium: true, premium_until: Some(9) }),
            Err(DebridError::Service("Hoster in maintenance".into())),
        ] {
            let check = DebridCheck { service: DebridService::AllDebrid, outcome };
            let (value, back) = round_trip(Event::Debrid { check: check.clone() });
            assert_eq!(value["op"], "debrid");
            assert_eq!(back, Event::Debrid { check });
        }
        let refused = udm_domain::debrid::Refused { service: DebridService::Premiumize, error: DebridError::BadKey };
        let (value, back) = round_trip(Event::DebridRefused { refused: refused.clone() });
        assert_eq!(
            value,
            serde_json::json!({"op": "debrid_refused", "refused": {"service": "premiumize", "error": {"kind": "bad_key"}}})
        );
        assert_eq!(back, Event::DebridRefused { refused });
    }

    #[test]
    fn playlists_and_tools_keep_their_shape() {
        use udm_domain::media::{Playlist, PlaylistEntry};
        use udm_domain::tools::{Tool, ToolError, ToolState};
        let playlist = Playlist {
            title: "T".into(),
            entries: vec![PlaylistEntry { url: "https://v/1".into(), title: "One".into(), duration: None }],
        };
        let msg = incoming(Command::PickVideos { page: "https://v/l".into(), playlist, user_agent: None });
        assert_eq!(
            json(&msg),
            serde_json::json!({"op": "pick_videos", "page": "https://v/l", "playlist": {"title": "T", "entries": [{"url": "https://v/1", "title": "One"}]}, "user_agent": null})
        );
        assert_eq!(
            json(&incoming(Command::InstallTool { tool: Tool::YtDlp })),
            serde_json::json!({"op": "install_tool", "tool": "yt_dlp"})
        );
        let state = ToolState {
            tool: Tool::Ffmpeg,
            path: None,
            installed: None,
            installable: false,
            command: "brew install ffmpeg".into(),
        };
        let failed = Event::Tools {
            tools: vec![state],
            done: Some(Tool::Ffmpeg),
            error: Some(ToolError::NoBuild("ffmpeg".into(), "brew install ffmpeg".into()).into()),
        };
        let (value, back) = round_trip(failed.clone());
        assert_eq!(value["op"], "tools");
        assert_eq!(
            value["error_kind"],
            serde_json::json!({"Service": {"Tool": {"NoBuild": ["ffmpeg", "brew install ffmpeg"]}}})
        );
        assert_eq!(back, failed);
    }

    #[test]
    fn galleries_keep_their_shape() {
        use udm_domain::gallery::{Gallery, GalleryError, Picture};
        assert_eq!(
            json(&incoming(Command::PickGallery { page: "https://g/a".into(), user_agent: None })),
            serde_json::json!({"op": "pick_gallery", "page": "https://g/a", "user_agent": null})
        );
        assert_eq!(
            json(&incoming(Command::ReadGallery {
                request: 4,
                page: "https://g/a".into(),
                user_agent: Some("UA".into())
            })),
            serde_json::json!({"op": "read_gallery", "request": 4, "page": "https://g/a", "user_agent": "UA"})
        );
        let gallery = Gallery {
            title: "T".into(),
            pictures: vec![Picture { url: "https://i/1.jpg".into(), width: Some(2), ..Picture::default() }],
        };
        let found = Event::Gallery { request: 4, page: "https://g/a".into(), gallery: Some(gallery), error: None };
        let (value, back) = round_trip(found.clone());
        assert_eq!(value["op"], "gallery");
        assert_eq!(
            value["gallery"],
            serde_json::json!({"title": "T", "pictures": [{"url": "https://i/1.jpg", "width": 2}]})
        );
        assert_eq!(back, found);
        let failed = Event::Gallery {
            request: 5,
            page: "https://g/b".into(),
            gallery: None,
            error: Some(GalleryError::TimedOut(55).into()),
        };
        let (value, back) = round_trip(failed.clone());
        assert_eq!(value["error"], "gallery-dl did not answer within 55 seconds.");
        assert_eq!(value["error_kind"], serde_json::json!({"Service": {"Gallery": {"TimedOut": 55}}}));
        assert_eq!(back, failed);
    }

    #[test]
    fn typed_failures_keep_their_kind_and_values() {
        let failures: Vec<Failure> = vec![
            ServiceError::NotDownloadAddress("javascript:x".into()).into(),
            ServiceError::CrawlRunning.into(),
            QueueError::NameTaken("Night".into()).into(),
            QueueError::Invalid(udm_domain::queue::InvalidQueue::StartTime("25:00".into())).into(),
            DownloadError::BadSpeedLimit { kbps: 1, lo: 2, hi: 3 }.into(),
            ServiceError::Confirm(ConfirmError::Download(DownloadError::AnswerNoFolder)).into(),
            ServiceError::Confirm(ConfirmError::Start(StartOrWaitError::Start(ControlError("busy".into())))).into(),
            ServiceError::Confirm(ConfirmError::Start(StartOrWaitError::Queue(QueueError::NoQueue(QueueId::new(4)))))
                .into(),
            UpdateError::Save { path: "/t/s".into(), reason: "denied".into() }.into(),
            ServiceError::Grab(InvalidGrab::Depth).into(),
            SettingsError::Invalid(udm_domain::settings::InvalidSettings::FolderNotFull(
                udm_domain::settings::SettingsField::CategoryFolder("Music".into()),
                "rel".into(),
            ))
            .into(),
            SettingsError::Refused("read-only".into()).into(),
            Failure::InvalidMessage("expected value".into()),
            Failure::Probe("HTTP 404".into()),
            ServiceError::Pairing(PairingError::NotListening).into(),
            ServiceError::Pairing(PairingError::NoDevice(udm_domain::id::DeviceId::new(3))).into(),
            ServiceError::NoPackage(PackageId::new(2)).into(),
            ServiceError::PackageBusy.into(),
        ];
        for f in failures {
            let text = f.to_string();
            let (value, back) = round_trip(Event::Error { error: f.clone() });
            assert_eq!(value["message"], text.as_str(), "text-only clients read the message");
            assert!(value.get("error_kind").is_some(), "{f:?}");
            assert_eq!(back, Event::Error { error: f.clone() });

            let (value, back) = round_trip(Event::Rejected { url: "u".into(), error: f.clone() });
            assert_eq!(value["message"], text.as_str());
            assert_eq!(back, Event::Rejected { url: "u".into(), error: f.clone() });

            let (value, back) = round_trip(Event::UpdateFailed { error: f.clone() });
            assert_eq!(value["error"], text.as_str());
            assert_eq!(back, Event::UpdateFailed { error: f.clone() });

            let (value, back) = round_trip(Event::Update { newer: None, error: Some(f.clone()) });
            assert_eq!(value["error"], text.as_str());
            assert_eq!(back, Event::Update { newer: None, error: Some(f.clone()) });

            let zip = Event::ZipPreview { id: DownloadId::new(1), entries: Vec::new(), error: Some(f.clone()) };
            let (value, back) = round_trip(zip.clone());
            assert_eq!(value["error"], text.as_str());
            assert_eq!(back, zip);

            let link = Event::LinkInfo { request: 1, index: 0, total: None, content_type: None, error: Some(f) };
            let (value, back) = round_trip(link.clone());
            assert_eq!(value["error"], text.as_str());
            assert_eq!(back, link);
        }
    }

    #[test]
    fn remote_control_goes_as_its_ops() {
        use udm_domain::id::DeviceId;
        use udm_domain::pairing::{
            DeviceInfo, DeviceName, ListenerState, PairingCode, PairingOffer, RemoteFault, RemoteState,
        };
        for (command, op) in [
            (Command::RemoteStatus, serde_json::json!({"op": "remote_status"})),
            (Command::StartPairing, serde_json::json!({"op": "start_pairing"})),
            (Command::CancelPairing, serde_json::json!({"op": "cancel_pairing"})),
            (Command::RemoveDevice { id: DeviceId::new(2) }, serde_json::json!({"op": "remove_device", "id": 2})),
        ] {
            let msg = incoming(command);
            assert_eq!(json(&msg), op);
            assert_eq!(json(&incoming(super::command(serde_json::from_value(op).unwrap()))), json(&msg));
        }
        let code = PairingCode::from_random(&[7; 32]);
        let state = RemoteState {
            listener: ListenerState {
                listening: vec!["0.0.0.0:18443".into()],
                page: Some("https://192.168.1.5:18443/".into()),
                fault: None,
                fingerprint: Some("AB:CD".into()),
            },
            devices: vec![DeviceInfo {
                id: DeviceId::new(1),
                name: DeviceName::clean("Pixel"),
                paired_at: 5,
                last_seen: None,
            }],
            pairing: Some(PairingOffer {
                link: format!("https://192.168.1.5:18443/#pair={}", code.as_str()),
                code: code.clone(),
                expires_at: 300,
            }),
        };
        let (value, back) = round_trip(Event::Remote { state: state.clone() });
        assert_eq!(
            value,
            serde_json::json!({"op": "remote", "state": {
                "listening": ["0.0.0.0:18443"],
                "page": "https://192.168.1.5:18443/",
                "fingerprint": "AB:CD",
                "devices": [{"id": 1, "name": "Pixel", "paired_at": 5}],
                "pairing": {"link": format!("https://192.168.1.5:18443/#pair={}", code.as_str()), "code": code.as_str(), "expires_at": 300},
            }})
        );
        assert_eq!(back, Event::Remote { state });
        let fault = RemoteState {
            listener: ListenerState {
                fault: Some(RemoteFault::PortInUse { address: "0.0.0.0:18443".into() }),
                ..ListenerState::default()
            },
            ..RemoteState::default()
        };
        let (value, back) = round_trip(Event::Remote { state: fault.clone() });
        assert_eq!(value["state"]["fault"], serde_json::json!({"PortInUse": {"address": "0.0.0.0:18443"}}));
        assert_eq!(back, Event::Remote { state: fault });
    }

    #[test]
    fn a_lifted_speed_limit_goes_as_its_op() {
        let (value, back) = round_trip(Event::LimitLifted { lifted: true });
        assert_eq!(value, serde_json::json!({"op": "limit_lifted", "lifted": true}));
        assert_eq!(back, Event::LimitLifted { lifted: true });
    }

    #[test]
    fn a_failure_known_only_by_its_text_goes_as_text() {
        let (value, back) = round_trip(Event::Error { error: Failure::Other("as said".into()) });
        assert_eq!(value, serde_json::json!({"op": "error", "message": "as said"}));
        assert_eq!(back, Event::Error { error: Failure::Other("as said".into()) });
        let (value, _) = round_trip(Event::Update { newer: None, error: None });
        assert_eq!(value, serde_json::json!({"op": "update"}), "no error, no kind");
    }

    /// Messages from a daemon that sent only text, or a kind this client
    /// does not know, still read.
    #[test]
    fn text_only_messages_read_as_their_text() {
        let read = |raw: &str| super::event(serde_json::from_str::<Outgoing>(raw).unwrap());
        assert_eq!(
            read(r#"{"op":"error","message":"no queue with id 7"}"#),
            Event::Error { error: Failure::Other("no queue with id 7".into()) }
        );
        assert_eq!(
            read(r#"{"op":"error","message":"invalid message: unknown variant `x`"}"#),
            Event::Error { error: Failure::InvalidMessage("unknown variant `x`".into()) }
        );
        assert_eq!(
            read(r#"{"op":"rejected","url":"u","message":"m","error_kind":{"FromTheFuture":1}}"#),
            Event::Rejected { url: "u".into(), error: Failure::Other("m".into()) }
        );
        assert_eq!(
            read(r#"{"op":"update_failed","error":"e"}"#),
            Event::UpdateFailed { error: Failure::Other("e".into()) }
        );
        assert_eq!(
            read(r#"{"op":"link_info","request":1,"index":2,"error":"refused"}"#),
            Event::LinkInfo {
                request: 1,
                index: 2,
                total: None,
                content_type: None,
                error: Some(Failure::Other("refused".into()))
            }
        );
        assert_eq!(
            read(r#"{"op":"zip_preview","id":3,"error":"not a zip"}"#),
            Event::ZipPreview {
                id: DownloadId::new(3),
                entries: Vec::new(),
                error: Some(Failure::Other("not a zip".into()))
            }
        );
    }

    /// What a typed failure looks like on the wire. The variant names of
    /// the wire mirrors and of the domain enums are the wire format, so a
    /// rename shows up here rather than in the clients.
    #[test]
    fn the_kind_names_the_error_and_its_values() {
        use serde_json::json;
        use udm_domain::command::CommandError;
        use udm_domain::queue::InvalidQueue;
        use udm_domain::settings::{InvalidSettings, ProxyEntry, SettingsField};
        let golden: Vec<(Failure, serde_json::Value)> = vec![
            (ServiceError::CrawlRunning.into(), json!({"Service": "CrawlRunning"})),
            (
                udm_domain::tools::ToolError::WrongChecksum("yt-dlp.exe".into()).into(),
                json!({"Service": {"Tool": {"WrongChecksum": "yt-dlp.exe"}}}),
            ),
            (ServiceError::BadBody("http://a/".into()).into(), json!({"Service": {"BadBody": "http://a/"}})),
            (ServiceError::NotDownloadAddress("x:y".into()).into(), json!({"Service": {"NotDownloadAddress": "x:y"}})),
            (QueueError::NoQueue(QueueId::new(7)).into(), json!({"Service": {"Queue": {"NoQueue": 7}}})),
            (QueueError::MainQueue.into(), json!({"Service": {"Queue": "MainQueue"}})),
            (QueueError::NameTaken("N".into()).into(), json!({"Service": {"Queue": {"NameTaken": "N"}}})),
            (
                QueueError::Invalid(InvalidQueue::StartTime("25:00".into())).into(),
                json!({"Service": {"Queue": {"Invalid": {"StartTime": "25:00"}}}}),
            ),
            (DownloadError::NoDownload(DownloadId::new(3)).into(), json!({"Service": {"Download": {"NoDownload": 3}}})),
            (
                DownloadError::BadSpeedLimit { kbps: 1, lo: 2, hi: 3 }.into(),
                json!({"Service": {"Download": {"BadSpeedLimit": {"kbps": 1, "lo": 2, "hi": 3}}}}),
            ),
            (
                DownloadError::FilesStay { url: "u".into(), reason: "r".into() }.into(),
                json!({"Service": {"Download": {"FilesStay": {"url": "u", "reason": "r"}}}}),
            ),
            (UpdateError::NewestAlready.into(), json!({"Service": {"Update": "NewestAlready"}})),
            (
                UpdateError::Save { path: "/t/s".into(), reason: "denied".into() }.into(),
                json!({"Service": {"Update": {"Save": {"path": "/t/s", "reason": "denied"}}}}),
            ),
            (ServiceError::Grab(InvalidGrab::Depth).into(), json!({"Service": {"Grab": "Depth"}})),
            (
                ServiceError::Confirm(ConfirmError::Download(DownloadError::AnswerNoFolder)).into(),
                json!({"Service": {"Confirm": {"Download": "AnswerNoFolder"}}}),
            ),
            (
                ServiceError::Confirm(ConfirmError::Start(StartOrWaitError::Start(ControlError("busy".into())))).into(),
                json!({"Service": {"Confirm": {"StartControl": "busy"}}}),
            ),
            (
                ServiceError::Confirm(ConfirmError::Start(StartOrWaitError::Queue(QueueError::NoQueue(QueueId::new(
                    4,
                )))))
                .into(),
                json!({"Service": {"Confirm": {"StartQueue": {"NoQueue": 4}}}}),
            ),
            (SettingsError::Refused("read-only".into()).into(), json!({"Settings": {"Refused": "read-only"}})),
            (SettingsError::Invalid(InvalidSettings::QuotaZero).into(), json!({"Settings": {"Invalid": "QuotaZero"}})),
            (
                SettingsError::Invalid(InvalidSettings::FolderNotFull(
                    SettingsField::CategoryFolder("Music".into()),
                    "rel".into(),
                ))
                .into(),
                json!({"Settings": {"Invalid": {"FolderNotFull": [{"CategoryFolder": "Music"}, "rel"]}}}),
            ),
            (
                SettingsError::Invalid(InvalidSettings::ProxyNotHostPort(ProxyEntry::Socks)).into(),
                json!({"Settings": {"Invalid": {"ProxyNotHostPort": "Socks"}}}),
            ),
            (
                SettingsError::Invalid(InvalidSettings::Scanner(CommandError::NoPath)).into(),
                json!({"Settings": {"Invalid": {"Scanner": "NoPath"}}}),
            ),
            (Mismatch::Changed.into(), json!({"Mismatch": "Changed"})),
            (
                Mismatch::Size { new: ByteCount::new(1), partial: ByteCount::new(2) }.into(),
                json!({"Mismatch": {"Size": {"new": 1, "partial": 2}}}),
            ),
            (Failure::InvalidMessage("eof".into()), json!({"InvalidMessage": "eof"})),
            (Failure::Probe("HTTP 404".into()), json!({"Probe": "HTTP 404"})),
        ];
        for (failure, kind) in golden {
            let (value, back) = round_trip(Event::Error { error: failure.clone() });
            assert_eq!(value["error_kind"], kind, "{failure:?}");
            assert_eq!(back, Event::Error { error: failure });
        }
    }

    #[test]
    fn a_new_links_mismatch_keeps_its_reason_and_kind() {
        let reason = Failure::from(Mismatch::Size { new: ByteCount::new(2 << 20), partial: ByteCount::new(1 << 20) });
        let (value, back) =
            round_trip(Event::RefreshMismatch { id: DownloadId::new(1), url: "u".into(), reason: reason.clone() });
        assert_eq!(value["reason"], "The new link is for a 2.0 MB file; the partial download is of a 1.0 MB file.");
        assert_eq!(
            value["error_kind"],
            serde_json::json!({"Mismatch": {"Size": {"new": 2 << 20, "partial": 1 << 20}}})
        );
        assert_eq!(back, Event::RefreshMismatch { id: DownloadId::new(1), url: "u".into(), reason });
        // From a daemon that sent the reason as text alone.
        let old: Outgoing =
            serde_json::from_str(r#"{"op":"refresh_mismatch","id":1,"url":"u","reason":"other file"}"#).unwrap();
        assert_eq!(
            super::event(old),
            Event::RefreshMismatch {
                id: DownloadId::new(1),
                url: "u".into(),
                reason: Failure::Other("other file".into())
            }
        );
    }
}
