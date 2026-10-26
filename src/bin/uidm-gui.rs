//! UIDM desktop window: lists downloads and controls them.
//!
//! It is one more daemon client. A background thread connects (starting the
//! daemon if needed, with the same mutual handshake as the host), asks for the
//! full list, then applies every event to a `DownloadTable`. The window draws
//! from a copy of that table each frame and sends `add`, `cancel`, `retry` and
//! `remove` requests over the same connection.

#![windows_subsystem = "windows"]

use std::collections::HashMap;
use std::net::TcpStream;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use uidm::app::MAX_MESSAGE_BYTES;
use uidm::ipc;
use uidm::protocol::{read_message, write_frame, Download, DownloadEntry, Incoming, Outgoing, Status};
use uidm::state::DownloadTable;

/// Ignore speed samples closer together than this; they are too noisy.
const SPEED_MIN_INTERVAL: Duration = Duration::from_millis(400);

/// The window icon as raw 64x64 RGBA, made from extension/icons/128.png so no
/// image decoder is needed at run time.
const ICON_RGBA: &[u8] = include_bytes!("../../assets/icon-64.rgba");
const ICON_SIZE: u32 = 64;

/// Width of the file-name column; long names are truncated with a tooltip.
const NAME_WIDTH: f32 = 240.0;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("UIDM")
            .with_icon(egui::IconData {
                rgba: ICON_RGBA.to_vec(),
                width: ICON_SIZE,
                height: ICON_SIZE,
            })
            .with_inner_size([1000.0, 520.0])
            .with_min_inner_size([720.0, 300.0]),
        ..Default::default()
    };
    eframe::run_native("UIDM", options, Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx)))))
}

#[derive(Clone)]
enum Link {
    Connecting,
    Connected,
    Lost(String),
}

/// Bytes per second, smoothed across progress events.
struct Speed {
    at: Instant,
    done: u64,
    bytes_per_sec: f64,
}

/// State the connection thread writes and the window reads.
struct Shared {
    link: Link,
    table: DownloadTable,
    speeds: HashMap<u64, Speed>,
    /// The daemon's most recent `error` reply, shown in the status bar.
    last_error: Option<String>,
}

impl Shared {
    fn apply(&mut self, msg: &Outgoing) {
        self.table.apply(msg);
        match msg {
            Outgoing::Progress { id, done, .. } => self.sample_speed(*id, *done),
            Outgoing::Done { id, .. }
            | Outgoing::Failed { id, .. }
            | Outgoing::Canceled { id, .. }
            | Outgoing::Removed { id } => {
                self.speeds.remove(id);
            }
            Outgoing::Error { message } => self.last_error = Some(message.clone()),
            _ => {}
        }
    }

    fn sample_speed(&mut self, id: u64, done: u64) {
        let now = Instant::now();
        let s = self.speeds.entry(id).or_insert(Speed {
            at: now,
            done,
            bytes_per_sec: 0.0,
        });
        let dt = now.duration_since(s.at);
        if dt < SPEED_MIN_INTERVAL || done < s.done {
            return;
        }
        let instant = (done - s.done) as f64 / dt.as_secs_f64();
        s.bytes_per_sec = if s.bytes_per_sec == 0.0 {
            instant
        } else {
            0.7 * s.bytes_per_sec + 0.3 * instant
        };
        s.at = now;
        s.done = done;
    }
}

/// What a click asked for, applied after drawing so no lock is held then.
enum Action {
    Add(String),
    Cancel(u64),
    Retry(u64),
    Remove(u64),
    ClearFinished,
    Open(String),
    ShowInFolder(String),
    Reconnect,
}

