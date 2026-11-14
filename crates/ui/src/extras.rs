//! What the daemon asks the window for beyond the list: a login, the
//! countdown before sleeping or shutting down, a new link, a package's
//! password, the browser's links, videos and galleries to pick from, the
//! quota and the speed limit's state, and the update check. The state the
//! actor keeps for them, the views, and the downloads the pickers make.
//! Plain functions, tested without Tauri.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::time::Instant;

use udm_application::daemon::Event;
use udm_domain::bytes::ByteCount;
use udm_domain::download::{login_wanted, Download, PageLink, Status};
use udm_domain::gallery::{gallery_folder, Gallery};
use udm_domain::id::{DownloadId, PackageId};
use udm_domain::media::{Playlist, PlaylistQuality, Target};
use udm_domain::package::{Package, PackageName, Packaging, Unpack};
use udm_domain::queue::WhenDone;
use udm_domain::settings::{Settings, GENERAL};
use udm_domain::update::Release;

use crate::contract::{
    CountdownView, GalleryView, Id, LinkFilter, LoginView, PackageMenu, PasswordView, PickLinksView, PickRow,
    PickVideosView, PictureRow, QueueItem, RefreshMismatchView, RefreshView, VideoRow,
};
use crate::i18n::{fill, Language};
use crate::projection::{format, Ctx};
use crate::state::AppState;

/// A page's links the browser sent to pick from, and what their servers
/// said of them.
#[derive(Debug, Clone)]
pub struct PickedLinks {
    pub page: String,
    pub links: Vec<PageLink>,
    pub user_agent: Option<String>,
    pub request: u64,
    /// Each probed link's size and type, by its place.
    pub info: HashMap<usize, (Option<ByteCount>, Option<String>)>,
    pub filter: LinkFilter,
}

/// The filters a link picker opens with: web pages, images and repeated
/// links hidden, every type shown.
pub fn default_link_filter() -> LinkFilter {
    LinkFilter { hide_pages: true, hide_images: true, hide_repeated: true, types: String::new() }
}

/// Extensions of links that lead to another web page rather than a file.
const PAGE_TYPES: [&str; 9] = ["", "htm", "html", "php", "asp", "aspx", "jsp", "shtml", "cgi"];

/// Why the ticked links cannot be added.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PickError {
    #[error(transparent)]
    Package(#[from] udm_domain::package::PackageError),
    #[error("A file name cannot contain \\ / : \" < > | or ?")]
    BadRename,
}

