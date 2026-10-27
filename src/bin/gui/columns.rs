//! The list's columns and sort order. Clicking a header sorts by that column,
//! clicking again reverses the order, and a third click goes back to the
//! order downloads were added in. The Columns dialog shows, hides and
//! reorders columns. Both are saved to `%LOCALAPPDATA%\UIDM\window.json`, so
//! they survive a restart. A queue's view always keeps the queue's run order.

use std::cmp::Ordering;
use std::path::PathBuf;

use eframe::egui;
use serde::{Deserialize, Serialize};
use uidm::protocol::{DownloadEntry, Status};
use uidm::queue;

use super::display_name;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Column {
    Name,
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
    pub const ALL: [Column; 10] = [
        Column::Name,
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

    const DEFAULT: [Column; 6] = [
        Column::Name,
        Column::Size,
        Column::Progress,
        Column::Speed,
        Column::TimeLeft,
        Column::Status,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Column::Name => "File",
            Column::Size => "Size",
            Column::Progress => "Progress",
            Column::Speed => "Speed",
            Column::TimeLeft => "Time left",
            Column::Status => "Status",
            Column::LastTry => "Last try",
            Column::SaveTo => "Save to",
            Column::Referrer => "Referrer",
            Column::Description => "Description",
        }
    }
}

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
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            columns: Column::DEFAULT.to_vec(),
            sort: None,
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

    /// The arrow after a header's title, if the list is sorted by it.
    pub fn arrow(&self, column: Column) -> &'static str {
        match self.sort {
            Some(Sort { column: c, descending }) if c == column => {
                if descending {
                    " \u{25BC}"
                } else {
                    " \u{25B2}"
                }
            }
            _ => "",
        }
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
        Column::Size => known(a.total, b.total, down),
        Column::Progress => known(fraction(a), fraction(b), down),
        Column::Speed => known(running(a), running(b), down),
        Column::TimeLeft => known(time_left(a, running(a)), time_left(b, running(b)), down),
        Column::Status => known(Some(status_rank(a.status)), Some(status_rank(b.status)), down),
        Column::LastTry => known(a.last_try, b.last_try, down),
        Column::SaveTo => known(lower(a.path.as_deref()), lower(b.path.as_deref()), down),
        Column::Referrer => known(a.referrer.as_deref(), b.referrer.as_deref(), down),
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
    egui::Window::new("Columns")
        .id(egui::Id::new("columns"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label("Tick the columns to show. Up and Down move a column left and right.");
            ui.add_space(6.0);
            // Shown columns in their order, then the hidden ones.
            let mut all: Vec<Column> = layout.columns.clone();
            all.extend(Column::ALL.iter().filter(|c| !layout.columns.contains(c)));
            egui::Grid::new("columns_grid").num_columns(3).spacing([12.0, 4.0]).show(ui, |ui| {
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
                    if ui.add_enabled(up, egui::Button::new("Up").small()).clicked() {
                        let i = pos.unwrap_or(0);
                        layout.columns.swap(i, i - 1);
                    }
                    let down = pos.is_some_and(|i| i + 1 < layout.columns.len());
                    if ui.add_enabled(down, egui::Button::new("Down").small()).clicked() {
                        let i = pos.unwrap_or(0);
                        layout.columns.swap(i, i + 1);
                    }
                    ui.end_row();
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Reset").on_hover_text("The default columns, in the order added").clicked() {
                    *layout = Layout::default();
                }
                if ui.button("Close").clicked() {
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
            login_user: None,
            speed_limit: None,
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
    fn time_left_needs_a_speed_and_a_size() {
        let mut e = entry(1, "a", Some(1000), Status::Downloading);
        e.done = 400;
        assert_eq!(time_left(&e, Some(100.0)), Some(6.0));
        assert_eq!(time_left(&e, None), None);
        e.total = None;
        assert_eq!(time_left(&e, Some(100.0)), None);
    }
}
