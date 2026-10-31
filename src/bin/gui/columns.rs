//! The list's columns and sort order. Clicking a header sorts by that column,
//! clicking again reverses the order, and a third click goes back to the
//! order downloads were added in. Dragging the edge after a header sets the
//! column's width. The Columns dialog shows, hides, reorders and sizes
//! columns. All of it is saved to `%LOCALAPPDATA%\UIDM\window.json`, so it
//! survives a restart. A queue's view always keeps the queue's run order.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::PathBuf;

use eframe::egui;
use serde::{Deserialize, Serialize};
use uidm::protocol::{DownloadEntry, Status};
use uidm::queue;

use super::display_name;
use crate::i18n::tr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Column {
    Name,
    /// The download's place in its queue.
    Queue,
    Size,
    Progress,
    Speed,
    TimeLeft,
    Status,
    LastTry,
    SaveTo,
    Referrer,
    Description,
}

impl Column {
    pub const ALL: [Column; 11] = [
        Column::Name,
        Column::Queue,
        Column::Size,
        Column::Progress,
        Column::Speed,
        Column::TimeLeft,
        Column::Status,
        Column::LastTry,
        Column::SaveTo,
        Column::Referrer,
        Column::Description,
    ];

    /// The columns, in the order of the download manager UIDM's list
    /// follows.
    const DEFAULT: [Column; 8] = [
        Column::Name,
        Column::Queue,
        Column::Size,
        Column::Status,
        Column::TimeLeft,
        Column::Speed,
        Column::LastTry,
        Column::Description,
    ];

    /// The default before the list followed that order. A layout still on
    /// it moves to the new one.
    const OLD_DEFAULT: [Column; 6] = [
        Column::Name,
        Column::Size,
        Column::Progress,
        Column::Speed,
        Column::TimeLeft,
        Column::Status,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Column::Name => tr("File Name"),
            Column::Queue => tr("Q"),
            Column::Size => tr("Size"),
            Column::Progress => tr("Progress"),
            Column::Speed => tr("Transfer rate"),
            Column::TimeLeft => tr("Time left"),
            Column::Status => tr("Status"),
            Column::LastTry => tr("Last Try Date"),
            Column::SaveTo => tr("Save to"),
            Column::Referrer => tr("Parent web page"),
            Column::Description => tr("Description"),
        }
    }

    /// The width the column has until one is set.
    pub fn default_width(self) -> f32 {
        match self {
            Column::Name => 220.0,
            Column::Queue => 22.0,
            Column::Size => 80.0,
            Column::Progress => 120.0,
            Column::Speed => 95.0,
            Column::TimeLeft => 80.0,
            Column::Status => 95.0,
            Column::LastTry => 110.0,
            Column::SaveTo | Column::Referrer | Column::Description => TEXT_WIDTH,
        }
    }

    /// Whether the list can be sorted by it.
    pub fn sortable(self) -> bool {
        self != Column::Queue
    }

    /// Whether the column holds long text, cut to a fixed width.
    pub fn is_text(self) -> bool {
        matches!(self, Column::Name | Column::SaveTo | Column::Referrer | Column::Description)
    }
}

/// The width a text column has until one is set.
pub const TEXT_WIDTH: f32 = 200.0;
/// The widths a column can be given.
pub const WIDTH_RANGE: (f32, f32) = (40.0, 800.0);

/// A column to sort by, and which way.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Sort {
    pub column: Column,
    pub descending: bool,
}

/// What the list shows and in what order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    /// The columns shown, left to right. The file name is always one.
    pub columns: Vec<Column>,
    /// No sort means the order downloads were added in.
    #[serde(default)]
    pub sort: Option<Sort>,
    /// Widths set by dragging a header's edge or in the Columns dialog.
    /// Other columns fit their content, and text columns are `TEXT_WIDTH`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub widths: BTreeMap<Column, f32>,
    /// The toolbar has small icon-only buttons rather than large labeled ones.
    #[serde(default)]
    pub toolbar_small: bool,
    /// The categories panel's width, once it was dragged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories_width: Option<f32>,
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            columns: Column::DEFAULT.to_vec(),
            sort: None,
            widths: BTreeMap::new(),
            toolbar_small: false,
            categories_width: None,
        }
    }
}