#[derive(Debug, Clone)]
pub struct PickedVideos {
    pub page: String,
    pub playlist: Playlist,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PickedGallery {
    pub page: String,
    pub user_agent: Option<String>,
    pub request: u64,
    /// `None` while the page is read.
    pub result: Option<Result<Gallery, String>>,
}

/// The countdown: what happens, when, and for which queue or file.
#[derive(Debug, Clone)]
pub struct Countdown {
    pub action: WhenDone,
    pub deadline: Instant,
    pub queue: String,
    pub file: Option<String>,
}

/// The daemon's asks, between its events.
#[derive(Debug, Default)]
pub struct Asks {
    pub login_wanted: VecDeque<DownloadId>,
    pub countdown: Option<Countdown>,
    /// The download the New link window is for.
    pub refresh: Option<DownloadId>,
    pub refresh_waiting: Option<DownloadId>,
    pub refresh_mismatch: Option<(DownloadId, String, String)>,
    pub packages: Vec<Package>,
    /// The package the password window asks for.
    pub password: Option<PackageId>,
    pub links: Option<PickedLinks>,
    pub videos: Option<PickedVideos>,
    pub gallery: Option<PickedGallery>,
    /// The quota's state, for the banner.
    pub quota: Option<(ByteCount, ByteCount, bool)>,
    pub limit_lifted: bool,
    /// The update check's answer: a newer release, none, or why it failed.
    pub update: Option<Result<Option<Release>, String>>,
    pub checking: bool,
    /// Install now: `None` while it downloads, or why it failed.
    pub installing: Option<Option<String>>,
    next_request: u64,
}

impl Asks {
    pub fn request(&mut self) -> u64 {
        self.next_request += 1;
        self.next_request
    }
}

/// Take the daemon's `event` into the asks. `at` is when it came.
pub fn take(state: &mut AppState, event: &Event, at: Instant) {
    let lang = crate::projection::language(state);
    let a = &mut state.asks;
    match event {
        Event::Failed { id, error, .. } if login_wanted(error).is_some() => {
            if !a.login_wanted.contains(id) {
                a.login_wanted.push_back(*id);
            }
        }
        Event::Countdown { action, seconds, queue, file } => {
            a.countdown = Some(Countdown {
                action: *action,
                deadline: at + std::time::Duration::from_secs(u64::from(*seconds)),
                queue: queue.clone(),
                file: file.clone(),
            });
        }
        Event::CountdownCanceled => a.countdown = None,
        Event::RefreshWaiting { id } => a.refresh_waiting = *id,
        Event::RefreshMismatch { id, url, reason } => {
            a.refresh_mismatch = Some((*id, url.to_string(), crate::errors::failure(lang, reason)));
        }
        Event::Packages { items } => {
            // Ask once each time a package comes to need its password, not
            // for one that needed it when the window connected.
            let known = !a.packages.is_empty();
            for p in items {
                let needs = matches!(p.unpack, Unpack::NeedsPassword { .. });
                let needed =
                    a.packages.iter().any(|o| o.id == p.id && matches!(o.unpack, Unpack::NeedsPassword { .. }));
                if needs && known && !needed && a.password.is_none() {
                    a.password = Some(p.id);
                }
            }
            a.packages = items.clone();
        }
        Event::PickLinks { page, links, user_agent } => {
            let request = a.request();
            a.links = Some(PickedLinks {
                page: page.clone(),
                links: links.clone(),
                user_agent: user_agent.clone(),
                request,
                info: HashMap::new(),
                filter: default_link_filter(),
            });
        }
        Event::LinkInfo { request, index, total, content_type, .. } => {
            if let Some(l) = a.links.as_mut().filter(|l| l.request == *request) {
                l.info.insert(*index, (*total, content_type.clone()));
            }
        }
        Event::PickVideos { page, playlist, user_agent } => {
            a.videos = Some(PickedVideos {
                page: page.clone(),
                playlist: playlist.clone().cleaned(),
                user_agent: user_agent.clone(),
            });
        }
        Event::PickGallery { page, user_agent } => {
            let request = a.request();
            a.gallery =
                Some(PickedGallery { page: page.clone(), user_agent: user_agent.clone(), request, result: None });
        }
        Event::Gallery { request, gallery, error, .. } => {
            if let Some(g) = a.gallery.as_mut().filter(|g| g.request == *request) {
                g.result = Some(match (gallery, error) {
                    (_, Some(e)) => Err(crate::errors::failure(lang, e)),
                    (Some(found), None) => Ok(found.clone()),
                    (None, None) => Err(String::new()),
                });
            }
        }
        Event::Quota { used, limit, paused, .. } => a.quota = Some((*used, *limit, *paused)),
        Event::LimitLifted { lifted } => a.limit_lifted = *lifted,
        Event::Update { newer, error } => {
            a.checking = false;
            a.update = Some(match error {
                Some(e) => Err(crate::errors::failure(lang, e)),
                None => Ok(newer.clone()),
            });
        }
        Event::UpdateFailed { error } => a.installing = Some(Some(crate::errors::failure(lang, error))),
        Event::Removed { id } => {
            a.login_wanted.retain(|x| x != id);
            if a.refresh == Some(*id) {
                a.refresh = None;
            }
        }
        _ => {}
    }
    // A login is asked only of downloads still failed for one.
    let table = &state.table;
    state.asks.login_wanted.retain(|id| table.get(*id).is_some_and(|e| e.status == Status::Failed));
}

/// The downloads waiting for the login the first one wants, and what it
/// wants.
pub fn login_group(state: &AppState) -> Option<(Vec<DownloadId>, udm_domain::download::LoginWanted)> {
    let first = state.asks.login_wanted.front()?;
    let wanted = state.table.get(*first).and_then(|e| e.error.as_deref()).and_then(login_wanted)?;
    let ids = state
        .asks
        .login_wanted
        .iter()
        .filter(|id| {
            state
                .table
                .get(**id)
                .and_then(|e| e.error.as_deref())
                .and_then(login_wanted)
                .is_some_and(|w| w.host == wanted.host)
        })
        .copied()
        .collect();
    Some((ids, wanted))
}

pub fn login_view(state: &AppState) -> Option<LoginView> {
    let (ids, wanted) = login_group(state)?;
    let own_site = ids
        .iter()
        .filter_map(|id| state.table.get(*id))
        .all(|e| udm_domain::site::host_of(e.url.as_str()).eq_ignore_ascii_case(&wanted.host));
    let user = state
        .settings
        .as_ref()
        .and_then(|s| s.site_logins.iter().find(|l| l.site.eq_ignore_ascii_case(&wanted.host)))
        .map(|l| l.username.clone())
        .or_else(|| ids.first().and_then(|id| state.table.get(*id)).and_then(|e| e.login_user.clone()))
        .unwrap_or_default();
    Some(LoginView {
        host: wanted.host,
        realm: wanted.realm.unwrap_or_default(),
        refused: wanted.refused,
        own_site,
        user,
        count: ids.len() as u32,
    })
}

/// The countdown before the computer sleeps or shuts down.
pub fn countdown_view(ctx: &Ctx, state: &AppState) -> Option<CountdownView> {
    let c = state.asks.countdown.as_ref()?;
    let lang = ctx.lang;
    let seconds = c.deadline.saturating_duration_since(ctx.now).as_secs() as u32;
    let left = seconds.to_string();
    let message = lang.tr(match c.action {
        WhenDone::Sleep => "This computer will go to sleep in {seconds} seconds.",
        WhenDone::Hibernate => "This computer will hibernate in {seconds} seconds.",
        WhenDone::ShutDown => "This computer will shut down in {seconds} seconds.",
        WhenDone::Exit | WhenDone::Nothing => "This computer will continue in {seconds} seconds.",
    });
    let done = match &c.file {
        Some(name) => fill(lang.tr("{name} has finished downloading."), &[("name", name)]),
        None => fill(lang.tr("The queue \"{queue}\" is done."), &[("queue", &c.queue)]),
    };
    let now = lang.tr(match c.action {
        WhenDone::Sleep => "Sleep now",
        WhenDone::Hibernate => "Hibernate now",
        _ => "Shut down now",
    });
    Some(CountdownView {
        title: lang.tr(if c.file.is_some() { "Download finished" } else { "Queue finished" }).to_string(),
        done,
        message: fill(message, &[("seconds", &left)]),
        now: now.to_string(),
        seconds,
    })
}

pub fn refresh_view(state: &AppState) -> Option<RefreshView> {
    let id = state.asks.refresh?;
    let e = state.table.get(id)?;
    let mismatch = state
        .asks
        .refresh_mismatch
        .as_ref()
        .filter(|m| m.0 == id)
        .map(|(_, url, reason)| RefreshMismatchView { url: url.clone(), reason: reason.clone() });
    Some(RefreshView {
        id: Id(id.get()),
        name: e.display_name(),
        address: e.url.to_string(),
        waiting: state.asks.refresh_waiting == Some(id),
        mismatch,
        connected: state.connected(),
    })
}

/// Whether the New link window may open for a download: it is stopped.
pub fn can_refresh(state: &AppState, id: DownloadId) -> bool {
    state.table.get(id).is_some_and(|e| e.status.is_retryable())
}

pub fn password_view(state: &AppState) -> Option<PasswordView> {
    let id = state.asks.password?;
    let p = state.asks.packages.iter().find(|p| p.id == id)?;
    Some(PasswordView {
        id: Id(id.get()),
        name: p.name.to_string(),
        wrong: matches!(p.unpack, Unpack::NeedsPassword { wrong: true }),
    })
}

/// What a package's menu offers: unpacking once all its files are in.
pub fn package_menu(state: &AppState, p: &Package) -> PackageMenu {
    let connected = state.connected();
    let all_in = p.downloads.iter().all(|id| state.table.get(*id).is_some_and(|e| e.status == Status::Done));
    let can = connected && all_in && p.can_unpack();
    let again = matches!(p.unpack, Unpack::Done { .. });
    PackageMenu {
        id: Id(p.id.get()),
        unpack: can || (connected && all_in && again),
        again,
        password: can,
        opened: again,
        connected,
    }
}

/// A package's node label: its name, how many of its files are in, and how
/// its unpacking stands.
pub fn package_label(lang: &'static Language, state: &AppState, p: &Package) -> String {
    let done = p.downloads.iter().filter(|id| state.table.get(**id).is_some_and(|e| e.status == Status::Done)).count();
    let count = format!("{done}/{}", p.downloads.len());
    let how = match &p.unpack {
        Unpack::Waiting => None,
        Unpack::Unpacking => Some("unpacking"),
        Unpack::NeedsPassword { .. } => Some("needs a password"),
        Unpack::Done { .. } => Some("unpacked"),
        Unpack::Failed { .. } => Some("not unpacked"),
    };
    match how {
        Some(how) => format!("{} ({count}, {})", p.name, lang.tr(how)),
        None => format!("{} ({count})", p.name),
    }
}

fn queue_items(lang: &'static Language, state: &AppState) -> Vec<QueueItem> {
    state
        .queues
        .iter()
        .map(|q| QueueItem { id: Id(q.id.get()), name: lang.tr(&q.name).into(), running: q.running })
        .collect()
}

/// A link's file name: the last part of its path, decoded.
fn link_name(l: &PageLink) -> String {
    let path = l.url.split(['?', '#']).next().unwrap_or(&l.url);
    let tail = path.rsplit('/').next().unwrap_or("");
    let tail = percent_decode(tail);
    if tail.is_empty() {
        l.url.clone()
    } else {
        tail
    }
}

fn percent_decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s).decode_utf8_lossy().into_owned()
}

