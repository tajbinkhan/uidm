//! The window's tools for many downloads at once: the Batch dialog (a URL
//! pattern, a pasted list or text file, or an imported list), exporting the
//! list, the site grabber, and the clipboard offer.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::thread;

use eframe::egui;
use uidm::batch::{self, MAX_BATCH};
use uidm::grab::{self, GrabFile, GrabRequest};
use uidm::listfile;
use uidm::protocol::{Download, DownloadEntry};
use uidm::queue::{Queue, MAIN_QUEUE};

use super::{browse_button, human_bytes, take_picked, Action, Picked};
use crate::i18n::{tr, trf};

/// Text read from a file on the picker's thread, or why it could not be.
pub type Loaded = Arc<Mutex<Option<Result<(String, String), String>>>>;

/// Ask for a text file on another thread (the picker blocks) and put its
/// name and contents in `slot`.
pub fn pick_text_file(ctx: &egui::Context, title: &'static str, slot: &Loaded) {
    let (slot, ctx) = (Arc::clone(slot), ctx.clone());
    thread::spawn(move || {
        let Some(path) = rfd::FileDialog::new()
            .set_title(title)
            .add_filter(tr("Lists"), &["txt", "json"])
            .add_filter(tr("All files"), &["*"])
            .pick_file()
        else {
            return;
        };
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let read = std::fs::read(&path)
            .map(|b| (name, String::from_utf8_lossy(&b).into_owned()))
            .map_err(|e| trf!("Could not read {path}: {error}", path = path.display(), error = e));
        *slot.lock().unwrap() = Some(read);
        ctx.request_repaint();
    });
}

/// A queue picker.
pub fn queue_combo(ui: &mut egui::Ui, id: &str, queue: &mut u64, queues: &[Queue]) {
    let name = queues
        .iter()
        .find(|q| q.id == *queue)
        .map_or(tr("Main queue").to_string(), |q| q.name.clone());
    egui::ComboBox::from_id_salt(id).selected_text(name).show_ui(ui, |ui| {
        for q in queues {
            ui.selectable_value(queue, q.id, &q.name);
        }
    });
}

#[derive(Clone, Copy, PartialEq)]
enum Source {
    Pattern,
    List,
    /// A list imported from a file.
    Imported,
}

/// The Batch dialog.
pub struct BatchDialog {
    source: Source,
    pattern: String,
    /// The pattern last expanded, and what it gave.
    expanded: Option<(String, Result<Vec<String>, String>)>,
    list: String,
    /// An imported file's name, downloads and the entries left out.
    imported: Option<(String, Vec<Download>, usize)>,
    queue: u64,
    start: bool,
    /// Where to save; empty lets each file's category decide.
    folder: String,
    picked: Picked,
    loaded: Loaded,
    error: Option<String>,
}

impl BatchDialog {
    pub fn new() -> Self {
        BatchDialog {
            source: Source::Pattern,
            pattern: String::new(),
            expanded: None,
            list: String::new(),
            imported: None,
            queue: MAIN_QUEUE,
            start: false,
            folder: String::new(),
            picked: Picked::default(),
            loaded: Loaded::default(),
            error: None,
        }
    }

    /// The dialog with `urls` in its list, such as every link on the
    /// clipboard.
    pub fn with_list(urls: &[String]) -> Self {
        BatchDialog {
            source: Source::List,
            list: urls.join("\n"),
            ..BatchDialog::new()
        }
    }

    /// The dialog for downloads imported from `name`.
    pub fn imported(name: String, items: Vec<Download>, rejected: usize) -> Self {
        BatchDialog {
            source: Source::Imported,
            imported: Some((name, items, rejected)),
            ..BatchDialog::new()
        }
    }

    /// The URLs to add, or why there are none.
    fn urls(&mut self) -> Result<(Vec<String>, usize), String> {
        match self.source {
            Source::Pattern => {
                let fresh = self.expanded.as_ref().is_none_or(|(p, _)| *p != self.pattern);
                if fresh {
                    let result = if self.pattern.trim().is_empty() {
                        Ok(Vec::new())
                    } else {
                        batch::expand(&self.pattern)
                    };
                    self.expanded = Some((self.pattern.clone(), result));
                }
                let (_, result) = self.expanded.as_ref().unwrap();
                result.clone().map(|u| (u, 0))
            }
            Source::List => batch::parse_list(&self.list),
            Source::Imported => Ok((Vec::new(), 0)),
        }
    }
}

