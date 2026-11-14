//! The window's layout file, `window.json`: the columns, their widths and
//! sort, the categories panel's width, the list's font and the toolbar. Its
//! format is the one UDM's earlier egui window wrote, so a layout saved by
//! an older UDM still reads; what this window does not use (the tray
//! icon's look, a dismissed release) is kept as it was read.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::contract::{ColumnKey, SortDir};
use crate::state::{AppState, ListFont, Sort, DEFAULT_COLUMNS};

/// The layout's file, as the window's files name it.
pub const FILE: &str = "window.json";

/// The toolbar's buttons in their default order.
pub const TOOLBAR: [&str; 11] = [
    "add",
    "resume",
    "stop",
    "stopAll",
    "delete",
    "deleteCompleted",
    "options",
    "scheduler",
    "startQueue",
    "stopQueue",
    "grabber",
];

/// What may stand on the toolbar: its buttons and separators.
pub const SEPARATOR: &str = "separator";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SortFile {
    column: String,
    descending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FontKind {
    Proportional,
    Monospace,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct FontFile {
    #[serde(default = "proportional")]
    kind: FontKind,
    size: f32,
}

fn proportional() -> FontKind {
    FontKind::Proportional
}

/// `window.json` as both windows read it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LayoutFile {
    #[serde(default)]
    columns: Vec<String>,
    #[serde(default)]
    sort: Option<SortFile>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    widths: BTreeMap<String, f32>,
    #[serde(default)]
    toolbar_small: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    categories_width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    list_font: Option<FontFile>,
    /// The toolbar's buttons and separators in order, when customized; an
    /// older UDM's file has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    toolbar_buttons: Option<Vec<String>>,
    /// Everything else, kept as read.
    #[serde(flatten)]
    rest: serde_json::Map<String, serde_json::Value>,
}

/// The names the file gives the columns, which call the parent web page
/// `referrer` and the Referer `referer`.
fn name(key: ColumnKey) -> &'static str {
    match key {
        ColumnKey::Name => "name",
        ColumnKey::Queue => "queue",
        ColumnKey::Size => "size",
        ColumnKey::Progress => "progress",
        ColumnKey::Speed => "speed",
        ColumnKey::TimeLeft => "time_left",
        ColumnKey::Status => "status",
        ColumnKey::LastTry => "last_try",
        ColumnKey::SaveTo => "save_to",
        ColumnKey::Page => "referrer",
        ColumnKey::Referrer => "referer",
        ColumnKey::Description => "description",
        ColumnKey::Country => "country",
    }
}

fn key(name: &str) -> Option<ColumnKey> {
    crate::tools::ALL_COLUMNS.into_iter().find(|k| self::name(*k) == name)
}

fn default_width(key: ColumnKey) -> f32 {
    DEFAULT_COLUMNS.iter().find(|(k, _)| *k == key).map_or(100.0, |(_, w)| *w)
}

/// Read the file's bytes; a file that does not parse is the default.
pub fn parse(bytes: Option<&[u8]>) -> LayoutFile {
    bytes.and_then(|b| serde_json::from_slice(b).ok()).unwrap_or_default()
}

/// Take the layout `file` into `state`.
pub fn apply(state: &mut AppState, file: &LayoutFile) {
    let mut columns: Vec<(ColumnKey, f32)> = file
        .columns
        .iter()
        .filter_map(|n| key(n))
        .map(|k| (k, file.widths.get(name(k)).copied().unwrap_or_else(|| default_width(k))))
        .collect();
    columns.dedup_by_key(|(k, _)| *k);
    if !columns.is_empty() {
        if !columns.iter().any(|(k, _)| *k == ColumnKey::Name) {
            columns.insert(0, (ColumnKey::Name, default_width(ColumnKey::Name)));
        }
        state.columns = columns;
    }
    state.sort = file.sort.as_ref().and_then(|s| {
        key(&s.column)
            .map(|column| Sort { column, dir: if s.descending { SortDir::Descending } else { SortDir::Ascending } })
    });
    if let Some(w) = file.categories_width {
        state.panel_width = w.clamp(100.0, 400.0);
    }
    state.list_font =
        file.list_font.map(|f| ListFont { mono: f.kind == FontKind::Monospace, size: f.size.clamp(8.0, 24.0) });
    state.toolbar_small = file.toolbar_small;
    state.toolbar = file
        .toolbar_buttons
        .clone()
        .map(|b| b.into_iter().filter(|id| id == SEPARATOR || TOOLBAR.contains(&id.as_str())).collect())
        .unwrap_or_else(|| TOOLBAR.iter().map(|s| s.to_string()).collect());
    state.layout_rest = file.rest.clone();
}

