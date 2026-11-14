//! What the main window shows: the rows the tree's node picks, sorted, the
//! tree itself, the columns and which actions the selection allows; and
//! the patch between two of its views.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;

use udm_domain::download::{DownloadEntry, Status};
use udm_domain::id::{DownloadId, QueueId};
use udm_domain::settings::{Settings, GENERAL};

use super::format;
use super::Ctx;
use crate::contract::{
    Banner, Can, ColumnHead, ColumnKey, FileIcon, Id, MainChrome, MainPatch, MainView, QueueItem, Row, SortDir, Tone,
    TreeIcon, TreeNode,
};
use crate::state::{AppState, Link, Node, Progress, Sort};

/// The node keys the tree uses. A category's node is its branch's key, a
/// slash and its name.
const ALL: &str = "all";
const UNFINISHED: &str = "unfinished";
const FINISHED: &str = "finished";
const GRABBER: &str = "grabber";
const QUEUES: &str = "queues";
const QUEUE: &str = "queue/";
const PACKAGES: &str = "packages";
const PACKAGE: &str = "package/";

/// The node a tree key names.
pub fn node_of(key: &str) -> Option<Node> {
    if let Some(id) = key.strip_prefix(QUEUE) {
        return id.parse().ok().map(|id| Node::Queue(QueueId::new(id)));
    }
    if let Some(id) = key.strip_prefix(PACKAGE) {
        return id.parse().ok().map(|id| Node::Package(udm_domain::id::PackageId::new(id)));
    }
    let (branch, category) = match key.split_once('/') {
        Some((b, c)) => (b, Some(c.to_string())),
        None => (key, None),
    };
    let progress = match branch {
        ALL => Progress::All,
        UNFINISHED => Progress::Unfinished,
        FINISHED => Progress::Finished,
        _ => return None,
    };
    Some(Node::Downloads { progress, category })
}

fn key_of(node: &Node) -> String {
    match node {
        Node::Queue(id) => format!("{QUEUE}{}", id.get()),
        Node::Package(id) => format!("{PACKAGE}{}", id.get()),
        Node::Downloads { progress, category } => {
            let branch = match progress {
                Progress::All => ALL,
                Progress::Unfinished => UNFINISHED,
                Progress::Finished => FINISHED,
            };
            match category {
                Some(c) => format!("{branch}/{c}"),
                None => branch.to_string(),
            }
        }
    }
}

fn settings(state: &AppState) -> std::borrow::Cow<'_, Settings> {
    match &state.settings {
        Some(s) => std::borrow::Cow::Borrowed(s),
        None => std::borrow::Cow::Owned(Settings::default()),
    }
}

/// The file name a row shows: its file's, else its address's.
pub fn display_name(e: &DownloadEntry) -> String {
    e.display_name()
}

/// The category the list files `e` under.
pub fn category_of<'a>(settings: &'a Settings, e: &DownloadEntry) -> &'a str {
    let name = display_name(e);
    settings.listed_under(e.category.as_deref(), &name, e.url.as_str())
}

/// The rows the window's node shows, sorted as the window asks.
pub fn shown(state: &AppState) -> Vec<&DownloadEntry> {
    let settings = settings(state);
    let mut rows: Vec<&DownloadEntry> = match &state.node {
        Node::Queue(id) => match state.queues.iter().find(|q| q.id == *id) {
            Some(q) => q.order.iter().filter_map(|d| state.table.get(*d)).collect(),
            None => Vec::new(),
        },
        Node::Package(id) => match state.asks.packages.iter().find(|p| p.id == *id) {
            Some(p) => p.downloads.iter().filter_map(|d| state.table.get(*d)).collect(),
            None => Vec::new(),
        },
        Node::Downloads { progress, category } => state
            .table
            .entries()
            .filter(|e| e.status != Status::Confirming)
            .filter(|e| match progress {
                Progress::All => true,
                Progress::Unfinished => e.status != Status::Done,
                Progress::Finished => e.status == Status::Done,
            })
            .filter(|e| category.as_deref().is_none_or(|c| category_of(&settings, e) == c))
            .collect(),
    };
    if let Some(sort) = state.sort {
        rows.sort_by(|a, b| compare(sort, a, b, state).then(a.id.cmp(&b.id)));
    }
    rows
}

