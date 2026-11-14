//! The site grabber: its state between the daemon's crawl events, the
//! wizard's fields made from a request and back, and what the window shows.
//! Plain functions, tested without Tauri.

use std::collections::BTreeSet;

use udm_domain::bytes::ByteCount;
use udm_domain::grab::{
    clean_cookies, Filter, GrabFile, GrabRequest, InvalidGrab, Mirror, Project, RunAction, SaveTo, Template,
    LOGOUT_PATTERNS,
};
use udm_domain::id::{DownloadId, QueueId};
use udm_domain::queue::{QueueKind, WhenDone, MAIN_QUEUE};
use udm_domain::settings::{parse_extensions, GENERAL};

use crate::contract::{
    FilterChoice, GrabDraft, GrabPhase, GrabRow, GrabRun, GrabScheduleDraft, GrabStats, GrabberView, Id, QueueItem,
    SaveChoice, SizeUnit, Then, TreeIcon, TreeNode,
};
use crate::i18n::fill;
use crate::projection::{format, Ctx};
use crate::state::AppState;

const KB: u64 = 1024;
const MB: u64 = 1024 * 1024;

/// What the projects folder holds, read on a worker when the grabber opens.
#[derive(Debug, Clone, Default)]
pub struct Library {
    /// The request a new project starts from.
    pub base: GrabRequest,
    pub templates: Vec<Template>,
    pub filters: Vec<Filter>,
    pub recent: Vec<String>,
}

/// The grabber between the daemon's events.
#[derive(Debug, Default)]
pub struct GrabState {
    pub running: bool,
    /// A crawl ran or a project with results was opened.
    pub ran: bool,
    pub pages: u64,
    pub explored: u64,
    pub found: usize,
    pub added: Vec<DownloadId>,
    pub files: Vec<GrabFile>,
    pub checked: Vec<bool>,
    pub stopped: Option<String>,
    /// The login page site's cookies the crawl used, for the files added
    /// from that site.
    pub login_cookies: Option<String>,
    /// The request the results came from.
    pub request: GrabRequest,
    /// The project as last opened or saved.
    pub project: Option<Project>,
    pub results: u32,
    /// The results tree's node.
    pub node: String,
    /// `None` until read, and when there is no projects folder.
    pub library: Option<Library>,
    pub library_read: bool,
}

impl GrabState {
    /// A crawl for `request` began.
    pub fn started(&mut self, request: GrabRequest) {
        self.request = request;
        self.running = true;
        self.ran = false;
        self.pages = 0;
        self.explored = 0;
        self.found = 0;
        self.added.clear();
        self.stopped = None;
        self.node = String::new();
    }

    pub fn progress(&mut self, pages: u64, found: usize, explored: u64, added: Vec<DownloadId>) {
        if self.running {
            (self.pages, self.found, self.explored, self.added) = (pages, found, explored, added);
        }
    }

    pub fn done(
        &mut self,
        files: Vec<GrabFile>,
        pages: u64,
        explored: u64,
        stopped: Option<String>,
        cookies: Option<String>,
    ) {
        if !self.running {
            return;
        }
        self.running = false;
        self.ran = true;
        self.checked = vec![true; files.len()];
        self.found = files.len();
        self.files = files;
        self.pages = pages;
        self.explored = explored;
        self.stopped = stopped;
        self.login_cookies = cookies;
        self.results = self.results.wrapping_add(1);
    }

    /// Show `project`, opened from the projects folder.
    pub fn opened(&mut self, project: Project) {
        self.running = false;
        self.ran = !project.files.is_empty();
        self.files = project.files.clone();
        self.checked = (0..project.files.len()).map(|i| project.checked.get(i).copied().unwrap_or(true)).collect();
        self.found = self.files.len();
        (self.pages, self.explored, self.stopped, self.login_cookies) = (0, 0, None, None);
        self.request = project.request.clone();
        self.project = Some(project);
        self.results = self.results.wrapping_add(1);
        self.node = String::new();
    }

    /// A new project, without results.
    pub fn reset(&mut self) {
        let (library, read, results) = (self.library.take(), self.library_read, self.results);
        *self = GrabState { library, library_read: read, results: results.wrapping_add(1), ..GrabState::default() };
    }

    /// Results the project has not saved.
    pub fn unsaved(&self) -> bool {
        self.ran && self.project.as_ref().is_none_or(|p| p.files != self.files)
    }
}

