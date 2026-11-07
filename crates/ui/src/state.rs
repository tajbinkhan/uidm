//! The window's state (AGENTS.md section 7): what it draws from, the
//! messages that change it, and [`apply`], the one function that does.
//!
//! The connection thread and the other threads outside the frame send
//! [`UiMessage`]s on the window's channel; the frame drains it with
//! `try_recv` and applies each. Those threads also keep a [`Watch`] of the
//! daemon's events, so they can wake a hidden window when a dialog must
//! open even when the frame is not running.
//!
//! The dialogs' own drafts still live in `App` (#199).

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use uidm_application::daemon::{self, Event, Lost};
use uidm_application::window::BrowserSlot;
use uidm_domain::download::{login_wanted, DownloadEntry, SegmentInfo, Status};
use uidm_domain::grab::{Filter, GrabFile, GrabRequest, Project, Template};
use uidm_domain::queue::{Queue, WhenDone};
use uidm_domain::settings::{AddedBrowser, Notify, Settings};
use uidm_domain::table::DownloadTable;

use crate::i18n::{tr, trf};
use crate::{columns, links, progress};

/// Ignore speed samples closer together than this; they are too noisy.
const SPEED_MIN_INTERVAL: Duration = Duration::from_millis(400);

#[derive(Clone)]
pub enum Link {
    Connecting,
    Connected,
    Lost(String),
}

/// Bytes per second, smoothed across progress events.
pub struct Speed {
    at: Instant,
    done: u64,
    pub bytes_per_sec: f64,
}

/// Everything the window draws from that the daemon and the threads
/// outside the frame change. Only [`apply`] changes it.
pub struct AppState {
    pub link: Link,
    pub table: DownloadTable,
    pub speeds: HashMap<u64, Speed>,
    /// The daemon's most recent `error` reply, shown under the toolbar.
    pub last_error: Option<String>,
    /// The daemon did not know a request this window sent: it is an older
    /// build than the window.
    pub stale_daemon: bool,
    /// The daemon's settings, once it has sent them.
    pub settings: Option<Settings>,
    /// True from sending `set_settings` until the daemon answers with
    /// `settings` or `error`.
    pub saving: bool,
    /// That answer, for the Options dialog to pick up.
    pub save_result: Option<Result<(), String>>,
    /// Downloads to show a completion dialog for, and whether each failed.
    pub completed: VecDeque<(u64, bool)>,
    /// The daemon's queues, the main queue first.
    pub queues: Vec<Queue>,
    /// A countdown before a finished queue or download sleeps or shuts down:
    /// the action, when it happens, the queue's name, and the download's
    /// file name when one download asked for it.
    pub countdown: Option<(WhenDone, Instant, String, Option<String>)>,
    /// The download waiting for a new link from the browser, if any.
    pub refresh_waiting: Option<u64>,
    /// The daemon's answer that a new link is for another file: the
    /// download, the link and why.
    pub refresh_mismatch: Option<(u64, String, String)>,
    /// A passing message for the status bar, such as how many downloads a
    /// batch added.
    pub notice: Option<String>,
    /// The site crawl's news, for the grabber dialog to pick up.
    pub grab_events: VecDeque<GrabEvent>,
    /// A copied link to offer to download.
    pub clipboard: Option<String>,
    /// The tray asked for a batch from the clipboard's links.
    pub clipboard_batch: bool,
    /// Downloads that failed for want of a login, to ask for one.
    pub login_wanted: VecDeque<u64>,
    /// Each download's connections, from its latest progress.
    pub segments: HashMap<u64, Vec<SegmentInfo>>,
    /// Downloads that started since the last frame, and whether a queue
    /// started each.
    pub started: Vec<(u64, bool)>,
    /// Links from a page for the link picker, until the window opens it.
    pub picked_links: Option<links::LinksDialog>,
    /// The daemon's answers about the link picker's links, until the
    /// picker takes them: the request, the link's place in it, and what
    /// its server said.
    pub link_info: Vec<(u64, usize, links::Probed)>,
    /// The quota's warning for the status bar, while it matters.
    pub quota: Option<String>,
    /// The last update check's answer, for the About box: a newer release,
    /// none, or why it could not look.
    pub update: Option<Result<Option<uidm_domain::update::Release>, String>>,
    /// A newer release, for the status bar.
    pub newer_release: Option<uidm_domain::update::Release>,
    /// Update now is under way: `None` while the daemon downloads the
    /// update, or why it failed.
    pub installing: Option<Option<String>>,
    /// A zip preview the daemon sent: the download and its files, or why
    /// not.
    pub zip_listed: Option<ZipListed>,
    /// Downloads are paused for the quota.
    pub quota_paused: bool,
    /// Whether the browser extension is connected, and when one last was,
    /// as the daemon last said.
    pub browser_link: Option<(bool, Option<u64>)>,
    /// The grabber's saved projects, the most recently used first, as a
    /// worker last read them.
    pub grab_projects: Vec<String>,
    /// The grabber's templates, built-in and saved, as a worker last read
    /// them.
    pub grab_templates: Vec<Template>,
    /// The request a new project starts from, as a worker last read it.
    pub grab_template: GrabRequest,
    /// The file-type filters the grabber offers, as a worker last read
    /// them.
    pub grab_filters: Vec<Filter>,
    /// A project the category tree asked to open, read on a worker, or why
    /// it could not be; until the window takes it.
    pub grab_opened: Option<Result<Project, String>>,
    /// The browsers a worker found for Options, until Options takes them.
    pub browsers_read: Option<BrowsersRead>,
}

