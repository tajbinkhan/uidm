//! A download's Connections dialog: a bar with each connection's range and
//! how much of it is in, and a table of the connections with their bytes and
//! state, updated live. A stream shows how many of its segments are in.

use eframe::egui;
use uidm::protocol::{DownloadEntry, SegmentInfo, SegmentState};

use crate::i18n::{tr, trf};

use super::{display_name, human_bytes};

fn state_text(s: SegmentState) -> &'static str {
    match s {
        SegmentState::Waiting => tr("Waiting"),
        SegmentState::Connecting => tr("Connecting"),
        SegmentState::Receiving => tr("Receiving"),
        SegmentState::Retrying => tr("Retrying"),
        SegmentState::Done => tr("Done"),
    }
}

/// Draw the dialog for download `id`. Returns false once closed or when the
/// download left the list.
pub fn dialog(ctx: &egui::Context, id: u64, entry: Option<&DownloadEntry>, segments: Option<&[SegmentInfo]>) -> bool {
    let Some(e) = entry else {
        return false;
    };
    let mut open = true;
    egui::Window::new(trf!("Connections of {name}", name = display_name(e)))
        .id(egui::Id::new(("connections", id)))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| details(ui, id, e, segments));
    open
}

/// The bar and table of a download's connections, or a line saying why
/// there are none. The download window shows these too.
pub fn details(ui: &mut egui::Ui, id: u64, e: &DownloadEntry, segments: Option<&[SegmentInfo]>) {
    if let Some([done, all]) = e.parts.filter(|[_, all]| *all > 0) {
        ui.label(trf!("A stream of {all} segments; {done} are in.", all = all, done = done));
        ui.add(egui::ProgressBar::new(done as f32 / all as f32).desired_width(480.0).show_percentage());
        return;
    }
    let Some(segments) = segments.filter(|s| !s.is_empty()) else {
        ui.weak(if e.connections == 1 {
            tr("This server sends the file over one connection.")
        } else {
            tr("No connection details yet. They appear while the download runs.")
        });
        return;
    };
    let mut sorted = segments.to_vec();
    sorted.sort_by_key(|s| s.start);
    let total = e.total.unwrap_or_else(|| sorted.last().map_or(0, |s| s.end + 1)).max(1);
    bar(ui, &sorted, total);
    ui.add_space(8.0);
    let active = sorted
        .iter()
        .filter(|s| matches!(s.state, SegmentState::Connecting | SegmentState::Receiving | SegmentState::Retrying))
        .count();
    ui.weak(trf!("{ranges} ranges, {active} connections working", ranges = sorted.len(), active = active));
    ui.add_space(4.0);
    egui::Grid::new(("connections_table", id))
        .striped(true)
        .num_columns(4)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            for title in [tr("Range"), tr("Downloaded"), "", tr("State")] {
                ui.strong(title);
            }
            ui.end_row();
            for s in &sorted {
                let len = s.end + 1 - s.start;
                ui.label(format!("{} - {}", human_bytes(s.start), human_bytes(s.end + 1)));
                ui.label(trf!("{done} of {total}", done = human_bytes(s.done), total = human_bytes(len)));
                ui.add(egui::ProgressBar::new(s.done as f32 / len.max(1) as f32).desired_width(100.0));
                let text = state_text(s.state);
                match s.state {
                    SegmentState::Retrying => ui.colored_label(ui.visuals().warn_fg_color, text),
                    SegmentState::Waiting | SegmentState::Done => ui.weak(text),
                    _ => ui.label(text),
                };
                ui.end_row();
            }
        });
}

/// One bar for the whole file: each range drawn where it sits, its fetched
/// part filled, with a thin gap between ranges.
fn bar(ui: &mut egui::Ui, segments: &[SegmentInfo], total: u64) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(480.0, 20.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    painter.rect_filled(rect, 3.0, visuals.extreme_bg_color);
    let x = |at: u64| rect.left() + rect.width() * (at as f32 / total as f32);
    for s in segments {
        let left = x(s.start);
        let fill_to = x(s.start + s.done.min(s.end + 1 - s.start));
        let color = match s.state {
            SegmentState::Retrying => visuals.warn_fg_color,
            SegmentState::Done => visuals.widgets.inactive.fg_stroke.color.gamma_multiply(0.55),
            _ => visuals.selection.bg_fill,
        };
        let filled = egui::Rect::from_min_max(egui::pos2(left, rect.top()), egui::pos2(fill_to, rect.bottom()));
        painter.rect_filled(filled, 0.0, color);
        // A separator at the start of each range.
        painter.line_segment(
            [egui::pos2(left, rect.top()), egui::pos2(left, rect.bottom())],
            egui::Stroke {
                width: 1.0,
                color: visuals.panel_fill,
            },
        );
    }
    painter.rect_stroke(rect, 3.0, visuals.widgets.noninteractive.bg_stroke, egui::StrokeKind::Inside);
}
