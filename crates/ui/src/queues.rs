//! The window's queue controls: the header above a queue's downloads, the
//! Queues dialog where queues and their schedules are edited, and the
//! countdown before a finished queue sleeps or shuts down the computer.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use eframe::egui;
use uidm_domain::queue::{
    Queue, QueueKind, Schedule, WhenDone, MAIN_QUEUE, QUEUE_CONCURRENCY_RANGE, QUEUE_RETRIES_MAX,
};

use super::{take_picked, Action, Picked};
use crate::i18n::{tr, trf};

const DAY_NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// When `q` next starts on its own, for display.
pub fn next_run(q: &Queue) -> Option<String> {
    let s = q.schedule.as_ref()?;
    let now = crate::ports::ports().now();
    if q.running && q.started_by_schedule {
        let end = q.last_scheduled_day.and_then(|d| s.end_of_run(d));
        return Some(match (&s.stop, end) {
            (Some(_), Some(end)) => trf!("running until {time}", time = end.display()),
            _ => tr("running").to_string(),
        });
    }
    s.next_run(now, q.last_scheduled_start).map(|m| m.display())
}

/// A new queue called `name`, added from outside the Queues dialog (the
/// "+" by the save-as dialog's queue picker), checked as that dialog checks
/// one before the daemon sees it, so a mistake shows next to the box.
pub fn new_named(name: &str, queues: &[Queue]) -> Result<Queue, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(tr("A queue needs a name.").to_string());
    }
    if named(queues, name).is_some() {
        return Err(trf!("There is already a queue named \"{name}\".", name = name));
    }
    // Id 0 asks the daemon to create it and pick the id.
    Ok(Queue::new(0, name))
}

/// The queue called `name`, matched the way the daemon matches names (case
/// ignored), so a queue just created can be found once it is listed.
pub fn named(queues: &[Queue], name: &str) -> Option<u64> {
    let name = name.trim();
    queues.iter().find(|q| q.name.eq_ignore_ascii_case(name)).map(|q| q.id)
}

/// The power plan's wake timer setting as a worker last read it, `None`
/// until it has. A worker reads it again at most every ten seconds, since
/// the dialogs draw many times a second and the read asks the system.
fn wake_timers_allowed(ctx: &egui::Context) -> Option<(bool, bool)> {
    use std::sync::{Mutex, PoisonError};
    use std::time::Duration;
    /// When a read was last started, and what the last one found.
    type Cached = (Option<Instant>, Option<(bool, bool)>);
    static CACHE: Mutex<Cached> = Mutex::new((None, None));
    // The cache is two plain values that a panicking thread cannot leave
    // half-changed.
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if cache.0.is_none_or(|at| at.elapsed() >= Duration::from_secs(10)) {
        cache.0 = Some(Instant::now());
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let value = crate::ports::ports().system.wake_timers_allowed();
            CACHE.lock().unwrap_or_else(PoisonError::into_inner).1 = value;
            ctx.request_repaint();
        });
    }
    cache.1
}

/// "Wake the computer to start" for a queue's or a grabber project's
/// schedule, with a warning when the power plan turns wake timers off.
pub fn wake_field(ui: &mut egui::Ui, wake: &mut bool) {
    ui.vertical(|ui| {
        ui.add_enabled(cfg!(windows), egui::Checkbox::new(wake, tr("Wake the computer to start")))
            .on_hover_text(tr("Wakes Windows from sleep or hibernation shortly before the start time. UIDM must be running."))
            .on_disabled_hover_text(tr("Only on Windows for now"));
        if *wake {
            if let Some((ac, dc)) = wake_timers_allowed(ui.ctx()) {
                let off = match (ac, dc) {
                    (false, false) => Some(tr("Windows has wake timers turned off, so it will not wake up for this.")),
                    (false, true) => Some(tr("Windows has wake timers turned off on mains power.")),
                    (true, false) => Some(tr("Windows has wake timers turned off on battery.")),
                    (true, true) => None,
                };
                if let Some(off) = off {
                    ui.colored_label(ui.visuals().warn_fg_color, off).on_hover_text(tr(
                        "Turn them on in Control Panel > Power Options > Change plan settings > Change advanced power settings > Sleep > Allow wake timers: Enable.",
                    ));
                }
            }
        }
    });
}