fn compare(sort: Sort, a: &DownloadEntry, b: &DownloadEntry, state: &AppState) -> Ordering {
    let down = sort.dir == SortDir::Descending;
    let running = |e: &DownloadEntry| state.speed(e.id).filter(|_| e.status == Status::Downloading);
    let lower = |s: Option<&str>| s.map(str::to_lowercase);
    match sort.column {
        ColumnKey::Name => known(Some(display_name(a).to_lowercase()), Some(display_name(b).to_lowercase()), down),
        ColumnKey::Queue => Ordering::Equal,
        ColumnKey::Size => known(a.total, b.total, down),
        ColumnKey::Progress => known(format::fraction(a), format::fraction(b), down),
        ColumnKey::Speed => known(running(a), running(b), down),
        ColumnKey::TimeLeft => known(format::time_left(a, running(a)), format::time_left(b, running(b)), down),
        ColumnKey::Status => known(Some(status_rank(a.status)), Some(status_rank(b.status)), down),
        ColumnKey::LastTry => known(a.last_try, b.last_try, down),
        ColumnKey::SaveTo => known(lower(a.path.as_deref()), lower(b.path.as_deref()), down),
        ColumnKey::Page => known(lower(a.page.as_deref()), lower(b.page.as_deref()), down),
        ColumnKey::Referrer => known(lower(a.referrer.as_deref()), lower(b.referrer.as_deref()), down),
        ColumnKey::Description => known(lower(a.description.as_deref()), lower(b.description.as_deref()), down),
        ColumnKey::Country => known(a.country, b.country, down),
    }
}

