//! UIDM desktop window: lists downloads and controls them.
//!
//! It is one more daemon client. A background thread connects (starting the
//! daemon if needed, with the same mutual handshake as the host), asks for the
//! full list and the settings, then applies every event to a `DownloadTable`.
//! The window draws from a copy of that table each frame and sends `add`,
//! `cancel`, `retry`, `remove` and `set_settings` requests over the same
//! connection.

#![windows_subsystem = "windows"]

use std::collections::HashMap;
use std::net::TcpStream;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use uidm::app::MAX_MESSAGE_BYTES;
use uidm::ipc;
use uidm::protocol::{read_message, write_frame, Download, DownloadEntry, Incoming, Outgoing, Status};
use uidm::settings::{
    parse_extensions, Category, FileExists, Settings, CAPTURE_MIN_SIZE_MAX_KB, CONNECTIONS_RANGE,
    GENERAL, MAX_CONCURRENT_RANGE, SPEED_LIMIT_RANGE_KBPS,
};
use uidm::state::DownloadTable;

/// Ignore speed samples closer together than this; they are too noisy.
const SPEED_MIN_INTERVAL: Duration = Duration::from_millis(400);

/// The window icon as raw 64x64 RGBA, made from extension/icons/128.png so no
/// image decoder is needed at run time.
const ICON_RGBA: &[u8] = include_bytes!("../../assets/icon-64.rgba");
const ICON_SIZE: u32 = 64;

/// Width of the file-name column; long names are truncated with a tooltip.
const NAME_WIDTH: f32 = 200.0;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("UIDM")
            .with_icon(egui::IconData {
                rgba: ICON_RGBA.to_vec(),
                width: ICON_SIZE,
                height: ICON_SIZE,
            })
            .with_inner_size([1120.0, 560.0])
            .with_min_inner_size([860.0, 320.0]),
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
    /// The daemon's settings, once it has sent them.
    settings: Option<Settings>,
    /// True from sending `set_settings` until the daemon answers with
    /// `settings` or `error`.
    saving: bool,
    /// That answer, for the Options dialog to pick up.
    save_result: Option<Result<(), String>>,
}

impl Shared {
    fn apply(&mut self, msg: &Outgoing) {
        self.table.apply(msg);
        match msg {
            Outgoing::Settings { settings } => {
                self.settings = Some(settings.clone());
                if self.saving {
                    self.saving = false;
                    self.save_result = Some(Ok(()));
                }
            }
            Outgoing::Error { message } if self.saving => {
                self.saving = false;
                self.save_result = Some(Err(message.clone()));
            }
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
    OpenOptions,
    SaveSettings(Settings),
    /// Send new settings without the Options dialog (the speed limit toggle).
    QuickSettings(Settings),
    /// Answer a save-as dialog, or start a waiting download.
    Confirm {
        id: u64,
        filename: Option<String>,
        folder: Option<String>,
        start: bool,
    },
}

struct App {
    shared: Arc<Mutex<Shared>>,
    /// The write half of the daemon connection, when connected.
    writer: Arc<Mutex<Option<TcpStream>>>,
    ctx: egui::Context,
    url_input: String,
    /// The Options dialog, while it is open.
    options: Option<OptionsDraft>,
    /// The save-as dialog for the oldest `confirming` download.
    confirm: Option<ConfirmDraft>,
    /// The last download whose dialog brought the window forward.
    raised_for: Option<u64>,
    /// Show only this category; `None` shows every download.
    category_filter: Option<String>,
}

/// A folder chosen in the picker, delivered by its thread.
type Picked = Arc<Mutex<Option<PathBuf>>>;

/// A "Browse..." button that opens the folder picker on its own thread (it
/// blocks) and puts the choice in `slot`.
fn browse_button(ui: &mut egui::Ui, title: &'static str, start: &str, slot: &Picked) {
    if ui.button("Browse...").clicked() {
        let slot = Arc::clone(slot);
        let ctx = ui.ctx().clone();
        let start = start.trim().to_string();
        thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title(title);
            if !start.is_empty() {
                dialog = dialog.set_directory(&start);
            }
            if let Some(folder) = dialog.pick_folder() {
                *slot.lock().unwrap() = Some(folder);
                ctx.request_repaint();
            }
        });
    }
}

