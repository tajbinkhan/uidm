//! The download list: a table with resizable columns, grid lines and
//! whole-row selection. A click selects a row, Ctrl+click adds or removes
//! one, Shift+click selects a run; a double-click opens a finished file or
//! shows a download's window; a right-click opens the row's menu. Column
//! widths dragged here are saved with the layout.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use eframe::egui::{self, Align, Rect, Sense, Stroke};
use egui_extras::TableBuilder;
use uidm::protocol::{Answer, DownloadEntry, Status};
use uidm::queue::Queue;

use crate::columns::{self, Column, Layout};
use crate::i18n::{tr, trf};
use crate::{display_name, human_bytes, human_duration, scan_label, Action};

/// IDM's rows are 17 px; the row's text sets the floor here.
const ROW_HEIGHT: f32 = 18.0;
const HEADER_HEIGHT: f32 = 20.0;

/// Everything the list needs for one frame.
pub struct List<'a> {
    pub shown: &'a [&'a DownloadEntry],
    pub speeds: &'a HashMap<u64, f64>,
    pub selected: &'a HashSet<u64>,
    pub queues: &'a [Queue],
    /// In a queue's view: that queue, whose order the list shows.
    pub queue: Option<&'a Queue>,
    pub connected: bool,
    /// The match the find bar moved to, and whether to scroll to it.
    pub found: Option<u64>,
    pub scroll: bool,
}

/// The table's own state between frames.
#[derive(Default)]
pub struct TableState {
    /// The widths last measured, to tell a change from the Columns dialog
    /// (which restarts the table at the new widths) from a drag here.
    measured: BTreeMap<Column, f32>,
    generation: u64,
}

/// What the list asked for besides actions.
#[derive(Default)]
pub struct Outcome {
    pub open_columns: bool,
}