/// The grabber's saved lists, as a worker reads them.
#[derive(Debug, Clone, PartialEq)]
pub struct GrabLibrary {
    pub projects: Vec<String>,
    pub templates: Vec<Template>,
    pub template: GrabRequest,
    pub filters: Vec<Filter>,
}

/// A browser row for Options, as a worker read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserRow {
    /// The browser whose registration it uses.
    pub browser: BrowserSlot,
    pub name: String,
    /// The browser the user added by its program, if it is one.
    pub added: Option<AddedBrowser>,
    /// Whether its place is known rather than guessed from its family.
    pub known: bool,
    pub registered: bool,
}

/// What Options shows of the browsers: each one found, registered or
/// added, and those that read Chrome's registration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BrowsersRead {
    pub rows: Vec<BrowserRow>,
    pub chrome_readers: Vec<String>,
}

/// News of a site crawl.
pub enum GrabEvent {
    /// Pages read, bytes read, files kept, file links looked at, and the
    /// downloads added to a queue so far.
    Progress(u64, u64, usize, u64, Vec<u64>),
    /// The files kept, pages read, file links looked at, why it stopped,
    /// and the login page site's cookies it used.
    Done(Vec<GrabFile>, u64, u64, Option<String>, Option<String>),
}

impl AppState {
    pub fn dialog_mode(&self) -> bool {
        self.settings.as_ref().is_some_and(|s| s.notify == Notify::Dialog)
    }

    /// The tray icon's tooltip: how many downloads run, and how fast.
    pub fn tooltip(&self) -> String {
        let active = self.table.entries().filter(|e| e.status == Status::Downloading).count();
        let queued = self.table.entries().filter(|e| e.status == Status::Queued).count();
        let speed: f64 = self
            .speeds
            .iter()
            .filter(|(id, _)| self.table.get(**id).is_some_and(|e| e.status == Status::Downloading))
            .map(|(_, s)| s.bytes_per_sec)
            .sum();
        match (active, queued) {
            (0, 0) => tr("UIDM: no active downloads").to_string(),
            (0, q) => trf!("UIDM: {queued} queued", queued = q),
            (a, _) => {
                trf!("UIDM: {active} downloading at {speed}/s", active = a, speed = crate::human_bytes(speed as u64))
            }
        }
    }

