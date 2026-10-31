//! A download's own window, after IDM's download progress dialog: its
//! address, status, size, speed, time left and whether it can resume, with
//! Pause or Resume and Cancel. "Show details" adds the Connections dialog's
//! bar and table. Two more tabs hold its speed limit and what to do once it
//! finishes; Options can hide them.

use eframe::egui;
use uidm::protocol::{DownloadEntry, OnDone, SegmentInfo, Status};
use uidm::queue::WhenDone;
use uidm::settings::SPEED_LIMIT_RANGE_KBPS;

use crate::i18n::{tr, trf};

use super::{columns, connections, display_name, human_bytes, human_duration, theme, Action};

#[derive(Clone, Copy, PartialEq)]
pub enum Tab {
    Status,
    SpeedLimit,
    OnDone,
}

/// The tabs Options shows besides the status.
pub struct Tabs {
    pub speed_limit: bool,
    pub on_done: bool,
}

pub struct ProgressWindow {
    pub id: u64,
    tab: Tab,
    /// The connections' bar and table are shown.
    details: bool,
    /// The Speed limit tab's fields: whether it is on, and KB/s.
    limit_on: bool,
    limit_kbps: u64,
    /// The When done tab's choices, sent as they change.
    on_done: OnDone,
}

impl ProgressWindow {
    pub fn new(e: &DownloadEntry) -> Self {
        ProgressWindow {
            id: e.id,
            tab: Tab::Status,
            details: false,
            limit_on: e.speed_limit.is_some(),
            limit_kbps: e.speed_limit.map_or(512, |b| (b / 1024).max(1)),
            on_done: e.on_done.unwrap_or_default(),
        }
    }
}

fn status_text(e: &DownloadEntry) -> &'static str {
    match e.status {
        Status::Queued => tr("Waiting in its queue"),
        Status::Confirming => tr("Waiting for the save-as dialog"),
        Status::Waiting => tr("Waiting to be started"),
        Status::Downloading if e.done == 0 => tr("Connecting..."),
        Status::Downloading => tr("Receiving data..."),
        Status::Done => tr("Done"),
        Status::Failed => tr("Failed"),
        Status::Canceled => tr("Canceled"),
        Status::Interrupted => tr("Interrupted"),
    }
}

/// Whether the server lets it go on from where it stopped. Unknown until it
/// has started once.
fn resume_text(e: &DownloadEntry) -> &'static str {
    match (e.last_try, e.resumable) {
        (None, _) => tr("(unknown)"),
        (Some(_), true) => tr("Yes"),
        (Some(_), false) => tr("No"),
    }
}

/// Draw the window for `w.id`. Returns false once closed, once the download
/// is done (its completion dialog or notification takes over), or when it
/// left the list.
#[allow(clippy::too_many_arguments)]
pub fn window(
    ctx: &egui::Context,
    w: &mut ProgressWindow,
    entry: Option<&DownloadEntry>,
    segments: Option<&[SegmentInfo]>,
    speed: Option<f64>,
    tabs: &Tabs,
    connected: bool,
    actions: &mut Vec<Action>,
) -> bool {
    let Some(e) = entry.filter(|e| e.status != Status::Done) else {
        return false;
    };
    if (w.tab == Tab::SpeedLimit && !tabs.speed_limit) || (w.tab == Tab::OnDone && !tabs.on_done) {
        w.tab = Tab::Status;
    }
    let running = speed.filter(|s| *s > 0.0 && e.status == Status::Downloading);
    let fraction = columns::fraction(e);
    let title = match fraction {
        Some(f) => trf!("{percent}% {name}", percent = (f * 100.0).floor() as u32, name = display_name(e)),
        None => display_name(e),
    };
    let mut open = true;
    let mut close = false;
    crate::dialog::Window::new(title)
        .id(egui::Id::new(("progress", w.id)))
        .dlu_width(353.0).taskbar(true)
        .open(&mut open)
        .resizable(false)
        .show(ctx, |ui| {
            if tabs.speed_limit || tabs.on_done {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut w.tab, Tab::Status, tr("Download status"));
                    if tabs.speed_limit {
                        ui.selectable_value(&mut w.tab, Tab::SpeedLimit, tr("Speed limit"));
                    }
                    if tabs.on_done {
                        ui.selectable_value(&mut w.tab, Tab::OnDone, tr("When done"));
                    }
                });
                ui.separator();
            }
            match w.tab {
                Tab::Status => status_tab(ui, e, fraction, running),
                Tab::SpeedLimit => limit_tab(ui, w, e, running, connected, actions),
                Tab::OnDone => done_tab(ui, w, connected, actions),
            }
            ui.add_space(6.0);
            let bar = egui::ProgressBar::new(fraction.unwrap_or(0.0)).desired_width(480.0);
            theme::progress_bar(ui, if fraction.is_some() {
                bar.show_percentage()
            } else {
                bar.text(human_bytes(e.done))
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let details = if w.details { tr("<< Hide details") } else { tr("Show details >>") };
                if ui.button(details).clicked() {
                    w.details = !w.details;
                }
                ui.add_enabled_ui(connected, |ui| match e.status {
                    Status::Queued | Status::Downloading => {
                        let hint = if e.resumable || e.last_try.is_none() {
                            tr("Stop it; Resume goes on from where it stopped")
                        } else {
                            tr("Stop it. This server can't resume, so Resume starts over.")
                        };
                        if ui.button(tr("Pause")).on_hover_text(hint).clicked() {
                            actions.push(Action::Cancel(e.id));
                        }
                    }
                    Status::Failed | Status::Canceled | Status::Interrupted => {
                        if ui.button(tr("Resume")).clicked() {
                            actions.push(Action::Retry(e.id));
                        }
                    }
                    Status::Confirming | Status::Waiting | Status::Done => {}
                });
                if ui.button(tr("Cancel")).on_hover_text(tr("Stop it and close this window; it stays in the list")).clicked() {
                    if matches!(e.status, Status::Queued | Status::Downloading) {
                        actions.push(Action::Cancel(e.id));
                    }
                    close = true;
                }
            });
            if w.details {
                ui.add_space(8.0);
                connections::details(ui, w.id, e, segments);
            }
        });
    open && !close
}