/// "Start again every N hours M minutes" for a queue's or a grabber
/// project's schedule.
pub fn repeat_fields(ui: &mut egui::Ui, on: &mut bool, hours: &mut u32, minutes: &mut u32) {
    ui.horizontal(|ui| {
        ui.checkbox(on, tr("Start again every"));
        ui.add_enabled_ui(*on, |ui| {
            ui.add(egui::DragValue::new(hours).range(0..=99));
            ui.label(tr("hours"));
            ui.add(egui::DragValue::new(minutes).range(0..=59));
            ui.label(tr("minutes"));
        });
    })
    .response
    .on_hover_text(tr("Counted from the last start, until the stop time (or midnight without one)"));
}

/// The fields of `repeat_fields` as a schedule's `repeat_minutes`; 0 is
/// left for validation to refuse.
pub fn repeat_value(on: bool, hours: u32, minutes: u32) -> Option<u32> {
    on.then_some(hours * 60 + minutes)
}

/// `repeat_minutes` split for `repeat_fields`, two hours when there is none.
pub fn repeat_split(repeat: Option<u32>) -> (bool, u32, u32) {
    let every = repeat.unwrap_or(120);
    (repeat.is_some(), every / 60, every % 60)
}

/// The folder the file picker opens in for an "open when done" field: the
/// one holding the file already named there, so changing it starts next to
/// it. None (the picker's own default) when the field is empty or not a
/// full path.
pub fn picker_folder(field: &str) -> Option<PathBuf> {
    let path = Path::new(field.trim());
    if !path.is_absolute() {
        return None;
    }
    path.parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf)
}

/// The "Browse..." button by an "open when done" field (a queue's or a
/// grabber project's): picks any file or program on its own thread, since
/// the picker blocks, and puts it in `slot` for `take_picked`.
pub fn open_file_button(ui: &mut egui::Ui, field: &str, slot: &Picked) {
    if ui.button(tr("Browse...")).clicked() {
        let (slot, ctx) = (Arc::clone(slot), ui.ctx().clone());
        let start = picker_folder(field);
        std::thread::spawn(move || {
            let title = tr("Open this file when done");
            if let Some(file) = crate::ports::ports().pickers.pick_file(title, &[], start.as_deref()) {
                *slot.lock().unwrap() = Some(file);
                ctx.request_repaint();
            }
        });
    }
}

