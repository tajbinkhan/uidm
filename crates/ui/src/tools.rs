//! The smaller tools' rules for the window: batch patterns, the list of
//! links to pick from, find, export's choice of downloads, a download's
//! properties, the columns and the categories. Plain functions, tested
//! without Tauri.

use std::path::Path;

use udm_domain::batch::{expand_asterisk, BatchError, Wildcard, MAX_WILDCARD_SIZE};
use udm_domain::download::{Download, DownloadEntry, DownloadLogin, Properties, Status};
use udm_domain::id::{DownloadId, QueueId};
use udm_domain::settings::{parse_extensions, Category, InvalidSettings, Settings, GENERAL};

use crate::contract::{
    BatchInput, BatchPreview, CategoryInput, ColumnChoice, ColumnKey, ExportScope, FindInput, Id, LimiterDraft,
    LinkRow, LinksView, PropertiesInput, PropertiesView, QueueItem,
};
use crate::i18n::fill;
use crate::projection::{format, main, Ctx};
use crate::state::AppState;

/// Every column, in the order the chooser lists the hidden ones.
pub const ALL_COLUMNS: [ColumnKey; 13] = [
    ColumnKey::Name,
    ColumnKey::Queue,
    ColumnKey::Size,
    ColumnKey::Status,
    ColumnKey::TimeLeft,
    ColumnKey::Speed,
    ColumnKey::LastTry,
    ColumnKey::Description,
    ColumnKey::Progress,
    ColumnKey::SaveTo,
    ColumnKey::Page,
    ColumnKey::Referrer,
    ColumnKey::Country,
];

fn addresses(input: &BatchInput) -> Result<Vec<udm_domain::address::Address>, BatchError> {
    let kind = if input.letters { Wildcard::Letters } else { Wildcard::Numbers };
    let size = (input.size as usize).clamp(1, MAX_WILDCARD_SIZE);
    expand_asterisk(&input.address, kind, &input.from, &input.to, size)
}

/// The first, second and last addresses `input` makes.
pub fn batch_preview(input: &BatchInput) -> Result<BatchPreview, BatchError> {
    let all = addresses(input)?;
    let at = |i: usize| all.get(i).map(|a| a.to_string()).unwrap_or_default();
    Ok(BatchPreview {
        first: at(0),
        second: at(1),
        last: if all.len() > 2 { at(all.len() - 1) } else { String::new() },
        count: all.len() as u32,
    })
}

/// The downloads `input` makes, each waiting in a queue, with `login`.
pub fn batch_downloads(input: &BatchInput, login: Option<DownloadLogin>) -> Result<Vec<Download>, BatchError> {
    Ok(addresses(input)?
        .into_iter()
        .map(|a| Download { url: a.into(), start: Some(false), login: login.clone(), ..Download::default() })
        .collect())
}

/// The links of some text, such as the clipboard's, as downloads to pick.
pub fn links_of(text: &str) -> Vec<Download> {
    udm_domain::batch::links_in(text)
        .into_iter()
        .map(|a| Download { url: a.into(), start: Some(false), ..Download::default() })
        .collect()
}

/// A list of links to tick, and where it came from.
#[derive(Debug, Clone)]
pub struct LinkList {
    /// The imported file's name; `None` for the clipboard.
    pub file: Option<String>,
    pub items: Vec<Download>,
    /// Lines that held no download address.
    pub rejected: usize,
}

fn name_of(d: &Download) -> String {
    d.filename.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
        let url = d.url.as_str();
        let path = url.split(['?', '#']).next().unwrap_or(url);
        path.rsplit('/').find(|p| !p.is_empty()).unwrap_or(path).to_string()
    })
}