pub fn show(ui: &mut egui::Ui, list: &List, layout: &mut Layout, state: &mut TableState, actions: &mut Vec<Action>) -> Outcome {
    let mut outcome = Outcome::default();
    let columns = layout.columns.clone();
    let wanted: BTreeMap<Column, f32> = columns.iter().map(|c| (*c, layout.shown_width(*c))).collect();
    if !state.measured.is_empty() && state.measured != wanted {
        state.generation += 1;
    }

    // Each download's place in its queue, for the Q column.
    let places: HashMap<u64, usize> =
        list.queues.iter().flat_map(|q| q.order.iter().enumerate().map(|(i, id)| (*id, i + 1))).collect();

    ui.scope(|ui| {
        let style = ui.style_mut();
        style.spacing.item_spacing = egui::vec2(8.0, 0.0);
        let grid = ui.visuals().widgets.noninteractive.bg_stroke.color.gamma_multiply(0.6);
        let line = Stroke::new(1.0_f32, grid);
        let outer = ui.clip_rect();

        let mut table = TableBuilder::new(ui)
            .id_salt(("downloads", state.generation, &columns))
            .striped(false)
            .resizable(true)
            .sense(Sense::click())
            .cell_layout(egui::Layout::left_to_right(Align::Center))
            .auto_shrink([false, false])
            .min_scrolled_height(0.0);
        for c in &columns {
            table = table.column(
                egui_extras::Column::initial(layout.shown_width(*c))
                    .at_least(columns::WIDTH_RANGE.0.min(c.default_width()))
                    .clip(true),
            );
        }
        if list.scroll {
            if let Some(i) = list.shown.iter().position(|e| Some(e.id) == list.found) {
                table = table.scroll_to_row(i, Some(Align::Center));
            }
        }

        let mut measured = BTreeMap::new();
        table
            .header(HEADER_HEIGHT, |mut header| {
                for &column in &columns {
                    header.col(|ui| {
                        let rect = ui.max_rect();
                        grid_lines(ui, outer, line);
                        measured.insert(column, rect.width());
                        ui.add(egui::Label::new(egui::RichText::new(column.title()).strong()).truncate().selectable(false));
                        let id = ui.id().with(("header", column));
                        let click = ui.interact(rect, id, Sense::click());
                        let click = if list.queue.is_some() {
                            click.on_hover_text(tr("A queue's downloads stay in its run order"))
                        } else {
                            click
                        };
                        if click.clicked() && list.queue.is_none() && column.sortable() {
                            layout.click(column);
                        }
                        click.context_menu(|ui| {
                            if ui.button(tr("Columns...")).clicked() {
                                outcome.open_columns = true;
                                ui.close_menu();
                            }
                        });
                        if let Some(sort) = layout.sort.filter(|s| s.column == column && list.queue.is_none()) {
                            sort_arrow(ui, rect, sort.descending);
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, list.shown.len(), |mut row| {
                    let i = row.index();
                    let e = list.shown[i];
                    row.set_selected(list.selected.contains(&e.id) || list.found == Some(e.id));
                    let speed = list.speeds.get(&e.id).copied();
                    for &column in &columns {
                        row.col(|ui| {
                            grid_lines(ui, outer, line);
                            cell(ui, column, e, speed, places.get(&e.id).copied());
                        });
                    }
                    let response = row.response();
                    let (ctrl, shift) = response.ctx.input(|i| (i.modifiers.command, i.modifiers.shift));
                    if response.clicked() {
                        actions.push(Action::Click { id: e.id, ctrl, shift });
                    }
                    if response.double_clicked() {
                        actions.push(if e.status == Status::Done {
                            Action::Open(e.id)
                        } else {
                            Action::OpenProgress(e.id)
                        });
                    }
                    if response.secondary_clicked() && !list.selected.contains(&e.id) {
                        actions.push(Action::Click {
                            id: e.id,
                            ctrl: false,
                            shift: false,
                        });
                    }
                    response.context_menu(|ui| row_menu(ui, e, list, i, actions));
                });
            });
        for (column, width) in measured {
            if (layout.shown_width(column) - width).abs() >= 1.0 {
                layout.set_width(column, width);
            }
        }
        state.measured = columns.iter().map(|c| (*c, layout.shown_width(*c))).collect();
    });
    outcome
}

/// The grid: a line at the cell's right edge and along its bottom, in the
/// gap between cells. The cell clips its contents to itself, so the lines
/// are drawn with the table's clip across and the cell's clip down (which
/// keeps rows scrolled under the header from drawing over it).
fn grid_lines(ui: &egui::Ui, outer: Rect, line: Stroke) {
    let r = ui.max_rect();
    let gap = ui.spacing().item_spacing.x / 2.0;
    let clip = Rect::from_x_y_ranges(outer.x_range(), ui.clip_rect().y_range());
    let painter = egui::Painter::new(ui.ctx().clone(), ui.layer_id(), clip);
    painter.vline(r.right() + gap, r.y_range(), line);
    painter.hline((r.left() - gap)..=(r.right() + gap), r.bottom() - 0.5, line);
}

/// The sort direction at the right edge of a header.
fn sort_arrow(ui: &egui::Ui, rect: Rect, descending: bool) {
    let c = egui::pos2(rect.right() - 7.0, rect.center().y);
    let (tip, base) = if descending { (4.0, -3.0) } else { (-4.0, 3.0) };
    let color = ui.visuals().weak_text_color();
    ui.painter().add(egui::Shape::convex_polygon(
        vec![egui::pos2(c.x, c.y + tip), egui::pos2(c.x - 4.0, c.y + base), egui::pos2(c.x + 4.0, c.y + base)],
        color,
        Stroke::NONE,
    ));
}

/// One cell of a download's row.
fn cell(ui: &mut egui::Ui, column: Column, e: &DownloadEntry, speed: Option<f64>, place: Option<usize>) {
    let running = speed.filter(|s| *s > 0.0 && e.status == Status::Downloading);
    let text = |ui: &mut egui::Ui, s: &str| {
        let label = ui.add(egui::Label::new(s).truncate().selectable(false));
        if !s.is_empty() {
            label.on_hover_text(s);
        }
    };
    match column {
        Column::Name => {
            ui.add(egui::Label::new(display_name(e)).truncate().selectable(false))
                .on_hover_text(e.path.as_deref().unwrap_or(&e.url));
        }
        Column::Queue => {
            if let Some(n) = place {
                ui.add(egui::Label::new(n.to_string()).selectable(false));
            }
        }
        Column::Size => {
            // The whole size once known, as in IDM; until then what has
            // arrived. A stream counts segments.
            let size = match e.total {
                Some(total) => human_bytes(total),
                None if e.done > 0 => human_bytes(e.done),
                None => String::new(),
            };
            let size = ui.add(egui::Label::new(size).selectable(false));
            if let Some([done, all]) = e.parts.filter(|[_, all]| *all > 0) {
                size.on_hover_text(trf!("{done} of {all} segments", done = done, all = all));
            }
        }
        Column::Progress => {
            let bar = egui::ProgressBar::new(columns::fraction(e).unwrap_or(0.0)).desired_height(12.0);
            crate::theme::progress_bar(ui, if e.total.is_some() || e.status == Status::Done {
                bar.show_percentage()
            } else {
                bar.text(human_bytes(e.done))
            });
        }
        Column::Speed => {
            ui.add(
                egui::Label::new(running.map(|bps| format!("{}/s", human_bytes(bps as u64))).unwrap_or_default())
                    .selectable(false),
            );
        }
        Column::TimeLeft => {
            ui.add(egui::Label::new(columns::time_left(e, running).map(human_duration).unwrap_or_default()).selectable(false));
        }
        Column::Status => status(ui, e),
        Column::LastTry => {
            ui.add(egui::Label::new(e.last_try.map(columns::local_date).unwrap_or_default()).selectable(false));
        }
        Column::SaveTo => {
            let folder = e.path.as_deref().and_then(|p| Path::new(p).parent()).map(|d| d.display().to_string());
            text(ui, folder.as_deref().unwrap_or(""));
        }
        Column::Referrer => text(ui, e.referrer.as_deref().unwrap_or("")),
        Column::Description => text(ui, e.description.as_deref().unwrap_or("")),
    }
}

/// The Status cell: "Complete", the percentage while downloading, or why it
/// stopped.
fn status(ui: &mut egui::Ui, e: &DownloadEntry) {
    let visuals = ui.visuals().clone();
    let percent;
    let (text, color) = match e.status {
        Status::Queued => (tr("Queued"), visuals.weak_text_color()),
        Status::Downloading => match columns::fraction(e).filter(|_| e.total.is_some()) {
            Some(f) => {
                percent = format!("{:.2}%", f * 100.0);
                (percent.as_str(), visuals.text_color())
            }
            None => (tr("Downloading"), visuals.text_color()),
        },
        Status::Done => (tr("Complete"), visuals.text_color()),
        Status::Failed => (tr("Failed"), visuals.error_fg_color),
        Status::Canceled => (tr("Canceled"), visuals.warn_fg_color),
        Status::Interrupted => (tr("Interrupted"), visuals.warn_fg_color),
        Status::Confirming => (tr("Confirm"), visuals.weak_text_color()),
        Status::Waiting => (tr("Waiting"), visuals.weak_text_color()),
    };
    let label = ui.add(egui::Label::new(egui::RichText::new(text).color(color)).selectable(false));
    if let Some(err) = &e.error {
        label.on_hover_text(err);
    }
    if let Some(scan) = &e.scan {
        scan_label(ui, scan);
    }
    if let Some(note) = &e.note {
        ui.weak(tr("(note)")).on_hover_text(note);
    }
    if let Some(limit) = e.speed_limit {
        ui.weak(tr("(limited)")).on_hover_text(trf!("Limited to {speed}/s", speed = human_bytes(limit)));
    }
}

/// A row's right-click menu.
fn row_menu(ui: &mut egui::Ui, e: &DownloadEntry, list: &List, i: usize, actions: &mut Vec<Action>) {
    let on = list.connected;
    let mut item = |ui: &mut egui::Ui, enabled: bool, text: &str, action: Action| {
        if ui.add_enabled(enabled, egui::Button::new(text)).clicked() {
            actions.push(action);
            ui.close_menu();
        }
    };
    let done = e.status == Status::Done;
    if let Some(path) = e.path.as_ref().filter(|_| done) {
        item(ui, true, tr("Open"), Action::Open(e.id));
        item(ui, true, tr("Open with..."), Action::OpenWith(path.clone()));
        item(ui, true, tr("Open folder"), Action::ShowInFolder(path.clone()));
        ui.separator();
    }
    if let Some(q) = list.queue {
        let len = list.shown.len();
        item(ui, on && i > 0, tr("Move up"), Action::MoveToQueue {
            id: e.id,
            queue: q.id,
            position: Some(i.saturating_sub(1)),
        });
        item(ui, on && i + 1 < len, tr("Move down"), Action::MoveToQueue {
            id: e.id,
            queue: q.id,
            position: Some(i + 1),
        });
        ui.separator();
    }
    let stopped = matches!(e.status, Status::Failed | Status::Canceled | Status::Interrupted);
    item(ui, on && (done || stopped), tr("Redownload"), Action::Redownload(e.id));
    match e.status {
        Status::Waiting => item(ui, on, tr("Download Now"), Action::Confirm(Answer::start(e.id))),
        _ => item(ui, on && stopped, tr("Resume Download"), Action::Retry(e.id)),
    }
    item(
        ui,
        on && matches!(e.status, Status::Queued | Status::Downloading | Status::Confirming),
        tr("Stop Download"),
        Action::Cancel(e.id),
    );
    item(ui, on && (stopped || e.status == Status::Waiting), tr("Refresh download address..."), Action::OpenRefresh(e.id));
    if e.error.as_deref().and_then(uidm::engine::login_wanted).is_some() {
        item(ui, on, tr("Log in..."), Action::OpenLogin(e.id));
    }
    let ids: Vec<u64> = if list.selected.contains(&e.id) {
        list.shown.iter().filter(|r| list.selected.contains(&r.id) && r.status.is_terminal()).map(|r| r.id).collect()
    } else {
        vec![e.id]
    };
    item(ui, on && e.status.is_terminal(), tr("Remove"), Action::Remove(ids));
    ui.add_enabled_ui(on && e.status.is_terminal() && !done, |ui| {
        ui.menu_button(tr("Move to queue"), |ui| {
            for q in list.queues {
                if ui.button(&q.name).clicked() {
                    actions.push(Action::MoveToQueue {
                        id: e.id,
                        queue: q.id,
                        position: None,
                    });
                    ui.close_menu();
                }
            }
        });
    });
    ui.separator();
    let mut item = |ui: &mut egui::Ui, enabled: bool, text: &str, action: Action| {
        if ui.add_enabled(enabled, egui::Button::new(text)).clicked() {
            actions.push(action);
            ui.close_menu();
        }
    };
    if !done {
        item(ui, true, tr("Download window"), Action::OpenProgress(e.id));
    }
    item(ui, true, tr("Connections..."), Action::OpenConnections(e.id));
    item(ui, on && !done, tr("Speed limit..."), Action::OpenSpeedLimit(e.id));
    if e.path.as_deref().filter(|_| done).is_some_and(uidm::media::convert::is_video) {
        item(ui, on, tr("Convert..."), Action::OpenConvert(e.id));
    }
    ui.separator();
    item(ui, true, tr("Properties"), Action::OpenProperties(e.id));
}