struct App {
    shared: Arc<Mutex<Shared>>,
    /// The write half of the daemon connection, when connected.
    writer: Arc<Mutex<Option<TcpStream>>>,
    ctx: egui::Context,
    url_input: String,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        let app = App {
            shared: Arc::new(Mutex::new(Shared {
                link: Link::Connecting,
                table: DownloadTable::new(),
                speeds: HashMap::new(),
                last_error: None,
            })),
            writer: Arc::new(Mutex::new(None)),
            ctx: ctx.clone(),
            url_input: String::new(),
        };
        app.connect();
        app
    }

    /// Connect on a background thread, then read events until the daemon goes.
    fn connect(&self) {
        let shared = Arc::clone(&self.shared);
        let writer = Arc::clone(&self.writer);
        let ctx = self.ctx.clone();
        shared.lock().unwrap().link = Link::Connecting;

        thread::spawn(move || {
            let set_link = |link: Link| {
                shared.lock().unwrap().link = link;
                ctx.request_repaint();
            };
            let stream = match ipc::connect_or_spawn() {
                Ok(s) => s,
                Err(e) => return set_link(Link::Lost(e.to_string())),
            };
            let mut reader = match stream.try_clone() {
                Ok(r) => r,
                Err(e) => return set_link(Link::Lost(e.to_string())),
            };
            *writer.lock().unwrap() = Some(stream);
            set_link(Link::Connected);
            send(&writer, &Incoming::List);

            loop {
                match read_message(&mut reader, MAX_MESSAGE_BYTES) {
                    Ok(Some(raw)) => {
                        if let Ok(msg) = serde_json::from_slice::<Outgoing>(&raw) {
                            shared.lock().unwrap().apply(&msg);
                            ctx.request_repaint();
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            *writer.lock().unwrap() = None;
            set_link(Link::Lost("the daemon closed the connection".into()));
        });
    }

    fn perform(&mut self, action: Action) {
        match action {
            Action::Add(url) => {
                send(
                    &self.writer,
                    &Incoming::Add(Download {
                        url,
                        ..Download::default()
                    }),
                );
            }
            Action::Cancel(id) => {
                send(&self.writer, &Incoming::Cancel { id });
            }
            Action::Retry(id) => {
                send(&self.writer, &Incoming::Retry { id });
            }
            Action::Remove(id) => {
                send(&self.writer, &Incoming::Remove { id });
            }
            Action::ClearFinished => {
                let done: Vec<u64> = {
                    let s = self.shared.lock().unwrap();
                    s.table
                        .entries()
                        .filter(|e| e.status == Status::Done)
                        .map(|e| e.id)
                        .collect()
                };
                for id in done {
                    send(&self.writer, &Incoming::Remove { id });
                }
            }
            Action::Open(path) => {
                // Explorer opens a file with its associated program.
                let _ = Command::new("explorer").arg(&path).spawn();
            }
            Action::ShowInFolder(path) => {
                // Explorer parses /select itself, so pass the argument raw with
                // the path quoted. Windows paths cannot contain a double quote.
                let _ = Command::new("explorer")
                    .raw_arg(format!("/select,\"{path}\""))
                    .spawn();
            }
            Action::Reconnect => self.connect(),
        }
    }
}

/// Send one request. Returns false when not connected or the write failed.
fn send(writer: &Arc<Mutex<Option<TcpStream>>>, msg: &Incoming) -> bool {
    let mut guard = writer.lock().unwrap();
    let Some(stream) = guard.as_mut() else {
        return false;
    };
    match serde_json::to_vec(msg) {
        Ok(frame) => write_frame(stream, &frame).is_ok(),
        Err(_) => false,
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Copy what this frame needs, then draw without holding the lock.
        let (link, rows, speeds, last_error) = {
            let s = self.shared.lock().unwrap();
            let speeds: HashMap<u64, f64> =
                s.speeds.iter().map(|(id, sp)| (*id, sp.bytes_per_sec)).collect();
            (
                s.link.clone(),
                s.table.entries().cloned().collect::<Vec<DownloadEntry>>(),
                speeds,
                s.last_error.clone(),
            )
        };
        let connected = matches!(link, Link::Connected);
        let mut actions = Vec::new();

        egui::TopBottomPanel::top("add").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading("UIDM");
                ui.add_space(8.0);
                let input = ui.add(
                    egui::TextEdit::singleline(&mut self.url_input)
                        .hint_text("Paste a download URL")
                        .desired_width((ui.available_width() - 100.0).max(120.0)),
                );
                let entered = input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let url = self.url_input.trim().to_string();
                let can_add = connected && (url.starts_with("http://") || url.starts_with("https://"));
                let clicked = ui.add_enabled(can_add, egui::Button::new("Download")).clicked();
                if can_add && (clicked || entered) {
                    actions.push(Action::Add(url));
                    self.url_input.clear();
                }
            });
            ui.add_space(6.0);
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                match &link {
                    Link::Connecting => {
                        ui.spinner();
                        ui.label("Connecting to the UIDM daemon...");
                    }
                    Link::Connected => {
                        ui.label("Connected");
                    }
                    Link::Lost(why) => {
                        ui.colored_label(ui.visuals().error_fg_color, format!("Disconnected: {why}"));
                        if ui.button("Reconnect").clicked() {
                            actions.push(Action::Reconnect);
                        }
                    }
                }
                let active = rows.iter().filter(|e| !e.status.is_terminal()).count();
                ui.separator();
                ui.label(format!("{active} active, {} total", rows.len()));
                if let Some(err) = &last_error {
                    ui.separator();
                    ui.colored_label(ui.visuals().warn_fg_color, err);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let any_done = rows.iter().any(|e| e.status == Status::Done);
                    if ui
                        .add_enabled(connected && any_done, egui::Button::new("Clear finished"))
                        .clicked()
                    {
                        actions.push(Action::ClearFinished);
                    }
                });
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if rows.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.weak("No downloads yet. Paste a URL above, or download something in your browser.");
                });
                return;
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                egui::Grid::new("downloads")
                    .striped(true)
                    .num_columns(6)
                    .spacing([14.0, 8.0])
                    .show(ui, |ui| {
                        for title in ["File", "Size", "Progress", "Speed", "Status", ""] {
                            ui.strong(title);
                        }
                        ui.end_row();
                        for entry in &rows {
                            draw_row(ui, entry, speeds.get(&entry.id).copied(), connected, &mut actions);
                            ui.end_row();
                        }
                    });
            });
        });

        for action in actions {
            self.perform(action);
        }
    }
}

