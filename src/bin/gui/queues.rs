//! The window's queue controls: the header above a queue's downloads, the
//! Queues dialog where queues and their schedules are edited, and the
//! countdown before a finished queue sleeps or shuts down the computer.

use std::time::Instant;

use eframe::egui;
use uidm::queue::{
    self, Queue, Schedule, WhenDone, MAIN_QUEUE, QUEUE_CONCURRENCY_RANGE, QUEUE_RETRIES_MAX,
};

use super::Action;

const DAY_NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// When `q` next starts on its own, for display.
pub fn next_run(q: &Queue) -> Option<String> {
    let s = q.schedule.as_ref()?;
    let now = queue::local_now();
    if q.running && q.started_by_schedule {
        let end = q.last_scheduled_day.and_then(|d| s.end_of_run(d));
        return Some(match (&s.stop, end) {
            (Some(_), Some(end)) => format!("running until {}", end.display()),
            _ => "running".to_string(),
        });
    }
    s.next_start(now).map(|m| m.display())
}

/// The strip above a queue's downloads: its state, Start or Stop, what
/// happens after this run, and its next scheduled run.
pub fn header(ui: &mut egui::Ui, q: &Queue, connected: bool, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        ui.strong(&q.name);
        if q.running {
            ui.colored_label(ui.visuals().text_color(), "Started");
        } else {
            ui.weak("Stopped");
        }
        ui.add_enabled_ui(connected, |ui| {
            if q.running {
                if ui.button("Stop").clicked() {
                    actions.push(Action::StopQueue(q.id));
                }
            } else if ui.button("Start").clicked() {
                actions.push(Action::StartQueue(q.id));
            }
            ui.separator();
            ui.label("When this run ends:");
            let current = q.when_done_once.unwrap_or(q.when_done);
            let mut chosen = current;
            egui::ComboBox::from_id_salt(("once", q.id))
                .selected_text(chosen.label())
                .show_ui(ui, |ui| {
                    for w in WhenDone::ALL {
                        ui.selectable_value(&mut chosen, w, w.label());
                    }
                });
            if chosen != current {
                // Back to the queue's own choice clears the one-off.
                let once = (chosen != q.when_done).then_some(chosen);
                actions.push(Action::SetQueueOnce(q.id, once));
            }
            ui.separator();
            if ui.button("Edit queue...").clicked() {
                actions.push(Action::OpenQueues(Some(q.id)));
            }
        });
        if let Some(next) = next_run(q) {
            ui.separator();
            ui.weak(format!("Next run: {next}"));
        }
    });
}

/// One queue being edited in the Queues dialog.
pub struct QueueDraft {
    /// 0 for a new queue.
    id: u64,
    name: String,
    max_concurrent: usize,
    retries: u32,
    when_done: WhenDone,
    scheduled: bool,
    start: String,
    has_stop: bool,
    stop: String,
    by_date: bool,
    days: [bool; 7],
    date: String,
    error: Option<String>,
}

impl QueueDraft {
    fn new_queue() -> Self {
        QueueDraft::from(&Queue::new(0, "New queue"))
    }

    fn from(q: &Queue) -> Self {
        let s = q.schedule.clone();
        let mut days = [false; 7];
        for d in s.as_ref().map(|s| s.days.clone()).unwrap_or_else(|| vec![0, 1, 2, 3, 4, 5, 6]) {
            if let Some(slot) = days.get_mut(d as usize) {
                *slot = true;
            }
        }
        QueueDraft {
            id: q.id,
            name: q.name.clone(),
            max_concurrent: q.max_concurrent,
            retries: q.retries,
            when_done: q.when_done,
            scheduled: s.is_some(),
            start: s.as_ref().map_or("23:00".into(), |s| s.start.clone()),
            has_stop: s.as_ref().is_some_and(|s| s.stop.is_some()),
            stop: s.as_ref().and_then(|s| s.stop.clone()).unwrap_or_else(|| "07:00".into()),
            by_date: s.as_ref().is_some_and(|s| s.date.is_some()),
            days,
            date: s.as_ref().and_then(|s| s.date.clone()).unwrap_or_default(),
            error: None,
        }
    }

    /// The queue these fields describe, checked as the daemon checks it.
    fn to_queue(&self) -> Result<Queue, String> {
        let mut q = Queue::new(self.id, self.name.trim());
        q.max_concurrent = self.max_concurrent;
        q.retries = self.retries;
        q.when_done = self.when_done;
        q.schedule = self.scheduled.then(|| Schedule {
            start: self.start.trim().to_string(),
            stop: self.has_stop.then(|| self.stop.trim().to_string()),
            days: if self.by_date {
                Vec::new()
            } else {
                (0u8..7).filter(|d| self.days[*d as usize]).collect()
            },
            date: self.by_date.then(|| self.date.trim().to_string()),
        });
        q.validate()?;
        Ok(q)
    }
}