fn status_tab(ui: &mut egui::Ui, e: &DownloadEntry, fraction: Option<f32>, running: Option<f64>) {
    egui::Grid::new("download_status").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
        ui.label(tr("Address"));
        ui.allocate_ui_with_layout(
            egui::vec2(380.0, 18.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_width(380.0);
                ui.add(egui::Label::new(&e.url).truncate()).on_hover_text(&e.url);
            },
        );
        ui.end_row();

        ui.label(tr("Status"));
        ui.vertical(|ui| {
            ui.label(status_text(e));
            if let Some(err) = &e.error {
                ui.colored_label(ui.visuals().error_fg_color, err);
            }
        });
        ui.end_row();

        ui.label(tr("File size"));
        ui.label(e.total.map_or_else(|| tr("(unknown)").to_string(), human_bytes));
        ui.end_row();

        ui.label(tr("Downloaded"));
        ui.label(match fraction {
            Some(f) => trf!("{done} ({percent}%)", done = human_bytes(e.done), percent = format!("{:.1}", f * 100.0)),
            None => human_bytes(e.done),
        });
        ui.end_row();

        let downloading = e.status == Status::Downloading;
        ui.label(tr("Transfer rate"));
        ui.label(running.map(|bps| format!("{}/s", human_bytes(bps as u64))).unwrap_or_default());
        ui.end_row();

        ui.label(tr("Time left"));
        ui.label(match columns::time_left(e, running) {
            Some(secs) => human_duration(secs),
            None if downloading => tr("(unknown)").to_string(),
            None => String::new(),
        });
        ui.end_row();

        ui.label(tr("Resume capability"));
        ui.label(resume_text(e));
        ui.end_row();
    });
}

fn limit_tab(
    ui: &mut egui::Ui,
    w: &mut ProgressWindow,
    e: &DownloadEntry,
    running: Option<f64>,
    connected: bool,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        ui.checkbox(&mut w.limit_on, tr("Limit to"));
        let (lo, hi) = SPEED_LIMIT_RANGE_KBPS;
        ui.add_enabled(w.limit_on, egui::DragValue::new(&mut w.limit_kbps).range(lo..=hi).suffix(" KB/s"));
    });
    ui.weak(tr("The total limit still applies; the lower one wins. The limit stays when it stops and resumes."));
    ui.add_space(4.0);
    ui.label(trf!(
        "Transfer rate: {speed}",
        speed = running.map(|bps| format!("{}/s", human_bytes(bps as u64))).unwrap_or_else(|| "-".into())
    ));
    ui.add_space(4.0);
    let wanted = w.limit_on.then_some(w.limit_kbps * 1024);
    ui.horizontal(|ui| {
        if ui.add_enabled(connected && wanted != e.speed_limit, egui::Button::new(tr("Apply"))).clicked() {
            actions.push(Action::SetSpeedLimit(e.id, wanted));
        }
        if ui.button(tr("Hide tab")).on_hover_text(tr("Options > General can show it again")).clicked() {
            actions.push(Action::HideProgressTab(Tab::SpeedLimit));
        }
    });
}

fn done_tab(ui: &mut egui::Ui, w: &mut ProgressWindow, connected: bool, actions: &mut Vec<Action>) {
    let before = w.on_done;
    ui.add_enabled_ui(connected, |ui| on_done_fields(ui, &mut w.on_done));
    if w.on_done != before {
        actions.push(Action::SetOnDone(w.id, Some(w.on_done)));
    }
    ui.add_space(4.0);
    if ui.button(tr("Hide tab")).on_hover_text(tr("Options > General can show it again")).clicked() {
        actions.push(Action::HideProgressTab(Tab::OnDone));
    }
}

/// What one download does once it finishes. Properties shows these too.
pub fn on_done_fields(ui: &mut egui::Ui, on_done: &mut OnDone) {
    ui.checkbox(&mut on_done.dialog, tr("Show the completion dialog"))
        .on_hover_text(tr("For this download, whatever Options > When done says"));
    ui.horizontal(|ui| {
        ui.label(tr("Then"));
        egui::ComboBox::from_id_salt("on_done_then")
            .selected_text(tr(on_done.then.label()))
            .show_ui(ui, |ui| {
                for action in WhenDone::ALL {
                    ui.selectable_value(&mut on_done.then, action, tr(action.label()));
                }
            });
    });
    if on_done.then == WhenDone::ShutDown {
        ui.checkbox(&mut on_done.force_close, tr("Force apps to close"))
            .on_hover_text(tr("Shut down even if a program asks to save its work first; unsaved work is lost"));
    } else {
        on_done.force_close = false;
    }
    if on_done.then.needs_countdown() {
        ui.weak(tr("A 60-second countdown comes first, which Cancel stops."));
    }
}
