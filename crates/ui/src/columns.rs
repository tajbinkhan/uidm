//! The list's columns and sort order. Clicking a header sorts by that column,
//! clicking again reverses the order, and a third click goes back to the
//! order downloads were added in. Dragging the edge after a header sets the
//! column's width. The Columns dialog shows, hides, reorders and sizes
//! columns. All of it is saved to `%LOCALAPPDATA%\UIDM\window.json`, so it
//! survives a restart. A queue's view always keeps the queue's run order.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use eframe::egui;
use serde::{Deserialize, Serialize};
use uidm_domain::download::{DownloadEntry, Status};
use uidm_domain::queue;

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
    /// The web page a download came from. Saved as `referrer`: that is
    /// what layouts called it when the referrer stood for the page too.
    #[serde(rename = "referrer")]
    Page,
    /// The Referer its request sends.
    Referer,
    Description,
}

impl Column {
    pub const ALL: [Column; 12] = [
        Column::Name,
        Column::Queue,
        Column::Size,
        Column::Progress,
        Column::Speed,
        Column::TimeLeft,
        Column::Status,
        Column::LastTry,
        Column::SaveTo,
        Column::Page,
        Column::Referer,
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
    const OLD_DEFAULT: [Column; 6] =
        [Column::Name, Column::Size, Column::Progress, Column::Speed, Column::TimeLeft, Column::Status];

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
            Column::Page => tr("Parent web page"),
            Column::Referer => tr("Referrer"),
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
            Column::SaveTo | Column::Page | Column::Referer | Column::Description => TEXT_WIDTH,
        }
    }

    /// Whether the list can be sorted by it.
    pub fn sortable(self) -> bool {
        self != Column::Queue
    }

    /// Whether the column holds long text, cut to a fixed width.
    pub fn is_text(self) -> bool {
        matches!(self, Column::Name | Column::SaveTo | Column::Page | Column::Referer | Column::Description)
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
    /// The list's text, from View > Font; `None` is the window's own font.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_font: Option<ListFont>,
    /// How the tray icon looks, from View > Tray icon.
    #[serde(default)]
    pub tray_style: crate::tray::Style,
    /// The release whose "is available" strip was closed. It stays closed
    /// for that version; a newer one shows the strip again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed_release: Option<String>,
}

impl Layout {
    /// Put back what the Columns dialog changes, from `before`: the columns
    /// and their widths. The rest (sort, toolbar, panel width, font, tray
    /// icon) can change through other controls while the dialog is open, and
    /// stays as it is now.
    pub fn restore_columns(&mut self, before: &Layout) {
        self.columns = before.columns.clone();
        self.widths = before.widths.clone();
    }

    /// Whether the strip offering release `version` shows: not once it was
    /// closed for that version.
    pub fn shows_release(&self, version: &str) -> bool {
        self.dismissed_release.as_deref() != Some(version)
    }
}

/// The kinds of font egui has: the window's text font and its code font.
/// On Windows these are Segoe UI and Consolas (see `theme`), elsewhere
/// egui's own; there is no font picker to offer the system's others.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FontKind {
    #[default]
    Proportional,
    Monospace,
}

/// A font chosen for the download list.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ListFont {
    #[serde(default)]
    pub kind: FontKind,
    /// In points, within `ListFont::SIZES`.
    pub size: f32,
}

impl ListFont {
    /// The sizes offered, like a font dialog's 8 to 24 points.
    pub const SIZES: (f32, f32) = (8.0, 24.0);
    /// The window's own list text: 12 point Segoe UI (see `theme`).
    pub const DEFAULT: ListFont = ListFont { kind: FontKind::Proportional, size: 12.0 };

    pub fn id(&self) -> egui::FontId {
        let size = self.size.clamp(Self::SIZES.0, Self::SIZES.1);
        match self.kind {
            FontKind::Proportional => egui::FontId::proportional(size),
            FontKind::Monospace => egui::FontId::monospace(size),
        }
    }
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            columns: Column::DEFAULT.to_vec(),
            sort: None,
            widths: BTreeMap::new(),
            toolbar_small: false,
            categories_width: None,
            list_font: None,
            tray_style: crate::tray::Style::Color,
            dismissed_release: None,
        }
    }
}

impl Layout {
    /// The saved layout, or the default one.
    pub fn load() -> Layout {
        let saved = crate::ports::ports().window_files.read(LAYOUT_FILE);
        let mut layout: Layout = saved.and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        if layout.columns == Column::OLD_DEFAULT {
            layout.columns = Column::DEFAULT.to_vec();
        }
        layout.tidy();
        layout
    }

