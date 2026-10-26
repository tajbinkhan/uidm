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

/// Text read from a file on the picker's thread, or why it could not be.
pub type Loaded = Arc<Mutex<Option<Result<(String, String), String>>>>;

/// Ask for a text file on another thread (the picker blocks) and put its
/// name and contents in `slot`.
pub fn pick_text_file(ctx: &egui::Context, title: &'static str, slot: &Loaded) {
    let (slot, ctx) = (Arc::clone(slot), ctx.clone());
    thread::spawn(move || {
        let Some(path) = rfd::FileDialog::new()
            .set_title(title)
            .add_filter("Lists", &["txt", "json"])
            .add_filter("All files", &["*"])
            .pick_file()
        else {
            return;
        };
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let read = std::fs::read(&path)
            .map(|b| (name, String::from_utf8_lossy(&b).into_owned()))
            .map_err(|e| format!("Could not read {}: {e}", path.display()));
        *slot.lock().unwrap() = Some(read);
        ctx.request_repaint();
    });
}

/// A queue picker.
fn queue_combo(ui: &mut egui::Ui, id: &str, queue: &mut u64, queues: &[Queue]) {
    let name = queues
        .iter()
        .find(|q| q.id == *queue)
        .map_or("Main queue".to_string(), |q| q.name.clone());
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
    egui::Window::new(title)
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .min_width(640.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            if d.source != Source::Imported {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut d.source, Source::Pattern, "From a pattern");
                    ui.selectable_value(&mut d.source, Source::List, "From a list");
                });
                ui.separator();
            }
            let urls = d.urls();
            match d.source {
                Source::Pattern => {
                    ui.label("Address with ranges in brackets:");
                    ui.add(
                        egui::TextEdit::singleline(&mut d.pattern)
                            .hint_text("https://example.com/photo[001-120].jpg")
                            .desired_width(600.0),
                    );
                    ui.weak("[1-20] counts, [001-120] keeps the zeros, [0-100:5] counts in fives, [a-z] runs through letters.");
                }
                Source::List => {
                    ui.horizontal(|ui| {
                        ui.label("One address per line:");
                        if ui.button("Load a text file...").clicked() {
                            pick_text_file(ui.ctx(), "Load a list of addresses", &d.loaded);
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
                            1 => format!("1 download in {name}."),
                            n => format!("{n} downloads in {name}."),
                        });
                        if *rejected > 0 {
                            ui.weak(format!("{rejected} entries are not web addresses and are left out."));
                        }
                        ui.weak("They are added waiting, with their file names and folders when the list has them.");
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
                        1 => "1 address".to_string(),
                        n => format!("{n} addresses"),
                    };
                    if *rejected > 0 {
                        line.push_str(&format!(", {rejected} lines skipped (not web addresses)"));
                    }
                    if u.len() == MAX_BATCH {
                        line.push_str(&format!(" (the most one batch can add is {MAX_BATCH})"));
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
                ui.label("Queue");
                ui.horizontal(|ui| {
                    queue_combo(ui, "batch_queue", &mut d.queue, queues);
                    ui.checkbox(&mut d.start, "Start the queue now");
                });
                ui.end_row();
                if d.source != Source::Imported {
                    ui.label("Save to");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut d.folder)
                                .hint_text("The folder of each file's category")
                                .desired_width(340.0),
                        );
                        browse_button(ui, "Save the files to", &d.folder, &d.picked);
                    });
                    ui.end_row();
                }
            });
            if let Some(e) = &d.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let add = ui.add_enabled(connected && count > 0, egui::Button::new(format!("Add {count}")));
                if add.clicked() {
                    let folder = d.folder.trim();
                    if !folder.is_empty() && !std::path::Path::new(folder).is_absolute() {
                        d.error = Some(format!("The folder must be a full path: {folder}"));
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
                if ui.button("Cancel").clicked() {
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
    egui::Window::new("Export the list")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .min_width(420.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.radio_value(&mut d.selected_only, false, format!("All downloads ({})", rows.len()));
            ui.add_enabled_ui(!selected.is_empty(), |ui| {
                ui.radio_value(&mut d.selected_only, true, format!("The ticked downloads ({})", selected.len()));
            });
            ui.add_space(6.0);
            ui.radio_value(&mut d.json, true, "UIDM list (.json): addresses, file names, folders and status");
            ui.radio_value(&mut d.json, false, "Text (.txt): one address per line");
            ui.add_space(4.0);
            ui.weak("Exports never include cookies, request headers or passwords.");
            if let Some(m) = &d.message {
                match m {
                    Ok(m) => ui.label(m),
                    Err(e) => ui.colored_label(ui.visuals().error_fg_color, e),
                };
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!picked.is_empty(), egui::Button::new("Save as...")).clicked() {
                    let (text, ext, default) = if d.json {
                        (listfile::export_json(&picked), "json", "downloads.json")
                    } else {
                        (listfile::export_text(&picked), "txt", "downloads.txt")
                    };
                    let count = picked.len();
                    let (slot, ctx) = (Arc::clone(&d.result), ui.ctx().clone());
                    thread::spawn(move || {
                        let Some(path) = rfd::FileDialog::new()
                            .set_title("Export the download list")
                            .set_file_name(default)
                            .add_filter(ext, &[ext])
                            .save_file()
                        else {
                            return;
                        };
                        let result = std::fs::write(&path, text)
                            .map(|_| format!("Saved {count} downloads to {}.", path.display()))
                            .map_err(|e| format!("Could not save {}: {e}", path.display()));
                        *slot.lock().unwrap() = Some(result);
                        ctx.request_repaint();
                    });
                }
                if ui.button("Close").clicked() {
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
    queue: u64,
    start_queue: bool,
    error: Option<String>,
}

impl GrabDialog {
    pub fn new() -> Self {
        let d = GrabRequest::default();
        GrabDialog {
            phase: GrabPhase::Form,
            start: String::new(),
            depth: d.depth,
            same_site: d.same_site,
            types: d.types.join(", "),
            max_pages: d.max_pages,
            max_mb: d.max_bytes / (1024 * 1024),
            delay_secs: d.delay_ms as f64 / 1000.0,
            queue: MAIN_QUEUE,
            start_queue: false,
            error: None,
        }
    }

    fn request(&self) -> GrabRequest {
        GrabRequest {
            start: self.start.trim().to_string(),
            depth: self.depth,
            same_site: self.same_site,
            types: uidm::settings::parse_extensions(&self.types),
            max_pages: self.max_pages,
            max_bytes: self.max_mb * 1024 * 1024,
            delay_ms: (self.delay_secs * 1000.0).round() as u64,
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
    let mut open = true;
    let mut close = false;
    let mut again = false;
    egui::Window::new("Site grabber")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .min_width(640.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| match &mut d.phase {
            GrabPhase::Form => {
                ui.weak("Reads a site's pages and lists the files they link to. It follows robots.txt and waits between pages.");
                ui.add_space(6.0);
                egui::Grid::new("grab_form").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Start page");
                    ui.add(
                        egui::TextEdit::singleline(&mut d.start)
                            .hint_text("https://example.com/docs/")
                            .desired_width(420.0),
                    );
                    ui.end_row();
                    ui.label("Links deep");
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut d.depth).range(grab::DEPTH_RANGE.0..=grab::DEPTH_RANGE.1));
                        ui.weak("0 reads only the start page");
                    });
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut d.same_site, "Stay on the start page's site");
                    ui.end_row();
                    ui.label("Keep files of type");
                    ui.add(egui::TextEdit::singleline(&mut d.types).hint_text("pdf, zip").desired_width(240.0));
                    ui.end_row();
                    ui.label("Budget");
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut d.max_pages).range(grab::MAX_PAGES_RANGE.0..=grab::MAX_PAGES_RANGE.1));
                        ui.label("pages, at most");
                        ui.add(egui::DragValue::new(&mut d.max_mb).range(grab::MAX_MB_RANGE.0..=grab::MAX_MB_RANGE.1));
                        ui.label("MB of pages");
                    });
                    ui.end_row();
                    ui.label("Wait between pages");
                    ui.horizontal(|ui| {
                        let (lo, hi) = grab::DELAY_MS_RANGE;
                        ui.add(
                            egui::DragValue::new(&mut d.delay_secs)
                                .range(lo as f64 / 1000.0..=hi as f64 / 1000.0)
                                .speed(0.1)
                                .max_decimals(1),
                        );
                        ui.label("seconds (longer if the site's robots.txt asks)");
                    });
                    ui.end_row();
                });
                ui.add_space(4.0);
                let least = d.max_pages as f64 * d.delay_secs;
                ui.label(format!(
                    "Stops after {} pages or {} MB, whichever comes first; {} pages take at least {}.",
                    d.max_pages,
                    d.max_mb,
                    d.max_pages,
                    super::human_duration(least)
                ));
                if let Some(e) = &d.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(connected, egui::Button::new("Start")).clicked() {
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
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            }
            GrabPhase::Running { pages, bytes, found } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!(
                        "Read {pages} of at most {} pages ({}); found {found} files.",
                        d.max_pages,
                        human_bytes(*bytes)
                    ));
                });
                ui.add_space(8.0);
                if ui.button("Stop").clicked() {
                    actions.push(Action::CancelGrab);
                }
            }
            GrabPhase::Review {
                files,
                checked,
                pages,
                stopped,
            } => {
                ui.label(format!("Read {pages} pages and found {} files.", files.len()));
                if let Some(why) = stopped {
                    ui.weak(why.as_str());
                }
                ui.horizontal(|ui| {
                    if ui.small_button("Tick all").clicked() {
                        checked.iter_mut().for_each(|c| *c = true);
                    }
                    if ui.small_button("Tick none").clicked() {
                        checked.iter_mut().for_each(|c| *c = false);
                    }
                });
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    for (f, c) in files.iter().zip(checked.iter_mut()) {
                        ui.horizontal(|ui| {
                            ui.checkbox(c, "");
                            ui.add(egui::Label::new(&f.name).truncate())
                                .on_hover_text(format!("{}\nfound on {}", f.url, f.page));
                        });
                    }
                });
                let n = checked.iter().filter(|c| **c).count();
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("Queue");
                    queue_combo(ui, "grab_queue", &mut d.queue, queues);
                    ui.checkbox(&mut d.start_queue, "Start the queue now");
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(connected && n > 0, egui::Button::new(format!("Add {n}"))).clicked() {
                        let items = files
                            .iter()
                            .zip(checked.iter())
                            .filter(|(_, c)| **c)
                            .map(|(f, _)| Download {
                                url: f.url.clone(),
                                referrer: Some(f.page.clone()),
                                start: Some(false),
                                ..Download::default()
                            })
                            .collect();
                        actions.push(Action::AddMany {
                            items,
                            queue: d.queue,
                            start_queue: d.start_queue,
                        });
                        close = true;
                    }
                    if ui.button("New crawl").clicked() {
                        again = true;
                    }
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            }
        });
    if again {
        d.phase = GrabPhase::Form;
    }
    let closed = !open || close;
    if closed && d.running() {
        actions.push(Action::CancelGrab);
    }
    !closed
}

/// Offer to download a copied link: Download or Ignore. Returns false once
/// answered.
pub fn clipboard_offer(ctx: &egui::Context, url: &str, connected: bool, actions: &mut Vec<Action>) -> bool {
    let mut answered = false;
    egui::Window::new("Download the copied link?")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -40.0])
        .show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.add(egui::Label::new(egui::RichText::new(url).monospace()).wrap());
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(connected, egui::Button::new("Download")).clicked() {
                    actions.push(Action::Add(url.to_string()));
                    answered = true;
                }
                if ui.button("Ignore").clicked() {
                    answered = true;
                }
            });
        });
    !answered
}