pub fn links_view(ctx: &Ctx, state: &AppState) -> Option<LinksView> {
    let lang = ctx.lang;
    let list = state.links.as_ref()?;
    let settings = state.settings.clone().unwrap_or_default();
    let rows = list
        .items
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let name = name_of(d);
            let ext = name.rsplit_once('.').map(|(_, e)| e.to_uppercase()).unwrap_or_default();
            LinkRow {
                index: i as u32,
                icon: format::file_icon(&name),
                kind: if ext.is_empty() { String::new() } else { fill(lang.tr("{type} file"), &[("type", &ext)]) },
                url: d.url.to_string(),
                save_to: d
                    .folder
                    .clone()
                    .unwrap_or_else(|| lang.tr(settings.category_name(&name, d.url.as_str())).to_string()),
                text: d.description.clone().unwrap_or_default(),
                name,
            }
        })
        .collect();
    let (title, note) = match &list.file {
        None => (
            lang.tr("Batch download from clipboard"),
            lang.tr("Please check the links which you want to add to the download list of UDM, and click the OK button."),
        ),
        Some(_) => (lang.tr("Import download list"), lang.tr("Please check the downloads which you want to add to the download list of UDM, and click the OK button.")),
    };
    let mut note = note.to_string();
    if list.rejected > 0 {
        note = format!(
            "{note} {}",
            fill(lang.tr("{count} lines held no download address."), &[("count", &list.rejected.to_string())])
        );
    }
    Some(LinksView {
        title: title.to_string(),
        note,
        rows,
        queues: state
            .queues
            .iter()
            .map(|q| QueueItem { id: Id(q.id.get()), name: lang.tr(&q.name).into(), running: q.running })
            .collect(),
        connected: state.connected(),
    })
}

fn matches(input: &FindInput, text: &str) -> bool {
    let (needle, hay) = if input.match_case {
        (input.text.trim().to_string(), text.to_string())
    } else {
        (input.text.trim().to_lowercase(), text.to_lowercase())
    };
    if needle.is_empty() {
        return false;
    }
    if input.whole {
        hay == needle
    } else {
        hay.contains(&needle)
    }
}

/// The next download the list shows that matches `input`, after the one
/// selected first, going round to the top.
pub fn find(state: &AppState, input: &FindInput) -> Option<DownloadId> {
    let rows = main::shown(state);
    let hit = |e: &DownloadEntry| {
        let text = |s: Option<&str>| s.is_some_and(|s| matches(input, s));
        (input.name && matches(input, &main::display_name(e)))
            || (input.description && text(e.description.as_deref()))
            || (input.sites
                && (matches(input, e.url.as_str()) || text(e.page.as_deref()) || text(e.referrer.as_deref())))
    };
    let start = state.selected.first().and_then(|id| rows.iter().position(|e| e.id == *id)).map_or(0, |i| i + 1);
    (0..rows.len()).map(|k| rows[(start + k) % rows.len()]).find(|e| hit(e)).map(|e| e.id)
}

/// The downloads Export writes.
pub fn export_entries(state: &AppState, scope: ExportScope) -> Vec<&DownloadEntry> {
    match scope {
        ExportScope::All => state.table.entries().collect(),
        ExportScope::Selected => state.selected.iter().filter_map(|id| state.table.get(*id)).collect(),
        ExportScope::Queue => state
            .queues
            .iter()
            .find(|q| q.id == QueueId::MAIN)
            .map(|q| q.order.iter().filter_map(|id| state.table.get(*id)).collect())
            .unwrap_or_default(),
    }
}

/// The extension (lower case) of a download's file, whose name the system
/// is asked for when its File Properties opens.
pub fn properties_extension(state: &AppState, id: DownloadId) -> Option<String> {
    state.table.get(id).and_then(|e| udm_domain::settings::extension_of(&main::display_name(e)))
}

/// A file's type for File Properties: what the system calls it, as Windows
/// names "PDF Document" or "BIN File", else its extension.
fn type_text(lang: &'static crate::i18n::Language, state: &AppState, name: &str) -> String {
    let Some(ext) = udm_domain::settings::extension_of(name) else { return String::new() };
    match state.type_names.get(&ext) {
        Some(Some(system)) => system.clone(),
        _ => fill(lang.tr("{type} file"), &[("type", &ext.to_uppercase())]),
    }
}