/// Copy a picked folder into `field`, if one arrived.
fn take_picked(slot: &Picked, field: &mut String) {
    if let Some(folder) = slot.lock().unwrap().take() {
        *field = folder.display().to_string();
    }
}

/// The save-as dialog's fields.
struct ConfirmDraft {
    id: u64,
    filename: String,
    folder: String,
    picked: Picked,
}

/// Units the speed limit can be entered in.
#[derive(Clone, Copy, PartialEq)]
enum SpeedUnit {
    Kb,
    Mb,
}

impl SpeedUnit {
    fn kb(self) -> f64 {
        match self {
            SpeedUnit::Kb => 1.0,
            SpeedUnit::Mb => 1024.0,
        }
    }

    fn label(self) -> &'static str {
        match self {
            SpeedUnit::Kb => "KB/s",
            SpeedUnit::Mb => "MB/s",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum OptionsTab {
    General,
    Categories,
    Capture,
}

/// One category being edited.
struct CategoryDraft {
    name: String,
    extensions: String,
    /// Empty means a folder named after the category.
    folder: String,
    picked: Picked,
}

/// The Options dialog's fields, edited freely and checked on Save.
struct OptionsDraft {
    tab: OptionsTab,
    /// Empty means the Downloads folder.
    folder: String,
    connections: usize,
    max_concurrent: usize,
    limit_enabled: bool,
    limit_value: f64,
    limit_unit: SpeedUnit,
    resume_at_startup: bool,
    file_exists: FileExists,
    ask_where_to_save: bool,
    categories: Vec<CategoryDraft>,
    capture_types: String,
    capture_min_size_kb: u64,
    /// One site per line.
    capture_excluded_sites: String,
    error: Option<String>,
    picked: Picked,
}

impl OptionsDraft {
    fn new(s: &Settings) -> Self {
        let kbps = s.speed_limit_kbps;
        let limit_unit = if kbps >= 1024 && kbps.is_multiple_of(1024) {
            SpeedUnit::Mb
        } else {
            SpeedUnit::Kb
        };
        OptionsDraft {
            tab: OptionsTab::General,
            folder: s.download_dir.clone().unwrap_or_default(),
            connections: s.connections,
            max_concurrent: s.max_concurrent,
            limit_enabled: s.speed_limit_enabled,
            limit_value: kbps as f64 / limit_unit.kb(),
            limit_unit,
            resume_at_startup: s.resume_at_startup,
            file_exists: s.file_exists,
            ask_where_to_save: s.ask_where_to_save,
            categories: s
                .categories
                .iter()
                .map(|c| CategoryDraft {
                    name: c.name.clone(),
                    extensions: c.extensions.join(", "),
                    folder: c.folder.clone().unwrap_or_default(),
                    picked: Picked::default(),
                })
                .collect(),
            capture_types: s.capture_types.join(", "),
            capture_min_size_kb: s.capture_min_size_kb,
            capture_excluded_sites: s.capture_excluded_sites.join("\n"),
            error: None,
            picked: Picked::default(),
        }
    }

    /// The settings these fields describe, checked the same way the daemon
    /// checks them.
    fn to_settings(&self) -> Result<Settings, String> {
        let optional = |text: &str| {
            let t = text.trim();
            (!t.is_empty()).then(|| t.to_string())
        };
        let settings = Settings {
            download_dir: optional(&self.folder),
            connections: self.connections,
            max_concurrent: self.max_concurrent,
            speed_limit_enabled: self.limit_enabled,
            speed_limit_kbps: (self.limit_value * self.limit_unit.kb()).round() as u64,
            resume_at_startup: self.resume_at_startup,
            file_exists: self.file_exists,
            ask_where_to_save: self.ask_where_to_save,
            categories: self
                .categories
                .iter()
                .map(|c| Category {
                    name: c.name.trim().to_string(),
                    extensions: parse_extensions(&c.extensions),
                    folder: optional(&c.folder),
                })
                .collect(),
            capture_types: parse_extensions(&self.capture_types),
            capture_min_size_kb: self.capture_min_size_kb,
            capture_excluded_sites: self
                .capture_excluded_sites
                .lines()
                .map(|l| l.trim().to_ascii_lowercase())
                .filter(|l| !l.is_empty())
                .collect(),
        };
        settings.validate()?;
        Ok(settings)
    }
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        let app = App {
            shared: Arc::new(Mutex::new(Shared {
                link: Link::Connecting,
                table: DownloadTable::new(),
                speeds: HashMap::new(),
                last_error: None,
                settings: None,
                saving: false,
                save_result: None,
            })),
            writer: Arc::new(Mutex::new(None)),
            ctx: ctx.clone(),
            url_input: String::new(),
            options: None,
            confirm: None,
            raised_for: None,
            category_filter: None,
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
            send(
                &writer,
                &Incoming::Hello {
                    version: Some(env!("CARGO_PKG_VERSION").to_string()),
                    client: Some("window".to_string()),
                },
            );
            send(&writer, &Incoming::List);
            send(&writer, &Incoming::GetSettings);

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
            Action::OpenOptions => {
                if let Some(settings) = &self.shared.lock().unwrap().settings {
                    self.options = Some(OptionsDraft::new(settings));
                }
            }
            Action::SaveSettings(settings) => {
                {
                    let mut s = self.shared.lock().unwrap();
                    s.saving = true;
                    s.save_result = None;
                }
                if !send(&self.writer, &Incoming::SetSettings { settings }) {
                    let mut s = self.shared.lock().unwrap();
                    s.saving = false;
                    s.save_result = Some(Err("not connected to the UIDM daemon".into()));
                }
            }
            Action::QuickSettings(settings) => {
                send(&self.writer, &Incoming::SetSettings { settings });
            }
            Action::Confirm {
                id,
                filename,
                folder,
                start,
            } => {
                send(
                    &self.writer,
                    &Incoming::Confirm {
                        id,
                        filename,
                        folder,
                        start,
                    },
                );
            }
        }
    }

    /// Draw the save-as dialog for the oldest download waiting for one, and
    /// bring the window forward when a new one arrives.
    fn confirm_window(
        &mut self,
        ctx: &egui::Context,
        rows: &[DownloadEntry],
        connected: bool,
        actions: &mut Vec<Action>,
    ) {
        let Some(entry) = rows.iter().find(|e| e.status == Status::Confirming) else {
            self.confirm = None;
            return;
        };
        if self.confirm.as_ref().map(|c| c.id) != Some(entry.id) {
            let path = entry.path.as_deref().map(Path::new);
            self.confirm = Some(ConfirmDraft {
                id: entry.id,
                filename: path
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| display_name(entry)),
                folder: path
                    .and_then(|p| p.parent())
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                picked: Picked::default(),
            });
        }
        if self.raised_for != Some(entry.id) {
            self.raised_for = Some(entry.id);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        let Some(draft) = self.confirm.as_mut() else {
            return;
        };
        take_picked(&draft.picked, &mut draft.folder);
        let waiting = rows.iter().filter(|e| e.status == Status::Confirming).count();

        egui::Window::new("Save download")
            .id(egui::Id::new("confirm"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::Grid::new("confirm_grid").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                    ui.label("Address");
                    ui.add(egui::Label::new(&entry.url).truncate())
                        .on_hover_text(&entry.url);
                    ui.end_row();

                    ui.label("File name");
                    ui.add(egui::TextEdit::singleline(&mut draft.filename).desired_width(360.0));
                    ui.end_row();

                    ui.label("Size");
                    ui.label(match entry.total {
                        Some(t) => human_bytes(t),
                        None => "Unknown".to_string(),
                    });
                    ui.end_row();

                    ui.label("Save to");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut draft.folder).desired_width(280.0));
                        browse_button(ui, "Save to", &draft.folder, &draft.picked);
                    });
                    ui.end_row();
                });
                if waiting > 1 {
                    ui.weak(format!("{} more downloads are waiting after this one.", waiting - 1));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(connected, |ui| {
                        let answer = |start: bool| Action::Confirm {
                            id: draft.id,
                            filename: Some(draft.filename.trim().to_string()),
                            folder: Some(draft.folder.trim().to_string()),
                            start,
                        };
                        if ui.button("Start download").clicked() {
                            actions.push(answer(true));
                        }
                        if ui.button("Download later").clicked() {
                            actions.push(answer(false));
                        }
                        if ui.button("Cancel").clicked() {
                            actions.push(Action::Cancel(draft.id));
                        }
                    });
                });
            });
    }

    /// Draw the Options dialog while it is open.
    fn options_window(&mut self, ctx: &egui::Context, connected: bool, actions: &mut Vec<Action>) {
        // Pick up the daemon's answer to a save.
        let result = self.shared.lock().unwrap().save_result.take();
        let saving = self.shared.lock().unwrap().saving;
        let Some(draft) = self.options.as_mut() else {
            return;
        };
        match result {
            Some(Ok(())) => {
                self.options = None;
                return;
            }
            Some(Err(e)) => draft.error = Some(e),
            None => {}
        }
        take_picked(&draft.picked, &mut draft.folder);
        for c in &mut draft.categories {
            take_picked(&c.picked, &mut c.folder);
        }

        let mut open = true;
        let mut close = false;
        egui::Window::new("Options")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .min_width(720.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut draft.tab, OptionsTab::General, "General");
                    ui.selectable_value(&mut draft.tab, OptionsTab::Categories, "Categories");
                    ui.selectable_value(&mut draft.tab, OptionsTab::Capture, "Browser capture");
                });
                ui.separator();
                match draft.tab {
                    OptionsTab::General => general_tab(ui, draft),
                    OptionsTab::Categories => categories_tab(ui, draft),
                    OptionsTab::Capture => capture_tab(ui, draft),
                }

                if let Some(err) = &draft.error {
                    ui.add_space(4.0);
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let save = ui.add_enabled(connected && !saving, egui::Button::new("Save"));
                    if save.clicked() {
                        match draft.to_settings() {
                            Ok(settings) => {
                                draft.error = None;
                                actions.push(Action::SaveSettings(settings));
                            }
                            Err(e) => draft.error = Some(e),
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if saving {
                        ui.spinner();
                    }
                });
            });
        if !open || close {
            self.options = None;
        }
    }
}