/// The strip above a queue's downloads: its state, Start or Stop, what
/// happens after this run, and its next scheduled run.
pub fn header(ui: &mut egui::Ui, q: &Queue, connected: bool, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        ui.strong(&q.name);
        if q.running {
            ui.colored_label(ui.visuals().text_color(), tr("Started"));
        } else {
            ui.weak(tr("Stopped"));
        }
        ui.add_enabled_ui(connected, |ui| {
            if q.running {
                if ui.button(tr("Stop")).clicked() {
                    actions.push(Action::StopQueue(q.id));
                }
            } else if ui.button(tr("Start")).clicked() {
                actions.push(Action::StartQueue(q.id));
            }
            ui.separator();
            ui.label(tr("When this run ends:"));
            let current = q.when_done_once.unwrap_or(q.when_done);
            let mut chosen = current;
            egui::ComboBox::from_id_salt(("once", q.id)).selected_text(tr(chosen.label())).show_ui(ui, |ui| {
                for w in WhenDone::ALL {
                    ui.selectable_value(&mut chosen, w, tr(w.label()));
                }
            });
            if chosen != current {
                // Back to the queue's own choice clears the one-off.
                let once = (chosen != q.when_done).then_some(chosen);
                actions.push(Action::SetQueueOnce(q.id, once));
            }
            ui.separator();
            if ui.button(tr("Edit queue...")).clicked() {
                actions.push(Action::OpenQueues(Some(q.id)));
            }
        });
        if let Some(next) = next_run(q) {
            ui.separator();
            ui.weak(trf!("Next run: {next}", next = next));
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
    kind: QueueKind,
    when_done: WhenDone,
    /// Empty opens nothing.
    open_when_done: String,
    /// The file the Browse button picked for `open_when_done`.
    open_picked: Picked,
    force_close: bool,
    start_on_launch: bool,
    scheduled: bool,
    start: String,
    has_stop: bool,
    stop: String,
    by_date: bool,
    days: [bool; 7],
    date: String,
    repeat: bool,
    repeat_hours: u32,
    repeat_minutes: u32,
    wake: bool,
    error: Option<String>,
}

impl QueueDraft {
    fn new_queue() -> Self {
        QueueDraft::from(&Queue::new(0, tr("New queue")))
    }

    fn from(q: &Queue) -> Self {
        let s = q.schedule.clone();
        let (repeat, repeat_hours, repeat_minutes) = repeat_split(s.as_ref().and_then(|s| s.repeat_minutes));
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
            kind: q.kind,
            when_done: q.when_done,
            open_when_done: q.open_when_done.clone().unwrap_or_default(),
            open_picked: Picked::default(),
            force_close: q.force_close,
            start_on_launch: q.start_on_launch,
            scheduled: s.is_some(),
            start: s.as_ref().map_or("23:00".into(), |s| s.start.clone()),
            has_stop: s.as_ref().is_some_and(|s| s.stop.is_some()),
            stop: s.as_ref().and_then(|s| s.stop.clone()).unwrap_or_else(|| "07:00".into()),
            by_date: s.as_ref().is_some_and(|s| s.date.is_some()),
            days,
            date: s.as_ref().and_then(|s| s.date.clone()).unwrap_or_default(),
            repeat,
            repeat_hours,
            repeat_minutes,
            wake: s.as_ref().is_none_or(|s| s.wake),
            error: None,
        }
    }

    /// The queue these fields describe, checked as the daemon checks it.
    fn to_queue(&self) -> Result<Queue, String> {
        let mut q = Queue::new(self.id, self.name.trim());
        q.max_concurrent = self.max_concurrent;
        q.retries = self.retries;
        q.kind = self.kind;
        q.when_done = self.when_done;
        q.open_when_done = Some(self.open_when_done.trim().to_string()).filter(|f| !f.is_empty());
        q.force_close = self.force_close && self.when_done == WhenDone::ShutDown;
        q.start_on_launch = self.start_on_launch;
        q.schedule = self.scheduled.then(|| Schedule {
            start: self.start.trim().to_string(),
            stop: self.has_stop.then(|| self.stop.trim().to_string()),
            days: if self.by_date { Vec::new() } else { (0u8..7).filter(|d| self.days[*d as usize]).collect() },
            date: self.by_date.then(|| self.date.trim().to_string()),
            repeat_minutes: repeat_value(self.repeat, self.repeat_hours, self.repeat_minutes),
            wake: self.wake,
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
    /// Say that a schedule only runs while UIDM does, until dismissed.
    pub schedule_note: bool,
}

impl QueuesDialog {
    pub fn open(queues: &[Queue], select: Option<u64>) -> Self {
        let q = select.and_then(|id| queues.iter().find(|q| q.id == id)).or_else(|| queues.first());
        match q {
            Some(q) => {
                QueuesDialog { selected: q.id, draft: QueueDraft::from(q), pending_name: None, schedule_note: false }
            }
            None => {
                QueuesDialog { selected: 0, draft: QueueDraft::new_queue(), pending_name: None, schedule_note: false }
            }
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
        if let Some(q) = named(queues, name).and_then(|id| queues.iter().find(|q| q.id == id)) {
            d.selected = q.id;
            d.draft = QueueDraft::from(q);
            d.pending_name = None;
        }
    }
    let mut open = true;
    let mut close = false;
    crate::dialog::Window::new(tr("Queues"))
        .id(egui::Id::new("Queues"))
        .dlu_width(434.0)
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
                        let label = if q.running { trf!("{name} (started)", name = q.name) } else { q.name.clone() };
                        if ui.selectable_label(d.selected == q.id, label).clicked() && d.selected != q.id {
                            d.selected = q.id;
                            d.draft = QueueDraft::from(q);
                        }
                    }
                    ui.add_space(6.0);
                    if ui.button(tr("New queue")).clicked() {
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
    take_picked(&draft.open_picked, &mut draft.open_when_done);
    egui::Grid::new("queue_form").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label(tr("Name"));
        ui.add(egui::TextEdit::singleline(&mut draft.name).desired_width(220.0));
        ui.end_row();

        ui.label(tr("Downloads at once"));
        let (lo, hi) = QUEUE_CONCURRENCY_RANGE;
        ui.add(egui::DragValue::new(&mut draft.max_concurrent).range(lo..=hi));
        ui.end_row();

        ui.label(tr("Retry a failed download"));
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut draft.retries).range(0..=QUEUE_RETRIES_MAX));
            ui.label(tr("times"));
        });
        ui.end_row();

        ui.label(tr("Type"));
        ui.vertical(|ui| {
            for k in QueueKind::ALL {
                ui.radio_value(&mut draft.kind, k, tr(k.label()));
            }
        })
        .response
        .on_hover_text(tr(
            "Periodic synchronization keeps finished downloads in the queue. Each run asks the server about them and downloads again those whose size, ETag or date changed.",
        ));
        ui.end_row();

        ui.label(tr("When it is done"));
        egui::ComboBox::from_id_salt("when_done")
            .selected_text(tr(draft.when_done.label()))
            .show_ui(ui, |ui| {
                for w in WhenDone::ALL {
                    ui.selectable_value(&mut draft.when_done, w, tr(w.label()));
                }
            });
        ui.end_row();

        if draft.when_done == WhenDone::ShutDown {
            ui.label("");
            ui.checkbox(&mut draft.force_close, tr("Force apps to close"))
                .on_hover_text(tr("Shut down even if a program asks to save its work first; unsaved work is lost"));
            ui.end_row();
        }

        ui.label(tr("Then open"));
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut draft.open_when_done)
                    .hint_text(tr("Nothing; or a program or file, as a full path"))
                    .desired_width(300.0),
            )
            .on_hover_text(tr("Opened once each time a run of the queue finishes"));
            open_file_button(ui, &draft.open_when_done, &draft.open_picked);
        });
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut draft.start_on_launch, tr("Start this queue whenever UIDM starts"));
        ui.end_row();

        ui.label(tr("Schedule"));
        ui.checkbox(&mut draft.scheduled, tr("Start and stop on a schedule"));
        ui.end_row();

        if draft.scheduled {
            ui.label(tr("Start at"));
            ui.add(egui::TextEdit::singleline(&mut draft.start).hint_text("23:00").desired_width(60.0));
            ui.end_row();

            ui.label("");
            ui.horizontal(|ui| {
                ui.checkbox(&mut draft.has_stop, tr("Stop at"));
                ui.add_enabled(
                    draft.has_stop,
                    egui::TextEdit::singleline(&mut draft.stop).hint_text("07:00").desired_width(60.0),
                );
            });
            ui.end_row();

            ui.label("");
            repeat_fields(ui, &mut draft.repeat, &mut draft.repeat_hours, &mut draft.repeat_minutes);
            ui.end_row();

            ui.label("");
            wake_field(ui, &mut draft.wake);
            ui.end_row();

            ui.label(tr("Run"));
            ui.vertical(|ui| {
                ui.radio_value(&mut draft.by_date, false, tr("On these days"));
                ui.add_enabled_ui(!draft.by_date, |ui| {
                    ui.horizontal(|ui| {
                        for (i, name) in DAY_NAMES.iter().enumerate() {
                            ui.checkbox(&mut draft.days[i], tr(name));
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.radio_value(&mut draft.by_date, true, tr("Once, on"));
                    ui.add_enabled(
                        draft.by_date,
                        egui::TextEdit::singleline(&mut draft.date)
                            .hint_text(tr("YYYY-MM-DD"))
                            .desired_width(100.0),
                    );
                });
            });
            ui.end_row();

            if let Ok(q) = draft.to_queue() {
                if let Some(next) = q.schedule.as_ref().and_then(|s| s.next_start(crate::ports::ports().now())) {
                    ui.label(tr("Next run"));
                    ui.label(next.display());
                    ui.end_row();
                }
            }
            ui.label("");
            ui.weak(tr("Times are this computer's. A stop time earlier than the start is the next day."));
            ui.end_row();

            if d.schedule_note {
                ui.label("");
                ui.vertical(|ui| {
                    ui.label(tr("UIDM must be running at the start time for the schedule to start this queue. If it starts later, the queue starts then, while its time is still open. Start with Windows (Options > General) keeps it running."));
                    if ui.add_enabled(connected, egui::Button::new(tr("Don't show this again")).small()).clicked() {
                        actions.push(Action::HideScheduleNote);
                    }
                });
                ui.end_row();
            }
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
            if ui.button(tr("Save")).clicked() {
                let duplicate =
                    queues.iter().any(|q| q.id != draft.id && q.name.eq_ignore_ascii_case(draft.name.trim()));
                match draft.to_queue() {
                    Ok(_) if duplicate => {
                        draft.error = Some(trf!("There is already a queue named \"{name}\".", name = draft.name.trim()))
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
                .add_enabled(deletable, egui::Button::new(tr("Delete")))
                .on_hover_text(tr("Its downloads move to the main queue"))
                .clicked()
            {
                actions.push(Action::DeleteQueue(draft.id));
                deleted = true;
            }
        });
        ui.separator();
        // Start now and Stop act on the queue as saved, as IDM's do; a new
        // queue has nothing to start until it is saved.
        let saved = queues.iter().find(|q| q.id == draft.id && draft.id != 0);
        let running = saved.is_some_and(|q| q.running);
        ui.add_enabled_ui(connected && saved.is_some(), |ui| {
            if ui
                .add_enabled(!running, egui::Button::new(tr("Start now")))
                .on_hover_text(tr("Start the queue as saved, without waiting for its schedule"))
                .clicked()
            {
                actions.push(Action::StartQueue(draft.id));
            }
            if ui.add_enabled(running, egui::Button::new(tr("Stop"))).clicked() {
                actions.push(Action::StopQueue(draft.id));
            }
        });
        ui.separator();
        if ui.button(tr("Help")).clicked() {
            ui.ctx().open_url(egui::OpenUrl::new_tab(crate::help::SCHEDULER));
        }
        if ui.button(tr("Close")).clicked() {
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
/// `file` names the download that asked for it, when not a queue.
pub fn countdown(
    ctx: &egui::Context,
    action: WhenDone,
    deadline: Instant,
    queue: &str,
    file: Option<&str>,
    connected: bool,
    actions: &mut Vec<Action>,
) {
    let left = deadline.saturating_duration_since(Instant::now()).as_secs();
    let message = match action {
        WhenDone::Sleep => trf!("This computer will go to sleep in {seconds} seconds.", seconds = left),
        WhenDone::Hibernate => trf!("This computer will hibernate in {seconds} seconds.", seconds = left),
        WhenDone::ShutDown => trf!("This computer will shut down in {seconds} seconds.", seconds = left),
        _ => trf!("This computer will continue in {seconds} seconds.", seconds = left),
    };
    let title = if file.is_some() { tr("Download finished") } else { tr("Queue finished") };
    crate::dialog::Window::new(title)
        .id(egui::Id::new("countdown"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(match file {
                Some(name) => trf!("{name} has finished downloading.", name = name),
                None => trf!("The queue \"{queue}\" is done.", queue = queue),
            });
            ui.heading(message);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let now = match action {
                    WhenDone::Sleep => tr("Sleep now"),
                    WhenDone::Hibernate => tr("Hibernate now"),
                    _ => tr("Shut down now"),
                };
                if ui.add_enabled(connected, egui::Button::new(now)).clicked() {
                    actions.push(Action::CountdownNow);
                }
                if ui.add_enabled(connected, egui::Button::new(tr("Cancel"))).clicked() {
                    actions.push(Action::CancelCountdown);
                }
            });
        });
    ctx.request_repaint_after(std::time::Duration::from_millis(250));
}

#[cfg(test)]
mod tests {
    use super::*;
    use uidm_domain::queue::{parse_date, parse_time};

    #[test]
    fn new_named_checks_the_name() {
        let queues = [Queue::main(), Queue::new(2, "Night")];
        let q = new_named("  Weekend ", &queues).unwrap();
        assert_eq!((q.id, q.name.as_str()), (0, "Weekend"));
        assert!(q.validate().is_ok());
        assert!(new_named("   ", &queues).is_err());
        assert!(new_named("night", &queues).is_err(), "names differ by more than case");
    }

    #[test]
    fn picker_folder_starts_next_to_the_named_file() {
        let (file, folder) =
            if cfg!(windows) { (r"C:\Tools\after.bat", r"C:\Tools") } else { ("/opt/tools/after.sh", "/opt/tools") };
        assert_eq!(picker_folder(&format!("  {file} ")), Some(PathBuf::from(folder)));
        assert_eq!(picker_folder(""), None);
        assert_eq!(picker_folder("after.bat"), None, "not a full path");
    }

    #[test]
    fn named_finds_a_queue_whatever_the_case() {
        let queues = [Queue::main(), Queue::new(2, "Night")];
        assert_eq!(named(&queues, "NIGHT "), Some(2));
        assert_eq!(named(&queues, "Day"), None);
    }

    #[test]
    fn draft_round_trips_a_queue() {
        let mut q = Queue::new(3, "Night");
        q.max_concurrent = 2;
        q.retries = 1;
        q.when_done = WhenDone::ShutDown;
        let after = if cfg!(windows) { r"C:\Tools\after.bat" } else { "/opt/tools/after.sh" };
        q.open_when_done = Some(after.into());
        q.start_on_launch = true;
        q.kind = QueueKind::Synchronize;
        q.schedule = Some(Schedule {
            start: "23:00".into(),
            stop: Some("07:00".into()),
            days: vec![0, 4],
            date: None,
            repeat_minutes: Some(150),
            wake: true,
        });
        let back = QueueDraft::from(&q).to_queue().unwrap();
        assert_eq!(back.schedule, q.schedule);
        assert_eq!((back.max_concurrent, back.retries, back.when_done), (2, 1, WhenDone::ShutDown));
        assert_eq!(back.open_when_done.as_deref(), Some(after));
        assert!(back.start_on_launch);
        assert_eq!(back.kind, QueueKind::Synchronize);
        let mut relative = QueueDraft::from(&q);
        relative.open_when_done = "after.bat".into();
        assert!(relative.to_queue().is_err(), "only full paths");

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