/// Compare two values, reversed when `descending`. Empty values sort after
/// the rest either way.
fn known<T: PartialOrd>(a: Option<T>, b: Option<T>, descending: bool) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => {
            let o = a.partial_cmp(&b).unwrap_or(Ordering::Equal);
            if descending {
                o.reverse()
            } else {
                o
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Running downloads first, finished ones last.
fn status_rank(s: Status) -> u8 {
    match s {
        Status::Downloading => 0,
        Status::Queued => 1,
        Status::Confirming => 2,
        Status::Waiting => 3,
        Status::Interrupted => 4,
        Status::Failed => 5,
        Status::Canceled => 6,
        Status::Done => 7,
    }
}

/// Running, or about to: what Stop stops.
pub fn is_running(s: Status) -> bool {
    matches!(s, Status::Queued | Status::Downloading | Status::Confirming)
}

/// Waiting, stopped or failed: what Resume starts.
pub fn is_startable(s: Status) -> bool {
    matches!(s, Status::Waiting | Status::Failed | Status::Canceled | Status::Interrupted)
}

/// Finished or stopped: what Redownload starts over.
pub fn is_finished(s: Status) -> bool {
    s.is_terminal() && s != Status::Waiting
}

/// One row of the list.
pub fn row(ctx: &Ctx, state: &AppState, queued: &HashMap<DownloadId, ()>, e: &DownloadEntry) -> Row {
    let lang = ctx.lang;
    let speed = state.speed(e.id).filter(|_| e.status == Status::Downloading);
    let fraction = format::fraction(e);
    let percent = fraction.map(|f| format!("{}%", lang.number(f64::from(f) * 100.0, 2)));
    let status = match super::torrent::row_status(lang, e, state.torrents.get(&e.id)) {
        Some(torrent) => torrent,
        None => match e.status {
            Status::Downloading => percent.clone().unwrap_or_else(|| format::status_word(lang, e.status).to_string()),
            s => format::status_word(lang, s).to_string(),
        },
    };
    let tone = match e.status {
        Status::Downloading => Tone::Running,
        Status::Failed => Tone::Failed,
        _ => Tone::Normal,
    };
    let name = display_name(e);
    let size = match e.total {
        Some(t) => format::bytes(lang, t, 2),
        None if !e.done.is_zero() => format::bytes(lang, e.done, 2),
        None => String::new(),
    };
    Row {
        id: Id(e.id.get()),
        icon: format::file_icon(&name),
        name,
        queued: queued.contains_key(&e.id) && e.status != Status::Done,
        finished: e.status == Status::Done && e.path.is_some(),
        size,
        status,
        tone,
        fraction,
        progress: percent.unwrap_or_default(),
        time_left: format::time_left(e, speed).map(|s| format::duration(lang, s)).unwrap_or_default(),
        speed: speed.map(|s| format::rate(lang, s, 2)).unwrap_or_default(),
        last_try: e.last_try.map(|t| format::date(lang, t, ctx.offset)).unwrap_or_default(),
        description: e.description.clone().unwrap_or_default(),
        save_to: e
            .path
            .as_deref()
            .and_then(|p| Path::new(p).parent())
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        page: e.page.clone().unwrap_or_default(),
        referrer: e.referrer.clone().unwrap_or_default(),
        country: e.country.map(|c| format::country(c, ctx.flags)).unwrap_or_default(),
    }
}

fn column_title(key: ColumnKey) -> &'static str {
    match key {
        ColumnKey::Name => "File Name",
        ColumnKey::Queue => "Q",
        ColumnKey::Size => "Size",
        ColumnKey::Progress => "Progress",
        ColumnKey::Speed => "Transfer rate",
        ColumnKey::TimeLeft => "Time left",
        ColumnKey::Status => "Status",
        ColumnKey::LastTry => "Last Try Date",
        ColumnKey::SaveTo => "Save to",
        ColumnKey::Page => "Parent web page",
        ColumnKey::Referrer => "Referrer",
        ColumnKey::Description => "Description",
        ColumnKey::Country => "Country",
    }
}

/// The icon a category's node gets: a built-in category's own, else the
/// kind of its first file type that has one.
fn category_icon(name: &str, extensions: &[String]) -> FileIcon {
    match name {
        "Compressed" => FileIcon::Archive,
        "Documents" => FileIcon::Document,
        "Music" => FileIcon::Music,
        "Programs" => FileIcon::Program,
        "Video" => FileIcon::Video,
        _ => extensions
            .iter()
            .map(|e| format::icon_of_extension(e))
            .find(|i| *i != FileIcon::Other)
            .unwrap_or(FileIcon::Other),
    }
}

/// The categories tree, flattened.
pub fn tree(ctx: &Ctx, state: &AppState) -> Vec<TreeNode> {
    let lang = ctx.lang;
    let settings = settings(state);
    let current = key_of(&state.node);
    let mut out = Vec::new();
    let mut push = |key: String, label: &str, depth: u8, icon: TreeIcon, open: Option<bool>| {
        out.push(TreeNode { selected: key == current, key, label: label.to_string(), depth, icon, open, tip: None });
    };
    let branches = [
        (ALL, "All Downloads", TreeIcon::AllDownloads),
        (UNFINISHED, "Unfinished", TreeIcon::Unfinished),
        (FINISHED, "Finished", TreeIcon::Finished),
    ];
    for (key, label, icon) in branches {
        let open = state.open_nodes.contains(key);
        push(key.to_string(), lang.tr(label), 0, icon, Some(open));
        if open {
            for c in settings.categories.iter().filter(|c| c.name != GENERAL) {
                let icon = TreeIcon::Category(category_icon(&c.name, &c.extensions));
                push(format!("{key}/{}", c.name), lang.tr(&c.name), 1, icon, None);
            }
        }
    }
    push(GRABBER.to_string(), lang.tr("Grabber projects"), 0, TreeIcon::Grabber, None);
    let open = state.open_nodes.contains(QUEUES);
    push(QUEUES.to_string(), lang.tr("Queues"), 0, TreeIcon::Queues, Some(open));
    if open {
        for q in &state.queues {
            push(format!("{QUEUE}{}", q.id.get()), lang.tr(&q.name), 1, TreeIcon::Queue, None);
        }
    }
    if !state.asks.packages.is_empty() {
        let open = state.open_nodes.contains(PACKAGES);
        push(PACKAGES.to_string(), lang.tr("Packages"), 0, TreeIcon::Queues, Some(open));
        if open {
            for p in &state.asks.packages {
                let key = format!("{PACKAGE}{}", p.id.get());
                let tip = match &p.unpack {
                    udm_domain::package::Unpack::Failed { reason } => Some(crate::errors::text::unpack(lang, reason)),
                    _ => None,
                };
                out.push(TreeNode {
                    selected: key == current,
                    key,
                    label: crate::extras::package_label(lang, state, p),
                    depth: 1,
                    icon: TreeIcon::Category(FileIcon::Archive),
                    open: None,
                    tip,
                });
            }
        }
    }
    out
}

/// The tray icon's tooltip: how many downloads run, and how fast.
pub fn tooltip(ctx: &Ctx, state: &AppState) -> String {
    let lang = ctx.lang;
    let active = state.table.entries().filter(|e| e.status == Status::Downloading).count();
    let queued = state.table.entries().filter(|e| e.status == Status::Queued).count();
    let speed: f64 = state
        .speeds
        .iter()
        .filter(|(id, _)| state.table.get(**id).is_some_and(|e| e.status == Status::Downloading))
        .map(|(_, s)| s.bytes_per_sec)
        .sum();
    let downloads = match (active, queued) {
        (0, 0) => lang.tr("UDM: no active downloads").to_string(),
        (0, q) => crate::i18n::fill(lang.tr("UDM: {queued} queued"), &[("queued", &q.to_string())]),
        (a, _) => {
            let speed = format::bytes(lang, udm_domain::bytes::ByteCount::new(speed.max(0.0) as u64), 1);
            crate::i18n::fill(
                lang.tr("UDM: {active} downloading at {speed}/s"),
                &[("active", &a.to_string()), ("speed", &speed)],
            )
        }
    };
    if state.asks.limit_lifted {
        format!("{downloads}\n{}", lang.tr("Speed limit lifted while you are away"))
    } else {
        downloads
    }
}

/// Which actions the selection allows.
pub fn can(state: &AppState) -> Can {
    let selected: Vec<&DownloadEntry> = state.selected.iter().filter_map(|id| state.table.get(*id)).collect();
    let all = || state.table.entries();
    let one = (selected.len() == 1).then(|| selected[0]);
    let connected = state.connected();
    let open_with = state.can_open_with;
    Can {
        resume: connected && selected.iter().any(|e| is_startable(e.status)),
        stop: connected && selected.iter().any(|e| is_running(e.status)),
        stop_all: connected && all().any(|e| is_running(e.status)),
        resume_all: connected && all().any(|e| matches!(e.status, Status::Interrupted | Status::Canceled)),
        delete: connected && !selected.is_empty(),
        delete_completed: connected && all().any(|e| e.status == Status::Done),
        open: one.is_some_and(|e| e.status == Status::Done && e.path.is_some()),
        redownload: connected && selected.iter().any(|e| is_finished(e.status)),
        progress: one.is_some_and(|e| e.status != Status::Done),
        refresh: connected && one.is_some_and(|e| crate::extras::can_refresh(state, e.id)),
        convert: connected
            && one.is_some_and(|e| {
                e.status == Status::Done && e.path.as_deref().is_some_and(udm_domain::media::is_video)
            }),
        verify: one.is_some_and(|e| e.status == Status::Done && e.path.is_some()),
        open_with: open_with && one.is_some_and(|e| e.status == Status::Done && e.path.is_some()),
        drag_out: crate::drag::CAN_DRAG,
        torrent: one.is_some_and(|e| e.torrent.is_some() && e.status != Status::Confirming),
        stop_seeding: connected && selected.iter().any(|e| super::torrent::is_seeding(e)),
        connected,
    }
}

fn banner(ctx: &Ctx, state: &AppState) -> Option<Banner> {
    let lang = ctx.lang;
    match &state.link {
        Link::Connecting => Some(Banner { text: lang.tr("Connecting to the UDM daemon...").to_string(), error: false }),
        Link::Lost(why) => {
            let mut text = lang.tr("Lost the connection to the UDM daemon.").to_string();
            if !why.is_empty() {
                text = format!("{text} {why}");
            }
            Some(Banner { text, error: true })
        }
        Link::Connected => state
            .notice
            .as_ref()
            .map(|n| Banner { text: lang.tr(&n.text).to_string(), error: n.error })
            .or_else(|| crate::extras::notice(lang, state).map(|text| Banner { text, error: false })),
    }
}

pub fn chrome(ctx: &Ctx, state: &AppState) -> MainChrome {
    let lang = ctx.lang;
    let columns = state
        .columns
        .iter()
        .map(|(key, width)| ColumnHead {
            key: *key,
            title: lang.tr(column_title(*key)).to_string(),
            width: width.round() as u32,
            sort: state.sort.filter(|s| s.column == *key).map(|s| s.dir),
            sortable: *key != ColumnKey::Queue,
        })
        .collect();
    MainChrome {
        title: "UDM".to_string(),
        columns,
        tree: tree(ctx, state),
        categories_panel: state.categories_panel,
        selected: state.selected.iter().map(|id| Id(id.get())).collect(),
        can: can(state),
        queues: state
            .queues
            .iter()
            .map(|q| QueueItem { id: Id(q.id.get()), name: lang.tr(&q.name).to_string(), running: q.running })
            .collect(),
        banner: banner(ctx, state),
        limiter: crate::tools::limiter(&settings(state)),
        all_columns: crate::tools::column_choices(ctx, state),
        default_columns: crate::tools::column_choices(ctx, &AppState::default()),
        categories: crate::tools::category_inputs(&settings(state)),
        drop_target: state.settings.as_ref().is_some_and(|s| s.drop_target),
        panel_width: state.panel_width.round() as u32,
        list_mono: state.list_font.is_some_and(|f| f.mono),
        list_size: state.list_font.map_or(12, |f| f.size.round() as u32),
        toolbar_small: state.toolbar_small,
        toolbar: state.toolbar.clone(),
        tips: crate::tips::TIPS.iter().map(|t| lang.tr(t).to_string()).collect(),
        next_tip: state.settings.as_ref().map_or(0, |s| (s.next_tip % crate::tips::TIPS.len()) as u32),
        tips_on_startup: state.settings.as_ref().is_some_and(|s| s.tips_on_startup),
        show_tip: !state.tip_seen && state.settings.as_ref().is_some_and(|s| s.tips_on_startup),
        newer: match &state.asks.update {
            Some(Ok(Some(r)))
                if state.layout_rest.get("dismissed_release").and_then(|v| v.as_str()) != Some(r.version.as_str()) =>
            {
                Some(r.version.clone())
            }
            _ => None,
        },
        newer_installable: matches!(&state.asks.update, Some(Ok(Some(r))) if r.setup.is_some()),
        packages: state.asks.packages.iter().map(|p| crate::extras::package_menu(state, p)).collect(),
        report: crate::reports::strip(lang, &state.reports),
        reconnect: matches!(state.link, Link::Lost(_)),
        checksum: match state.selected.iter().collect::<Vec<_>>().as_slice() {
            [id] => {
                state.table.get(**id).and_then(|e| e.checksum.as_ref()).map(ToString::to_string).unwrap_or_default()
            }
            _ => String::new(),
        },
    }
}

pub fn view(ctx: &Ctx, state: &AppState) -> MainView {
    let queued: HashMap<DownloadId, ()> =
        state.queues.iter().flat_map(|q| q.order.iter().map(|id| (*id, ()))).collect();
    let rows = shown(state).into_iter().map(|e| row(ctx, state, &queued, e)).collect();
    MainView { rows, chrome: chrome(ctx, state) }
}

/// The patch from `before` to `after`, or `None` when nothing changed.
pub fn diff(before: &MainView, after: &MainView) -> Option<MainPatch> {
    let old: HashMap<Id, &Row> = before.rows.iter().map(|r| (r.id, r)).collect();
    let rows: Vec<Row> = after.rows.iter().filter(|r| old.get(&r.id) != Some(r)).cloned().collect();
    let same_order =
        before.rows.len() == after.rows.len() && before.rows.iter().zip(&after.rows).all(|(a, b)| a.id == b.id);
    let order = (!same_order).then(|| after.rows.iter().map(|r| r.id).collect());
    let chrome = (before.chrome != after.chrome).then(|| after.chrome.clone());
    if rows.is_empty() && order.is_none() && chrome.is_none() {
        return None;
    }
    Some(MainPatch { rows, order, chrome })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::resolve;
    use crate::state::{apply, UiMessage};
    use std::time::Instant;
    use udm_application::daemon::Event;

    fn ctx() -> Ctx {
        Ctx {
            lang: resolve("en", &[]),
            offset: 0,
            today: "2026-11-12".into(),
            now: std::time::Instant::now(),
            unix: 1_700_000_000,
            downloads: std::path::PathBuf::from("/home/u/Downloads"),
            flags: true,
        }
    }

    fn entry(id: u64, name: &str, status: Status, total: u64) -> DownloadEntry {
        crate::testing::entry(id, name, status, total, 0)
    }

    fn state_with(entries: Vec<DownloadEntry>) -> AppState {
        let mut s = AppState::default();
        let event = Event::Downloads { items: entries };
        apply(&mut s, UiMessage::Daemon { event: Box::new(event), at: Instant::now(), unix: 0 });
        apply(&mut s, UiMessage::Daemon { event: Box::new(Event::Connected), at: Instant::now(), unix: 0 });
        s
    }

    #[test]
    fn nodes_round_trip_through_their_keys() {
        for key in ["all", "unfinished/Music", "finished", "queue/3"] {
            let node = node_of(key).unwrap();
            assert_eq!(key_of(&node), key);
        }
        assert_eq!(node_of("nonsense"), None);
    }

    #[test]
    fn the_finished_node_shows_finished_downloads_sorted_by_size() {
        let mut s = state_with(vec![
            entry(1, "a.zip", Status::Done, 300),
            entry(2, "b.mp3", Status::Downloading, 100),
            entry(3, "c.pdf", Status::Done, 200),
        ]);
        apply(&mut s, UiMessage::SelectNode(node_of("finished").unwrap()));
        apply(&mut s, UiMessage::Sort(ColumnKey::Size));
        let ids: Vec<u64> = shown(&s).iter().map(|e| e.id.get()).collect();
        assert_eq!(ids, [3, 1]);
    }

    #[test]
    fn categories_filter_by_file_type() {
        let mut s = state_with(vec![entry(1, "a.zip", Status::Done, 1), entry(2, "b.mp3", Status::Done, 1)]);
        apply(
            &mut s,
            UiMessage::Daemon {
                event: Box::new(Event::Settings { settings: Settings::default() }),
                at: Instant::now(),
                unix: 0,
            },
        );
        apply(&mut s, UiMessage::SelectNode(node_of("all/Music").unwrap()));
        let ids: Vec<u64> = shown(&s).iter().map(|e| e.id.get()).collect();
        assert_eq!(ids, [2]);
    }

    #[test]
    fn rows_read_as_the_list_shows_them() {
        let s = state_with(vec![entry(1, "a.zip", Status::Done, 70_686)]);
        let v = view(&ctx(), &s);
        let r = &v.rows[0];
        assert_eq!((r.name.as_str(), r.size.as_str(), r.status.as_str()), ("a.zip", "69.03 KB", "Complete"));
        assert_eq!(r.icon, FileIcon::Archive);
        assert_eq!(r.save_to, "/home/u/Downloads");
    }

    #[test]
    fn the_selection_decides_what_the_toolbar_allows() {
        let mut s = state_with(vec![entry(1, "a.zip", Status::Done, 1), entry(2, "b.zip", Status::Failed, 1)]);
        assert!(!can(&s).delete);
        apply(&mut s, UiMessage::Select(vec![DownloadId::new(2)]));
        let c = can(&s);
        assert!(c.resume && c.delete && c.redownload && !c.stop && !c.open);
        apply(&mut s, UiMessage::Select(vec![DownloadId::new(1)]));
        assert!(can(&s).open);
    }

    #[test]
    fn a_patch_carries_only_what_changed() {
        let mut s = state_with(vec![entry(1, "a.zip", Status::Done, 1), entry(2, "b.zip", Status::Failed, 1)]);
        let before = view(&ctx(), &s);
        assert_eq!(diff(&before, &before), None);
        apply(&mut s, UiMessage::Select(vec![DownloadId::new(2)]));
        let after = view(&ctx(), &s);
        let patch = diff(&before, &after).unwrap();
        assert!(patch.rows.is_empty() && patch.order.is_none() && patch.chrome.is_some());
        apply(&mut s, UiMessage::Sort(ColumnKey::Status));
        let sorted = view(&ctx(), &s);
        assert!(diff(&after, &sorted).unwrap().order.is_some());
    }

    #[test]
    fn the_tree_opens_its_branches() {
        let mut s = state_with(vec![]);
        apply(
            &mut s,
            UiMessage::Daemon {
                event: Box::new(Event::Settings { settings: Settings::default() }),
                at: Instant::now(),
                unix: 0,
            },
        );
        let closed = tree(&ctx(), &s).iter().filter(|n| n.key.starts_with("unfinished/")).count();
        assert_eq!(closed, 0);
        apply(&mut s, UiMessage::ToggleNode("unfinished".into()));
        let open = tree(&ctx(), &s).iter().filter(|n| n.key.starts_with("unfinished/")).count();
        assert!(open > 0);
        assert!(tree(&ctx(), &s).iter().any(|n| n.key == "all" && n.selected));
    }
}