/// Patterns separated by semicolons, commas or spaces.
fn patterns(text: &str) -> Vec<String> {
    text.split([';', ',']).flat_map(str::split_whitespace).map(str::to_string).collect()
}

fn size_of(size: Option<ByteCount>, default: (u32, SizeUnit)) -> (bool, u32, SizeUnit) {
    match size {
        None => (false, default.0, default.1),
        Some(b) if b.get() % MB == 0 && b.get() >= MB => (true, (b.get() / MB) as u32, SizeUnit::Mb),
        Some(b) => (true, (b.get() / KB).max(1) as u32, SizeUnit::Kb),
    }
}

fn bytes_of(on: bool, n: u32, unit: SizeUnit) -> Option<ByteCount> {
    on.then(|| ByteCount::new(u64::from(n) * if unit == SizeUnit::Mb { MB } else { KB }))
}

/// The wizard's fields for `r`.
pub fn draft(r: &GrabRequest) -> GrabDraft {
    let (min_on, min_size, min_unit) = size_of(r.min_size, (1, SizeUnit::Kb));
    let (max_on, max_size, max_unit) = size_of(r.max_size, (10, SizeUnit::Mb));
    let logouts: Vec<&str> = LOGOUT_PATTERNS.to_vec();
    GrabDraft {
        start: r.start.clone(),
        login_on: r.login_user.is_some(),
        login_user: r.login_user.clone().unwrap_or_default(),
        login_password: String::new(),
        login_page_on: r.login_url.is_some(),
        login_page: r.login_url.clone().unwrap_or_default(),
        logout_on: r.explore_skip.iter().any(|p| logouts.contains(&p.as_str())),
        save: match r.save_to {
            SaveTo::Categories => SaveChoice::Categories,
            SaveTo::Category(_) => SaveChoice::Category,
            SaveTo::Folder(_) => SaveChoice::Folder,
        },
        save_category: match &r.save_to {
            SaveTo::Category(c) => c.clone(),
            _ => GENERAL.to_string(),
        },
        save_folder: match (&r.save_to, &r.mirror) {
            (_, Some(m)) => m.folder.clone(),
            (SaveTo::Folder(f), None) => f.clone(),
            _ => String::new(),
        },
        subfolders: r.subfolders,
        offline: r.mirror.as_ref().is_some_and(|m| m.pages),
        overwrite: r.overwrite || r.mirror.as_ref().is_some_and(|m| m.overwrite),
        whole_site: r.whole_site,
        depth: r.depth,
        other_depth: r.other_depth.unwrap_or(0),
        ignore_popups: r.ignore_popups,
        no_parent: r.no_parent,
        subdomains: r.subdomains,
        render_scripts: r.render_scripts,
        explore_only_on: !r.explore_only.is_empty(),
        explore_only: r.explore_only.join("; "),
        explore_skip_on: !r.explore_skip.is_empty(),
        explore_skip: r.explore_skip.join("; "),
        types: r.types.join(", "),
        skip_types: r.skip_types.join(", "),
        files_this_site: r.files_this_site,
        hide_duplicates: r.hide_duplicates,
        download_found: r.download_found.is_some(),
        min_on,
        min_size,
        min_unit,
        max_on,
        max_size,
        max_unit,
        files_only_on: !r.files_only.is_empty(),
        files_only: r.files_only.join("; "),
        files_skip_on: !r.files_skip.is_empty(),
        files_skip: r.files_skip.join("; "),
        explore_at_once: r.explore_at_once,
        download_at_once: r.download_at_once,
        link_text: r.link_text,
        max_pages: r.max_pages.min(u64::from(u32::MAX)) as u32,
        max_mb: (r.max_bytes.get() / MB).min(u64::from(u32::MAX)) as u32,
        delay_secs: (r.delay_ms / 1000) as u32,
    }
}