/// A link's file type: its name's extension in lower case, or "" for none.
fn link_type(l: &PageLink) -> String {
    extension(&link_name(l))
}

/// The places of the links `filter` leaves in view, in order.
pub fn visible_links(links: &[PageLink], filter: &LinkFilter) -> Vec<usize> {
    let types = udm_domain::settings::parse_extensions(&filter.types);
    let mut seen = std::collections::HashSet::new();
    (0..links.len())
        .filter(|&i| {
            let l = &links[i];
            let ext = link_type(l);
            if filter.hide_repeated && !seen.insert(l.url.split('#').next().unwrap_or(&l.url).to_string()) {
                return false;
            }
            if filter.hide_images && l.image {
                return false;
            }
            if filter.hide_pages && !l.image && PAGE_TYPES.contains(&ext.as_str()) {
                return false;
            }
            types.is_empty() || types.contains(&ext)
        })
        .collect()
}

/// The name of file `n` (from 1) of `count` renamed by `pattern`: `*` (or
/// the end of the name, without one) becomes `n`, padded with zeros to the
/// width of `count`. A pattern without its own extension keeps `ext`.
pub fn pattern_name(pattern: &str, n: usize, count: usize, ext: &str) -> String {
    let width = count.max(1).to_string().len();
    let number = format!("{n:0width$}");
    let pattern = pattern.trim();
    let mut name = if pattern.contains('*') {
        pattern.replacen('*', &number, 1).replace('*', "")
    } else {
        format!("{pattern}{number}")
    };
    // `photo_*.jpg` names its extension; `photo_*` keeps each file's.
    let after_star = pattern.rsplit('*').next().unwrap_or("");
    if !ext.is_empty() && !after_star.contains('.') {
        name = format!("{name}.{ext}");
    }
    name
}