fn general_tab(ui: &mut egui::Ui, draft: &mut OptionsDraft) {
    egui::Grid::new("options").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
        ui.label("Download folder");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut draft.folder)
                    .hint_text("Your Downloads folder")
                    .desired_width(280.0),
            );
            browse_button(ui, "Download folder", &draft.folder, &draft.picked);
            if ui.button("Default").on_hover_text("Use your Downloads folder").clicked() {
                draft.folder.clear();
            }
        });
        ui.end_row();

        ui.label("Connections per file");
        let (lo, hi) = CONNECTIONS_RANGE;
        ui.add(egui::DragValue::new(&mut draft.connections).range(lo..=hi));
        ui.end_row();

        ui.label("Downloads at once");
        let (lo, hi) = MAX_CONCURRENT_RANGE;
        ui.add(egui::DragValue::new(&mut draft.max_concurrent).range(lo..=hi));
        ui.end_row();

        ui.label("Speed limit");
        ui.horizontal(|ui| {
            ui.checkbox(&mut draft.limit_enabled, "Limit to");
            ui.add_enabled_ui(draft.limit_enabled, |ui| {
                let (lo, hi) = SPEED_LIMIT_RANGE_KBPS;
                let unit = draft.limit_unit.kb();
                ui.add(
                    egui::DragValue::new(&mut draft.limit_value)
                        .range(lo as f64 / unit..=hi as f64 / unit)
                        .max_decimals(1),
                );
                let before = draft.limit_unit;
                egui::ComboBox::from_id_salt("unit")
                    .selected_text(draft.limit_unit.label())
                    .width(70.0)
                    .show_ui(ui, |ui| {
                        for u in [SpeedUnit::Kb, SpeedUnit::Mb] {
                            ui.selectable_value(&mut draft.limit_unit, u, u.label());
                        }
                    });
                // Keep the same speed when only the unit changes.
                if before != draft.limit_unit {
                    draft.limit_value = draft.limit_value * before.kb() / draft.limit_unit.kb();
                }
            });
        });
        ui.end_row();

        ui.label("If the file exists");
        egui::ComboBox::from_id_salt("file_exists")
            .selected_text(file_exists_label(draft.file_exists))
            .show_ui(ui, |ui| {
                for f in [FileExists::Rename, FileExists::Overwrite] {
                    ui.selectable_value(&mut draft.file_exists, f, file_exists_label(f));
                }
            });
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut draft.ask_where_to_save, "Ask where to save each new download");
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut draft.resume_at_startup, "Resume unfinished downloads at startup");
        ui.end_row();
    });
}