/// The request the wizard's fields describe, checked. `base` keeps what the
/// wizard does not show (the pasted login cookies).
pub fn request(d: &GrabDraft, base: &GrabRequest) -> Result<GrabRequest, InvalidGrab> {
    let list = |on: bool, text: &str| if on { patterns(text) } else { Vec::new() };
    let folder = d.save_folder.trim().to_string();
    let mut explore_skip = list(d.explore_skip_on, &d.explore_skip);
    if d.logout_on {
        for p in LOGOUT_PATTERNS {
            if !explore_skip.iter().any(|s| s == p) {
                explore_skip.push(p.to_string());
            }
        }
    } else {
        explore_skip.retain(|p| !LOGOUT_PATTERNS.contains(&p.as_str()));
    }
    let offline = d.offline && d.save == SaveChoice::Folder;
    let login_url = Some(d.login_page.trim().to_string()).filter(|u| d.login_page_on && !u.is_empty());
    let r = GrabRequest {
        start: d.start.trim().to_string(),
        depth: d.depth,
        same_site: base.same_site,
        types: parse_extensions(&d.types),
        max_pages: u64::from(d.max_pages),
        max_bytes: ByteCount::mib(u64::from(d.max_mb)),
        delay_ms: u64::from(d.delay_secs) * 1000,
        explore_only: list(d.explore_only_on, &d.explore_only),
        explore_skip,
        files_only: list(d.files_only_on, &d.files_only),
        files_skip: list(d.files_skip_on, &d.files_skip),
        subdomains: d.subdomains,
        no_parent: d.no_parent,
        min_size: bytes_of(d.min_on, d.min_size, d.min_unit),
        max_size: bytes_of(d.max_on, d.max_size, d.max_unit),
        hide_duplicates: d.hide_duplicates,
        browser_cookies: base.browser_cookies,
        mirror: offline.then(|| Mirror { folder: folder.clone(), pages: true, overwrite: d.overwrite }),
        render_scripts: d.render_scripts,
        ignore_popups: d.ignore_popups,
        explore_at_once: d.explore_at_once,
        download_at_once: d.download_at_once,
        link_text: d.link_text,
        skip_types: parse_extensions(&d.skip_types),
        files_this_site: d.files_this_site,
        save_to: match d.save {
            SaveChoice::Categories => SaveTo::Categories,
            SaveChoice::Category => SaveTo::Category(d.save_category.trim().to_string()),
            SaveChoice::Folder => SaveTo::Folder(folder),
        },
        download_found: d.download_found.then_some(base.download_found.unwrap_or(MAIN_QUEUE)),
        whole_site: d.whole_site,
        other_depth: Some(d.other_depth).filter(|n| *n > 0),
        login_user: Some(d.login_user.trim().to_string()).filter(|u| d.login_on && !u.is_empty()),
        login_cookies: base.login_cookies.as_deref().and_then(clean_cookies).filter(|_| login_url.is_some()),
        login_url,
        subfolders: d.subfolders,
        overwrite: d.overwrite,
    };
    r.validate()?;
    Ok(r)
}

fn then_of(w: WhenDone) -> Then {
    match w {
        WhenDone::Nothing => Then::Nothing,
        WhenDone::Exit => Then::Exit,
        WhenDone::Sleep => Then::Sleep,
        WhenDone::Hibernate => Then::Hibernate,
        WhenDone::ShutDown => Then::ShutDown,
    }
}

/// The schedule sheet's fields for the project `p`, or a new project's.
pub fn schedule_draft(p: Option<&Project>, today: &str) -> GrabScheduleDraft {
    let s = p.and_then(|p| p.schedule.as_ref());
    let repeat = s.and_then(|s| s.repeat_minutes);
    GrabScheduleDraft {
        on: s.is_some(),
        start: s.map_or("03:00".into(), |s| s.start.clone()),
        once: s.is_some_and(|s| s.date.is_some()),
        date: s.and_then(|s| s.date.clone()).unwrap_or_else(|| today.to_string()),
        days: s.map(|s| s.days.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| (0..7).collect()),
        stop_on: s.is_some_and(|s| s.stop.is_some()),
        stop: s.and_then(|s| s.stop.clone()).unwrap_or_else(|| "06:00".into()),
        repeat_on: repeat.is_some(),
        repeat_hours: repeat.map_or(2, |m| m / 60),
        repeat_minutes: repeat.map_or(0, |m| m % 60),
        wake: s.is_none_or(|s| s.wake),
        run: match p.map_or(RunAction::Explore, |p| p.run) {
            RunAction::Explore => GrabRun::Explore,
            RunAction::ExploreAndDownload => GrabRun::ExploreAndDownload,
            RunAction::DownloadChecked => GrabRun::DownloadChecked,
        },
        sync: p.is_some_and(|p| p.kind.is_sync()),
        queue: Id(p.map_or(QueueId::MAIN, |p| p.queue).get()),
        then: then_of(p.map_or(WhenDone::Nothing, |p| p.when_done)),
        force_close: p.is_some_and(|p| p.force_close),
        open_on: p.is_some_and(|p| p.open_when_done.is_some()),
        open_file: p.and_then(|p| p.open_when_done.clone()).unwrap_or_default(),
    }
}