/// The ticked links (`indices`) the filters leave in view, in order.
fn ticked_links(l: &PickedLinks, indices: &[u32]) -> Vec<usize> {
    visible_links(&l.links, &l.filter).into_iter().filter(|i| indices.contains(&(*i as u32))).collect()
}

/// The names the ticked links get from the rename pattern `rename`: all of
/// them, or none when it is empty.
pub fn renamed_links(l: &PickedLinks, indices: &[u32], rename: &str) -> Result<Vec<String>, PickError> {
    if rename.trim().is_empty() {
        return Ok(Vec::new());
    }
    if rename.trim().chars().any(|c| matches!(c, '\\' | '/' | ':' | '"' | '<' | '>' | '|' | '?')) {
        return Err(PickError::BadRename);
    }
    let ticked = ticked_links(l, indices);
    Ok(ticked
        .iter()
        .enumerate()
        .map(|(n, &i)| pattern_name(rename, n + 1, ticked.len(), &link_type(&l.links[i])))
        .collect())
}

fn extension(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() && !ext.contains('/') => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
}

pub fn links_view(ctx: &Ctx, state: &AppState) -> Option<PickLinksView> {
    let lang = ctx.lang;
    let l = state.asks.links.as_ref()?;
    let mut types: Vec<String> = Vec::new();
    let rows = visible_links(&l.links, &l.filter)
        .into_iter()
        .map(|i| {
            let link = &l.links[i];
            let name = link_name(link);
            let ext = extension(&name);
            if !ext.is_empty() && !types.contains(&ext) {
                types.push(ext.clone());
            }
            let info = l.info.get(&i);
            PickRow {
                index: i as u32,
                icon: format::file_icon(&name),
                kind: info.and_then(|(_, t)| t.clone()).unwrap_or_else(|| ext.to_uppercase()),
                size: info.and_then(|(s, _)| *s).map(|s| format::bytes(lang, s, 2)).unwrap_or_default(),
                url: link.url.clone(),
                text: link.text.trim().to_string(),
                name,
            }
        })
        .collect();
    types.sort();
    let suggested = l
        .links
        .first()
        .map(link_name)
        .and_then(|n| Path::new(&n).file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| udm_domain::site::host_of(&l.page));
    Some(PickLinksView {
        page: l.page.clone(),
        rows,
        filter: l.filter.clone(),
        total: l.links.len() as u32,
        types,
        suggested_package: suggested,
        queues: queue_items(lang, state),
        connected: state.connected(),
    })
}