fn categories_tab(ui: &mut egui::Ui, draft: &mut OptionsDraft) {
    ui.weak(format!(
        "Each download goes to its category's folder, chosen by its file type. \
         Other files are \"{GENERAL}\" and go to the download folder."
    ));
    ui.add_space(6.0);
    let mut remove = None;
    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
        egui::Grid::new("categories").num_columns(4).spacing([8.0, 8.0]).show(ui, |ui| {
            ui.strong("Name");
            ui.strong("File types");
            ui.strong("Folder");
            ui.label("");
            ui.end_row();
            for (i, c) in draft.categories.iter_mut().enumerate() {
                ui.add_sized([110.0, 20.0], egui::TextEdit::singleline(&mut c.name));
                ui.add_sized(
                    [210.0, 20.0],
                    egui::TextEdit::singleline(&mut c.extensions).hint_text("zip, rar"),
                );
                ui.horizontal(|ui| {
                    let hint = format!("Download folder\\{}", c.name.trim());
                    ui.add_sized(
                        [210.0, 20.0],
                        egui::TextEdit::singleline(&mut c.folder).hint_text(hint),
                    );
                    browse_button(ui, "Category folder", &c.folder, &c.picked);
                });
                if ui.small_button("Remove").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
    });
    if let Some(i) = remove {
        draft.categories.remove(i);
    }
    ui.add_space(4.0);
    if ui.button("Add category").clicked() {
        draft.categories.push(CategoryDraft {
            name: String::new(),
            extensions: String::new(),
            folder: String::new(),
            picked: Picked::default(),
        });
    }
}

fn capture_tab(ui: &mut egui::Ui, draft: &mut OptionsDraft) {
    ui.weak("These rules decide which browser downloads UIDM takes. The rest stay in the browser.");
    ui.add_space(6.0);
    egui::Grid::new("capture").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
        ui.label("File types");
        ui.add(
            egui::TextEdit::multiline(&mut draft.capture_types)
                .hint_text("Empty takes every type")
                .desired_rows(3)
                .desired_width(360.0),
        );
        ui.end_row();

        ui.label("Minimum size");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut draft.capture_min_size_kb).range(0..=CAPTURE_MIN_SIZE_MAX_KB));
            ui.label("KB (0 takes every size)");
        });
        ui.end_row();

        ui.label("Never from");
        ui.add(
            egui::TextEdit::multiline(&mut draft.capture_excluded_sites)
                .hint_text("One site per line, such as *.example.com")
                .desired_rows(4)
                .desired_width(360.0),
        );
        ui.end_row();
    });
}