/// The Queues dialog: the queues on the left, the selected one's settings on
/// the right.
pub struct QueuesDialog {
    selected: u64,
    draft: QueueDraft,
    /// A new queue just saved, to select once the daemon lists it.
    pending_name: Option<String>,
}

impl QueuesDialog {
    pub fn open(queues: &[Queue], select: Option<u64>) -> Self {
        let q = select
            .and_then(|id| queues.iter().find(|q| q.id == id))
            .or_else(|| queues.first());
        match q {
            Some(q) => QueuesDialog {
                selected: q.id,
                draft: QueueDraft::from(q),
                pending_name: None,
            },
            None => QueuesDialog {
                selected: 0,
                draft: QueueDraft::new_queue(),
                pending_name: None,
            },
        }
    }
}

/// Draw the Queues dialog. Returns false once it is closed.
pub fn dialog(
    ctx: &egui::Context,
    d: &mut QueuesDialog,
    queues: &[Queue],
    connected: bool,
    actions: &mut Vec<Action>,
) -> bool {
    // Select a queue that was just created, once it exists.
    if let Some(name) = &d.pending_name {
        if let Some(q) = queues.iter().find(|q| q.name.eq_ignore_ascii_case(name)) {
            d.selected = q.id;
            d.draft = QueueDraft::from(q);
            d.pending_name = None;
        }
    }
    let mut open = true;
    let mut close = false;
    egui::Window::new("Queues")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .min_width(640.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(160.0);
                    for q in queues {
                        let label = if q.running { format!("{} (started)", q.name) } else { q.name.clone() };
                        if ui.selectable_label(d.selected == q.id, label).clicked() && d.selected != q.id {
                            d.selected = q.id;
                            d.draft = QueueDraft::from(q);
                        }
                    }
                    ui.add_space(6.0);
                    if ui.button("New queue").clicked() {
                        d.selected = 0;
                        d.draft = QueueDraft::new_queue();
                    }
                });
                ui.separator();
                ui.vertical(|ui| form(ui, d, queues, connected, actions, &mut close));
            });
        });
    open && !close
}