/// The layout file for `state`.
pub fn file(state: &AppState) -> LayoutFile {
    let default_toolbar: Vec<String> = TOOLBAR.iter().map(|s| s.to_string()).collect();
    LayoutFile {
        columns: state.columns.iter().map(|(k, _)| name(*k).to_string()).collect(),
        sort: state.sort.map(|s| SortFile { column: name(s.column).into(), descending: s.dir == SortDir::Descending }),
        widths: state.columns.iter().map(|(k, w)| (name(*k).to_string(), *w)).collect(),
        toolbar_small: state.toolbar_small,
        categories_width: Some(state.panel_width),
        list_font: state.list_font.map(|f| FontFile {
            kind: if f.mono { FontKind::Monospace } else { FontKind::Proportional },
            size: f.size,
        }),
        toolbar_buttons: (state.toolbar != default_toolbar).then(|| state.toolbar.clone()),
        rest: state.layout_rest.clone(),
    }
}

/// The file's bytes for `state`.
pub fn bytes(state: &AppState) -> Option<Vec<u8>> {
    serde_json::to_vec_pretty(&file(state)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_older_udms_file_reads() {
        let text = br#"{
            "columns": ["name", "size", "referrer", "time_left"],
            "sort": {"column": "size", "descending": true},
            "widths": {"name": 300.0},
            "toolbar_small": true,
            "categories_width": 180.0,
            "list_font": {"kind": "monospace", "size": 14.0},
            "tray_style": "white",
            "dismissed_release": "0.5.0"
        }"#;
        let mut s = AppState::default();
        apply(&mut s, &parse(Some(text)));
        assert_eq!(
            s.columns.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            [ColumnKey::Name, ColumnKey::Size, ColumnKey::Page, ColumnKey::TimeLeft]
        );
        assert_eq!(s.columns[0].1, 300.0);
        assert_eq!(s.sort, Some(Sort { column: ColumnKey::Size, dir: SortDir::Descending }));
        assert_eq!(s.panel_width, 180.0);
        assert_eq!(s.list_font, Some(ListFont { mono: true, size: 14.0 }));
        assert!(s.toolbar_small);
        let back = file(&s);
        assert_eq!(back.rest.get("tray_style"), Some(&serde_json::json!("white")));
        assert_eq!(back.rest.get("dismissed_release"), Some(&serde_json::json!("0.5.0")));
        assert_eq!(back.toolbar_buttons, None);
    }

    #[test]
    fn a_broken_file_is_the_default_and_the_name_column_stays() {
        let mut s = AppState::default();
        apply(&mut s, &parse(Some(b"not json")));
        assert_eq!(s.columns, DEFAULT_COLUMNS.to_vec());
        apply(&mut s, &parse(Some(br#"{"columns": ["size"]}"#)));
        assert_eq!(s.columns[0].0, ColumnKey::Name);
    }

    #[test]
    fn a_customized_toolbar_is_kept() {
        let mut s = AppState::default();
        apply(&mut s, &LayoutFile::default());
        s.toolbar = vec!["add".into(), SEPARATOR.into(), "grabber".into(), "bogus".into()];
        let mut t = AppState::default();
        apply(&mut t, &parse(bytes(&s).as_deref()));
        assert_eq!(t.toolbar, ["add", SEPARATOR, "grabber"]);
    }
}