    /// What a daemon event changes in the list and the window's news.
    fn apply_event(&mut self, msg: &Event) {
        daemon::apply(&mut self.table, msg, crate::ports::ports().unix_now());
        match msg {
            Event::Queues { items } => self.queues = items.clone(),
            Event::Countdown { action, seconds, queue, file } => {
                let deadline = Instant::now() + Duration::from_secs(*seconds as u64);
                self.countdown = Some((*action, deadline, queue.clone(), file.clone()));
            }
            Event::CountdownCanceled => self.countdown = None,
            Event::RefreshWaiting { id } => self.refresh_waiting = *id,
            Event::RefreshMismatch { id, url, reason } => {
                self.refresh_mismatch = Some((*id, url.clone(), reason.clone()));
            }
            Event::Settings { settings } => {
                self.settings = Some(settings.clone());
                if self.saving {
                    self.saving = false;
                    self.save_result = Some(Ok(()));
                }
            }
            // An older daemon than this window cannot read its newer
            // requests; its parser's complaint means nothing to the user.
            Event::Error { message } if message.starts_with("invalid message: unknown variant") => {
                self.saving = false;
                self.stale_daemon = true;
                self.last_error = Some(
                    tr("The running UIDM daemon is older than this window, so it did not understand a request. Restart the daemon to run the new one.")
                        .into(),
                );
            }
            Event::Error { message } if self.saving => {
                self.saving = false;
                self.save_result = Some(Err(message.clone()));
            }
            Event::Progress { id, done, segments, .. } => {
                self.sample_speed(*id, *done);
                if let Some(s) = segments {
                    self.segments.insert(*id, s.clone());
                }
            }
            Event::BrowserLink { connected, last_seen } => self.browser_link = Some((*connected, *last_seen)),
            Event::Started { id, .. } => {
                self.segments.remove(id);
                self.started.push((*id, progress::started_by_queue(&self.queues, *id)));
            }
            Event::Quota { used, limit, paused, resumes_at } => {
                let amount =
                    trf!("{used} of {limit}", used = crate::human_bytes(*used), limit = crate::human_bytes(*limit));
                let was_paused = std::mem::replace(&mut self.quota_paused, *paused);
                self.quota = if *paused {
                    Some(match *resumes_at {
                        Some(t) => trf!(
                            "Download quota reached ({amount}); downloads are paused. They resume around {time}.",
                            amount = amount,
                            time = columns::local_date(t)
                        ),
                        None => trf!("Download quota reached ({amount}); downloads are paused.", amount = amount),
                    })
                } else if was_paused {
                    self.notice = Some(tr("The download quota allows downloads again; paused ones resumed.").into());
                    None
                } else if *used >= *limit / 10 * 9 {
                    Some(trf!(
                        "Download quota nearly used: {amount}. Downloads pause when it runs out.",
                        amount = amount
                    ))
                } else {
                    None
                };
            }
            Event::UpdateFailed { error } => {
                self.installing = Some(Some(error.clone()));
            }
            Event::Update { newer, error } => {
                self.update = Some(match error {
                    Some(e) => Err(e.clone()),
                    None => Ok(newer.clone()),
                });
                if newer.is_some() {
                    self.newer_release = newer.clone();
                }
            }
            Event::ZipPreview { id, entries, error } => {
                let result = match error {
                    Some(e) => Err(e.clone()),
                    None => Ok(entries.clone()),
                };
                self.zip_listed = Some((*id, result));
            }
            Event::PickLinks { page, links, user_agent } => {
                self.picked_links = Some(links::LinksDialog::new(page.clone(), links.clone(), user_agent.clone()));
            }
            Event::LinkInfo { request, index, total, content_type, error } => {
                let probed = match error {
                    Some(e) => links::Probed::Failed(e.clone()),
                    None => links::Probed::Found { total: *total, content_type: content_type.clone() },
                };
                self.link_info.push((*request, *index, probed));
            }
            Event::AddedMany { count, skipped } => {
                let mut notice = match count {
                    1 => tr("Added 1 download, waiting in its queue.").to_string(),
                    n => trf!("Added {count} downloads, waiting in their queue.", count = n),
                };
                match skipped {
                    0 => {}
                    1 => {
                        notice.push(' ');
                        notice.push_str(tr("Skipped 1 already in the list."));
                    }
                    n => {
                        notice.push(' ');
                        notice.push_str(&trf!("Skipped {count} already in the list.", count = n));
                    }
                }
                self.notice = Some(notice);
            }
            Event::GrabProgress { pages, bytes, found, explored, added } => {
                self.grab_events.push_back(GrabEvent::Progress(*pages, *bytes, *found, *explored, added.clone()));
            }
            Event::GrabDone { files, pages, explored, stopped, login_cookies } => {
                self.grab_events.push_back(GrabEvent::Done(
                    files.clone(),
                    *pages,
                    *explored,
                    stopped.clone(),
                    login_cookies.clone(),
                ));
            }
            Event::Done { id, .. } | Event::Failed { id, .. } | Event::Canceled { id, .. } | Event::Removed { id } => {
                self.speeds.remove(id);
                if matches!(msg, Event::Removed { .. }) {
                    self.segments.remove(id);
                }
            }
            Event::Error { message } | Event::Rejected { message, .. } => self.last_error = Some(message.clone()),
            _ => {}
        }
    }

    fn sample_speed(&mut self, id: u64, done: u64) {
        let now = Instant::now();
        let s = self.speeds.entry(id).or_insert(Speed { at: now, done, bytes_per_sec: 0.0 });
        let dt = now.duration_since(s.at);
        if dt < SPEED_MIN_INTERVAL || done < s.done {
            return;
        }
        let instant = (done - s.done) as f64 / dt.as_secs_f64();
        s.bytes_per_sec = if s.bytes_per_sec == 0.0 { instant } else { 0.7 * s.bytes_per_sec + 0.3 * instant };
        s.at = now;
        s.done = done;
    }
}

impl Default for AppState {
    fn default() -> Self {
        AppState {
            link: Link::Connecting,
            table: DownloadTable::new(),
            speeds: HashMap::new(),
            last_error: None,
            stale_daemon: false,
            settings: None,
            saving: false,
            save_result: None,
            completed: VecDeque::new(),
            queues: Vec::new(),
            countdown: None,
            refresh_waiting: None,
            refresh_mismatch: None,
            notice: None,
            grab_events: VecDeque::new(),
            clipboard: None,
            clipboard_batch: false,
            login_wanted: VecDeque::new(),
            segments: HashMap::new(),
            started: Vec::new(),
            picked_links: None,
            link_info: Vec::new(),
            quota: None,
            zip_listed: None,
            update: None,
            newer_release: None,
            installing: None,
            quota_paused: false,
            browser_link: None,
            grab_projects: Vec::new(),
            grab_templates: uidm_domain::grab::built_in_templates(),
            grab_template: GrabRequest::default(),
            grab_filters: uidm_domain::grab::default_filters(),
            grab_opened: None,
            browsers_read: None,
        }
    }
}