/// `p` with the schedule sheet's fields, checked as a queue's schedule is.
pub fn with_schedule(mut p: Project, d: &GrabScheduleDraft) -> Result<Project, udm_domain::queue::InvalidQueue> {
    let mut days = d.days.clone();
    days.sort_unstable();
    days.dedup();
    let schedule = d.on.then(|| udm_domain::queue::Schedule {
        start: d.start.trim().to_string(),
        stop: d.stop_on.then(|| d.stop.trim().to_string()),
        days: if d.once { Vec::new() } else { days },
        date: d.once.then(|| d.date.trim().to_string()),
        repeat_minutes: d.repeat_on.then_some(d.repeat_hours * 60 + d.repeat_minutes),
        wake: d.wake,
    });
    if let Some(s) = &schedule {
        s.validate()?;
    }
    let open = Some(d.open_file.trim().to_string()).filter(|f| d.open_on && !f.is_empty());
    if let Some(f) = open.as_deref().filter(|f| !std::path::Path::new(f).is_absolute()) {
        return Err(udm_domain::queue::InvalidQueue::OpenNotFull(f.to_string()));
    }
    p.schedule = schedule;
    p.run = match d.run {
        GrabRun::Explore => RunAction::Explore,
        GrabRun::ExploreAndDownload => RunAction::ExploreAndDownload,
        GrabRun::DownloadChecked => RunAction::DownloadChecked,
    };
    p.kind = if d.sync { QueueKind::Synchronize } else { QueueKind::OneTime };
    p.queue = QueueId::new(d.queue.0);
    p.when_done = crate::projection::dialogs::when_done(d.then);
    p.force_close = d.force_close && p.when_done.needs_countdown();
    p.open_when_done = open;
    Ok(p)
}

/// The project named `name` holding `request` and the results, keeping
/// what an earlier save of it had (its schedule and history).
pub fn project(name: &str, request: GrabRequest, s: &GrabState, checked: &[bool]) -> Project {
    let checked = (0..s.files.len()).map(|i| checked.get(i).copied().unwrap_or(true)).collect();
    match s.project.clone() {
        Some(p) => Project { name: name.trim().to_string(), request, files: s.files.clone(), checked, ..p },
        None => Project {
            name: name.trim().to_string(),
            request,
            files: s.files.clone(),
            checked,
            schedule: None,
            run: RunAction::Explore,
            download_new: false,
            queue: QueueId::MAIN,
            when_done: WhenDone::Nothing,
            open_when_done: None,
            force_close: false,
            seen: Vec::new(),
            last_scheduled_day: None,
            last_scheduled_start: None,
            kind: QueueKind::OneTime,
        },
    }
}

fn host(url: &str) -> String {
    udm_domain::site::host_of(url)
}

const ALL: &str = "all";
const LINKS: &str = "links";
const FOLDERS: &str = "folders";

/// The results tree: every file, by the page that links to it, by site.
fn tree(ctx: &Ctx, s: &GrabState) -> Vec<TreeNode> {
    let lang = ctx.lang;
    let selected = |key: &str| if s.node.is_empty() { key == ALL } else { s.node == key };
    let mut out = vec![TreeNode {
        key: ALL.into(),
        label: lang.tr("All Files").into(),
        depth: 0,
        icon: TreeIcon::AllDownloads,
        open: None,
        selected: selected(ALL),
        tip: None,
    }];
    let pages: BTreeSet<&str> = s.files.iter().map(|f| f.page.as_str()).collect();
    out.push(TreeNode {
        key: LINKS.into(),
        label: lang.tr("Link View").into(),
        depth: 0,
        icon: TreeIcon::Grabber,
        open: Some(true),
        selected: false,
        tip: None,
    });
    for p in pages {
        let key = format!("page/{p}");
        let label = p.split_once("://").map_or(p, |(_, rest)| rest).to_string();
        out.push(TreeNode {
            selected: selected(&key),
            key,
            label,
            depth: 1,
            icon: TreeIcon::Grabber,
            open: None,
            tip: None,
        });
    }
    let hosts: BTreeSet<String> = s.files.iter().map(|f| host(&f.url)).collect();
    out.push(TreeNode {
        key: FOLDERS.into(),
        label: lang.tr("Folder View").into(),
        depth: 0,
        icon: TreeIcon::Queues,
        open: Some(true),
        selected: false,
        tip: None,
    });
    for h in hosts {
        let key = format!("host/{h}");
        out.push(TreeNode {
            selected: selected(&key),
            label: h,
            key,
            depth: 1,
            icon: TreeIcon::Finished,
            open: None,
            tip: None,
        });
    }
    out
}