fn form(
    ui: &mut egui::Ui,
    d: &mut QueuesDialog,
    queues: &[Queue],
    connected: bool,
    actions: &mut Vec<Action>,
    close: &mut bool,
) {
    let draft = &mut d.draft;
    egui::Grid::new("queue_form").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label("Name");
        ui.add(egui::TextEdit::singleline(&mut draft.name).desired_width(220.0));
        ui.end_row();

        ui.label("Downloads at once");
        let (lo, hi) = QUEUE_CONCURRENCY_RANGE;
        ui.add(egui::DragValue::new(&mut draft.max_concurrent).range(lo..=hi));
        ui.end_row();

        ui.label("Retry a failed download");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut draft.retries).range(0..=QUEUE_RETRIES_MAX));
            ui.label("times");
        });
        ui.end_row();

        ui.label("When it is done");
        egui::ComboBox::from_id_salt("when_done")
            .selected_text(draft.when_done.label())
            .show_ui(ui, |ui| {
                for w in WhenDone::ALL {
                    ui.selectable_value(&mut draft.when_done, w, w.label());
                }
            });
        ui.end_row();

        ui.label("Schedule");
        ui.checkbox(&mut draft.scheduled, "Start and stop on a schedule");
        ui.end_row();

        if draft.scheduled {
            ui.label("Start at");
            ui.add(egui::TextEdit::singleline(&mut draft.start).hint_text("23:00").desired_width(60.0));
            ui.end_row();

            ui.label("");
            ui.horizontal(|ui| {
                ui.checkbox(&mut draft.has_stop, "Stop at");
                ui.add_enabled(
                    draft.has_stop,
                    egui::TextEdit::singleline(&mut draft.stop).hint_text("07:00").desired_width(60.0),
                );
            });
            ui.end_row();

            ui.label("Run");
            ui.vertical(|ui| {
                ui.radio_value(&mut draft.by_date, false, "On these days");
                ui.add_enabled_ui(!draft.by_date, |ui| {
                    ui.horizontal(|ui| {
                        for (i, name) in DAY_NAMES.iter().enumerate() {
                            ui.checkbox(&mut draft.days[i], *name);
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.radio_value(&mut draft.by_date, true, "Once, on");
                    ui.add_enabled(
                        draft.by_date,
                        egui::TextEdit::singleline(&mut draft.date)
                            .hint_text("YYYY-MM-DD")
                            .desired_width(100.0),
                    );
                });
            });
            ui.end_row();

            if let Ok(q) = draft.to_queue() {
                if let Some(next) = q.schedule.as_ref().and_then(|s| s.next_start(queue::local_now())) {
                    ui.label("Next run");
                    ui.label(next.display());
                    ui.end_row();
                }
            }
            ui.label("");
            ui.weak("Times are this computer's. A stop time earlier than the start is the next day.");
            ui.end_row();
        }
    });

    if let Some(err) = &draft.error {
        ui.colored_label(ui.visuals().error_fg_color, err);
    }
    ui.add_space(8.0);
    let mut pending_name = None;
    let mut deleted = false;
    ui.horizontal(|ui| {
        ui.add_enabled_ui(connected, |ui| {
            if ui.button("Save").clicked() {
                let duplicate = queues
                    .iter()
                    .any(|q| q.id != draft.id && q.name.eq_ignore_ascii_case(draft.name.trim()));
                match draft.to_queue() {
                    Ok(_) if duplicate => {
                        draft.error = Some(format!("There is already a queue named \"{}\".", draft.name.trim()))
                    }
                    Ok(q) => {
                        draft.error = None;
                        if q.id == 0 {
                            pending_name = Some(q.name.clone());
                        }
                        actions.push(Action::SaveQueue(q));
                    }
                    Err(e) => draft.error = Some(e),
                }
            }
            let deletable = draft.id != 0 && draft.id != MAIN_QUEUE;
            if ui
                .add_enabled(deletable, egui::Button::new("Delete"))
                .on_hover_text("Its downloads move to the main queue")
                .clicked()
            {
                actions.push(Action::DeleteQueue(draft.id));
                deleted = true;
            }
        });
        if ui.button("Close").clicked() {
            *close = true;
        }
    });
    if pending_name.is_some() {
        d.pending_name = pending_name;
    }
    if deleted {
        d.selected = MAIN_QUEUE;
        if let Some(main) = queues.iter().find(|q| q.id == MAIN_QUEUE) {
            d.draft = QueueDraft::from(main);
        }
    }
}

/// The countdown before a power action, with Cancel. Repaints itself.
pub fn countdown(
    ctx: &egui::Context,
    action: WhenDone,
    deadline: Instant,
    queue: &str,
    connected: bool,
    actions: &mut Vec<Action>,
) {
    let left = deadline.saturating_duration_since(Instant::now()).as_secs();
    let verb = match action {
        WhenDone::Sleep => "go to sleep",
        WhenDone::Hibernate => "hibernate",
        WhenDone::ShutDown => "shut down",
        _ => "continue",
    };
    egui::Window::new("Queue finished")
        .id(egui::Id::new("countdown"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(format!("The queue \"{queue}\" is done."));
            ui.heading(format!("This computer will {verb} in {left} seconds."));
            ui.add_space(8.0);
            if ui.add_enabled(connected, egui::Button::new("Cancel")).clicked() {
                actions.push(Action::CancelCountdown);
            }
        });
    ctx.request_repaint_after(std::time::Duration::from_millis(250));
}

#[cfg(test)]
mod tests {
    use super::*;
    use uidm::queue::{parse_date, parse_time};

    #[test]
    fn draft_round_trips_a_queue() {
        let mut q = Queue::new(3, "Night");
        q.max_concurrent = 2;
        q.retries = 1;
        q.when_done = WhenDone::ShutDown;
        q.schedule = Some(Schedule {
            start: "23:00".into(),
            stop: Some("07:00".into()),
            days: vec![0, 4],
            date: None,
        });
        let back = QueueDraft::from(&q).to_queue().unwrap();
        assert_eq!(back.schedule, q.schedule);
        assert_eq!((back.max_concurrent, back.retries, back.when_done), (2, 1, WhenDone::ShutDown));

        let mut once = QueueDraft::from(&q);
        once.by_date = true;
        once.date = "2026-12-24".into();
        once.has_stop = false;
        let s = once.to_queue().unwrap().schedule.unwrap();
        assert_eq!((s.date.as_deref(), s.days.len(), s.stop), (Some("2026-12-24"), 0, None));

        once.start = "25:00".into();
        assert!(once.to_queue().is_err());
        assert!(parse_time("23:00").is_some() && parse_date("2026-12-24").is_some());
    }
}