/// What changes the window's state: the daemon's events and the news of
/// the threads that act outside the frame. The frame drains them from its
/// channel and hands each to [`apply`].
#[derive(Debug)]
// Each message lives only while it is applied, so boxing the daemon's events would add churn for no gain.
#[allow(clippy::large_enum_variant)]
pub enum UiMessage {
    /// The connection is being made again.
    Connecting,
    /// An event from the daemon, the connection's state among them.
    Daemon(Event),
    /// The clipboard watcher found a link to offer.
    ClipboardOffer(String),
    /// The tray asked for a batch from the clipboard's links.
    ClipboardBatch,
    /// A problem to show under the toolbar.
    Error(String),
    /// Download `id` failed a moment ago and is still failed: its dialog
    /// can show now.
    FailedSettled(u64),
    /// Update now was chosen; its answer is awaited.
    InstallStarted,
    /// A new update check was asked for; its answer is awaited.
    UpdateCheckStarted,
    /// The daemon is being restarted to run the new one.
    DaemonRestarting,
    /// The user asked to give download `id` a login: it goes first.
    LoginAsked(u64),
    /// Downloads whose login prompt waits for another site's, to ask about
    /// next.
    LoginsWaiting(Vec<u64>),
    /// Options' settings went to the daemon; its answer is awaited.
    SavingSettings,
    /// A worker read the grabber's saved projects, templates, default
    /// template and filters.
    GrabLibraryRead(GrabLibrary),
    /// A worker read the project the category tree asked to open.
    GrabProjectRead(Result<Project, String>),
    /// A worker read the browsers for Options.
    BrowsersRead(BrowsersRead),
    /// They could not be sent, and why.
    SaveFailed(String),
    /// The window is done with something: the state forgets it.
    Forget(Forget),
    /// The window takes something to show: [`apply`] hands it over as a
    /// [`Taken`] and the state forgets it.
    Take(What),
}

/// What the window is done with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forget {
    /// The message under the toolbar.
    Error,
    /// The passing notice in the status bar.
    Notice,
    /// The answer that a new link is for another file.
    RefreshMismatch,
    /// The offer to download a copied link.
    ClipboardOffer,
    /// A countdown that ran out.
    Countdown,
    /// The grabber's news, for a new crawl.
    GrabEvents,
    /// The completion dialog shown first.
    Completed,
    /// Every completion dialog waiting.
    AllCompleted,
}

/// What the window takes from the state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum What {
    GrabEvents,
    ClipboardBatch,
    LoginWanted,
    SaveResult,
    Started,
    ZipListed,
    PickedLinks,
    LinkInfo,
    BrowsersRead,
    GrabOpened,
}

/// What [`apply`] hands over for a [`UiMessage::Take`].
pub enum Taken {
    GrabEvents(Vec<GrabEvent>),
    ClipboardBatch(bool),
    LoginWanted(Vec<u64>),
    SaveResult(Option<Result<(), String>>),
    Started(Vec<(u64, bool)>),
    ZipListed(Option<ZipListed>),
    PickedLinks(Option<Box<links::LinksDialog>>),
    LinkInfo(Vec<(u64, usize, links::Probed)>),
    BrowsersRead(Option<BrowsersRead>),
    GrabOpened(Option<Result<Box<Project>, String>>),
}

/// A zip preview's download and its files, or why they could not be
/// listed.
pub type ZipListed = (u64, Result<Vec<uidm_domain::download::ZipEntry>, String>);