fn shown(s: &GrabState, f: &GrabFile) -> bool {
    match s.node.split_once('/') {
        Some(("page", page)) => f.page == page,
        Some(("host", h)) => host(&f.url) == h,
        _ => true,
    }
}

pub fn view(ctx: &Ctx, state: &AppState) -> GrabberView {
    let lang = ctx.lang;
    let s = &state.grab;
    let settings = state.settings.clone().unwrap_or_default();
    let count = |n: u64| n.to_string();
    let phase = match (s.running, s.ran) {
        (true, _) => GrabPhase::Running,
        (false, true) => GrabPhase::Done,
        (false, false) => GrabPhase::Setup,
    };
    let rows = s
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| shown(s, f))
        .map(|(i, f)| {
            let ext = f.name.rsplit_once('.').map(|(_, e)| e.to_uppercase()).unwrap_or_default();
            GrabRow {
                index: i as u32,
                name: f.name.clone(),
                icon: format::file_icon(&f.name),
                kind: if ext.is_empty() { String::new() } else { fill(lang.tr("{type} file"), &[("type", &ext)]) },
                size: f.size.map(|b| format::bytes(lang, b, 2)).unwrap_or_default(),
                link_text: f.text.clone().unwrap_or_default(),
                url: f.url.clone(),
                page: f.page.clone(),
            }
        })
        .collect();
    let library = s.library.clone().unwrap_or_default();
    let mut categories = vec![GENERAL.to_string()];
    categories.extend(settings.categories.iter().filter(|c| c.name != GENERAL).map(|c| c.name.clone()));
    GrabberView {
        base: draft(&library.base),
        library: s.library.is_some(),
        templates: library.templates.iter().map(|t| t.name.clone()).collect(),
        filters: library
            .filters
            .iter()
            .map(|f| FilterChoice { name: f.name.clone(), mask: f.mask.clone(), types: f.types().join(", ") })
            .collect(),
        recent: library.recent.clone(),
        categories,
        queues: state
            .queues
            .iter()
            .map(|q| QueueItem { id: Id(q.id.get()), name: lang.tr(&q.name).into(), running: q.running })
            .collect(),
        phase,
        stats: GrabStats {
            status: lang
                .tr(match phase {
                    GrabPhase::Running => "Exploring",
                    GrabPhase::Done if s.stopped.is_some() => "Stopped",
                    GrabPhase::Done => "Complete",
                    GrabPhase::Setup => "Ready",
                })
                .into(),
            pages: count(s.pages),
            explored: count(s.explored),
            matched: count(s.found as u64),
            added: count(s.added.len() as u64),
            stopped: s.stopped.clone(),
        },
        results: s.results,
        checked: s.checked.clone(),
        tree: tree(ctx, s),
        rows,
        project: s.project.as_ref().map(|p| p.name.clone()).unwrap_or_default(),
        unsaved: s.unsaved(),
        add_on_close: settings.grab_add_on_close,
        schedule: schedule_draft(s.project.as_ref(), &ctx.today),
        wake_warning: crate::scheduler::wake_warning(lang, state.wake_timers),
        connected: state.connected(),
    }
}