pub fn properties_view(ctx: &Ctx, state: &AppState, id: DownloadId) -> Option<PropertiesView> {
    let lang = ctx.lang;
    let e = state.table.get(id)?;
    let name = main::display_name(e);
    let size = e.total.map(|t| format!("{} ({} {})", format::bytes(lang, t, 2), t.get(), lang.tr("Bytes")));
    Some(PropertiesView {
        id: Id(id.get()),
        icon: format::file_icon(&name),
        kind: type_text(lang, state, &name),
        status: format::status_word(lang, e.status).to_string(),
        size: size.unwrap_or_default(),
        save_to: e.path.clone().unwrap_or_default(),
        address: e.url.to_string(),
        description: e.description.clone().unwrap_or_default(),
        page: e.page.clone().unwrap_or_else(|| lang.tr("Unknown").to_string()),
        referrer: e.referrer.clone().unwrap_or_default(),
        login_user: e.login_user.clone().unwrap_or_default(),
        editable: e.status != Status::Done,
        can_open: e.status == Status::Done && e.path.is_some(),
        can_move: main::is_finished(e.status) || e.status == Status::Waiting,
        name,
    })
}

/// The changes the properties window's fields make to `e`.
pub fn properties_changes(input: &PropertiesInput, e: &DownloadEntry) -> Properties {
    let changed =
        |new: &str, old: Option<&str>| (new.trim() != old.unwrap_or_default()).then(|| new.trim().to_string());
    let editable = e.status != Status::Done;
    let user = input.login_user.trim();
    let login_changed =
        editable && (user != e.login_user.as_deref().unwrap_or_default() || !input.login_password.is_empty());
    Properties {
        url: changed(&input.address, Some(e.url.as_str())).filter(|_| editable),
        referrer: changed(&input.referrer, e.referrer.as_deref()),
        description: changed(&input.description, e.description.as_deref()),
        login: login_changed.then(|| DownloadLogin {
            username: user.to_string(),
            password: Some(input.login_password.clone()).filter(|p| !p.is_empty()),
        }),
        filename: None,
        folder: None,
    }
}