impl Layout {
    /// The saved layout, or the default one.
    pub fn load() -> Layout {
        let saved = path().and_then(|p| std::fs::read(p).ok());
        let mut layout: Layout = saved
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if layout.columns == Column::OLD_DEFAULT {
            layout.columns = Column::DEFAULT.to_vec();
        }
        layout.tidy();
        layout
    }

    pub fn save(&self) {
        let Some(path) = path() else {
            return;
        };
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            if let Err(e) = uidm::settings::write_atomic(&path, &bytes) {
                eprintln!("[uidm-gui] could not save {}: {e}", path.display());
            }
        }
    }

    /// The width `column` is drawn at: the one set, else `TEXT_WIDTH` for
    /// text, else none (it fits its content).
    pub fn width(&self, column: Column) -> Option<f32> {
        self.widths.get(&column).copied().or(column.is_text().then_some(TEXT_WIDTH))
    }

    /// The width `column` is drawn at in the list: the one set, else its
    /// default.
    pub fn shown_width(&self, column: Column) -> f32 {
        self.widths.get(&column).copied().unwrap_or(column.default_width())
    }

    /// Set `column`'s width, kept within `WIDTH_RANGE`.
    pub fn set_width(&mut self, column: Column, width: f32) {
        let (lo, hi) = WIDTH_RANGE;
        self.widths.insert(column, width.clamp(lo, hi).round());
    }

    /// No column twice, and the file name always there.
    fn tidy(&mut self) {
        let mut seen = Vec::new();
        self.columns.retain(|c| {
            let new = !seen.contains(c);
            seen.push(*c);
            new
        });
        if !self.columns.contains(&Column::Name) {
            self.columns.insert(0, Column::Name);
        }
    }

    /// A click on `column`'s header: ascending, then descending, then the
    /// order added.
    pub fn click(&mut self, column: Column) {
        self.sort = match self.sort {
            Some(s) if s.column == column && !s.descending => Some(Sort {
                column,
                descending: true,
            }),
            Some(s) if s.column == column => None,
            _ => Some(Sort {
                column,
                descending: false,
            }),
        };
    }

    /// Sort `rows` by the chosen column. `speed` gives a running download's
    /// bytes per second. Ties keep the order added.
    pub fn sort(&self, rows: &mut [&DownloadEntry], speed: impl Fn(u64) -> Option<f64>) {
        let Some(sort) = self.sort else {
            return;
        };
        rows.sort_by(|a, b| compare(sort, a, b, &speed).then(a.id.cmp(&b.id)));
    }
}

/// `%LOCALAPPDATA%\UIDM\window.json`.
fn path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|base| PathBuf::from(base).join("UIDM").join("window.json"))
}

fn compare(sort: Sort, a: &DownloadEntry, b: &DownloadEntry, speed: &impl Fn(u64) -> Option<f64>) -> Ordering {
    let down = sort.descending;
    let running = |e: &DownloadEntry| speed(e.id).filter(|s| *s > 0.0 && e.status == Status::Downloading);
    let lower = |s: Option<&str>| s.map(str::to_lowercase);
    match sort.column {
        Column::Name => known(Some(display_name(a).to_lowercase()), Some(display_name(b).to_lowercase()), down),
        // The place in a queue is not the download's own; a queue's view
        // shows its order anyway.
        Column::Queue => Ordering::Equal,
        Column::Size => known(a.total, b.total, down),
        Column::Progress => known(fraction(a), fraction(b), down),
        Column::Speed => known(running(a), running(b), down),
        Column::TimeLeft => known(time_left(a, running(a)), time_left(b, running(b)), down),
        Column::Status => known(Some(status_rank(a.status)), Some(status_rank(b.status)), down),
        Column::LastTry => known(a.last_try, b.last_try, down),
        Column::SaveTo => known(lower(a.path.as_deref()), lower(b.path.as_deref()), down),
        Column::Referrer => known(lower(a.referrer.as_deref()), lower(b.referrer.as_deref()), down),
        Column::Description => known(lower(a.description.as_deref()), lower(b.description.as_deref()), down),
    }
}

/// Compare two values, reversed when `descending`. Empty values (an unknown
/// size, no speed) sort after the rest either way.
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

/// How far along a download is, from 0 to 1, when that is known.
pub fn fraction(e: &DownloadEntry) -> Option<f32> {
    let parts = e.parts.filter(|[_, all]| *all > 0);
    match (e.total, parts) {
        _ if e.status == Status::Done => Some(1.0),
        (Some(t), _) if t > 0 => Some((e.done as f32 / t as f32).clamp(0.0, 1.0)),
        (_, Some([done, all])) => Some((done as f32 / all as f32).clamp(0.0, 1.0)),
        _ => None,
    }
}