/// Draw the Batch dialog. Returns false once it is closed.
pub fn batch_dialog(
    ctx: &egui::Context,
    d: &mut BatchDialog,
    queues: &[Queue],
    connected: bool,
    actions: &mut Vec<Action>,
) -> bool {
    take_picked(&d.picked, &mut d.folder);
    if let Some(loaded) = d.loaded.lock().unwrap().take() {
        match loaded {
            Ok((_, text)) => {
                d.list = text;
                d.error = None;
            }
            Err(e) => d.error = Some(e),
        }
    }
    let mut open = true;
    let mut close = false;
    let title = if d.source == Source::Imported { "Import downloads" } else { "Add many downloads" };
    egui::Window::new(tr(title))
        .id(egui::Id::new(title))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .min_width(640.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            if d.source != Source::Imported {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut d.source, Source::Pattern, tr("From a pattern"));
                    ui.selectable_value(&mut d.source, Source::List, tr("From a list"));
                });
                ui.separator();
            }
            let urls = d.urls();
            match d.source {
                Source::Pattern => {
                    ui.label(tr("Address with ranges in brackets:"));
                    ui.add(
                        egui::TextEdit::singleline(&mut d.pattern)
                            .hint_text("https://example.com/photo[001-120].jpg")
                            .desired_width(600.0),
                    );
                    ui.weak(tr("[1-20] counts, [001-120] keeps the zeros, [0-100:5] counts in fives, [a-z] runs through letters."));
                }
                Source::List => {
                    ui.horizontal(|ui| {
                        ui.label(tr("One address per line:"));
                        if ui.button(tr("Load a text file...")).clicked() {
                            pick_text_file(ui.ctx(), tr("Load a list of addresses"), &d.loaded);
                        }
                    });
                    egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut d.list)
                                .hint_text("https://example.com/a.zip\nhttps://example.com/b.zip")
                                .desired_rows(8)
                                .desired_width(600.0),
                        );
                    });
                }
                Source::Imported => {
                    if let Some((name, items, rejected)) = &d.imported {
                        ui.label(match items.len() {
                            1 => trf!("1 download in {name}.", name = name),
                            n => trf!("{count} downloads in {name}.", count = n, name = name),
                        });
                        if *rejected > 0 {
                            ui.weak(trf!("{rejected} entries are not web addresses and are left out.", rejected = rejected));
                        }
                        ui.weak(tr("They are added waiting, with their file names and folders when the list has them."));
                    }
                }
            }
            ui.add_space(6.0);

            let count = match (&urls, &d.imported) {
                (_, Some((_, items, _))) => items.len(),
                (Ok((u, _)), None) => u.len(),
                (Err(_), None) => 0,
            };
            match &urls {
                Err(e) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                Ok((u, rejected)) if d.source != Source::Imported => {
                    let mut line = match u.len() {
                        1 => tr("1 address").to_string(),
                        n => trf!("{count} addresses", count = n),
                    };
                    if *rejected > 0 {
                        line.push_str(&trf!(", {rejected} lines skipped (not web addresses)", rejected = rejected));
                    }
                    if u.len() == MAX_BATCH {
                        line.push_str(&trf!(" (the most one batch can add is {max})", max = MAX_BATCH));
                    }
                    ui.label(line);
                    let preview: Vec<&String> = if u.len() <= 6 {
                        u.iter().collect()
                    } else {
                        u.iter().take(3).chain(u.iter().skip(u.len() - 2)).collect()
                    };
                    for (i, url) in preview.iter().enumerate() {
                        if u.len() > 6 && i == 3 {
                            ui.weak("...");
                        }
                        ui.add(egui::Label::new(egui::RichText::new(url.as_str()).monospace()).truncate());
                    }
                }
                Ok(_) => {}
            }

            ui.add_space(6.0);
            egui::Grid::new("batch_target").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label(tr("Queue"));
                ui.horizontal(|ui| {
                    queue_combo(ui, "batch_queue", &mut d.queue, queues);
                    ui.checkbox(&mut d.start, tr("Start the queue now"));
                });
                ui.end_row();
                if d.source != Source::Imported {
                    ui.label(tr("Save to"));
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut d.folder)
                                .hint_text(tr("The folder of each file's category"))
                                .desired_width(340.0),
                        );
                        browse_button(ui, tr("Save the files to"), &d.folder, &d.picked);
                    });
                    ui.end_row();
                }
            });
            if let Some(e) = &d.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let add = ui.add_enabled(connected && count > 0, egui::Button::new(trf!("Add {count}", count = count)));
                if add.clicked() {
                    let folder = d.folder.trim();
                    if !folder.is_empty() && !std::path::Path::new(folder).is_absolute() {
                        d.error = Some(trf!("The folder must be a full path: {folder}", folder = folder));
                        return;
                    }
                    let folder = (!folder.is_empty()).then(|| folder.to_string());
                    let items = match (&d.imported, urls) {
                        (Some((_, items, _)), _) => items.clone(),
                        (None, Ok((u, _))) => u
                            .into_iter()
                            .map(|url| Download {
                                url,
                                start: Some(false),
                                folder: folder.clone(),
                                ..Download::default()
                            })
                            .collect(),
                        (None, Err(_)) => return,
                    };
                    actions.push(Action::AddMany {
                        items,
                        queue: d.queue,
                        start_queue: d.start,
                    });
                    close = true;
                }
                if ui.button(tr("Cancel")).clicked() {
                    close = true;
                }
            });
        });
    open && !close
}