/// Moving a download's file to `path`: its new name and folder.
pub fn moved_to(path: &Path) -> Properties {
    Properties {
        filename: path.file_name().map(|n| n.to_string_lossy().into_owned()),
        folder: path.parent().map(|p| p.display().to_string()),
        ..Properties::default()
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

/// The column chooser's list: the shown columns in order, then the rest.
pub fn column_choices(ctx: &Ctx, state: &AppState) -> Vec<ColumnChoice> {
    let mut out: Vec<ColumnChoice> = state
        .columns
        .iter()
        .map(|(k, w)| ColumnChoice {
            key: *k,
            title: ctx.lang.tr(column_title(*k)).into(),
            shown: true,
            width: w.round() as u32,
        })
        .collect();
    for k in ALL_COLUMNS {
        if !out.iter().any(|c| c.key == k) {
            out.push(ColumnChoice { key: k, title: ctx.lang.tr(column_title(k)).into(), shown: false, width: 100 });
        }
    }
    out
}

/// The shown columns and widths the chooser picked. The file name always
/// shows.
pub fn columns_of(choices: &[ColumnChoice]) -> Vec<(ColumnKey, f32)> {
    let mut out: Vec<(ColumnKey, f32)> = Vec::new();
    for c in choices.iter().filter(|c| c.shown || c.key == ColumnKey::Name) {
        if !out.iter().any(|(k, _)| *k == c.key) {
            out.push((c.key, (c.width as f32).clamp(crate::state::WIDTH_RANGE.0, crate::state::WIDTH_RANGE.1)));
        }
    }
    out
}

pub fn limiter(s: &Settings) -> LimiterDraft {
    LimiterDraft {
        on: s.speed_limit_enabled,
        kb_per_sec: s.speed_limit_kbps.min(u64::from(u32::MAX)) as u32,
        on_startup: s.speed_limit_on_startup,
    }
}

pub fn limiter_settings(d: &LimiterDraft, current: &Settings) -> Result<Settings, InvalidSettings> {
    let mut s = current.clone();
    s.speed_limit_enabled = d.on;
    s.speed_limit_kbps = u64::from(d.kb_per_sec);
    s.speed_limit_on_startup = d.on_startup;
    s.validate()?;
    Ok(s)
}

/// The categories the sheets edit, General left out.
pub fn category_inputs(s: &Settings) -> Vec<CategoryInput> {
    s.categories
        .iter()
        .filter(|c| c.name != GENERAL)
        .map(|c| CategoryInput {
            name: c.name.clone(),
            extensions: c.extensions.join(" "),
            sites_on: !c.sites.is_empty(),
            sites: c.sites.join(" "),
            folder: c.folder.clone().unwrap_or_default(),
            remember_folder: c.remember_folder,
        })
        .collect()
}

/// Why a category cannot be saved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CategoryError {
    #[error("Type a name for the category.")]
    NoName,
    #[error("There is a category named \"{0}\" already.")]
    Taken(String),
}

/// `current` with category `original` (a new one when `None`) set to
/// `input`, or without category `name` when `input` is `None`.
pub fn with_category(
    current: &Settings,
    original: Option<&str>,
    input: Option<&CategoryInput>,
) -> Result<Settings, CategoryError> {
    let mut s = current.clone();
    let Some(input) = input else {
        s.categories.retain(|c| Some(c.name.as_str()) != original);
        return Ok(s);
    };
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(CategoryError::NoName);
    }
    let taken = name.eq_ignore_ascii_case(GENERAL)
        || s.categories.iter().any(|c| c.name.eq_ignore_ascii_case(&name) && Some(c.name.as_str()) != original);
    if taken {
        return Err(CategoryError::Taken(name));
    }
    let before = original.and_then(|o| s.categories.iter().position(|c| c.name == o));
    let sites = if input.sites_on {
        input
            .sites
            .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
            .map(|x| x.trim().to_ascii_lowercase())
            .filter(|x| !x.is_empty())
            .collect()
    } else {
        Vec::new()
    };
    let category = Category {
        name,
        extensions: parse_extensions(&input.extensions),
        folder: Some(input.folder.trim().to_string()).filter(|f| !f.is_empty()),
        sites,
        remember_folder: input.remember_folder,
        last_folder: before.and_then(|i| s.categories[i].last_folder.clone()).filter(|_| input.remember_folder),
    };
    match before {
        Some(i) => s.categories[i] = category,
        None => s.categories.push(category),
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{apply, UiMessage};
    use crate::testing::entry;
    use std::time::Instant;
    use udm_application::daemon::Event;

    fn with(entries: Vec<DownloadEntry>) -> AppState {
        let mut s = AppState::default();
        apply(
            &mut s,
            UiMessage::Daemon { event: Box::new(Event::Downloads { items: entries }), at: Instant::now(), unix: 0 },
        );
        s
    }

    #[test]
    fn a_batch_pattern_previews_its_ends() {
        let input = BatchInput {
            address: "https://e.com/img*.jpg".into(),
            letters: false,
            from: "1".into(),
            to: "10".into(),
            size: 3,
        };
        let p = batch_preview(&input).unwrap();
        assert_eq!(
            (p.first.as_str(), p.second.as_str(), p.last.as_str(), p.count),
            ("https://e.com/img001.jpg", "https://e.com/img002.jpg", "https://e.com/img010.jpg", 10)
        );
        assert!(batch_preview(&BatchInput { address: "https://e.com/x.jpg".into(), ..input }).is_err());
    }

    #[test]
    fn the_clipboard_gives_its_links() {
        let items = links_of("see https://e.com/a.zip and https://e.com/b.pdf, not ftp:/x");
        assert_eq!(items.len(), 2);
        assert_eq!(name_of(&items[1]), "b.pdf");
    }

    #[test]
    fn find_goes_on_from_the_selection_and_wraps() {
        let mut s = with(vec![
            entry(1, "alpha.zip", Status::Done, 1, 1),
            entry(2, "beta.zip", Status::Done, 1, 1),
            entry(3, "alpha2.zip", Status::Done, 1, 1),
        ]);
        let q = FindInput {
            text: "ALPHA".into(),
            name: true,
            description: false,
            sites: false,
            match_case: false,
            whole: false,
        };
        assert_eq!(find(&s, &q), Some(DownloadId::new(1)));
        apply(&mut s, UiMessage::Select(vec![DownloadId::new(1)]));
        assert_eq!(find(&s, &q), Some(DownloadId::new(3)));
        apply(&mut s, UiMessage::Select(vec![DownloadId::new(3)]));
        assert_eq!(find(&s, &q), Some(DownloadId::new(1)));
        assert_eq!(find(&s, &FindInput { match_case: true, ..q }), None);
    }

    #[test]
    fn properties_name_the_type_as_the_system_does() {
        let ctx = Ctx {
            lang: crate::i18n::resolve("en", &[]),
            offset: 0,
            today: String::new(),
            now: Instant::now(),
            unix: 1_700_000_000,
            downloads: std::path::PathBuf::from("/home/u/Downloads"),
        };
        let mut s = with(vec![entry(1, "Report.PDF", Status::Done, 1, 1), entry(2, "README", Status::Done, 1, 1)]);
        let id = DownloadId::new(1);
        assert_eq!(properties_extension(&s, id).as_deref(), Some("pdf"));
        assert_eq!(properties_view(&ctx, &s, id).unwrap().kind, "PDF file");
        apply(&mut s, UiMessage::TypeNameRead { ext: "pdf".into(), name: Some("PDF Document".into()) });
        assert_eq!(properties_view(&ctx, &s, id).unwrap().kind, "PDF Document");
        assert_eq!(properties_extension(&s, DownloadId::new(2)), None);
        assert_eq!(properties_view(&ctx, &s, DownloadId::new(2)).unwrap().kind, "");
    }

    #[test]
    fn properties_send_only_what_changed() {
        let e = entry(1, "a.zip", Status::Failed, 1, 0);
        let input = PropertiesInput {
            address: e.url.to_string(),
            description: "notes".into(),
            referrer: String::new(),
            login_user: String::new(),
            login_password: String::new(),
        };
        let p = properties_changes(&input, &e);
        assert_eq!(p.description.as_deref(), Some("notes"));
        assert!(p.url.is_none() && p.referrer.is_none() && p.login.is_none());
        let moved = moved_to(Path::new("/home/u/x/b.zip"));
        assert_eq!((moved.filename.as_deref(), moved.folder.as_deref()), (Some("b.zip"), Some("/home/u/x")));
    }

    #[test]
    fn the_column_chooser_keeps_the_name() {
        let s = AppState::default();
        let ctx = Ctx {
            lang: crate::i18n::resolve("en", &[]),
            offset: 0,
            today: String::new(),
            now: std::time::Instant::now(),
            unix: 1_700_000_000,
            downloads: std::path::PathBuf::from("/home/u/Downloads"),
        };
        let mut choices = column_choices(&ctx, &s);
        assert_eq!(choices.len(), ALL_COLUMNS.len());
        for c in &mut choices {
            c.shown = c.key == ColumnKey::Size;
        }
        let cols = columns_of(&choices);
        assert_eq!(cols.iter().map(|(k, _)| *k).collect::<Vec<_>>(), [ColumnKey::Name, ColumnKey::Size]);
    }

    #[test]
    fn categories_are_added_renamed_and_deleted() {
        let s = Settings::default();
        let input = CategoryInput {
            name: "Books".into(),
            extensions: "epub mobi".into(),
            sites_on: false,
            sites: String::new(),
            folder: String::new(),
            remember_folder: false,
        };
        let added = with_category(&s, None, Some(&input)).unwrap();
        assert!(added.category("Books").is_some());
        assert_eq!(with_category(&added, None, Some(&input)), Err(CategoryError::Taken("Books".into())));
        let renamed =
            with_category(&added, Some("Books"), Some(&CategoryInput { name: "Ebooks".into(), ..input.clone() }))
                .unwrap();
        assert!(renamed.category("Ebooks").is_some() && renamed.category("Books").is_none());
        let gone = with_category(&renamed, Some("Ebooks"), None).unwrap();
        assert!(gone.category("Ebooks").is_none());
    }
}