/// Seconds left at `bytes_per_sec`: from the total, or for a stream from the
/// average segment so far.
pub fn time_left(e: &DownloadEntry, bytes_per_sec: Option<f64>) -> Option<f64> {
    let bps = bytes_per_sec.filter(|s| *s > 0.0)?;
    let estimate = e.total.or_else(|| match e.parts {
        Some([done, all]) if done > 0 => Some(e.done / done * all),
        _ => None,
    })?;
    (estimate > e.done).then(|| (estimate - e.done) as f64 / bps)
}

/// `2026-10-27 14:05` in local time.
pub fn local_date(unix: u64) -> String {
    let now = queue::local_now();
    let local_now = now.day * 1440 + now.minute as i64;
    let utc_now = uidm::state::unix_now() as i64 / 60;
    // Round the offset to the quarter hour the clock reading straddles.
    let offset = ((local_now - utc_now) as f64 / 15.0).round() as i64 * 15;
    let minutes = unix as i64 / 60 + offset;
    let (y, m, d) = queue::civil_from_days(minutes.div_euclid(1440));
    let minute = minutes.rem_euclid(1440);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", minute / 60, minute % 60)
}

/// The Columns dialog, editing `layout` in place. Returns false once closed.
pub fn dialog(ctx: &egui::Context, layout: &mut Layout) -> bool {
    let mut open = true;
    let mut close = false;
    egui::Window::new(tr("Columns"))
        .id(egui::Id::new("columns"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label(tr("Tick the columns to show. Up and Down move a column left and right."));
            ui.weak(tr("A width is in points; Fit sizes the column to what it holds. Dragging the edge after a header sets it too."));
            ui.add_space(6.0);
            // Shown columns in their order, then the hidden ones.
            let mut all: Vec<Column> = layout.columns.clone();
            all.extend(Column::ALL.iter().filter(|c| !layout.columns.contains(c)));
            egui::Grid::new("columns_grid").num_columns(5).spacing([12.0, 4.0]).show(ui, |ui| {
                for c in all {
                    let mut shown = layout.columns.contains(&c);
                    let pos = layout.columns.iter().position(|x| *x == c);
                    let toggle = ui.add_enabled(c != Column::Name, egui::Checkbox::new(&mut shown, c.title()));
                    if toggle.changed() {
                        if shown {
                            layout.columns.push(c);
                        } else {
                            layout.columns.retain(|x| *x != c);
                        }
                    }
                    let up = pos.is_some_and(|i| i > 0);
                    if ui.add_enabled(up, egui::Button::new(tr("Up")).small()).clicked() {
                        let i = pos.unwrap_or(0);
                        layout.columns.swap(i, i - 1);
                    }
                    let down = pos.is_some_and(|i| i + 1 < layout.columns.len());
                    if ui.add_enabled(down, egui::Button::new(tr("Down")).small()).clicked() {
                        let i = pos.unwrap_or(0);
                        layout.columns.swap(i, i + 1);
                    }
                    let mut fit = !layout.widths.contains_key(&c) && !c.is_text();
                    if !c.is_text() && ui.checkbox(&mut fit, tr("Fit")).changed() {
                        if fit {
                            layout.widths.remove(&c);
                        } else {
                            layout.set_width(c, 100.0);
                        }
                    }
                    if c.is_text() {
                        ui.label("");
                    }
                    let mut width = layout.width(c).unwrap_or(100.0);
                    let (lo, hi) = WIDTH_RANGE;
                    if ui.add_enabled(!fit, egui::DragValue::new(&mut width).range(lo..=hi).speed(1.0)).changed() {
                        layout.set_width(c, width);
                    }
                    ui.end_row();
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(tr("Reset")).on_hover_text(tr("The default columns, in the order added")).clicked() {
                    *layout = Layout::default();
                }
                if ui.button(tr("Close")).clicked() {
                    close = true;
                }
            });
        });
    open && !close
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u64, name: &str, total: Option<u64>, status: Status) -> DownloadEntry {
        DownloadEntry {
            id,
            url: format!("https://x/{name}"),
            path: None,
            total,
            done: 0,
            status,
            error: None,
            resumable: false,
            connections: 0,
            scan: None,
            parts: None,
            note: None,
            duplicate_of: None,
            referrer: None,
            last_try: None,
            description: None,
            category: None,
            login_user: None,
            speed_limit: None,
            partial: None,
            from_browser: false,
            on_done: None,
        }
    }

    fn ids(rows: &[&DownloadEntry]) -> Vec<u64> {
        rows.iter().map(|e| e.id).collect()
    }

    #[test]
    fn header_clicks_cycle_through_both_orders_and_back() {
        let mut l = Layout::default();
        l.click(Column::Size);
        assert_eq!(l.sort, Some(Sort { column: Column::Size, descending: false }));
        l.click(Column::Size);
        assert_eq!(l.sort, Some(Sort { column: Column::Size, descending: true }));
        l.click(Column::Size);
        assert_eq!(l.sort, None);
        l.click(Column::Size);
        l.click(Column::Name);
        assert_eq!(l.sort, Some(Sort { column: Column::Name, descending: false }), "another column starts ascending");
    }

    #[test]
    fn sorts_by_size_with_unknown_sizes_last() {
        let a = entry(1, "a", Some(300), Status::Done);
        let b = entry(2, "b", None, Status::Queued);
        let c = entry(3, "c", Some(100), Status::Done);
        let mut l = Layout::default();
        let mut rows = vec![&a, &b, &c];
        l.click(Column::Size);
        l.sort(&mut rows, |_| None);
        assert_eq!(ids(&rows), [3, 1, 2]);
        l.click(Column::Size);
        l.sort(&mut rows, |_| None);
        assert_eq!(ids(&rows), [1, 3, 2]);
    }

    #[test]
    fn sorts_names_without_case_and_ties_keep_the_order_added() {
        let a = entry(1, "beta.zip", None, Status::Done);
        let b = entry(2, "Alpha.zip", None, Status::Done);
        let c = entry(3, "alpha.zip", None, Status::Done);
        let mut l = Layout::default();
        l.click(Column::Name);
        let mut rows = vec![&a, &b, &c];
        l.sort(&mut rows, |_| None);
        assert_eq!(ids(&rows), [2, 3, 1]);
        l.click(Column::Status);
        l.sort(&mut rows, |_| None);
        assert_eq!(ids(&rows), [1, 2, 3]);
    }

    #[test]
    fn a_saved_layout_always_has_the_name_once() {
        let mut l: Layout = serde_json::from_str(r#"{"columns":["size","size","status"]}"#).unwrap();
        l.tidy();
        assert_eq!(l.columns, [Column::Name, Column::Size, Column::Status]);
        assert_eq!(l.sort, None);
    }

    #[test]
    fn widths_are_kept_within_bounds_and_saved() {
        let mut l = Layout::default();
        assert_eq!(l.width(Column::Name), Some(TEXT_WIDTH));
        assert_eq!(l.width(Column::Size), None, "fits its content");
        l.set_width(Column::Size, 5.0);
        l.set_width(Column::Referrer, 333.4);
        assert_eq!(l.width(Column::Size), Some(WIDTH_RANGE.0));
        assert_eq!(l.width(Column::Referrer), Some(333.0));
        let json = serde_json::to_string(&l).unwrap();
        let back: Layout = serde_json::from_str(&json).unwrap();
        assert_eq!(back, l);
        assert!(json.contains(r#""referrer":333.0"#), "{json}");
        let old: Layout = serde_json::from_str(r#"{"columns":["name"]}"#).unwrap();
        assert!(old.widths.is_empty());
    }

    #[test]
    fn sorts_by_parent_page_without_case() {
        let mut a = entry(1, "a", None, Status::Done);
        a.referrer = Some("https://B.example/page".into());
        let mut b = entry(2, "b", None, Status::Done);
        b.referrer = Some("https://a.example/page".into());
        let c = entry(3, "c", None, Status::Done);
        let mut l = Layout::default();
        l.click(Column::Referrer);
        let mut rows = vec![&a, &b, &c];
        l.sort(&mut rows, |_| None);
        assert_eq!(ids(&rows), [2, 1, 3], "no page last");
    }

    #[test]
    fn time_left_needs_a_speed_and_a_size() {
        let mut e = entry(1, "a", Some(1000), Status::Downloading);
        e.done = 400;
        assert_eq!(time_left(&e, Some(100.0)), Some(6.0));
        assert_eq!(time_left(&e, None), None);
        e.total = None;
        assert_eq!(time_left(&e, Some(100.0)), None);
    }
}