fn file_exists_label(f: FileExists) -> &'static str {
    match f {
        FileExists::Rename => "Save with a new name",
        FileExists::Overwrite => "Overwrite it",
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
        let (link, rows, speeds, last_error, settings) = {
            let s = self.shared.lock().unwrap();
            let speeds: HashMap<u64, f64> =
                s.speeds.iter().map(|(id, sp)| (*id, sp.bytes_per_sec)).collect();
            (
                s.link.clone(),
                s.table.entries().cloned().collect::<Vec<DownloadEntry>>(),
                speeds,
                s.last_error.clone(),
                s.settings.clone(),
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
                        .desired_width((ui.available_width() - 180.0).max(120.0)),
                );
                let entered = input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let url = self.url_input.trim().to_string();
                let can_add = connected && (url.starts_with("http://") || url.starts_with("https://"));
                let clicked = ui.add_enabled(can_add, egui::Button::new("Download")).clicked();
                if can_add && (clicked || entered) {
                    actions.push(Action::Add(url));
                    self.url_input.clear();
                }
                let can_open = connected && settings.is_some();
                if ui.add_enabled(can_open, egui::Button::new("Options")).clicked() {
                    actions.push(Action::OpenOptions);
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
                    if let Some(s) = &settings {
                        ui.separator();
                        let toggle = if s.speed_limit_enabled { "Turn off" } else { "Limit speed" };
                        let hover = format!("Limit: {}/s", human_bytes(s.speed_limit_kbps * 1024));
                        if ui
                            .add_enabled(connected, egui::Button::new(toggle).small())
                            .on_hover_text(hover)
                            .clicked()
                        {
                            actions.push(Action::QuickSettings(Settings {
                                speed_limit_enabled: !s.speed_limit_enabled,
                                ..s.clone()
                            }));
                        }
                        if s.speed_limit_enabled {
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                format!("Speed limit {}/s", human_bytes(s.speed_limit_kbps * 1024)),
                            );
                        } else {
                            ui.weak("No speed limit");
                        }
                    }
                });
            });
            ui.add_space(4.0);
        });

        // Category of each row, from its file name, and the sidebar to filter by
        // it.
        let category = |e: &DownloadEntry| -> String {
            settings
                .as_ref()
                .map_or(GENERAL, |s| s.category_name(&display_name(e)))
                .to_string()
        };
        if let Some(s) = &settings {
            let mut names: Vec<String> = s.categories.iter().map(|c| c.name.clone()).collect();
            names.push(GENERAL.to_string());
            if self.category_filter.as_ref().is_some_and(|f| !names.contains(f)) {
                self.category_filter = None;
            }
            egui::SidePanel::left("categories")
                .resizable(false)
                .exact_width(150.0)
                .show(ctx, |ui| {
                    ui.add_space(6.0);
                    let all = format!("All downloads ({})", rows.len());
                    ui.selectable_value(&mut self.category_filter, None, all);
                    ui.add_space(4.0);
                    for name in names {
                        let count = rows.iter().filter(|e| category(e) == name).count();
                        let label = format!("{name} ({count})");
                        ui.selectable_value(&mut self.category_filter, Some(name), label);
                    }
                });
        }
        let shown: Vec<&DownloadEntry> = rows
            .iter()
            .filter(|e| self.category_filter.as_ref().is_none_or(|f| *f == category(e)))
            .collect();

        egui::CentralPanel::default().show(ctx, |ui| {
            if shown.is_empty() && !rows.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.weak("No downloads in this category.");
                });
                return;
            }
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
                        for entry in shown.iter().copied() {
                            draw_row(ui, entry, speeds.get(&entry.id).copied(), connected, &mut actions);
                            ui.end_row();
                        }
                    });
            });
        });

        self.confirm_window(ctx, &rows, connected, &mut actions);
        self.options_window(ctx, connected, &mut actions);

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
    let bar = egui::ProgressBar::new(fraction).desired_width(130.0);
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
        Status::Interrupted => ("Interrupted", visuals.warn_fg_color),
        Status::Confirming => ("Confirm", visuals.weak_text_color()),
        Status::Waiting => ("Waiting", visuals.weak_text_color()),
    };
    let status = ui.colored_label(color, text);
    if let Some(err) = &e.error {
        status.on_hover_text(err);
    }

    ui.horizontal(|ui| {
        ui.add_enabled_ui(connected, |ui| match e.status {
            Status::Queued | Status::Downloading | Status::Confirming => {
                if ui.small_button("Cancel").clicked() {
                    actions.push(Action::Cancel(e.id));
                }
            }
            Status::Waiting => {
                if ui.small_button("Start").clicked() {
                    actions.push(Action::Confirm {
                        id: e.id,
                        filename: None,
                        folder: None,
                        start: true,
                    });
                }
                if ui.small_button("Remove").clicked() {
                    actions.push(Action::Remove(e.id));
                }
            }
            Status::Failed | Status::Canceled | Status::Interrupted => {
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