/// The ticked files, by their place in the results.
pub fn ticked(s: &GrabState, indices: &[u32]) -> Vec<GrabFile> {
    indices.iter().filter_map(|i| s.files.get(*i as usize).cloned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::resolve;

    fn file(name: &str, page: &str) -> GrabFile {
        GrabFile {
            url: format!("https://e.com/f/{name}"),
            name: name.into(),
            page: page.into(),
            size: None,
            text: None,
        }
    }

    #[test]
    fn the_default_request_survives_the_wizard() {
        let r = GrabRequest { start: "https://e.com/".into(), ..GrabRequest::default() };
        let back = request(&draft(&r), &r).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn the_wizard_fills_the_request() {
        let mut d = draft(&GrabRequest::default());
        d.start = "https://e.com/index.html".into();
        d.types = "*.pdf, zip".into();
        d.save = SaveChoice::Folder;
        // A full path on the system the test runs on.
        let folder = if cfg!(windows) { r"C:\Users\u\site" } else { "/home/u/site" };
        d.save_folder = folder.into();
        d.offline = true;
        d.max_on = true;
        d.max_size = 5;
        d.max_unit = SizeUnit::Mb;
        d.explore_only_on = true;
        d.explore_only = "e.com/docs/*; e.com/files/*".into();
        d.other_depth = 1;
        let r = request(&d, &GrabRequest::default()).unwrap();
        assert_eq!(r.types, ["*.pdf", "zip"]);
        assert_eq!(r.mirror.as_ref().map(|m| m.folder.as_str()), Some(folder));
        assert_eq!(r.max_size, Some(ByteCount::new(5 * MB)));
        assert_eq!(r.explore_only, ["e.com/docs/*", "e.com/files/*"]);
        assert_eq!(r.other_depth, Some(1));
    }

    #[test]
    fn a_bad_start_page_is_refused() {
        let d = draft(&GrabRequest::default());
        assert!(request(&d, &GrabRequest::default()).is_err());
    }

    #[test]
    fn the_logout_box_adds_and_takes_the_patterns() {
        let mut d = draft(&GrabRequest { start: "https://e.com/".into(), ..GrabRequest::default() });
        assert!(d.logout_on);
        d.logout_on = false;
        let r = request(&d, &GrabRequest::default()).unwrap();
        assert!(r.explore_skip.is_empty());
    }

    #[test]
    fn results_tick_every_file_and_filter_by_page() {
        let mut s = AppState::default();
        s.grab.started(GrabRequest::default());
        s.grab.done(vec![file("a.pdf", "https://e.com/1"), file("b.pdf", "https://e.com/2")], 3, 4, None, None);
        assert_eq!(s.grab.checked, [true, true]);
        assert!(s.grab.unsaved());
        let ctx = Ctx {
            lang: resolve("en", &[]),
            offset: 0,
            today: String::new(),
            now: std::time::Instant::now(),
            unix: 1_700_000_000,
            downloads: std::path::PathBuf::from("/home/u/Downloads"),
            flags: true,
        };
        let v = view(&ctx, &s);
        assert_eq!(v.phase, GrabPhase::Done);
        assert_eq!(v.rows.len(), 2);
        assert!(v.tree.iter().any(|n| n.key == "page/https://e.com/2"));
        s.grab.node = "page/https://e.com/2".into();
        let v = view(&ctx, &s);
        assert_eq!(v.rows.iter().map(|r| r.index).collect::<Vec<_>>(), [1]);
        assert_eq!(v.rows[0].kind, "PDF file");
    }

    #[test]
    fn a_project_schedule_round_trips_and_is_checked() {
        let s = GrabState::default();
        let p = project("site", GrabRequest::default(), &s, &[]);
        let mut d = schedule_draft(Some(&p), "2026-11-12");
        assert!(!d.on);
        d.on = true;
        d.run = GrabRun::ExploreAndDownload;
        d.then = Then::Sleep;
        let scheduled = with_schedule(p.clone(), &d).unwrap();
        assert_eq!(scheduled.schedule.as_ref().map(|s| s.start.as_str()), Some("03:00"));
        assert_eq!(scheduled.run, RunAction::ExploreAndDownload);
        assert_eq!(schedule_draft(Some(&scheduled), "2026-11-12"), d);
        d.start = "7pm".into();
        assert!(with_schedule(p, &d).is_err());
    }

    #[test]
    fn saving_keeps_the_results_and_ticks() {
        let mut s = GrabState::default();
        s.started(GrabRequest::default());
        s.done(vec![file("a.pdf", "p"), file("b.pdf", "p")], 1, 2, None, None);
        let p = project("site", GrabRequest::default(), &s, &[true, false]);
        assert_eq!(p.checked, [true, false]);
        s.project = Some(p);
        assert!(!s.unsaved());
    }
}