/// The Export dialog.
pub struct ExportDialog {
    selected_only: bool,
    json: bool,
    result: Arc<Mutex<Option<Result<String, String>>>>,
    message: Option<Result<String, String>>,
}

impl ExportDialog {
    pub fn new(any_selected: bool) -> Self {
        ExportDialog {
            selected_only: any_selected,
            json: true,
            result: Arc::default(),
            message: None,
        }
    }
}

/// Draw the Export dialog. Returns false once it is closed.
pub fn export_dialog(
    ctx: &egui::Context,
    d: &mut ExportDialog,
    rows: &[DownloadEntry],
    selected: &HashSet<u64>,
) -> bool {
    if let Some(r) = d.result.lock().unwrap().take() {
        d.message = Some(r);
    }
    let picked: Vec<&DownloadEntry> = rows
        .iter()
        .filter(|e| !d.selected_only || selected.contains(&e.id))
        .collect();
    let mut open = true;
    let mut close = false;
    egui::Window::new(tr("Export the list"))
        .id(egui::Id::new("Export the list"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .min_width(420.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.radio_value(&mut d.selected_only, false, trf!("All downloads ({count})", count = rows.len()));
            ui.add_enabled_ui(!selected.is_empty(), |ui| {
                ui.radio_value(&mut d.selected_only, true, trf!("The ticked downloads ({count})", count = selected.len()));
            });
            ui.add_space(6.0);
            ui.radio_value(&mut d.json, true, tr("UIDM list (.json): addresses, file names, folders and status"));
            ui.radio_value(&mut d.json, false, tr("Text (.txt): one address per line"));
            ui.add_space(4.0);
            ui.weak(tr("Exports never include cookies, request headers or passwords."));
            if let Some(m) = &d.message {
                match m {
                    Ok(m) => ui.label(m),
                    Err(e) => ui.colored_label(ui.visuals().error_fg_color, e),
                };
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!picked.is_empty(), egui::Button::new(tr("Save as..."))).clicked() {
                    let (text, ext, default) = if d.json {
                        (listfile::export_json(&picked), "json", "downloads.json")
                    } else {
                        (listfile::export_text(&picked), "txt", "downloads.txt")
                    };
                    let count = picked.len();
                    let (slot, ctx) = (Arc::clone(&d.result), ui.ctx().clone());
                    let title = tr("Export the download list");
                    thread::spawn(move || {
                        let Some(path) = rfd::FileDialog::new()
                            .set_title(title)
                            .set_file_name(default)
                            .add_filter(ext, &[ext])
                            .save_file()
                        else {
                            return;
                        };
                        let result = std::fs::write(&path, text)
                            .map(|_| trf!("Saved {count} downloads to {path}.", count = count, path = path.display()))
                            .map_err(|e| trf!("Could not save {path}: {error}", path = path.display(), error = e));
                        *slot.lock().unwrap() = Some(result);
                        ctx.request_repaint();
                    });
                }
                if ui.button(tr("Close")).clicked() {
                    close = true;
                }
            });
        });
    open && !close
}

/// Where a crawl is.
pub enum GrabPhase {
    Form,
    Running {
        pages: u64,
        bytes: u64,
        found: usize,
    },
    Review {
        files: Vec<GrabFile>,
        checked: Vec<bool>,
        pages: u64,
        stopped: Option<String>,
    },
}