/// Change `state` as `msg` says. The only place the window's state
/// changes. A [`UiMessage::Take`] returns what was taken.
pub fn apply(state: &mut AppState, msg: UiMessage) -> Option<Taken> {
    match msg {
        UiMessage::Connecting => state.link = Link::Connecting,
        UiMessage::Daemon(Event::Connected) => state.link = Link::Connected,
        UiMessage::Daemon(Event::Lost(lost)) => {
            state.link = Link::Lost(match lost {
                Lost::Closed => tr("the daemon closed the connection").to_string(),
                Lost::Failed(e) => e,
            });
        }
        UiMessage::Daemon(event) => {
            state.apply_event(&event);
            match &event {
                // A duplicate of a finished download shows its dialog again.
                Event::ShowDone { id } => state.completed.push_back((*id, false)),
                // A site that wants a login gets the login prompt.
                Event::Failed { id, error, .. } if login_wanted(error).is_some() => {
                    if !state.login_wanted.contains(id) {
                        state.login_wanted.push_back(*id);
                    }
                }
                // Options, or the download itself, can ask for the dialog.
                Event::Done { id, .. }
                    if state.dialog_mode()
                        || state.table.get(*id).and_then(|e| e.on_done).is_some_and(|o| o.dialog) =>
                {
                    state.completed.push_back((*id, false));
                }
                _ => {}
            }
        }
        UiMessage::ClipboardOffer(url) => state.clipboard = Some(url),
        UiMessage::ClipboardBatch => state.clipboard_batch = true,
        UiMessage::Error(message) => state.last_error = Some(message),
        UiMessage::FailedSettled(id) => {
            if state.dialog_mode() && state.table.get(id).is_some_and(|e| e.status == Status::Failed) {
                state.completed.push_back((id, true));
            }
        }
        UiMessage::InstallStarted => state.installing = Some(None),
        UiMessage::UpdateCheckStarted => state.update = None,
        UiMessage::DaemonRestarting => {
            state.stale_daemon = false;
            state.last_error = None;
        }
        UiMessage::LoginAsked(id) => {
            if !state.login_wanted.contains(&id) {
                state.login_wanted.push_front(id);
            }
        }
        UiMessage::LoginsWaiting(ids) => state.login_wanted.extend(ids),
        UiMessage::GrabLibraryRead(read) => {
            state.grab_projects = read.projects;
            state.grab_templates = read.templates;
            state.grab_template = read.template;
            state.grab_filters = read.filters;
        }
        UiMessage::GrabProjectRead(read) => state.grab_opened = Some(read),
        UiMessage::BrowsersRead(read) => state.browsers_read = Some(read),
        UiMessage::SavingSettings => {
            state.saving = true;
            state.save_result = None;
        }
        UiMessage::SaveFailed(why) => {
            state.saving = false;
            state.save_result = Some(Err(why));
        }
        UiMessage::Forget(what) => match what {
            Forget::Error => state.last_error = None,
            Forget::Notice => state.notice = None,
            Forget::RefreshMismatch => state.refresh_mismatch = None,
            Forget::ClipboardOffer => state.clipboard = None,
            Forget::Countdown => state.countdown = None,
            Forget::GrabEvents => state.grab_events.clear(),
            Forget::Completed => {
                state.completed.pop_front();
            }
            Forget::AllCompleted => state.completed.clear(),
        },
        UiMessage::Take(what) => {
            return Some(match what {
                What::GrabEvents => Taken::GrabEvents(state.grab_events.drain(..).collect()),
                What::ClipboardBatch => Taken::ClipboardBatch(std::mem::take(&mut state.clipboard_batch)),
                What::LoginWanted => Taken::LoginWanted(state.login_wanted.drain(..).collect()),
                What::SaveResult => Taken::SaveResult(state.save_result.take()),
                What::Started => Taken::Started(std::mem::take(&mut state.started)),
                What::ZipListed => Taken::ZipListed(state.zip_listed.take()),
                What::PickedLinks => Taken::PickedLinks(state.picked_links.take().map(Box::new)),
                What::LinkInfo => Taken::LinkInfo(std::mem::take(&mut state.link_info)),
                What::BrowsersRead => Taken::BrowsersRead(state.browsers_read.take()),
                What::GrabOpened => Taken::GrabOpened(state.grab_opened.take().map(|r| r.map(Box::new))),
            });
        }
    }
    None
}

/// The crawl's news since the last frame.
pub fn take_grab_events(state: &mut AppState) -> Vec<GrabEvent> {
    match apply(state, UiMessage::Take(What::GrabEvents)) {
        Some(Taken::GrabEvents(events)) => events,
        _ => Vec::new(),
    }
}

/// Whether the tray asked for a batch from the clipboard.
pub fn take_clipboard_batch(state: &mut AppState) -> bool {
    matches!(apply(state, UiMessage::Take(What::ClipboardBatch)), Some(Taken::ClipboardBatch(true)))
}

/// The downloads waiting for a login prompt, the next first.
pub fn take_login_wanted(state: &mut AppState) -> Vec<u64> {
    match apply(state, UiMessage::Take(What::LoginWanted)) {
        Some(Taken::LoginWanted(ids)) => ids,
        _ => Vec::new(),
    }
}

/// The daemon's answer to Options' save, once it came.
pub fn take_save_result(state: &mut AppState) -> Option<Result<(), String>> {
    match apply(state, UiMessage::Take(What::SaveResult)) {
        Some(Taken::SaveResult(result)) => result,
        _ => None,
    }
}