/// The downloads for the ticked links the filters leave in view, named by
/// the rename pattern when there is one, and how they are kept.
pub fn link_downloads(
    l: &PickedLinks,
    indices: &[u32],
    package: Option<&str>,
    rename: &str,
    settings: &Settings,
    downloads_dir: &Path,
) -> Result<(Vec<Download>, Packaging), PickError> {
    let names = renamed_links(l, indices, rename)?;
    let (folder, packaging) = match package.map(str::trim).filter(|p| !p.is_empty()) {
        Some(name) => {
            let name = PackageName::parse(name)?;
            let folder = settings
                .category_folder(GENERAL, downloads_dir)
                .join(name.folder_name().as_str())
                .display()
                .to_string();
            (Some(folder.clone()), Packaging::One { name, folder: Some(folder) })
        }
        None => (None, Packaging::Archives),
    };
    let items = ticked_links(l, indices)
        .into_iter()
        .enumerate()
        .map(|(n, i)| (&l.links[i], names.get(n).cloned()))
        .map(|(link, filename)| Download {
            url: link.url.as_str().into(),
            filename,
            referrer: Some(l.page.clone()),
            page: Some(l.page.clone()),
            cookies: link.cookies.clone(),
            user_agent: l.user_agent.clone(),
            folder: folder.clone(),
            start: Some(false),
            ..Download::default()
        })
        .collect();
    Ok((items, packaging))
}

/// The probes the links window asks of the links' servers.
pub fn link_probes(l: &PickedLinks) -> Vec<Download> {
    l.links
        .iter()
        .map(|link| Download {
            url: link.url.as_str().into(),
            referrer: Some(l.page.clone()),
            cookies: link.cookies.clone(),
            user_agent: l.user_agent.clone(),
            ..Download::default()
        })
        .collect()
}

fn quality_label(lang: &'static Language, q: PlaylistQuality) -> String {
    match q {
        PlaylistQuality::Best => lang.tr("Best quality").to_string(),
        PlaylistQuality::UpTo(h) => fill(lang.tr("Up to {height}p"), &[("height", &h.to_string())]),
        PlaylistQuality::Sound(t) => {
            let name = match t {
                Target::Mp3 => "MP3",
                Target::M4a => "M4A",
                Target::Opus => "Opus",
                Target::Flac => "FLAC",
                _ => "audio",
            };
            fill(lang.tr("Sound only, {format}"), &[("format", name)])
        }
    }
}