/// The site grabber: a form, the crawl's progress, then the files found.
pub struct GrabDialog {
    pub phase: GrabPhase,
    start: String,
    depth: u32,
    same_site: bool,
    types: String,
    max_pages: u64,
    max_mb: u64,
    delay_secs: f64,
    /// The filters, each list one pattern per line or separated by spaces.
    explore_only: String,
    explore_skip: String,
    files_only: String,
    files_skip: String,
    subdomains: bool,
    no_parent: bool,
    /// Size limits in MB, when ticked.
    min_mb: Option<f64>,
    max_mb_file: Option<f64>,
    hide_duplicates: bool,
    browser_cookies: bool,
    render_scripts: bool,
    /// Keep the site's folders under `mirror_folder`.
    mirror: bool,
    mirror_folder: String,
    mirror_pages: bool,
    mirror_overwrite: bool,
    mirror_picked: Picked,
    queue: u64,
    start_queue: bool,
    error: Option<String>,
    /// The project's name, for saving.
    project: String,
    /// What the last project action did.
    project_note: Option<String>,
    /// The project's schedule, as in a queue's.
    sched: bool,
    sched_start: String,
    sched_has_stop: bool,
    sched_stop: String,
    sched_by_date: bool,
    sched_days: [bool; 7],
    sched_date: String,
    /// A scheduled run adds new finds to `sched_queue`.
    download_new: bool,
    sched_queue: u64,
    /// What earlier runs found and the last run's day, kept as they are.
    kept: (Vec<String>, Option<i64>),
}