/// The downloads that started since the last frame, and whether a queue
/// started each.
pub fn take_started(state: &mut AppState) -> Vec<(u64, bool)> {
    match apply(state, UiMessage::Take(What::Started)) {
        Some(Taken::Started(started)) => started,
        _ => Vec::new(),
    }
}

/// The zip preview the daemon sent, if one came.
pub fn take_zip_listed(state: &mut AppState) -> Option<ZipListed> {
    match apply(state, UiMessage::Take(What::ZipListed)) {
        Some(Taken::ZipListed(listed)) => listed,
        _ => None,
    }
}

/// Links from a page for the link picker, if some came.
pub fn take_picked_links(state: &mut AppState) -> Option<links::LinksDialog> {
    match apply(state, UiMessage::Take(What::PickedLinks)) {
        Some(Taken::PickedLinks(dialog)) => dialog.map(|d| *d),
        _ => None,
    }
}

/// The project the category tree asked to open, once a worker read it.
pub fn take_grab_opened(state: &mut AppState) -> Option<Result<Project, String>> {
    match apply(state, UiMessage::Take(What::GrabOpened)) {
        Some(Taken::GrabOpened(read)) => read.map(|r| r.map(|p| *p)),
        _ => None,
    }
}

/// The browsers a worker read for Options, once it has.
pub fn take_browsers_read(state: &mut AppState) -> Option<BrowsersRead> {
    match apply(state, UiMessage::Take(What::BrowsersRead)) {
        Some(Taken::BrowsersRead(read)) => read,
        _ => None,
    }
}

/// The daemon's answers about the link picker's links.
pub fn take_link_info(state: &mut AppState) -> Vec<(u64, usize, links::Probed)> {
    match apply(state, UiMessage::Take(What::LinkInfo)) {
        Some(Taken::LinkInfo(info)) => info,
        _ => Vec::new(),
    }
}

/// What the window must do at once, outside its frame, for a daemon event:
/// a hidden window may get no frames until it comes up (see `win::wake`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    Nothing,
    /// Bring the window up (a click on a notification).
    Show,
    /// A dialog opens on its own: let it.
    Wake,
    /// Wait a moment, then wake for download `id`'s failure if it is still
    /// failed.
    WakeIfStillFailed(u64),
    /// UIDM exits.
    Quit,
}

/// What the threads outside the frame keep from the daemon's events: the
/// settings, and which downloads asked for a completion dialog or failed.
/// The connection thread updates it from each event before the frame sees
/// that event, so the clipboard watcher, the tray menu and the window's
/// wake-ups work while the frame is not running.
#[derive(Debug, Default)]
pub struct Watch {
    pub settings: Option<Settings>,
    /// Downloads that asked for their completion dialog.
    done_dialog: HashSet<u64>,
    failed: HashSet<u64>,
}

impl Watch {
    /// Note download `e` as it is now.
    fn note(&mut self, e: &DownloadEntry) {
        if e.on_done.is_some_and(|o| o.dialog) {
            self.done_dialog.insert(e.id);
        } else {
            self.done_dialog.remove(&e.id);
        }
        if e.status == Status::Failed {
            self.failed.insert(e.id);
        } else {
            self.failed.remove(&e.id);
        }
    }

    /// Note what `event` changes.
    pub fn observe(&mut self, event: &Event) {
        match event {
            Event::Settings { settings } => self.settings = Some(settings.clone()),
            Event::Downloads { items } => {
                self.done_dialog.clear();
                self.failed.clear();
                for e in items {
                    self.note(e);
                }
            }
            Event::Updated { item } => self.note(item),
            Event::Failed { id, .. } => {
                self.failed.insert(*id);
            }
            Event::Added { id, .. }
            | Event::Started { id, .. }
            | Event::Done { id, .. }
            | Event::Canceled { id, .. }
            | Event::Removed { id } => {
                self.failed.remove(id);
            }
            _ => {}
        }
        if let Event::Removed { id } = event {
            self.done_dialog.remove(id);
        }
    }

    fn dialog_mode(&self) -> bool {
        self.settings.as_ref().is_some_and(|s| s.notify == Notify::Dialog)
    }

    /// Whether download `id` is still failed.
    pub fn still_failed(&self, id: u64) -> bool {
        self.failed.contains(&id)
    }