pub fn videos_view(ctx: &Ctx, state: &AppState) -> Option<PickVideosView> {
    let lang = ctx.lang;
    let v = state.asks.videos.as_ref()?;
    let rows = v
        .playlist
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| VideoRow {
            index: i as u32,
            title: e.title.clone(),
            duration: e.duration.map(|d| format::duration(lang, d)).unwrap_or_default(),
            url: e.url.clone(),
        })
        .collect();
    Some(PickVideosView {
        title: v.playlist.title.clone(),
        rows,
        qualities: PlaylistQuality::CHOICES.iter().map(|q| quality_label(lang, *q)).collect(),
        queues: queue_items(lang, state),
        connected: state.connected(),
    })
}

pub fn video_downloads(v: &PickedVideos, indices: &[u32], quality: u32) -> Vec<Download> {
    let q = PlaylistQuality::CHOICES.get(quality as usize).copied().unwrap_or(PlaylistQuality::Best);
    let picked: Vec<usize> = indices.iter().map(|i| *i as usize).collect();
    let mut items = v.playlist.downloads(&picked, q, &v.page);
    for item in &mut items {
        item.user_agent = v.user_agent.clone();
    }
    items
}

/// The folder a gallery's pictures go in by default.
pub fn gallery_default_folder(state: &AppState, downloads_dir: &Path) -> String {
    let settings = state.settings.clone().unwrap_or_default();
    let base = settings.category_folder(GENERAL, downloads_dir);
    match state.asks.gallery.as_ref().and_then(|g| g.result.as_ref()).and_then(|r| r.as_ref().ok()) {
        Some(g) => gallery_folder(&base, &g.title).display().to_string(),
        None => base.display().to_string(),
    }
}

pub fn gallery_view(ctx: &Ctx, state: &AppState, downloads_dir: &Path) -> Option<GalleryView> {
    let lang = ctx.lang;
    let g = state.asks.gallery.as_ref()?;
    let (title, rows, error) = match &g.result {
        None => (String::new(), Vec::new(), None),
        Some(Err(why)) => (
            String::new(),
            Vec::new(),
            Some(if why.is_empty() { lang.tr("The page holds no pictures.").to_string() } else { why.clone() }),
        ),
        Some(Ok(found)) => (
            found.title.clone(),
            found
                .pictures
                .iter()
                .enumerate()
                .map(|(i, p)| PictureRow {
                    index: i as u32,
                    name: p.name.clone(),
                    size: match (p.width, p.height) {
                        (Some(w), Some(h)) => format!("{w} × {h}"),
                        _ => String::new(),
                    },
                    url: p.url.clone(),
                })
                .collect(),
            None,
        ),
    };
    Some(GalleryView {
        page: g.page.clone(),
        reading: g.result.is_none(),
        title,
        rows,
        error,
        folder: gallery_default_folder(state, downloads_dir),
        queues: queue_items(lang, state),
        connected: state.connected(),
    })
}

/// Why the gallery's pictures cannot be added.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FolderError {
    #[error("Choose the folder to save the files to.")]
    Empty,
    #[error("The folder must be a full path: {0}")]
    NotFull(String),
}

pub fn gallery_downloads(g: &PickedGallery, indices: &[u32], folder: &str) -> Result<Vec<Download>, FolderError> {
    let folder = folder.trim();
    if folder.is_empty() {
        return Err(FolderError::Empty);
    }
    if !Path::new(folder).is_absolute() {
        return Err(FolderError::NotFull(folder.to_string()));
    }
    let Some(Ok(gallery)) = &g.result else { return Ok(Vec::new()) };
    let picked: Vec<usize> = indices.iter().map(|i| *i as usize).collect();
    let mut items = gallery.downloads(&picked, Path::new(folder), &g.page);
    for item in &mut items {
        item.user_agent = g.user_agent.clone();
    }
    Ok(items)
}