/// Patterns typed one per line or separated by spaces.
fn pattern_list(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

const MB: f64 = 1024.0 * 1024.0;

impl GrabDialog {
    /// A new project, from the saved template.
    pub fn new() -> Self {
        let template = grab::projects_dir().map(|d| grab::template(&d)).unwrap_or_default();
        Self::from_request(&template)
    }

    /// The project these fields and the crawl's results make, or why the
    /// schedule is not valid.
    fn project(&self) -> Result<grab::Project, String> {
        let (files, checked) = match &self.phase {
            GrabPhase::Review { files, checked, .. } => (files.clone(), checked.clone()),
            _ => (Vec::new(), Vec::new()),
        };
        let schedule = self.sched.then(|| uidm::queue::Schedule {
            start: self.sched_start.trim().to_string(),
            stop: self.sched_has_stop.then(|| self.sched_stop.trim().to_string()),
            days: if self.sched_by_date {
                Vec::new()
            } else {
                (0u8..7).filter(|d| self.sched_days[*d as usize]).collect()
            },
            date: self.sched_by_date.then(|| self.sched_date.trim().to_string()),
        });
        if let Some(s) = &schedule {
            s.validate()?;
        }
        Ok(grab::Project {
            name: self.project.trim().to_string(),
            request: self.request(),
            files,
            checked,
            schedule,
            download_new: self.download_new,
            queue: self.sched_queue,
            seen: self.kept.0.clone(),
            last_scheduled_day: self.kept.1,
        })
    }

    /// The form and results of a saved project.
    fn open(p: grab::Project) -> Self {
        let mut d = Self::from_request(&p.request);
        d.project = p.name.clone();
        if let Some(s) = &p.schedule {
            d.sched = true;
            d.sched_start = s.start.clone();
            d.sched_has_stop = s.stop.is_some();
            d.sched_stop = s.stop.clone().unwrap_or_else(|| "06:00".into());
            d.sched_by_date = s.date.is_some();
            d.sched_date = s.date.clone().unwrap_or_default();
            d.sched_days = [false; 7];
            for day in &s.days {
                if let Some(slot) = d.sched_days.get_mut(*day as usize) {
                    *slot = true;
                }
            }
        }
        d.download_new = p.download_new;
        d.sched_queue = p.queue;
        d.kept = (p.seen.clone(), p.last_scheduled_day);
        if !p.files.is_empty() {
            let mut checked = p.checked;
            checked.resize(p.files.len(), true);
            d.phase = GrabPhase::Review {
                files: p.files,
                checked,
                pages: 0,
                stopped: Some(tr("Opened from the saved project. New crawl explores the site again.").into()),
            };
        }
        d
    }

    /// The form filled from `d`.
    pub fn from_request(d: &GrabRequest) -> Self {
        GrabDialog {
            phase: GrabPhase::Form,
            start: d.start.clone(),
            depth: d.depth,
            same_site: d.same_site,
            types: d.types.join(", "),
            max_pages: d.max_pages,
            max_mb: d.max_bytes / (1024 * 1024),
            delay_secs: d.delay_ms as f64 / 1000.0,
            explore_only: d.explore_only.join("\n"),
            explore_skip: d.explore_skip.join("\n"),
            files_only: d.files_only.join("\n"),
            files_skip: d.files_skip.join("\n"),
            subdomains: d.subdomains,
            no_parent: d.no_parent,
            min_mb: d.min_size.map(|b| b as f64 / MB),
            max_mb_file: d.max_size.map(|b| b as f64 / MB),
            hide_duplicates: d.hide_duplicates,
            browser_cookies: d.browser_cookies,
            render_scripts: d.render_scripts,
            mirror: d.mirror.is_some(),
            mirror_folder: d.mirror.as_ref().map(|m| m.folder.clone()).unwrap_or_default(),
            mirror_pages: d.mirror.as_ref().is_some_and(|m| m.pages),
            mirror_overwrite: d.mirror.as_ref().is_some_and(|m| m.overwrite),
            mirror_picked: Picked::default(),
            queue: MAIN_QUEUE,
            start_queue: false,
            error: None,
            project: String::new(),
            project_note: None,
            sched: false,
            sched_start: "03:00".into(),
            sched_has_stop: false,
            sched_stop: "06:00".into(),
            sched_by_date: false,
            sched_days: [true; 7],
            sched_date: String::new(),
            download_new: false,
            sched_queue: MAIN_QUEUE,
            kept: (Vec::new(), None),
        }
    }

    pub fn request(&self) -> GrabRequest {
        GrabRequest {
            start: self.start.trim().to_string(),
            depth: self.depth,
            same_site: self.same_site,
            types: uidm::settings::parse_extensions(&self.types),
            max_pages: self.max_pages,
            max_bytes: self.max_mb * 1024 * 1024,
            delay_ms: (self.delay_secs * 1000.0).round() as u64,
            explore_only: pattern_list(&self.explore_only),
            explore_skip: pattern_list(&self.explore_skip),
            files_only: pattern_list(&self.files_only),
            files_skip: pattern_list(&self.files_skip),
            subdomains: self.subdomains,
            no_parent: self.no_parent,
            min_size: self.min_mb.map(|m| (m * MB).round() as u64),
            max_size: self.max_mb_file.map(|m| (m * MB).round() as u64),
            hide_duplicates: self.hide_duplicates,
            browser_cookies: self.browser_cookies,
            render_scripts: self.render_scripts,
            mirror: self.mirror.then(|| grab::Mirror {
                folder: self.mirror_folder.trim().to_string(),
                pages: self.mirror_pages,
                overwrite: self.mirror_overwrite,
            }),
        }
    }

    /// The daemon's report of the crawl.
    pub fn progress(&mut self, pages: u64, bytes: u64, found: usize) {
        if matches!(self.phase, GrabPhase::Running { .. }) {
            self.phase = GrabPhase::Running { pages, bytes, found };
        }
    }

    pub fn done(&mut self, files: Vec<GrabFile>, pages: u64, stopped: Option<String>) {
        if matches!(self.phase, GrabPhase::Running { .. }) {
            let checked = vec![true; files.len()];
            self.phase = GrabPhase::Review {
                files,
                checked,
                pages,
                stopped,
            };
        }
    }

    /// The daemon refused the crawl.
    pub fn failed(&mut self, error: String) {
        if matches!(self.phase, GrabPhase::Running { .. }) {
            self.phase = GrabPhase::Form;
            self.error = Some(error);
        }
    }

    pub fn running(&self) -> bool {
        matches!(self.phase, GrabPhase::Running { .. })
    }
}

/// Draw the site grabber. Returns false once it is closed (a running crawl
/// is stopped first).
pub fn grab_dialog(
    ctx: &egui::Context,
    d: &mut GrabDialog,
    queues: &[Queue],
    connected: bool,
    actions: &mut Vec<Action>,
) -> bool {
    take_picked(&d.mirror_picked, &mut d.mirror_folder);
    let mut open = true;
    let mut close = false;
    let mut again = false;
    let mut replace = None;
    egui::Window::new(tr("Site grabber"))
        .id(egui::Id::new("Site grabber"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .min_width(640.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            if !d.running() {
                replace = project_bar(ui, d);
                ui.separator();
            }
            grab_body(ui, d, queues, connected, actions, &mut close, &mut again);
        });
    if let Some(next) = replace {
        *d = next;
    }
    if again {
        d.phase = GrabPhase::Form;
    }
    let closed = !open || close;
    if closed && d.running() {
        actions.push(Action::CancelGrab);
    }
    !closed
}

/// The project strip: its name, Save, Open, New and Save as template.
/// Returns the dialog to switch to when a project is opened or a new one
/// started.
fn project_bar(ui: &mut egui::Ui, d: &mut GrabDialog) -> Option<GrabDialog> {
    let dir = grab::projects_dir().ok();
    let mut next = None;
    ui.horizontal(|ui| {
        ui.label(tr("Project"));
        ui.add(egui::TextEdit::singleline(&mut d.project).hint_text(tr("Name, to save it")).desired_width(160.0));
        let named = !d.project.trim().is_empty();
        if ui.add_enabled(named && dir.is_some(), egui::Button::new(tr("Save"))).clicked() {
            if let Some(dir) = &dir {
                d.project_note = Some(match d.project().map(|p| p.save(dir)) {
                    Ok(Ok(())) => trf!("Saved \"{name}\".", name = d.project.trim()),
                    Ok(Err(e)) => trf!("Could not save it: {error}", error = e),
                    Err(e) => e,
                });
            }
        }
        let recent = dir.as_deref().map(grab::recent_projects).unwrap_or_default();
        ui.add_enabled_ui(!recent.is_empty(), |ui| {
            ui.menu_button(tr("Open"), |ui| {
                for name in &recent {
                    if ui.button(name).clicked() {
                        if let Some(dir) = &dir {
                            match grab::Project::load(dir, name) {
                                Ok(p) => next = Some(GrabDialog::open(p)),
                                Err(e) => d.project_note = Some(trf!("Could not open {name}: {error}", name = name, error = e)),
                            }
                        }
                        ui.close_menu();
                    }
                }
            });
        });
        if ui.button(tr("New")).on_hover_text(tr("A new project from the template")).clicked() {
            next = Some(GrabDialog::new());
        }
        if ui
            .add_enabled(dir.is_some(), egui::Button::new(tr("Save as template")))
            .on_hover_text(tr("New projects start with these settings"))
            .clicked()
        {
            if let Some(dir) = &dir {
                d.project_note = Some(match grab::save_template(dir, &d.request()) {
                    Ok(()) => tr("New projects now start with these settings.").into(),
                    Err(e) => trf!("Could not save the template: {error}", error = e),
                });
            }
        }
    });
    if let Some(note) = &d.project_note {
        ui.weak(note.as_str());
    }
    next
}

fn grab_body(
    ui: &mut egui::Ui,
    d: &mut GrabDialog,
    queues: &[Queue],
    connected: bool,
    actions: &mut Vec<Action>,
    close: &mut bool,
    again: &mut bool,
) {
    // The settings found files are saved with.
    let req = d.request();
    match &mut d.phase {
        GrabPhase::Form => {
            ui.weak(tr("Reads a site's pages and lists the files they link to. It follows robots.txt and waits between pages."));
            ui.add_space(6.0);
            egui::Grid::new("grab_form").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label(tr("Start page"));
                ui.add(
                    egui::TextEdit::singleline(&mut d.start)
                        .hint_text("https://example.com/docs/")
                        .desired_width(420.0),
                );
                ui.end_row();
                ui.label(tr("Links deep"));
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut d.depth).range(grab::DEPTH_RANGE.0..=grab::DEPTH_RANGE.1));
                    ui.weak(tr("0 reads only the start page"));
                });
                ui.end_row();
                ui.label("");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut d.same_site, tr("Stay on the start page's site"));
                    ui.add_enabled(d.same_site, egui::Checkbox::new(&mut d.subdomains, tr("and its subdomains")));
                });
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut d.no_parent, tr("Do not go above the start page's folder"));
                ui.end_row();
                ui.label(tr("Sign-in"));
                ui.vertical(|ui| {
                    ui.checkbox(&mut d.browser_cookies, tr("Use my browser's sign-in for the start site"))
                        .on_hover_text(tr("Sign in on the site in your browser first. UIDM's extension hands over the site's cookies, which go to that site's pages only."));
                    ui.weak(tr("A login set in Options > Connection is used when the site asks for one."));
                });
                ui.end_row();
                ui.label(tr("Keep files of type"));
                ui.add(egui::TextEdit::singleline(&mut d.types).hint_text("pdf, zip").desired_width(240.0));
                ui.end_row();
                ui.label(tr("Budget"));
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut d.max_pages).range(grab::MAX_PAGES_RANGE.0..=grab::MAX_PAGES_RANGE.1));
                    ui.label(tr("pages, at most"));
                    ui.add(egui::DragValue::new(&mut d.max_mb).range(grab::MAX_MB_RANGE.0..=grab::MAX_MB_RANGE.1));
                    ui.label(tr("MB of pages"));
                });
                ui.end_row();
                ui.label(tr("Wait between pages"));
                ui.horizontal(|ui| {
                    let (lo, hi) = grab::DELAY_MS_RANGE;
                    ui.add(
                        egui::DragValue::new(&mut d.delay_secs)
                            .range(lo as f64 / 1000.0..=hi as f64 / 1000.0)
                            .speed(0.1)
                            .max_decimals(1),
                    );
                    ui.label(tr("seconds (longer if the site's robots.txt asks)"));
                });
                ui.end_row();
            });
            egui::CollapsingHeader::new(tr("Filters")).id_salt("grab_filters").show(ui, |ui| {
                ui.weak(tr("One pattern per line. * matches anything; a pattern without * matches anywhere in the address, as /docs/ does."));
                egui::Grid::new("grab_filter_grid").num_columns(4).spacing([10.0, 6.0]).show(ui, |ui| {
                    let field = |ui: &mut egui::Ui, text: &mut String, hint: &str| {
                        ui.add(egui::TextEdit::multiline(text).hint_text(hint).desired_rows(2).desired_width(190.0));
                    };
                    ui.label(tr("Explore only"));
                    field(ui, &mut d.explore_only, tr("Every page in reach"));
                    ui.label(tr("Never explore"));
                    field(ui, &mut d.explore_skip, "*/logout*");
                    ui.end_row();
                    ui.label(tr("Keep only files"));
                    field(ui, &mut d.files_only, tr("Every file of the types"));
                    ui.label(tr("Never keep"));
                    field(ui, &mut d.files_skip, "*/old/*");
                    ui.end_row();
                });
                ui.horizontal(|ui| {
                    let size = |ui: &mut egui::Ui, label: &str, value: &mut Option<f64>, default: f64| {
                        let mut on = value.is_some();
                        ui.checkbox(&mut on, label);
                        let mut v = value.unwrap_or(default);
                        ui.add_enabled(on, egui::DragValue::new(&mut v).range(0.0..=1_000_000.0).speed(0.5).max_decimals(1).suffix(tr(" MB")));
                        *value = on.then_some(v);
                    };
                    size(ui, tr("At least"), &mut d.min_mb, 1.0);
                    size(ui, tr("At most"), &mut d.max_mb_file, 100.0);
                })
                .response
                .on_hover_text(tr("Sizes are asked with one request per file, after the usual wait"));
                ui.checkbox(&mut d.hide_duplicates, tr("List a file once when the same name and size are found at other addresses"));
                ui.checkbox(&mut d.render_scripts, tr("Find links that page scripts add (slower)"))
                    .on_hover_text(tr("The browser extension opens each page in a background tab and lets its scripts run. Without the browser, pages are read as the server sends them."));
            });
            egui::CollapsingHeader::new(tr("Save for offline browsing")).id_salt("grab_mirror").show(ui, |ui| {
                ui.checkbox(&mut d.mirror, tr("Keep the site's folders: save each file under a folder that follows its address"));
                ui.add_enabled_ui(d.mirror, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(tr("Under"));
                        ui.add(egui::TextEdit::singleline(&mut d.mirror_folder).hint_text(tr("A full path")).desired_width(320.0));
                        browse_button(ui, tr("Keep the site in"), &d.mirror_folder, &d.mirror_picked);
                    });
                    ui.checkbox(&mut d.mirror_pages, tr("Save the pages too, with their links changed to the saved copies"));
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut d.mirror_overwrite, false, tr("Skip files already there"));
                        ui.radio_value(&mut d.mirror_overwrite, true, tr("Replace them"));
                    });
                });
            });
            egui::CollapsingHeader::new(tr("Schedule")).id_salt("grab_schedule").show(ui, |ui| {
                ui.weak(tr("A saved project can run by itself while UIDM is running. Save the project after changing this."));
                ui.checkbox(&mut d.sched, tr("Run this project on a schedule"));
                ui.add_enabled_ui(d.sched, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(tr("At"));
                        ui.add(egui::TextEdit::singleline(&mut d.sched_start).hint_text("03:00").desired_width(50.0));
                        ui.checkbox(&mut d.sched_has_stop, tr("stop at"));
                        ui.add_enabled(d.sched_has_stop, egui::TextEdit::singleline(&mut d.sched_stop).desired_width(50.0));
                    });
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut d.sched_by_date, false, tr("Every"));
                        ui.add_enabled_ui(!d.sched_by_date, |ui| {
                            for (i, name) in ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"].iter().enumerate() {
                                ui.checkbox(&mut d.sched_days[i], tr(*name));
                            }
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut d.sched_by_date, true, tr("Once, on"));
                        ui.add_enabled(
                            d.sched_by_date,
                            egui::TextEdit::singleline(&mut d.sched_date).hint_text("2026-12-31").desired_width(90.0),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut d.download_new, false, tr("Explore only"));
                        ui.radio_value(&mut d.download_new, true, tr("Explore and add new files to"));
                        ui.add_enabled_ui(d.download_new, |ui| queue_combo(ui, "grab_sched_queue", &mut d.sched_queue, queues));
                    });
                    ui.weak(tr("Each run adds only files no earlier run found."));
                });
            });
            ui.add_space(4.0);
            let least = d.max_pages as f64 * d.delay_secs;
            ui.label(trf!(
                "Stops after {pages} pages or {mb} MB, whichever comes first; {pages} pages take at least {time}.",
                pages = d.max_pages,
                mb = d.max_mb,
                time = super::human_duration(least)
            ));
            if let Some(e) = &d.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(connected, egui::Button::new(tr("Start"))).clicked() {
                    let req = d.request();
                    match req.validate() {
                        Ok(()) => {
                            d.error = None;
                            d.phase = GrabPhase::Running {
                                pages: 0,
                                bytes: 0,
                                found: 0,
                            };
                            actions.push(Action::Grab(req));
                        }
                        Err(e) => d.error = Some(e),
                    }
                }
                if ui.button(tr("Cancel")).clicked() {
                    *close = true;
                }
            });
        }
        GrabPhase::Running { pages, bytes, found } => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(trf!(
                    "Read {pages} of at most {max} pages ({size}); found {found} files.",
                    pages = pages,
                    max = d.max_pages,
                    size = human_bytes(*bytes),
                    found = found
                ));
            });
            ui.add_space(8.0);
            if ui.button(tr("Stop")).clicked() {
                actions.push(Action::CancelGrab);
            }
        }
        GrabPhase::Review {
            files,
            checked,
            pages,
            stopped,
        } => {
            ui.label(trf!("Read {pages} pages and found {count} files.", pages = pages, count = files.len()));
            if let Some(why) = stopped {
                ui.weak(why.as_str());
            }
            ui.horizontal(|ui| {
                if ui.small_button(tr("Tick all")).clicked() {
                    checked.iter_mut().for_each(|c| *c = true);
                }
                if ui.small_button(tr("Tick none")).clicked() {
                    checked.iter_mut().for_each(|c| *c = false);
                }
            });
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                for (f, c) in files.iter().zip(checked.iter_mut()) {
                    ui.horizontal(|ui| {
                        ui.checkbox(c, "");
                        ui.add(egui::Label::new(&f.name).truncate())
                            .on_hover_text(trf!("{url}\nfound on {page}", url = f.url, page = f.page));
                        if let Some(n) = f.size {
                            ui.weak(human_bytes(n));
                        }
                    });
                }
            });
            let n = checked.iter().filter(|c| **c).count();
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(tr("Queue"));
                queue_combo(ui, "grab_queue", &mut d.queue, queues);
                ui.checkbox(&mut d.start_queue, tr("Start the queue now"));
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(connected && n > 0, egui::Button::new(trf!("Add {count}", count = n))).clicked() {
                    let items = files
                        .iter()
                        .zip(checked.iter())
                        .filter(|(_, c)| **c)
                        .filter_map(|(f, _)| grab::download_for(&req, f))
                        .collect();
                    actions.push(Action::AddMany {
                        items,
                        queue: d.queue,
                        start_queue: d.start_queue,
                    });
                    *close = true;
                }
                if ui.button(tr("New crawl")).clicked() {
                    *again = true;
                }
                if ui.button(tr("Close")).clicked() {
                    *close = true;
                }
            });
        }
    }
}

/// Offer to download a copied link: Download or Ignore. Returns false once
/// answered.
pub fn clipboard_offer(ctx: &egui::Context, url: &str, connected: bool, actions: &mut Vec<Action>) -> bool {
    let mut answered = false;
    egui::Window::new(tr("Download the copied link?"))
        .id(egui::Id::new("Download the copied link?"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -40.0])
        .show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.add(egui::Label::new(egui::RichText::new(url).monospace()).wrap());
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(connected, egui::Button::new(tr("Download"))).clicked() {
                    actions.push(Action::Add(url.to_string()));
                    answered = true;
                }
                if ui.button(tr("Ignore")).clicked() {
                    answered = true;
                }
            });
        });
    !answered
}