    /// What the window must do at once for `event`, which `observe` has
    /// seen.
    pub fn need(&self, event: &Event) -> Need {
        match event {
            Event::Show => Need::Show,
            Event::ShowDone { .. } | Event::Countdown { .. } | Event::NameTaken { .. } => Need::Wake,
            // The download's own window opens when it starts.
            Event::Started { .. } if self.settings.as_ref().is_some_and(|s| s.progress_on_start) => Need::Wake,
            Event::Quit => Need::Quit,
            Event::Updated { item } if item.status == Status::Confirming => Need::Wake,
            // A window that connects late, or started in the tray, still
            // asks about a download already waiting for its save-as dialog.
            Event::Downloads { items } if items.iter().any(|e| e.status == Status::Confirming) => Need::Wake,
            Event::Failed { error, .. } if login_wanted(error).is_some() => Need::Wake,
            Event::Done { id, .. } if self.dialog_mode() || self.done_dialog.contains(id) => Need::Wake,
            Event::Failed { id, .. } if self.dialog_mode() => Need::WakeIfStillFailed(*id),
            _ => Need::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uidm_domain::download::OnDone;

    fn entry(id: u64, status: Status) -> DownloadEntry {
        let mut e: DownloadEntry = serde_json::from_value(serde_json::json!({
            "id": id, "url": format!("https://x/{id}.bin"), "path": null, "total": null, "done": 0,
            "status": "queued", "error": null, "resumable": false, "connections": 0,
        }))
        .unwrap();
        e.status = status;
        e
    }

    fn settings(notify: Notify) -> Settings {
        Settings { notify, ..Settings::default() }
    }

    fn daemon(state: &mut AppState, event: Event) {
        apply(state, UiMessage::Daemon(event));
    }

    #[test]
    fn what_the_window_takes_is_handed_over_once() {
        let mut s = AppState::default();
        daemon(&mut s, Event::GrabProgress { pages: 1, bytes: 2, found: 3, explored: 4, added: vec![] });
        apply(&mut s, UiMessage::ClipboardBatch);
        assert_eq!(take_grab_events(&mut s).len(), 1);
        assert!(take_grab_events(&mut s).is_empty(), "taken already");
        assert!(take_clipboard_batch(&mut s));
        assert!(!take_clipboard_batch(&mut s));
        apply(&mut s, UiMessage::LoginAsked(7));
        apply(&mut s, UiMessage::LoginAsked(7));
        apply(&mut s, UiMessage::LoginsWaiting(vec![8]));
        assert_eq!(take_login_wanted(&mut s), [7, 8]);
        assert!(take_login_wanted(&mut s).is_empty());
    }

    #[test]
    fn saving_settings_waits_for_the_daemons_answer() {
        let mut s = AppState::default();
        apply(&mut s, UiMessage::SavingSettings);
        assert!(s.saving);
        assert_eq!(take_save_result(&mut s), None, "no answer yet");
        daemon(&mut s, Event::Settings { settings: Settings::default() });
        assert!(!s.saving);
        assert_eq!(take_save_result(&mut s), Some(Ok(())));
        apply(&mut s, UiMessage::SaveFailed("not connected".into()));
        assert_eq!(take_save_result(&mut s), Some(Err("not connected".into())));
    }

    #[test]
    fn what_the_workers_read_lands_in_the_state() {
        let mut s = AppState::default();
        assert!(!s.grab_templates.is_empty(), "the built-in templates until the first read");
        let templates = uidm_domain::grab::built_in_templates();
        let read = GrabLibrary {
            projects: vec!["nightly".into()],
            templates: templates.clone(),
            template: GrabRequest::default(),
            filters: Vec::new(),
        };
        apply(&mut s, UiMessage::GrabLibraryRead(read));
        assert!(s.grab_filters.is_empty());
        assert_eq!(s.grab_projects, ["nightly"]);
        assert_eq!(s.grab_templates.len(), templates.len());
        assert_eq!(take_browsers_read(&mut s), None, "not read yet");
        let read = BrowsersRead { rows: Vec::new(), chrome_readers: vec!["Brave".into()] };
        apply(&mut s, UiMessage::BrowsersRead(read.clone()));
        assert_eq!(take_browsers_read(&mut s), Some(read));
        assert_eq!(take_browsers_read(&mut s), None, "taken once");
    }

    #[test]
    fn forgetting_clears_only_what_it_names() {
        let mut s = AppState::default();
        apply(&mut s, UiMessage::Error("e".into()));
        daemon(&mut s, Event::AddedMany { count: 2, skipped: 0 });
        apply(&mut s, UiMessage::Forget(Forget::Error));
        assert_eq!(s.last_error, None);
        assert!(s.notice.is_some());
        s.completed.extend([(1, false), (2, true)]);
        apply(&mut s, UiMessage::Forget(Forget::Completed));
        assert_eq!(s.completed, [(2, true)]);
        apply(&mut s, UiMessage::Forget(Forget::AllCompleted));
        assert!(s.completed.is_empty());
        apply(&mut s, UiMessage::Error("stale".into()));
        apply(&mut s, UiMessage::DaemonRestarting);
        assert_eq!(s.last_error, None);
    }

    #[test]
    fn the_connection_state_follows_its_messages() {
        let mut s = AppState::default();
        daemon(&mut s, Event::Connected);
        assert!(matches!(s.link, Link::Connected));
        daemon(&mut s, Event::Lost(Lost::Failed("refused".into())));
        assert!(matches!(&s.link, Link::Lost(why) if why == "refused"));
        apply(&mut s, UiMessage::Connecting);
        assert!(matches!(s.link, Link::Connecting));
    }

    #[test]
    fn a_finished_download_gets_its_dialog_when_asked() {
        let mut s = AppState::default();
        daemon(&mut s, Event::Downloads { items: vec![entry(1, Status::Downloading), entry(2, Status::Downloading)] });
        let asked = DownloadEntry {
            on_done: Some(OnDone { dialog: true, ..OnDone::default() }),
            ..entry(2, Status::Downloading)
        };
        daemon(&mut s, Event::Updated { item: asked });
        daemon(&mut s, Event::Done { id: 1, url: "https://x/1.bin".into(), path: "/d/1.bin".into(), note: None });
        daemon(&mut s, Event::Done { id: 2, url: "https://x/2.bin".into(), path: "/d/2.bin".into(), note: None });
        assert_eq!(s.completed, [(2, false)], "only the one that asked");
        daemon(&mut s, Event::Settings { settings: settings(Notify::Dialog) });
        daemon(&mut s, Event::ShowDone { id: 1 });
        assert_eq!(s.completed.back(), Some(&(1, false)));
    }

    #[test]
    fn a_failure_gets_its_dialog_only_if_still_failed() {
        let mut s = AppState::default();
        daemon(&mut s, Event::Settings { settings: settings(Notify::Dialog) });
        daemon(&mut s, Event::Downloads { items: vec![entry(1, Status::Downloading), entry(2, Status::Downloading)] });
        for id in [1, 2] {
            daemon(&mut s, Event::Failed { id, url: format!("https://x/{id}.bin"), error: "gone".into() });
        }
        assert!(s.completed.is_empty(), "not at once");
        daemon(&mut s, Event::Removed { id: 2 });
        apply(&mut s, UiMessage::FailedSettled(1));
        apply(&mut s, UiMessage::FailedSettled(2));
        assert_eq!(s.completed, [(1, true)]);
    }

    #[test]
    fn a_login_failure_asks_for_the_login_once() {
        let mut s = AppState::default();
        let error = uidm_domain::download::needs_login_error("files.example.com", None);
        for _ in 0..2 {
            daemon(&mut s, Event::Failed { id: 4, url: "https://files.example.com/f".into(), error: error.clone() });
        }
        assert_eq!(s.login_wanted, [4]);
    }

    #[test]
    fn the_threads_messages_land_in_the_state() {
        let mut s = AppState::default();
        apply(&mut s, UiMessage::ClipboardOffer("https://x/a.zip".into()));
        apply(&mut s, UiMessage::ClipboardBatch);
        apply(&mut s, UiMessage::Error("no tray".into()));
        assert_eq!(s.clipboard.as_deref(), Some("https://x/a.zip"));
        assert!(s.clipboard_batch);
        assert_eq!(s.last_error.as_deref(), Some("no tray"));
    }

    #[test]
    fn the_watch_wakes_the_window_for_what_needs_it() {
        let mut w = Watch::default();
        let see = |w: &mut Watch, e: Event| {
            w.observe(&e);
            w.need(&e)
        };
        assert_eq!(see(&mut w, Event::Show), Need::Show);
        assert_eq!(see(&mut w, Event::Quit), Need::Quit);
        let done = |id| Event::Done { id, url: String::new(), path: String::new(), note: None };
        assert_eq!(see(&mut w, done(1)), Need::Nothing, "no dialog asked for");
        let asked = DownloadEntry {
            on_done: Some(OnDone { dialog: true, ..OnDone::default() }),
            ..entry(2, Status::Downloading)
        };
        see(&mut w, Event::Updated { item: asked });
        assert_eq!(see(&mut w, done(2)), Need::Wake);
        let failed = |id| Event::Failed { id, url: String::new(), error: "gone".into() };
        assert_eq!(see(&mut w, failed(3)), Need::Nothing, "notifications, not dialogs");
        see(&mut w, Event::Settings { settings: Settings { progress_on_start: true, ..settings(Notify::Dialog) } });
        assert_eq!(see(&mut w, failed(3)), Need::WakeIfStillFailed(3));
        assert!(w.still_failed(3));
        see(&mut w, Event::Removed { id: 3 });
        assert!(!w.still_failed(3));
        let confirming = Event::Updated { item: entry(5, Status::Confirming) };
        assert_eq!(see(&mut w, confirming), Need::Wake);
        let started = Event::Started {
            id: 6,
            url: String::new(),
            path: String::new(),
            total: None,
            resumable: false,
            connections: 1,
            partial: None,
            validator: None,
        };
        assert_eq!(see(&mut w, started), Need::Wake, "its window opens");
    }
}