    /// Save the layout, on another thread.
    pub fn save(&self) {
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            crate::ports::ports().window_files.save(LAYOUT_FILE, bytes);
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
            Some(s) if s.column == column && !s.descending => Some(Sort { column, descending: true }),
            Some(s) if s.column == column => None,
            _ => Some(Sort { column, descending: false }),
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

/// The window file that holds the layout.
const LAYOUT_FILE: &str = "window.json";

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
        Column::Page => known(lower(a.page.as_deref()), lower(b.page.as_deref()), down),
        Column::Referer => known(lower(a.referrer.as_deref()), lower(b.referrer.as_deref()), down),
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
    let now = crate::ports::ports().now();
    let local_now = now.day * 1440 + now.minute as i64;
    let utc_now = crate::ports::ports().unix_now() as i64 / 60;
    // Round the offset to the quarter hour the clock reading straddles.
    let offset = ((local_now - utc_now) as f64 / 15.0).round() as i64 * 15;
    let minutes = unix as i64 / 60 + offset;
    let (y, m, d) = queue::civil_from_days(minutes.div_euclid(1440));
    let minute = minutes.rem_euclid(1440);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", minute / 60, minute % 60)
}

/// What the font dialog ended with.
pub enum FontChoice {
    Open,
    Cancel,
    Apply(ListFont),
}

/// View > Font > Choose Font: a kind and a size for the list's text, with
/// a sample in them. egui has no system font dialog, so it offers the two
/// fonts the window has loaded.
pub fn font_dialog(ctx: &egui::Context, draft: &mut ListFont) -> FontChoice {
    let mut open = true;
    let mut choice = FontChoice::Open;
    crate::dialog::Window::new(tr("Font"))
        .id(egui::Id::new("list_font"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            egui::Grid::new("list_font_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label(tr("Font:"));
                ui.horizontal(|ui| {
                    ui.radio_value(&mut draft.kind, FontKind::Proportional, tr("Text font"));
                    ui.radio_value(&mut draft.kind, FontKind::Monospace, tr("Fixed-width font"));
                });
                ui.end_row();
                ui.label(tr("Size:"));
                ui.add(
                    egui::Slider::new(&mut draft.size, ListFont::SIZES.0..=ListFont::SIZES.1)
                        .step_by(1.0)
                        .suffix(" pt"),
                );
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.group(|ui| {
                ui.set_min_width(300.0);
                ui.label(egui::RichText::new(tr("The quick brown fox jumps over the lazy dog")).font(draft.id()));
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(tr("OK")).clicked() {
                    choice = FontChoice::Apply(*draft);
                }
                if ui.button(tr("Cancel")).clicked() {
                    choice = FontChoice::Cancel;
                }
            });
        });
    if !open {
        choice = FontChoice::Cancel;
    }
    choice
}

/// The Columns dialog, editing `layout` in place so the list shows each
/// change at once. `before` is the layout it opened with: Cancel (or the
/// window's X) puts that back. Returns false once closed.
pub fn dialog(ctx: &egui::Context, layout: &mut Layout, before: &Layout) -> bool {
    let mut open = true;
    let mut close = false;
    crate::dialog::Window::new(tr("Columns"))
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
                    // The columns only: the rest has controls of its own.
                    layout.restore_columns(&Layout::default());
                }
                if ui.button(tr("OK")).clicked() {
                    close = true;
                }
                if ui.button(tr("Cancel")).on_hover_text(tr("Undo the changes made here")).clicked() {
                    layout.restore_columns(before);
                    close = true;
                }
            });
        });
    if !open {
        layout.restore_columns(before);
    }
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
            page: None,
            last_try: None,
            description: None,
            category: None,
            login_user: None,
            speed_limit: None,
            limit_remember: true,
            partial: None,
            from_browser: false,
            on_done: None,
            validator: None,
        }
    }

    fn ids(rows: &[&DownloadEntry]) -> Vec<u64> {
        rows.iter().map(|e| e.id).collect()
    }

    #[test]
    fn cancel_puts_the_columns_back_but_not_the_font() {
        let before = Layout::default();
        let mut now = before.clone();
        now.columns.retain(|c| *c != Column::Size);
        now.set_width(Column::Status, 140.0);
        now.list_font = Some(ListFont::DEFAULT);
        now.tray_style = crate::tray::Style::Mono;
        now.toolbar_small = !before.toolbar_small;
        now.categories_width = Some(250.0);
        now.restore_columns(&before);
        assert_eq!(now.toolbar_small, !before.toolbar_small, "changed outside the dialog");
        assert_eq!(now.categories_width, Some(250.0));
        assert_eq!(now.columns, before.columns);
        assert_eq!(now.widths, before.widths);
        assert_eq!(now.list_font, Some(ListFont::DEFAULT), "the font has a menu of its own");
        assert_eq!(now.tray_style, crate::tray::Style::Mono);
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
        l.set_width(Column::Page, 333.4);
        assert_eq!(l.width(Column::Size), Some(WIDTH_RANGE.0));
        assert_eq!(l.width(Column::Page), Some(333.0));
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
        a.page = Some("https://B.example/page".into());
        let mut b = entry(2, "b", None, Status::Done);
        b.page = Some("https://a.example/page".into());
        let c = entry(3, "c", None, Status::Done);
        let mut l = Layout::default();
        l.click(Column::Page);
        let mut rows = vec![&a, &b, &c];
        l.sort(&mut rows, |_| None);
        assert_eq!(ids(&rows), [2, 1, 3], "no page last");
    }

    #[test]
    fn sorts_by_referrer_apart_from_the_page() {
        // Same page, different frames: only the referrer tells them apart.
        let mut a = entry(1, "a", None, Status::Done);
        a.page = Some("https://a.example/".into());
        a.referrer = Some("https://ads.example/frame".into());
        let mut b = entry(2, "b", None, Status::Done);
        b.page = Some("https://b.example/".into());
        b.referrer = Some("https://Cdn.example/frame".into());
        let mut c = entry(3, "c", None, Status::Done);
        c.page = Some("https://c.example/".into());
        let mut l = Layout::default();
        l.click(Column::Referer);
        let mut rows = vec![&c, &b, &a];
        l.sort(&mut rows, |_| None);
        assert_eq!(ids(&rows), [1, 2, 3], "by referrer, none last");
        l.click(Column::Page);
        l.sort(&mut rows, |_| None);
        assert_eq!(ids(&rows), [1, 2, 3]);
        l.click(Column::Page);
        l.sort(&mut rows, |_| None);
        assert_eq!(ids(&rows), [3, 2, 1], "by page, descending");
    }

    #[test]
    fn a_saved_referrer_column_is_still_the_parent_page() {
        let l: Layout = serde_json::from_str(
            r#"{"columns":["name","referrer","referer"],"sort":{"column":"referrer","descending":false}}"#,
        )
        .unwrap();
        assert_eq!(l.columns, [Column::Name, Column::Page, Column::Referer]);
        assert_eq!(l.sort.map(|s| s.column), Some(Column::Page));
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

    #[test]
    fn an_old_layout_has_the_default_font_and_tray_icon() {
        let l: Layout = serde_json::from_str(r#"{"columns":["name","size"]}"#).unwrap();
        assert_eq!(l.list_font, None);
        assert_eq!(l.tray_style, crate::tray::Style::Color);
        // And the default writes neither the font nor anything new to read.
        let back: Layout = serde_json::from_str(&serde_json::to_string(&Layout::default()).unwrap()).unwrap();
        assert_eq!(back, Layout::default());
    }

    #[test]
    fn a_closed_release_strip_stays_closed_until_a_newer_one() {
        let mut l = Layout::default();
        assert!(l.shows_release("1.4.0"));
        l.dismissed_release = Some("1.4.0".into());
        let back: Layout = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        assert!(!back.shows_release("1.4.0"), "remembered across a restart");
        assert!(back.shows_release("1.5.0"));
    }

    #[test]
    fn a_chosen_font_and_tray_style_survive_a_save() {
        let l = Layout {
            list_font: Some(ListFont { kind: FontKind::Monospace, size: 16.0 }),
            tray_style: crate::tray::Style::Mono,
            ..Layout::default()
        };
        let json = serde_json::to_string(&l).unwrap();
        assert!(json.contains(r#""kind":"monospace""#) && json.contains(r#""tray_style":"mono""#));
        assert_eq!(serde_json::from_str::<Layout>(&json).unwrap(), l);
        // A font saved without its kind is the text font.
        let f: ListFont = serde_json::from_str(r#"{"size":10.0}"#).unwrap();
        assert_eq!(f.kind, FontKind::Proportional);
    }

    #[test]
    fn a_font_size_stays_in_range() {
        let big = ListFont { kind: FontKind::Proportional, size: 300.0 };
        assert_eq!(big.id(), egui::FontId::proportional(ListFont::SIZES.1));
        let small = ListFont { kind: FontKind::Monospace, size: 1.0 };
        assert_eq!(small.id(), egui::FontId::monospace(ListFont::SIZES.0));
    }
}