/// The banner's text for the quota or a lifted speed limit, when either
/// matters.
pub fn notice(lang: &'static Language, state: &AppState) -> Option<String> {
    if let Some((used, limit, paused)) = state.asks.quota {
        let amount = fill(
            lang.tr("{used} of {limit}"),
            &[("used", &format::bytes(lang, used, 1)), ("limit", &format::bytes(lang, limit, 1))],
        );
        if paused {
            return Some(fill(
                lang.tr("Download quota reached ({amount}); downloads are paused."),
                &[("amount", &amount)],
            ));
        }
        if limit.get() > 0 && used.get() * 10 >= limit.get() * 9 {
            return Some(fill(
                lang.tr("Download quota nearly used: {amount}. Downloads pause when it runs out."),
                &[("amount", &amount)],
            ));
        }
    }
    state.asks.limit_lifted.then(|| {
        lang.tr("The speed limit is lifted while you are away. It applies again when you use the computer.").to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{apply, UiMessage};
    use crate::testing::entry;

    fn event(s: &mut AppState, e: Event) {
        apply(s, UiMessage::Daemon { event: Box::new(e), at: Instant::now(), unix: 0 });
    }

    fn ctx() -> Ctx {
        Ctx {
            lang: crate::i18n::resolve("en", &[]),
            offset: 0,
            today: String::new(),
            now: Instant::now(),
            unix: 1_700_000_000,
            downloads: std::path::PathBuf::from("/home/u/Downloads"),
            flags: true,
        }
    }

    #[test]
    fn a_site_that_wants_a_login_gets_the_prompt() {
        let mut s = AppState::default();
        let mut e = entry(1, "a.zip", Status::Failed, 1, 0);
        e.error = Some(udm_domain::download::needs_login_error("127.0.0.1", Some("files")));
        event(&mut s, Event::Downloads { items: vec![e.clone()] });
        event(&mut s, Event::Failed { id: e.id, url: e.url.clone(), error: e.error.clone().unwrap_or_default() });
        let v = login_view(&s).unwrap();
        assert_eq!((v.host.as_str(), v.count, v.own_site), ("127.0.0.1", 1, true));
    }

    #[test]
    fn the_countdown_counts_down() {
        let mut s = AppState::default();
        event(
            &mut s,
            Event::Countdown { action: WhenDone::ShutDown, seconds: 60, queue: "Main queue".into(), file: None },
        );
        let v = countdown_view(&ctx(), &s).unwrap();
        assert!(v.seconds <= 60 && v.seconds >= 58);
        assert!(v.message.contains("shut down"));
        event(&mut s, Event::CountdownCanceled);
        assert!(countdown_view(&ctx(), &s).is_none());
    }

    #[test]
    fn picked_links_are_named_probed_and_added() {
        let mut s = AppState::default();
        let link = |url: &str| PageLink { url: url.into(), text: " Get it ".into(), image: false, cookies: None };
        event(
            &mut s,
            Event::PickLinks {
                page: "https://e.com/p".into(),
                links: vec![link("https://e.com/f/a%20b.zip"), link("https://e.com/f/c.pdf")],
                user_agent: None,
            },
        );
        let request = s.asks.links.as_ref().unwrap().request;
        event(
            &mut s,
            Event::LinkInfo { request, index: 0, total: Some(ByteCount::new(2048)), content_type: None, error: None },
        );
        let v = links_view(&ctx(), &s).unwrap();
        assert_eq!(v.rows[0].name, "a b.zip");
        assert_eq!(v.rows[0].size, "2.00 KB");
        assert_eq!(v.types, ["pdf", "zip"]);
        let (items, packaging) =
            link_downloads(s.asks.links.as_ref().unwrap(), &[1], None, "", &Settings::default(), Path::new("/tmp/d"))
                .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].referrer.as_deref(), Some("https://e.com/p"));
        assert!(matches!(packaging, Packaging::Archives));
    }

    fn picked(links: Vec<PageLink>) -> PickedLinks {
        PickedLinks {
            page: "https://x.net/".into(),
            links,
            user_agent: None,
            request: 1,
            info: HashMap::new(),
            filter: default_link_filter(),
        }
    }

    fn link(url: &str, image: bool) -> PageLink {
        PageLink { url: url.into(), text: "Get it".into(), image, cookies: Some("s=1".into()) }
    }

    #[test]
    fn filters_hide_pages_images_and_repeats() {
        let mut s = AppState::default();
        let links = vec![
            link("https://x.net/a.zip", false),
            link("https://x.net/about.html", false),
            link("https://x.net/logo.png", true),
            link("https://x.net/a.zip#again", false),
            link("https://x.net/docs/", false),
            link("https://x.net/b.pdf", false),
        ];
        event(&mut s, Event::PickLinks { page: "https://x.net/".into(), links: links.clone(), user_agent: None });
        let v = links_view(&ctx(), &s).unwrap();
        assert_eq!(v.rows.iter().map(|r| r.index).collect::<Vec<_>>(), [0, 5]);
        assert_eq!(v.total, 6);
        let only_pdf = LinkFilter { types: "PDF".into(), ..default_link_filter() };
        apply(&mut s, UiMessage::LinkFilter(only_pdf));
        assert_eq!(links_view(&ctx(), &s).unwrap().rows.iter().map(|r| r.index).collect::<Vec<_>>(), [5]);
        let all = LinkFilter { hide_pages: false, hide_images: false, hide_repeated: false, types: String::new() };
        apply(&mut s, UiMessage::LinkFilter(all.clone()));
        let v = links_view(&ctx(), &s).unwrap();
        assert_eq!((v.rows.len(), v.filter), (6, all));
        // Ticked links the filters hide are not added.
        let l = picked(links);
        let (items, _) = link_downloads(&l, &[0, 1, 5], None, "", &Settings::default(), Path::new("/d")).unwrap();
        let urls: Vec<_> = items.iter().map(|i| i.url.to_string()).collect();
        assert_eq!(urls, ["https://x.net/a.zip", "https://x.net/b.pdf"]);
        assert_eq!(items[0].cookies.as_deref(), Some("s=1"));
    }

    #[test]
    fn a_pattern_names_the_files_in_order() {
        assert_eq!(pattern_name("photo_*", 3, 12, "jpg"), "photo_03.jpg");
        assert_eq!(pattern_name("photo_*.png", 3, 9, "jpg"), "photo_3.png");
        assert_eq!(pattern_name("part", 2, 2, ""), "part2");
        let l = picked(vec![
            link("https://x.net/a.jpg", false),
            link("https://x.net/b.jpg", false),
            link("https://x.net/c.gif", false),
        ]);
        assert_eq!(renamed_links(&l, &[2, 0], "photo_*").unwrap(), ["photo_1.jpg", "photo_2.gif"]);
        assert!(renamed_links(&l, &[0], " ").unwrap().is_empty());
        assert_eq!(renamed_links(&l, &[0], "a/b*"), Err(PickError::BadRename));
        let (items, _) = link_downloads(&l, &[0, 2], None, "photo_*", &Settings::default(), Path::new("/d")).unwrap();
        let names: Vec<_> = items.into_iter().map(|i| i.filename.unwrap()).collect();
        assert_eq!(names, ["photo_1.jpg", "photo_2.gif"]);
    }

    #[test]
    fn a_gallery_needs_a_full_folder() {
        let g = PickedGallery {
            page: "https://e.com/g".into(),
            user_agent: None,
            request: 1,
            result: Some(Ok(Gallery { title: "t".into(), pictures: Vec::new() })),
        };
        assert_eq!(gallery_downloads(&g, &[], "").err(), Some(FolderError::Empty));
        assert_eq!(gallery_downloads(&g, &[], "rel").err(), Some(FolderError::NotFull("rel".into())));
    }

    #[test]
    fn the_quota_shows_when_it_nearly_runs_out() {
        let mut s = AppState::default();
        let lang = crate::i18n::resolve("en", &[]);
        event(
            &mut s,
            Event::Quota { used: ByteCount::new(50), limit: ByteCount::new(100), paused: false, resumes_at: None },
        );
        assert_eq!(notice(lang, &s), None);
        event(
            &mut s,
            Event::Quota { used: ByteCount::new(95), limit: ByteCount::new(100), paused: false, resumes_at: None },
        );
        assert!(notice(lang, &s).unwrap().contains("nearly used"));
    }
}