fn draw_row(
    ui: &mut egui::Ui,
    e: &DownloadEntry,
    speed: Option<f64>,
    connected: bool,
    actions: &mut Vec<Action>,
) {
    // A fixed-width, left-aligned cell so long names truncate instead of
    // pushing the action buttons off the window.
    ui.allocate_ui_with_layout(
        egui::vec2(NAME_WIDTH, 18.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_width(NAME_WIDTH);
            ui.add(egui::Label::new(display_name(e)).truncate())
                .on_hover_text(e.path.as_deref().unwrap_or(&e.url));
        },
    );

    ui.label(match e.total {
        Some(t) => format!("{} / {}", human_bytes(e.done), human_bytes(t)),
        None => human_bytes(e.done),
    });

    let fraction = match e.total {
        Some(t) if t > 0 => (e.done as f32 / t as f32).clamp(0.0, 1.0),
        _ if e.status == Status::Done => 1.0,
        _ => 0.0,
    };
    let bar = egui::ProgressBar::new(fraction).desired_width(160.0);
    ui.add(if e.total.is_some() || e.status == Status::Done {
        bar.show_percentage()
    } else {
        bar.text(human_bytes(e.done))
    });

    match (e.status, speed) {
        (Status::Downloading, Some(bps)) if bps > 0.0 => {
            let eta = e
                .total
                .filter(|t| *t > e.done)
                .map(|t| format!(", {} left", human_duration((t - e.done) as f64 / bps)))
                .unwrap_or_default();
            ui.label(format!("{}/s{eta}", human_bytes(bps as u64)));
        }
        _ => {
            ui.label("");
        }
    }

    let visuals = ui.visuals().clone();
    let (text, color) = match e.status {
        Status::Queued => ("Queued", visuals.weak_text_color()),
        Status::Downloading => ("Downloading", visuals.text_color()),
        Status::Done => ("Done", visuals.text_color()),
        Status::Failed => ("Failed", visuals.error_fg_color),
        Status::Canceled => ("Canceled", visuals.warn_fg_color),
    };
    let status = ui.colored_label(color, text);
    if let Some(err) = &e.error {
        status.on_hover_text(err);
    }

    ui.horizontal(|ui| {
        ui.add_enabled_ui(connected, |ui| match e.status {
            Status::Queued | Status::Downloading => {
                if ui.small_button("Cancel").clicked() {
                    actions.push(Action::Cancel(e.id));
                }
            }
            Status::Failed | Status::Canceled => {
                if ui.small_button("Retry").clicked() {
                    actions.push(Action::Retry(e.id));
                }
                if ui.small_button("Remove").clicked() {
                    actions.push(Action::Remove(e.id));
                }
            }
            Status::Done => {
                if let Some(path) = &e.path {
                    if ui.small_button("Open").clicked() {
                        actions.push(Action::Open(path.clone()));
                    }
                    if ui.small_button("Show in folder").clicked() {
                        actions.push(Action::ShowInFolder(path.clone()));
                    }
                }
                if ui.small_button("Remove").clicked() {
                    actions.push(Action::Remove(e.id));
                }
            }
        });
    });
}

/// The saved file's name, or the last part of the URL before it is known.
fn display_name(e: &DownloadEntry) -> String {
    if let Some(name) = e.path.as_deref().and_then(|p| Path::new(p).file_name()) {
        return name.to_string_lossy().into_owned();
    }
    let no_query = e.url.split(['?', '#']).next().unwrap_or(&e.url);
    match no_query.rsplit('/').next() {
        Some(tail) if !tail.is_empty() => tail.to_string(),
        _ => e.url.clone(),
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn human_duration(secs: f64) -> String {
    let s = secs.round().max(0.0) as u64;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {}m", s / 3600, (s % 3600) / 60),
    }
}
