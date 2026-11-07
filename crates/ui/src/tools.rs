//! The window's tools for many downloads at once: the Batch dialog (a URL
//! pattern, a pasted list or text file, or an imported list), exporting the
//! list, the site grabber, and the clipboard offer.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use eframe::egui;
use uidm_application::window::{FileKind, GrabLibrary, ListFormat};

use crate::pending::Pending;
use uidm_domain::batch::{self, Wildcard, MAX_BATCH, MAX_WILDCARD_SIZE};
use uidm_domain::download::{Download, DownloadEntry, DownloadLogin, Status};
use uidm_domain::grab::{GrabFile, GrabRequest, SaveTo};
use uidm_domain::queue::{Queue, MAIN_QUEUE};

use super::{browse_button, human_bytes, take_picked, Action, Picked};
use crate::i18n::{tr, trf};

/// Text read from a file on the picker's thread, or why it could not be.
pub type Loaded = Arc<Mutex<Option<Result<(String, String), String>>>>;

/// Ask for a text file on another thread (the picker blocks) and put its
/// name and contents in `slot`.
pub fn pick_text_file(ctx: &egui::Context, title: &'static str, slot: &Loaded) {
    let (slot, ctx) = (Arc::clone(slot), ctx.clone());
    thread::spawn(move || {
        let kinds = [FileKind::new(tr("Lists"), &["txt", "json", "ef2"]), FileKind::new(tr("All files"), &["*"])];
        let ports = crate::ports::ports();
        let Some(path) = ports.pickers.pick_file(title, &kinds, None) else {
            return;
        };
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let read = ports
            .lists
            .read(&path)
            .map(|text| (name, text))
            .map_err(|e| trf!("Could not read {path}: {error}", path = path.display(), error = e));
        *slot.lock().unwrap() = Some(read);
        ctx.request_repaint();
    });
}

/// A queue picker.
pub fn queue_combo(ui: &mut egui::Ui, id: &str, queue: &mut u64, queues: &[Queue]) {
    let name = queues.iter().find(|q| q.id == *queue).map_or(tr("Main queue").to_string(), |q| q.name.clone());
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
    /// IDM's batch settings, used when the pattern has a `*`: what replaces
    /// it, From, To and the wildcard size numbers are padded to.
    wildcard: Wildcard,
    wild_from: String,
    wild_to: String,
    wild_size: usize,
    /// The settings last expanded (pattern and wildcard settings), and what
    /// they gave, so the list is not rebuilt every frame.
    expanded: Option<(String, Result<Vec<String>, String>)>,
    list: String,
    /// An imported file's name, downloads and the entries left out.
    imported: Option<(String, Vec<Download>, usize)>,
    queue: u64,
    start: bool,
    /// Where to save; empty lets each file's category decide.
    folder: String,
    picked: Picked,
    /// "Use a login": the user name and password sent with every address.
    login: Option<(String, String)>,
    loaded: Loaded,
    error: Option<String>,
}

impl BatchDialog {
    pub fn new() -> Self {
        BatchDialog {
            source: Source::Pattern,
            pattern: String::new(),
            wildcard: Wildcard::Numbers,
            wild_from: "1".into(),
            wild_to: "10".into(),
            wild_size: 1,
            expanded: None,
            list: String::new(),
            imported: None,
            queue: MAIN_QUEUE,
            start: false,
            folder: String::new(),
            picked: Picked::default(),
            login: None,
            loaded: Loaded::default(),
            error: None,
        }
    }

    /// The dialog with `urls` in its list, such as every link on the
    /// clipboard.
    pub fn with_list(urls: &[String]) -> Self {
        BatchDialog { source: Source::List, list: urls.join("\n"), ..BatchDialog::new() }
    }

    /// The dialog for downloads imported from `name`.
    pub fn imported(name: String, items: Vec<Download>, rejected: usize) -> Self {
        BatchDialog { source: Source::Imported, imported: Some((name, items, rejected)), ..BatchDialog::new() }
    }

    /// The URLs to add, or why there are none.
    fn urls(&mut self) -> Result<(Vec<String>, usize), String> {
        match self.source {
            Source::Pattern => {
                // A `*` means IDM's form, so its settings count too.
                let asterisk = self.pattern.contains('*');
                let key = if asterisk {
                    format!(
                        "{}\n{:?}\n{}\n{}\n{}",
                        self.pattern, self.wildcard, self.wild_from, self.wild_to, self.wild_size
                    )
                } else {
                    self.pattern.clone()
                };
                let fresh = self.expanded.as_ref().is_none_or(|(k, _)| *k != key);
                if fresh {
                    let result = if self.pattern.trim().is_empty() {
                        Ok(Vec::new())
                    } else if asterisk {
                        batch::expand_asterisk(
                            &self.pattern,
                            self.wildcard,
                            &self.wild_from,
                            &self.wild_to,
                            self.wild_size,
                        )
                    } else {
                        batch::expand(&self.pattern)
                    };
                    self.expanded = Some((key, result));
                }
                let (_, result) = self.expanded.as_ref().unwrap();
                result.clone().map(|u| (u, 0))
            }
            Source::List => batch::parse_list(&self.list),
            Source::Imported => Ok((Vec::new(), 0)),
        }
    }
}

/// IDM's batch settings for an address with a `*`: numbers or letters, From,
/// To and the wildcard size. Changes show at once, as the preview is rebuilt
/// whenever they differ from the ones last expanded.
fn wildcard_controls(ui: &mut egui::Ui, d: &mut BatchDialog) {
    egui::Grid::new("batch_wildcard").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label(tr("Replace the * with"));
        ui.horizontal(|ui| {
            ui.radio_value(&mut d.wildcard, Wildcard::Numbers, tr("Numbers"));
            ui.radio_value(&mut d.wildcard, Wildcard::Letters, tr("Letters (a to z)"));
        });
        ui.end_row();
        ui.label(tr("From"));
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut d.wild_from).desired_width(90.0));
            ui.label(tr("To"));
            ui.add(egui::TextEdit::singleline(&mut d.wild_to).desired_width(90.0));
        });
        ui.end_row();
        let numbers = d.wildcard == Wildcard::Numbers;
        ui.add_enabled(numbers, egui::Label::new(tr("Wildcard size")));
        ui.horizontal(|ui| {
            ui.add_enabled(numbers, egui::DragValue::new(&mut d.wild_size).range(1..=MAX_WILDCARD_SIZE));
            ui.add_enabled(
                numbers,
                egui::Label::new(egui::RichText::new(tr("digits; numbers are padded with zeros, 1 adds none")).weak()),
            );
        });
        ui.end_row();
    });
}

/// Draw the Batch dialog. Returns false once it is closed.
/// `queue_panel` shows the queue picker; Options can hide it, and the
/// downloads then go to the main queue without starting it.
pub fn batch_dialog(
    ctx: &egui::Context,
    d: &mut BatchDialog,
    queues: &[Queue],
    queue_panel: bool,
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
    crate::dialog::Window::new(tr(title))
        .id(egui::Id::new(title))
        .dlu_width(316.0)
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
                    ui.label(tr("Address with ranges in brackets, or with one * as in IDM:"));
                    ui.add(
                        egui::TextEdit::singleline(&mut d.pattern)
                            .hint_text("https://example.com/photo[001-120].jpg")
                            .desired_width(600.0),
                    );
                    if d.pattern.contains('*') {
                        wildcard_controls(ui, d);
                    } else {
                        ui.weak(tr("[1-20] counts, [001-120] keeps the zeros, [0-100:5] counts in fives, [a-z] runs through letters."));
                    }
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
                if queue_panel {
                    ui.label(tr("Queue"));
                    ui.horizontal(|ui| {
                        queue_combo(ui, "batch_queue", &mut d.queue, queues);
                        ui.checkbox(&mut d.start, tr("Start the queue now"));
                    });
                    ui.end_row();
                }
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
                ui.label(tr("Login"));
                ui.horizontal(|ui| {
                    let mut use_login = d.login.is_some();
                    if ui.checkbox(&mut use_login, tr("Use a login")).changed() {
                        d.login = use_login.then(Default::default);
                    }
                    if let Some((user, password)) = d.login.as_mut() {
                        ui.add(egui::TextEdit::singleline(user).hint_text(tr("User name")).desired_width(140.0));
                        ui.add(egui::TextEdit::singleline(password).password(true).hint_text(tr("Password")).desired_width(140.0));
                    }
                });
                ui.end_row();
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
                    let login = d.login.as_ref().map(|(u, p)| DownloadLogin {
                        username: u.trim().to_string(),
                        password: Some(p.clone()),
                    });
                    if login.as_ref().is_some_and(|l| l.username.is_empty()) {
                        d.error = Some(tr("Type the user name for the login, or untick Use a login.").into());
                        return;
                    }
                    let mut items = match (&d.imported, urls) {
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
                    for item in items.iter_mut().filter(|i| i.login.is_none()) {
                        item.login = login.clone();
                    }
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

/// A file format the Export dialog writes.
#[derive(Clone, Copy, PartialEq)]
enum ExportFormat {
    Json,
    Text,
    /// IDM's export file.
    Ef2,
}

/// Which downloads the Export dialog writes.
#[derive(Clone, Copy, PartialEq)]
enum ExportScope {
    All,
    Ticked,
    /// The downloads waiting in `ExportDialog::queue`, in its order.
    Queue,
}

/// The Export dialog.
pub struct ExportDialog {
    scope: ExportScope,
    queue: u64,
    format: ExportFormat,
    result: Arc<Mutex<Option<Result<String, String>>>>,
    message: Option<Result<String, String>>,
}

impl ExportDialog {
    /// Starts on the ticked downloads if any, else on `queue` (the queue
    /// the list shows) if given.
    pub fn new(any_selected: bool, queue: Option<u64>) -> Self {
        ExportDialog {
            scope: match (any_selected, queue) {
                (true, _) => ExportScope::Ticked,
                (false, Some(_)) => ExportScope::Queue,
                (false, None) => ExportScope::All,
            },
            queue: queue.unwrap_or(MAIN_QUEUE),
            format: ExportFormat::Json,
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
    queues: &[Queue],
) -> bool {
    if let Some(r) = d.result.lock().unwrap().take() {
        d.message = Some(r);
    }
    let in_queue = |id: u64| -> Vec<&DownloadEntry> {
        let order = queues.iter().find(|q| q.id == id).map_or(&[][..], |q| q.order.as_slice());
        order.iter().filter_map(|i| rows.iter().find(|e| e.id == *i)).collect()
    };
    let picked: Vec<&DownloadEntry> = match d.scope {
        ExportScope::All => rows.iter().collect(),
        ExportScope::Ticked => rows.iter().filter(|e| selected.contains(&e.id)).collect(),
        ExportScope::Queue => in_queue(d.queue),
    };
    let mut open = true;
    let mut close = false;
    crate::dialog::Window::new(tr("Export the list"))
        .id(egui::Id::new("Export the list"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .min_width(420.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.radio_value(&mut d.scope, ExportScope::All, trf!("All downloads ({count})", count = rows.len()));
            ui.add_enabled_ui(!selected.is_empty(), |ui| {
                ui.radio_value(
                    &mut d.scope,
                    ExportScope::Ticked,
                    trf!("The ticked downloads ({count})", count = selected.len()),
                );
            });
            ui.horizontal(|ui| {
                ui.radio_value(&mut d.scope, ExportScope::Queue, tr("The downloads waiting in"));
                queue_combo(ui, "export_queue", &mut d.queue, queues);
                ui.weak(format!("({})", in_queue(d.queue).len()));
            });
            ui.add_space(6.0);
            ui.radio_value(
                &mut d.format,
                ExportFormat::Json,
                tr("UIDM list (.json): addresses, file names, folders and status"),
            );
            ui.radio_value(&mut d.format, ExportFormat::Text, tr("Text (.txt): one address per line"));
            ui.radio_value(
                &mut d.format,
                ExportFormat::Ef2,
                tr("IDM export file (.ef2): addresses and referrers, for IDM to import"),
            );
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
                    let (text, ext, default) = match d.format {
                        ExportFormat::Json => {
                            (crate::ports::ports().lists.export(ListFormat::Json, &picked), "json", "downloads.json")
                        }
                        ExportFormat::Text => {
                            (crate::ports::ports().lists.export(ListFormat::Text, &picked), "txt", "downloads.txt")
                        }
                        ExportFormat::Ef2 => {
                            (crate::ports::ports().lists.export(ListFormat::Ef2, &picked), "ef2", "downloads.ef2")
                        }
                    };
                    let count = picked.len();
                    let (slot, ctx) = (Arc::clone(&d.result), ui.ctx().clone());
                    let title = tr("Export the download list");
                    thread::spawn(move || {
                        let ports = crate::ports::ports();
                        let Some(path) = ports.pickers.save_file(title, default, &[FileKind::new(ext, &[ext])]) else {
                            return;
                        };
                        let result = ports
                            .lists
                            .write(&path, &text)
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
        explored: u64,
        /// The downloads the crawl added to a queue so far, to count the
        /// finished ones.
        added: Vec<u64>,
    },
    Review {
        files: Vec<GrabFile>,
        checked: Vec<bool>,
        pages: u64,
        explored: u64,
        stopped: Option<String>,
    },
}

/// Where the found files are saved, as the form picks it.
#[derive(Clone, Copy, PartialEq)]
enum SaveChoice {
    Categories,
    Category,
    Folder,
}

/// How the found files are listed.
#[derive(Clone, Copy, PartialEq)]
enum ResultsView {
    List,
    ByPage,
    ByFolder,
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
    ignore_popups: bool,
    explore_at_once: u32,
    download_at_once: u32,
    link_text: bool,
    /// Types never kept, separated by commas.
    skip_types: String,
    files_this_site: bool,
    /// The named filters, and the one being added or changed: its place
    /// (`None` for a new one), name and mask.
    filters: Vec<uidm_domain::grab::Filter>,
    filter_edit: Option<(Option<usize>, String, String)>,
    save_choice: SaveChoice,
    save_category: String,
    save_folder: String,
    save_picked: Picked,
    /// Add each match to `queue` as it is found, and start it.
    download_found: bool,
    whole_site: bool,
    other_levels_on: bool,
    other_levels: u32,
    /// The project's own login for its start site.
    login_on: bool,
    login_user: String,
    /// A new password, when `login_changed`.
    login_password: String,
    login_changed: bool,
    /// The site's sign-in page, opened in the browser to sign in there,
    /// and cookies pasted for its site when the browser cannot give them.
    login_page: String,
    login_cookies: String,
    /// The login page site's cookies the last crawl used (the browser's or
    /// the pasted ones), for the files added from that site; kept only
    /// while the grabber is open.
    crawl_login_cookies: Option<String>,
    /// Normal saves: keep the site's subfolders; and replace files already
    /// there (a mirror's choice too).
    subfolders: bool,
    overwrite: bool,
    /// The name typed for Save as template, while asking for one.
    template_name: Option<String>,
    view: ResultsView,
    /// Keep the site's folders under `mirror_folder`.
    mirror: bool,
    mirror_folder: String,
    mirror_pages: bool,
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
    sched_repeat: bool,
    sched_repeat_hours: u32,
    sched_repeat_minutes: u32,
    sched_wake: bool,
    /// What a scheduled run does, in `sched_queue`.
    run: uidm_domain::grab::RunAction,
    /// One-time downloading, or periodic synchronization of what earlier
    /// runs downloaded.
    sync: uidm_domain::queue::QueueKind,
    sched_queue: u64,
    when_done: uidm_domain::queue::WhenDone,
    /// Shutting down when done closes programs that would hold it up; only
    /// kept with shutting down, as the Queues dialog does.
    force_close: bool,
    /// A file to open after a scheduled run; empty for none.
    open_when_done: String,
    /// The file the Browse button picked for `open_when_done`.
    open_picked: Picked,
    /// What earlier runs found and the last run's day and start time, kept
    /// as they are.
    kept: (Vec<String>, Option<i64>, Option<i64>),
    /// The project as it was last saved or opened, to tell whether leaving
    /// it loses changes; `None` until it is saved.
    saved: Option<uidm_domain::grab::Project>,
    /// The name its copy in the projects folder has, so saving over that
    /// copy does not count as taking another project's name.
    saved_as: Option<String>,
    /// What waits on the save question, and whether the question shows.
    leaving: Option<Leave>,
    asking: bool,
    /// The question's "Don't ask if the name is unique" box, and why saving
    /// from it failed.
    dont_ask: bool,
    ask_error: Option<String>,
    /// A project file picked anywhere with Open > From another folder.
    file_picked: Picked,
    /// The list and its ticks while Update all explores the pages again
    /// with their scripts run; what that finds is added to it.
    updating: Option<(Vec<GrabFile>, Vec<bool>)>,
    /// The project being saved on a worker, and what waits on it.
    saving: Pending<Result<uidm_domain::grab::Project, String>>,
    after_save: AfterSave,
    /// A project being read on a worker, with its file when it came from
    /// another folder.
    opening: Pending<Result<(uidm_domain::grab::Project, Option<PathBuf>), String>>,
    /// A template or the filters being saved, and what the note says after.
    noting: Pending<Note>,
    /// The project's new password being kept before its crawl starts.
    keeping_login: Pending<Result<(), String>>,
}

/// What a project saved on a worker goes on to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AfterSave {
    /// Stay on the project (its Save button).
    Stay,
    /// Do what waited on the save question.
    GoOn,
}

/// What a save done on a worker leaves under the project strip.
enum Note {
    /// The note as it is.
    Keep,
    /// This note, or none.
    Set(Option<String>),
}

/// What closing or leaving a project goes on to do once the save question
/// is answered.
enum Leave {
    /// Close the grabber; `add` adds the ticked files first, for the Add
    /// button or when the grabber adds them on closing.
    Close { add: bool },
    /// Start the crawl the form describes.
    Start,
    /// Show another project, opened or new.
    Replace(Box<GrabDialog>),
}

/// Patterns typed one per line or separated by spaces.
fn pattern_list(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

const MB: f64 = 1024.0 * 1024.0;

impl GrabDialog {
    /// A new project, from the saved `template`, offering `filters`.
    pub fn new(template: &GrabRequest, filters: Vec<uidm_domain::grab::Filter>) -> Self {
        Self::from_request(template, filters)
    }

    /// The project these fields and the crawl's results make, or why the
    /// schedule is not valid.
    fn project(&self) -> Result<uidm_domain::grab::Project, String> {
        let (files, checked) = match &self.phase {
            GrabPhase::Review { files, checked, .. } => (files.clone(), checked.clone()),
            _ => (Vec::new(), Vec::new()),
        };
        let schedule = self.sched.then(|| uidm_domain::queue::Schedule {
            start: self.sched_start.trim().to_string(),
            stop: self.sched_has_stop.then(|| self.sched_stop.trim().to_string()),
            days: if self.sched_by_date {
                Vec::new()
            } else {
                (0u8..7).filter(|d| self.sched_days[*d as usize]).collect()
            },
            date: self.sched_by_date.then(|| self.sched_date.trim().to_string()),
            repeat_minutes: super::queues::repeat_value(
                self.sched_repeat,
                self.sched_repeat_hours,
                self.sched_repeat_minutes,
            ),
            wake: self.sched_wake,
        });
        if let Some(s) = &schedule {
            s.validate()?;
        }
        Ok(uidm_domain::grab::Project {
            name: self.project.trim().to_string(),
            request: self.request(),
            files,
            checked,
            schedule,
            run: self.run,
            kind: self.sync,
            download_new: false,
            queue: self.sched_queue,
            when_done: self.when_done,
            force_close: self.force_close && self.when_done == uidm_domain::queue::WhenDone::ShutDown,
            open_when_done: Some(self.open_when_done.trim().to_string()).filter(|f| !f.is_empty()),
            seen: self.kept.0.clone(),
            last_scheduled_day: self.kept.1,
            last_scheduled_start: self.kept.2,
        })
    }

    /// The form and results of a saved project.
    pub fn open(p: uidm_domain::grab::Project, filters: Vec<uidm_domain::grab::Filter>) -> Self {
        let mut d = Self::from_request(&p.request, filters);
        d.project = p.name.clone();
        if let Some(s) = &p.schedule {
            d.sched = true;
            d.sched_start = s.start.clone();
            d.sched_has_stop = s.stop.is_some();
            d.sched_stop = s.stop.clone().unwrap_or_else(|| "06:00".into());
            d.sched_by_date = s.date.is_some();
            d.sched_date = s.date.clone().unwrap_or_default();
            (d.sched_repeat, d.sched_repeat_hours, d.sched_repeat_minutes) =
                super::queues::repeat_split(s.repeat_minutes);
            d.sched_wake = s.wake;
            d.sched_days = [false; 7];
            for day in &s.days {
                if let Some(slot) = d.sched_days.get_mut(*day as usize) {
                    *slot = true;
                }
            }
        }
        d.run = p.run;
        d.sync = p.kind;
        d.sched_queue = p.queue;
        d.when_done = p.when_done;
        d.force_close = p.force_close;
        d.open_when_done = p.open_when_done.clone().unwrap_or_default();
        d.kept = (p.seen.clone(), p.last_scheduled_day, p.last_scheduled_start);
        d.saved_as = Some(p.name.clone());
        if !p.files.is_empty() {
            let mut checked = p.checked;
            checked.resize(p.files.len(), true);
            d.phase = GrabPhase::Review {
                files: p.files,
                checked,
                pages: 0,
                explored: 0,
                stopped: Some(tr("Opened from the saved project. New crawl explores the site again.").into()),
            };
        }
        // Compared with what the fields make of it, so opening it is not
        // a change by itself.
        d.saved = d.project().ok();
        d
    }

    /// A project read from a file outside the projects folder. Saving puts
    /// it in the folder with the others.
    fn open_file(
        p: uidm_domain::grab::Project,
        path: &std::path::Path,
        filters: Vec<uidm_domain::grab::Filter>,
    ) -> Self {
        let mut d = Self::open(p, filters);
        d.saved_as = None;
        // Not with the others yet, so closing it asks to save it there.
        d.saved = None;
        d.project_note = Some(trf!("Opened {file}. Save keeps it with your projects.", file = path.display()));
        d
    }

    /// Show `next` instead, once the save question for this project is
    /// answered.
    pub fn switch_to(&mut self, next: GrabDialog) {
        self.leave(Leave::Replace(Box::new(next)));
    }

    fn leave(&mut self, then: Leave) {
        self.leaving = Some(then);
        self.asking = false;
        self.ask_error = None;
    }

    /// Whether leaving the project loses changes; a schedule that is not
    /// valid counts, so the question can say why it cannot be saved.
    fn unsaved(&self) -> bool {
        match self.project() {
            Ok(p) => uidm_domain::grab::unsaved(&p, self.saved.as_ref()),
            Err(_) => true,
        }
    }

    /// Save the project on a worker, with a new password first; `after`
    /// says what follows once it is saved. Fails at once when the project
    /// is not valid.
    fn save_project(&mut self, ctx: &egui::Context, after: AfterSave) -> Result<(), String> {
        let Some(lib) = crate::ports::ports().grab.as_deref() else {
            return Err(tr("There is no projects folder.").to_string());
        };
        let p = self.project()?;
        let login = self.login_to_keep();
        self.after_save = after;
        self.saving.start(ctx, move || {
            if let Some((target, user, password)) = login {
                keep_password(&target, &user, &password)?;
            }
            lib.save(&p).map_err(|e| trf!("Could not save it: {error}", error = e))?;
            Ok(p)
        });
        Ok(())
    }

    /// Note that `p` was saved.
    fn saved_project(&mut self, p: uidm_domain::grab::Project) {
        self.login_kept();
        self.project_note = Some(trf!("Saved \"{name}\".", name = p.name));
        self.saved_as = Some(p.name.clone());
        self.saved = Some(p);
    }

    /// IDM's Update all: explore the site again with page scripts run, as
    /// pages explored without them may hide links, keeping the list and its
    /// ticks and adding what the new crawl finds.
    fn update_all(&mut self, ctx: &egui::Context, queues: &[Queue], actions: &mut Vec<Action>) {
        let GrabPhase::Review { files, checked, .. } = &self.phase else {
            return;
        };
        self.updating = Some((files.clone(), checked.clone()));
        self.render_scripts = true;
        self.start_crawl(ctx, queues, actions);
        if !self.running() && !self.keeping_login.busy() {
            // The form did not validate: back to the list as it was.
            if let Some((files, checked)) = self.updating.take() {
                self.phase = GrabPhase::Review { files, checked, pages: 0, explored: 0, stopped: self.error.clone() };
            }
        }
    }

    /// Start the crawl the form describes, keeping a new password first on
    /// a worker.
    fn start_crawl(&mut self, ctx: &egui::Context, queues: &[Queue], actions: &mut Vec<Action>) {
        let req = self.request();
        if let Err(e) = req.validate() {
            self.error = Some(e);
            return;
        }
        match self.login_to_keep() {
            Some((target, user, password)) => {
                self.error = None;
                self.keeping_login.start(ctx, move || keep_password(&target, &user, &password));
            }
            None => self.begin_crawl(req, queues, actions),
        }
    }

    /// Start the crawl for `req`, which is valid.
    fn begin_crawl(&mut self, req: GrabRequest, queues: &[Queue], actions: &mut Vec<Action>) {
        self.error = None;
        self.phase = GrabPhase::Running { pages: 0, bytes: 0, found: 0, explored: 0, added: Vec::new() };
        if let Some(q) = req.download_found.and_then(|id| queues.iter().find(|q| q.id == id)) {
            actions.push(Action::SaveQueue(Queue { max_concurrent: req.download_at_once as usize, ..q.clone() }));
        }
        actions.push(Action::Grab(req));
    }

    /// Add the ticked results to the list in the chosen queue. Returns
    /// false when none are ticked.
    fn add_checked(
        &self,
        queues: &[Queue],
        settings: &uidm_domain::settings::Settings,
        actions: &mut Vec<Action>,
    ) -> bool {
        let GrabPhase::Review { files, checked, .. } = &self.phase else {
            return false;
        };
        // The settings found files are saved with, and the login site's
        // cookies for the files on that site.
        let req = self.request();
        let login = self.crawl_login_cookies.as_deref().or(req.login_cookies.as_deref());
        let items: Vec<Download> = files
            .iter()
            .zip(checked.iter())
            .filter(|(_, c)| **c)
            .filter_map(|(f, _)| crate::ports::ports().found.download_for(&req, f, settings, login))
            .collect();
        if items.is_empty() {
            return false;
        }
        // The queue downloads as many at once as the grabber says.
        if let Some(q) = queues.iter().find(|q| q.id == self.queue && q.max_concurrent != req.download_at_once as usize)
        {
            actions.push(Action::SaveQueue(Queue { max_concurrent: req.download_at_once as usize, ..q.clone() }));
        }
        actions.push(Action::AddMany { items, queue: self.queue, start_queue: self.start_queue });
        true
    }

    /// The form filled from `d`.
    pub fn from_request(d: &GrabRequest, filters: Vec<uidm_domain::grab::Filter>) -> Self {
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
            ignore_popups: d.ignore_popups,
            explore_at_once: d.explore_at_once,
            download_at_once: d.download_at_once,
            link_text: d.link_text,
            skip_types: d.skip_types.join(", "),
            files_this_site: d.files_this_site,
            filters,
            filter_edit: None,
            save_choice: match d.save_to {
                SaveTo::Categories => SaveChoice::Categories,
                SaveTo::Category(_) => SaveChoice::Category,
                SaveTo::Folder(_) => SaveChoice::Folder,
            },
            save_category: match &d.save_to {
                SaveTo::Category(name) => name.clone(),
                _ => String::new(),
            },
            save_folder: match &d.save_to {
                SaveTo::Folder(folder) => folder.clone(),
                _ => String::new(),
            },
            save_picked: Picked::default(),
            download_found: d.download_found.is_some(),
            whole_site: d.whole_site,
            other_levels_on: d.other_depth.is_some(),
            other_levels: d.other_depth.unwrap_or(1),
            login_on: d.login_user.is_some(),
            login_user: d.login_user.clone().unwrap_or_default(),
            login_password: String::new(),
            login_changed: false,
            login_page: d.login_url.clone().unwrap_or_default(),
            login_cookies: d.login_cookies.clone().unwrap_or_default(),
            crawl_login_cookies: None,
            subfolders: d.subfolders,
            overwrite: d.overwrite || d.mirror.as_ref().is_some_and(|m| m.overwrite),
            template_name: None,
            view: ResultsView::List,
            mirror: d.mirror.is_some(),
            mirror_folder: d.mirror.as_ref().map(|m| m.folder.clone()).unwrap_or_default(),
            mirror_pages: d.mirror.as_ref().is_some_and(|m| m.pages),
            mirror_picked: Picked::default(),
            queue: d.download_found.unwrap_or(MAIN_QUEUE),
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
            sched_repeat: false,
            sched_repeat_hours: 2,
            sched_repeat_minutes: 0,
            sched_wake: true,
            run: uidm_domain::grab::RunAction::Explore,
            sync: uidm_domain::queue::QueueKind::OneTime,
            sched_queue: MAIN_QUEUE,
            when_done: uidm_domain::queue::WhenDone::Nothing,
            force_close: false,
            open_when_done: String::new(),
            open_picked: Picked::default(),
            kept: (Vec::new(), None, None),
            saved: None,
            saved_as: None,
            leaving: None,
            asking: false,
            dont_ask: false,
            ask_error: None,
            updating: None,
            file_picked: Picked::default(),
            saving: Pending::default(),
            after_save: AfterSave::Stay,
            opening: Pending::default(),
            noting: Pending::default(),
            keeping_login: Pending::default(),
        }
    }

    pub fn request(&self) -> GrabRequest {
        GrabRequest {
            start: self.start.trim().to_string(),
            depth: self.depth,
            same_site: self.same_site,
            types: uidm_domain::settings::parse_extensions(&self.types),
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
            mirror: self.mirror.then(|| uidm_domain::grab::Mirror {
                folder: self.mirror_folder.trim().to_string(),
                pages: self.mirror_pages,
                overwrite: self.overwrite,
            }),
            ignore_popups: self.ignore_popups,
            explore_at_once: self.explore_at_once,
            download_at_once: self.download_at_once,
            link_text: self.link_text,
            skip_types: uidm_domain::settings::parse_extensions(&self.skip_types),
            files_this_site: self.files_this_site,
            save_to: match self.save_choice {
                SaveChoice::Categories => SaveTo::Categories,
                SaveChoice::Category => SaveTo::Category(self.save_category.trim().to_string()),
                SaveChoice::Folder => SaveTo::Folder(self.save_folder.trim().to_string()),
            },
            download_found: self.download_found.then_some(self.queue),
            whole_site: self.whole_site,
            other_depth: self.other_levels_on.then_some(self.other_levels),
            login_user: self.login_on.then(|| self.login_user.trim().to_string()).filter(|u| !u.is_empty()),
            login_url: Some(self.login_page.trim().to_string()).filter(|u| !u.is_empty()),
            login_cookies: uidm_domain::grab::clean_cookies(&self.login_cookies)
                .filter(|_| !self.login_page.trim().is_empty()),
            subfolders: self.subfolders,
            overwrite: self.overwrite,
        }
    }

    /// The new project password to keep in Credential Manager, for the
    /// start site and user name: the target, the user and the password.
    fn login_to_keep(&self) -> Option<(String, String, String)> {
        if !self.login_on || !self.login_changed {
            return None;
        }
        let target = uidm_domain::grab::project_login_target(&self.start, &self.login_user);
        Some((target, self.login_user.trim().to_string(), self.login_password.clone()))
    }

    /// The new password was kept: it is not shown or kept again.
    fn login_kept(&mut self) {
        if self.login_on {
            self.login_changed = false;
            self.login_password.clear();
        }
    }

    /// The daemon's report of the crawl.
    pub fn progress(&mut self, pages: u64, bytes: u64, found: usize, explored: u64, added: Vec<u64>) {
        if matches!(self.phase, GrabPhase::Running { .. }) {
            self.phase = GrabPhase::Running { pages, bytes, found, explored, added };
        }
    }

    pub fn done(
        &mut self,
        files: Vec<GrabFile>,
        pages: u64,
        explored: u64,
        stopped: Option<String>,
        login_cookies: Option<String>,
    ) {
        if matches!(self.phase, GrabPhase::Running { .. }) {
            self.crawl_login_cookies = login_cookies;
            let (files, checked) = match self.updating.take() {
                Some((old, ticks)) => merge_update(old, ticks, files),
                None => {
                    let checked = vec![true; files.len()];
                    (files, checked)
                }
            };
            self.phase = GrabPhase::Review { files, checked, pages, explored, stopped };
        }
    }

    /// The daemon refused the crawl.
    pub fn failed(&mut self, error: String) {
        if matches!(self.phase, GrabPhase::Running { .. }) {
            // A failed Update all goes back to the list it started from.
            self.phase = match self.updating.take() {
                Some((files, checked)) => {
                    GrabPhase::Review { files, checked, pages: 0, explored: 0, stopped: Some(error.clone()) }
                }
                None => GrabPhase::Form,
            };
            self.error = Some(error);
        }
    }

    pub fn running(&self) -> bool {
        matches!(self.phase, GrabPhase::Running { .. })
    }
}

/// Draw the site grabber. `settings` give the categories to save into
/// and their folders; `rows` the downloads in the list, for counting what
/// was downloaded and stopping the ticked files' downloads. Returns false once it is closed (a
/// running crawl is stopped first). Closing, starting a crawl or switching
/// projects asks to save an unsaved project first.
pub fn grab_dialog(
    ctx: &egui::Context,
    d: &mut GrabDialog,
    lists: &GrabLists,
    settings: &uidm_domain::settings::Settings,
    rows: &[DownloadEntry],
    connected: bool,
    actions: &mut Vec<Action>,
) -> bool {
    let queues = lists.queues;
    if let Some(r) = d.keeping_login.take() {
        match r {
            Ok(()) => {
                d.login_kept();
                let req = d.request();
                d.begin_crawl(req, queues, actions);
            }
            Err(e) => d.error = Some(e),
        }
    }
    if let Some(r) = d.opening.take() {
        match r {
            Ok((p, None)) => d.switch_to(GrabDialog::open(p, lists.filters.to_vec())),
            Ok((p, Some(path))) => d.switch_to(GrabDialog::open_file(p, &path, lists.filters.to_vec())),
            Err(e) => d.project_note = Some(e),
        }
    }
    if let Some(Note::Set(note)) = d.noting.take() {
        d.project_note = note;
    }
    if let Some(r) = d.saving.take() {
        let after = d.after_save;
        match r {
            Ok(p) => {
                d.saved_project(p);
                if after == AfterSave::GoOn {
                    return go_on(ctx, d, queues, settings, connected, actions);
                }
            }
            Err(e) if after == AfterSave::GoOn => {
                d.ask_error = Some(e);
                d.asking = true;
            }
            Err(e) => d.project_note = Some(e),
        }
    }
    take_picked(&d.mirror_picked, &mut d.mirror_folder);
    take_picked(&d.save_picked, &mut d.save_folder);
    take_picked(&d.open_picked, &mut d.open_when_done);
    let file = d.file_picked.lock().unwrap().take();
    if let Some(path) = file {
        match crate::ports::ports().grab.as_deref() {
            Some(lib) => d.opening.start(ctx, move || match lib.load_file(&path) {
                Ok(p) => Ok((p, Some(path))),
                Err(e) => Err(trf!("Could not open {name}: {error}", name = path.display(), error = e)),
            }),
            None => d.project_note = Some(tr("There is no projects folder.").to_string()),
        }
    }
    let mut open = true;
    let mut close = false;
    let mut added = false;
    let mut again = false;
    let mut update = false;
    let mut start = false;
    let mut replace = None;
    crate::dialog::Window::new(tr("Site grabber"))
        .id(egui::Id::new("Site grabber"))
        .dlu_width(409.0)
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .min_width(640.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            if !d.running() {
                replace = project_bar(ui, d, lists);
                ui.separator();
            }
            grab_body(
                ui,
                d,
                queues,
                settings,
                rows,
                connected,
                actions,
                &mut close,
                &mut added,
                &mut again,
                &mut update,
                &mut start,
            );
        });
    if update {
        d.update_all(ctx, queues, actions);
    }
    if let Some(next) = replace {
        d.switch_to(next);
    }
    if again {
        d.phase = GrabPhase::Form;
    }
    if !open || close || added {
        if d.running() {
            // Nothing found yet to keep; Start already offered to save.
            actions.push(Action::CancelGrab);
            return false;
        }
        // A crawl that downloaded its finds as it went has them in the list
        // already: adding them again on close would make copies.
        d.leave(Leave::Close { add: added || (settings.grab_add_on_close && !d.download_found) });
    }
    if start {
        d.leave(Leave::Start);
    }
    let dir = crate::ports::ports().grab.as_deref();
    if d.leaving.is_some() && !d.asking && !d.saving.busy() {
        if !d.unsaved() {
            return go_on(ctx, d, queues, settings, connected, actions);
        }
        let silent = dir.is_some()
            && settings.grab_save_unique_silently
            && uidm_domain::grab::project_name_is_unique(lists.projects, &d.project, d.saved_as.as_deref());
        if silent {
            if let Err(e) = d.save_project(ctx, AfterSave::GoOn) {
                d.ask_error = Some(e);
            }
        }
        if !d.saving.busy() {
            d.asking = true;
            d.dont_ask = settings.grab_save_unique_silently;
        }
    }
    if d.asking {
        match save_question(ctx, d, dir, lists.projects, settings, connected, actions) {
            Some(true) => return go_on(ctx, d, queues, settings, connected, actions),
            Some(false) => {
                d.leaving = None;
                d.asking = false;
            }
            None => {}
        }
    }
    true
}

/// Do what waited on the save question. Returns false when that closes the
/// grabber.
fn go_on(
    ctx: &egui::Context,
    d: &mut GrabDialog,
    queues: &[Queue],
    settings: &uidm_domain::settings::Settings,
    connected: bool,
    actions: &mut Vec<Action>,
) -> bool {
    d.asking = false;
    match d.leaving.take() {
        Some(Leave::Close { add }) => {
            if add && connected {
                d.add_checked(queues, settings, actions);
            }
            false
        }
        Some(Leave::Start) => {
            d.start_crawl(ctx, queues, actions);
            true
        }
        Some(Leave::Replace(next)) => {
            *d = *next;
            true
        }
        None => true,
    }
}

/// Ask to save the unsaved project before leaving it. Returns `Some(true)`
/// to go on (saved, or not wanted), `Some(false)` to stay, `None` while it
/// is open.
fn save_question(
    ctx: &egui::Context,
    d: &mut GrabDialog,
    dir: Option<&dyn GrabLibrary>,
    projects: &[String],
    settings: &uidm_domain::settings::Settings,
    connected: bool,
    actions: &mut Vec<Action>,
) -> Option<bool> {
    let mut answer = None;
    let mut open = true;
    crate::dialog::Window::new(tr("Save the project?"))
        .id(egui::Id::new("Save the grabber project"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(tr("This project has changes that are not saved. Save it first?"));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(tr("Project"));
                ui.add(
                    egui::TextEdit::singleline(&mut d.project).hint_text(tr("Name, to save it")).desired_width(200.0),
                );
            });
            let named = !d.project.trim().is_empty();
            if named && !uidm_domain::grab::project_name_is_unique(projects, &d.project, d.saved_as.as_deref()) {
                ui.weak(tr("Another saved project has this name; saving replaces it."));
            }
            if ui.checkbox(&mut d.dont_ask, tr("Don't ask if the name is unique")).changed() && connected {
                actions.push(Action::GrabOptions {
                    add_on_close: settings.grab_add_on_close,
                    save_unique_silently: d.dont_ask,
                });
            }
            if let Some(e) = &d.ask_error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let can_save = named && dir.is_some() && !d.saving.busy();
                if ui.add_enabled(can_save, egui::Button::new(tr("Save"))).clicked() {
                    // Saved on a worker; the grabber goes on once it is.
                    if let Err(e) = d.save_project(ui.ctx(), AfterSave::GoOn) {
                        d.ask_error = Some(e);
                    }
                }
                if ui.button(tr("Don't save")).clicked() {
                    answer = Some(true);
                }
                if ui.button(tr("Cancel")).clicked() {
                    answer = Some(false);
                }
            });
        });
    if !open {
        answer = Some(false);
    }
    answer
}

/// What the grabber picks from: the queues, and the saved projects,
/// templates, default template and filters as a worker last read them.
pub struct GrabLists<'a> {
    pub queues: &'a [Queue],
    pub projects: &'a [String],
    pub templates: &'a [uidm_domain::grab::Template],
    pub template: &'a GrabRequest,
    pub filters: &'a [uidm_domain::grab::Filter],
}

/// The project strip: its name, Save, Open, New and Save as template.
/// Returns the dialog to switch to when a new project is started; an opened
/// one is read on a worker first.
fn project_bar(ui: &mut egui::Ui, d: &mut GrabDialog, lists: &GrabLists) -> Option<GrabDialog> {
    let dir = crate::ports::ports().grab.as_deref();
    let mut next = None;
    ui.horizontal(|ui| {
        ui.label(tr("Project"));
        ui.add(egui::TextEdit::singleline(&mut d.project).hint_text(tr("Name, to save it")).desired_width(160.0));
        let named = !d.project.trim().is_empty();
        if ui.add_enabled(named && dir.is_some() && !d.saving.busy(), egui::Button::new(tr("Save"))).clicked() {
            if let Err(e) = d.save_project(ui.ctx(), AfterSave::Stay) {
                d.project_note = Some(e);
            }
        }
        let recent = lists.projects;
        ui.menu_button(tr("Open"), |ui| {
            for name in recent {
                if ui.button(name).clicked() {
                    if let Some(lib) = dir {
                        let name = name.clone();
                        d.opening.start(ui.ctx(), move || match lib.load(&name) {
                            Ok(p) => Ok((p, None)),
                            Err(e) => Err(trf!("Could not open {name}: {error}", name = name, error = e)),
                        });
                    }
                    ui.close_menu();
                }
            }
            if !recent.is_empty() {
                ui.separator();
            }
            if ui
                .button(tr("From another folder..."))
                .on_hover_text(tr("A project file kept anywhere, such as one copied from another computer"))
                .clicked()
            {
                // The picker blocks, so it gets its own thread.
                let (slot, ctx, start) = (Arc::clone(&d.file_picked), ui.ctx().clone(), dir.map(|l| l.folder()));
                thread::spawn(move || {
                    let kinds = [FileKind::new(tr("Grabber projects"), &["json"])];
                    let title = tr("Open a grabber project");
                    if let Some(file) = crate::ports::ports().pickers.pick_file(title, &kinds, start.as_deref()) {
                        *slot.lock().unwrap() = Some(file);
                        ctx.request_repaint();
                    }
                });
                ui.close_menu();
            }
        });
        ui.menu_button(tr("New"), |ui| {
            if ui
                .button(tr("From my last template"))
                .on_hover_text(tr("The settings saved with Save as template, or the defaults"))
                .clicked()
            {
                next = Some(GrabDialog::new(lists.template, lists.filters.to_vec()));
                ui.close_menu();
            }
            ui.separator();
            let templates = lists.templates;
            for t in templates {
                ui.horizontal(|ui| {
                    let label = uidm_domain::grab::BUILT_IN_TEMPLATES
                        .iter()
                        .find(|(n, _, _)| *n == t.name)
                        .map_or_else(|| t.name.clone(), |(n, _, _)| tr(n).to_string());
                    if ui.button(label).clicked() {
                        next = Some(GrabDialog::from_request(&t.request, lists.filters.to_vec()));
                        ui.close_menu();
                    }
                    if !t.built_in && ui.small_button("x").on_hover_text(tr("Delete this template")).clicked() {
                        if let Some(lib) = dir {
                            let name = t.name.clone();
                            d.noting.start(ui.ctx(), move || {
                                Note::Set(
                                    lib.delete_template(&name)
                                        .err()
                                        .map(|e| trf!("Could not delete the template: {error}", error = e)),
                                )
                            });
                        }
                    }
                });
            }
        })
        .response
        .on_hover_text(tr("A new project from a template"));
        match &mut d.template_name {
            None => {
                if ui
                    .add_enabled(dir.is_some(), egui::Button::new(tr("Save as template")))
                    .on_hover_text(tr("Keep these settings under a name, for new projects"))
                    .clicked()
                {
                    d.template_name = Some(String::new());
                }
            }
            Some(name) => {
                ui.add(egui::TextEdit::singleline(name).hint_text(tr("Template name")).desired_width(120.0));
                let ready = !name.trim().is_empty();
                if ui.add_enabled(ready, egui::Button::new(tr("Keep"))).clicked() {
                    if let Some(lib) = dir {
                        let name = name.trim().to_string();
                        let req = d.request();
                        // The unnamed template New starts from follows too.
                        d.noting.start(ui.ctx(), move || {
                            Note::Set(Some(match lib.save_template(&name, &req) {
                                Ok(()) => trf!("Saved the template \"{name}\".", name = name),
                                Err(e) => trf!("Could not save the template: {error}", error = e),
                            }))
                        });
                    }
                    d.template_name = None;
                }
                if ui.button(tr("Cancel")).clicked() {
                    d.template_name = None;
                }
            }
        }
    });
    if let Some(note) = &d.project_note {
        ui.weak(note.as_str());
    }
    next
}

#[allow(clippy::too_many_arguments)]
fn grab_body(
    ui: &mut egui::Ui,
    d: &mut GrabDialog,
    queues: &[Queue],
    settings: &uidm_domain::settings::Settings,
    rows: &[DownloadEntry],
    connected: bool,
    actions: &mut Vec<Action>,
    close: &mut bool,
    added: &mut bool,
    again: &mut bool,
    update: &mut bool,
    start: &mut bool,
) {
    match &mut d.phase {
        GrabPhase::Form => {
            ui.weak(tr(
                "Reads a site's pages and lists the files they link to. It follows robots.txt and waits between pages.",
            ));
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
                    ui.add_enabled(
                        !d.whole_site,
                        egui::DragValue::new(&mut d.depth).range(uidm_domain::grab::DEPTH_RANGE.0..=uidm_domain::grab::DEPTH_RANGE.1),
                    );
                    ui.weak(tr("0 reads only the start page"));
                    ui.checkbox(&mut d.whole_site, tr("Explore the whole site"))
                        .on_hover_text(tr("No limit on the start site; the page and MB budgets still stop the crawl"));
                });
                ui.end_row();
                ui.label("");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut d.same_site, tr("Stay on the start page's site"));
                    ui.add_enabled(d.same_site, egui::Checkbox::new(&mut d.subdomains, tr("and its subdomains")));
                });
                ui.end_row();
                ui.label("");
                ui.add_enabled_ui(!d.same_site, |ui| {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut d.other_levels_on, tr("On other sites, follow at most"));
                        ui.add_enabled(
                            d.other_levels_on,
                            egui::DragValue::new(&mut d.other_levels).range(uidm_domain::grab::DEPTH_RANGE.0..=uidm_domain::grab::DEPTH_RANGE.1),
                        );
                        ui.label(tr("links"));
                    });
                });
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut d.no_parent, tr("Do not go above the start page's folder"));
                ui.end_row();
                ui.label(tr("Sign-in"));
                ui.vertical(|ui| {
                    ui.checkbox(&mut d.browser_cookies, tr("Use my browser's sign-in for the start site"))
                        .on_hover_text(tr("Sign in on the site in your browser first. UIDM's extension hands over the site's cookies, which go to that site's pages only."));
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut d.login_on, tr("Log in to the start site as"));
                        ui.add_enabled_ui(d.login_on, |ui| {
                            // The password is kept per user name: another name needs its own.
                            if ui.add(egui::TextEdit::singleline(&mut d.login_user).hint_text(tr("User name")).desired_width(110.0)).changed() {
                                d.login_changed = true;
                            }
                            let hint = if d.login_changed { tr("Password") } else { tr("Password (unchanged)") };
                            let pw = ui.add(egui::TextEdit::singleline(&mut d.login_password).password(true).hint_text(hint).desired_width(120.0));
                            if pw.changed() {
                                d.login_changed = true;
                            }
                        });
                    })
                    .response
                    .on_hover_text(tr("The project's own login, sent when the start site asks for one. Windows Credential Manager keeps the password."));
                    ui.weak(tr("Otherwise a login set in Options > Connection is used when the site asks for one."));
                    ui.horizontal(|ui| {
                        ui.label(tr("Login page"));
                        ui.add(egui::TextEdit::singleline(&mut d.login_page).hint_text("https://example.com/login").desired_width(260.0));
                        let page = d.login_page.trim();
                        let web = page.starts_with("http://") || page.starts_with("https://");
                        if ui
                            .add_enabled(web, egui::Button::new(tr("Open")))
                            .on_hover_text(tr("Opens the page in your browser. Sign in there, then start the crawl."))
                            .clicked()
                        {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(page));
                        }
                    })
                    .response
                    .on_hover_text(tr("When the crawl starts, UIDM's extension hands over the login page's site cookies, which go to that site's pages only."));
                    ui.add_enabled_ui(!d.login_page.trim().is_empty(), |ui| {
                        ui.horizontal(|ui| {
                            ui.label(tr("Cookies"));
                            ui.add(
                                egui::TextEdit::singleline(&mut d.login_cookies)
                                    .password(true)
                                    .hint_text(tr("name=value; name2=value2"))
                                    .desired_width(260.0),
                            );
                        })
                        .response
                        .on_hover_text(tr("Only needed when the browser can't hand them over, such as without UIDM's extension. Saved projects keep them encrypted."));
                    });
                });
                ui.end_row();
                ui.label(tr("Keep files of type"));
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut d.types).hint_text(tr("pdf, zip, photo_*.jpg, or * for every type")).desired_width(220.0));
                        filter_menu(ui, d, false);
                    });
                    filter_editor(ui, d);
                });
                ui.end_row();
                ui.label(tr("But not"));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut d.skip_types).hint_text("html, js").desired_width(160.0));
                    filter_menu(ui, d, true);
                    ui.checkbox(&mut d.files_this_site, tr("Only files on this site"));
                });
                ui.end_row();
                ui.label(tr("Budget"));
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut d.max_pages).range(uidm_domain::grab::MAX_PAGES_RANGE.0..=uidm_domain::grab::MAX_PAGES_RANGE.1));
                    ui.label(tr("pages, at most"));
                    ui.add(egui::DragValue::new(&mut d.max_mb).range(uidm_domain::grab::MAX_MB_RANGE.0..=uidm_domain::grab::MAX_MB_RANGE.1));
                    ui.label(tr("MB of pages"));
                });
                ui.end_row();
                ui.label(tr("Wait between pages"));
                ui.horizontal(|ui| {
                    let (lo, hi) = uidm_domain::grab::DELAY_MS_RANGE;
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
                ui.add_enabled(d.render_scripts, egui::Checkbox::new(&mut d.ignore_popups, tr("Ignore the windows page scripts open")))
                    .on_hover_text(tr("Off, the pages a script opens in a new window are explored like links"));
            });
            egui::CollapsingHeader::new(tr("Grabber settings")).id_salt("grab_settings").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(tr("Explore"));
                    ui.add(
                        egui::DragValue::new(&mut d.explore_at_once)
                            .range(uidm_domain::grab::AT_ONCE_RANGE.0..=uidm_domain::grab::AT_ONCE_RANGE.1),
                    );
                    ui.label(tr("pages at once, and download"));
                    ui.add(
                        egui::DragValue::new(&mut d.download_at_once)
                            .range(uidm_domain::grab::AT_ONCE_RANGE.0..=uidm_domain::grab::AT_ONCE_RANGE.1),
                    );
                    ui.label(tr("files at once"));
                })
                .response
                .on_hover_text(tr("Files at once becomes the queue's own setting when the files are added to it"));
                ui.checkbox(&mut d.link_text, tr("Use link texts as descriptions"));
                ui.horizontal(|ui| {
                    ui.checkbox(&mut d.download_found, tr("Start downloading all matched files at once, in"));
                    ui.add_enabled_ui(d.download_found, |ui| queue_combo(ui, "grab_found_queue", &mut d.queue, queues));
                });
            });
            egui::CollapsingHeader::new(tr("Save to")).id_salt("grab_save_to").show(ui, |ui| {
                ui.add_enabled_ui(!d.mirror, |ui| {
                    ui.radio_value(&mut d.save_choice, SaveChoice::Categories, tr("Each file to the folder of its category"));
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut d.save_choice, SaveChoice::Category, tr("All files to the folder of this category:"));
                        if d.save_category.is_empty() {
                            d.save_category = settings.categories.first().map_or(uidm_domain::settings::GENERAL, |c| c.name.as_str()).to_string();
                        }
                        egui::ComboBox::from_id_salt("grab_save_category").selected_text(d.save_category.as_str()).show_ui(ui, |ui| {
                            for c in settings.categories.iter().map(|c| c.name.as_str()).chain([uidm_domain::settings::GENERAL]) {
                                ui.selectable_value(&mut d.save_category, c.to_string(), c);
                            }
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut d.save_choice, SaveChoice::Folder, tr("All files to this folder:"));
                        ui.add(egui::TextEdit::singleline(&mut d.save_folder).hint_text(tr("A full path")).desired_width(240.0));
                        browse_button(ui, tr("Save the files in"), &d.save_folder, &d.save_picked);
                    });
                })
                .response
                .on_disabled_hover_text(tr("Saving for offline browsing keeps the site's folders instead"));
                ui.add_enabled(!d.mirror, egui::Checkbox::new(&mut d.subfolders, tr("Use original relative subfolders")))
                    .on_hover_text(tr("Each file goes under folders that follow its address below the start page's folder"));
                ui.checkbox(&mut d.overwrite, tr("Overwrite existing files"))
                    .on_hover_text(tr("Off, a file already there is kept: the new one gets another name, or saving for offline browsing skips it"));
            });
            egui::CollapsingHeader::new(tr("Save for offline browsing")).id_salt("grab_mirror").show(ui, |ui| {
                ui.checkbox(
                    &mut d.mirror,
                    tr("Keep the site's folders: save each file under a folder that follows its address"),
                );
                ui.add_enabled_ui(d.mirror, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(tr("Under"));
                        ui.add(
                            egui::TextEdit::singleline(&mut d.mirror_folder)
                                .hint_text(tr("A full path"))
                                .desired_width(320.0),
                        );
                        browse_button(ui, tr("Keep the site in"), &d.mirror_folder, &d.mirror_picked);
                    });
                    ui.checkbox(
                        &mut d.mirror_pages,
                        tr("Save the pages too, with their links changed to the saved copies"),
                    );
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut d.overwrite, false, tr("Skip files already there"));
                        ui.radio_value(&mut d.overwrite, true, tr("Replace them"));
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
                    super::queues::repeat_fields(
                        ui,
                        &mut d.sched_repeat,
                        &mut d.sched_repeat_hours,
                        &mut d.sched_repeat_minutes,
                    );
                    super::queues::wake_field(ui, &mut d.sched_wake);
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut d.sched_by_date, false, tr("Every"));
                        ui.add_enabled_ui(!d.sched_by_date, |ui| {
                            for (i, name) in ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"].iter().enumerate() {
                                ui.checkbox(&mut d.sched_days[i], tr(name));
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
                    for run in uidm_domain::grab::RunAction::ALL {
                        ui.radio_value(&mut d.run, run, tr(run.label()));
                    }
                    ui.horizontal(|ui| {
                        ui.label(tr("Download in"));
                        ui.add_enabled_ui(d.run != uidm_domain::grab::RunAction::Explore, |ui| {
                            queue_combo(ui, "grab_sched_queue", &mut d.sched_queue, queues)
                        });
                    });
                    ui.add_enabled_ui(d.run != uidm_domain::grab::RunAction::Explore, |ui| {
                        ui.horizontal(|ui| {
                            for k in uidm_domain::queue::QueueKind::ALL {
                                ui.radio_value(&mut d.sync, k, tr(k.label()));
                            }
                        })
                    });
                    ui.weak(tr(if d.sync.is_sync() {
                        "Each run also checks the files earlier runs downloaded and downloads again those changed on the server."
                    } else {
                        "Exploring and downloading adds only files no earlier run found, then starts the queue."
                    }));
                    ui.horizontal(|ui| {
                        ui.label(tr("When done"));
                        egui::ComboBox::from_id_salt("grab_when_done").selected_text(tr(d.when_done.label())).show_ui(ui, |ui| {
                            for w in uidm_domain::queue::WhenDone::ALL {
                                ui.selectable_value(&mut d.when_done, w, tr(w.label()));
                            }
                        });
                        ui.label(tr("and open"));
                        ui.add(egui::TextEdit::singleline(&mut d.open_when_done).hint_text(tr("A file or program, or nothing")).desired_width(200.0));
                        crate::queues::open_file_button(ui, &d.open_when_done, &d.open_picked);
                    })
                    .response
                    .on_hover_text(tr("After the crawl, or once the queue has downloaded what the run added"));
                    ui.add_enabled(
                        d.when_done == uidm_domain::queue::WhenDone::ShutDown,
                        egui::Checkbox::new(&mut d.force_close, tr("Force apps to close")),
                    )
                    .on_hover_text(tr("Shut down even if a program asks to save its work first; unsaved work is lost"));
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
                    // Checked before the save question, which comes next.
                    match d.request().validate() {
                        Ok(()) => {
                            d.error = None;
                            *start = true;
                        }
                        Err(e) => d.error = Some(e),
                    }
                }
                if ui.button(tr("Cancel")).clicked() {
                    *close = true;
                }
            });
        }
        GrabPhase::Running { pages, bytes, found, explored, added } => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(trf!(
                    "Read {pages} of at most {max} pages ({size}); looked at {explored} file links and kept {found}.",
                    pages = pages,
                    max = d.max_pages,
                    size = human_bytes(*bytes),
                    explored = explored,
                    found = found
                ));
            });
            if d.download_found {
                ui.weak(tr("The files found are downloading in the queue as they turn up."));
                let downloaded = rows.iter().filter(|e| e.status == Status::Done && added.contains(&e.id)).count();
                ui.label(trf!(
                    "Downloaded {done} of {count} files added so far.",
                    done = downloaded,
                    count = added.len()
                ));
            }
            ui.add_space(8.0);
            if ui.button(tr("Stop")).clicked() {
                actions.push(Action::CancelGrab);
            }
        }
        GrabPhase::Review { files, checked, pages, explored, stopped } => {
            let done: HashSet<&str> =
                rows.iter().filter(|e| e.status == Status::Done).map(|e| e.url.as_str()).collect();
            let downloaded = files.iter().filter(|f| done.contains(f.url.as_str())).count();
            ui.label(trf!(
                "Pages processed: {pages}. File links looked at: {explored}. Matched: {matched}. Downloaded: {downloaded}.",
                pages = pages,
                explored = explored,
                matched = files.len(),
                downloaded = downloaded
            ));
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
                ui.separator();
                ui.label(tr("Show"));
                ui.selectable_value(&mut d.view, ResultsView::List, tr("A list"));
                ui.selectable_value(&mut d.view, ResultsView::ByPage, tr("By page"));
                ui.selectable_value(&mut d.view, ResultsView::ByFolder, tr("By folder"));
            });
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                let row = |ui: &mut egui::Ui, f: &GrabFile, c: &mut bool| {
                    ui.horizontal(|ui| {
                        ui.checkbox(c, "");
                        ui.add(egui::Label::new(&f.name).truncate()).on_hover_text(trf!(
                            "{url}\nfound on {page}",
                            url = f.url,
                            page = f.page
                        ));
                        if let Some(n) = f.size {
                            ui.weak(human_bytes(n));
                        }
                        if let Some(t) = &f.text {
                            ui.weak(t.as_str());
                        }
                        if done.contains(f.url.as_str()) {
                            ui.weak(tr("downloaded"));
                        }
                    });
                };
                match d.view {
                    ResultsView::List => {
                        for (f, c) in files.iter().zip(checked.iter_mut()) {
                            row(ui, f, c);
                        }
                    }
                    view => {
                        for node in &file_tree(files, view == ResultsView::ByPage) {
                            tree_ui(ui, node, files, checked, &row);
                        }
                    }
                }
            });
            let n = checked.iter().filter(|c| **c).count();
            let stoppable = stoppable_downloads(files, checked, rows);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(tr("Queue"));
                queue_combo(ui, "grab_queue", &mut d.queue, queues);
                ui.checkbox(&mut d.start_queue, tr("Start the queue now"));
            });
            let mut on_close = settings.grab_add_on_close;
            if ui
                .add_enabled(
                    connected,
                    egui::Checkbox::new(&mut on_close, tr("Add the ticked files when the grabber closes")),
                )
                .on_hover_text(tr("Closing the grabber adds them to the list and queue, as Add does"))
                .changed()
            {
                actions.push(Action::GrabOptions {
                    add_on_close: on_close,
                    save_unique_silently: settings.grab_save_unique_silently,
                });
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(connected && n > 0, egui::Button::new(trf!("Add {count}", count = n))).clicked() {
                    *added = true;
                }
                if ui
                    .add_enabled(connected && !stoppable.is_empty(), egui::Button::new(trf!("Stop {count}", count = stoppable.len())))
                    .on_hover_text(tr("Stop the ticked files' downloads that are running or waiting; Resume goes on from where they stopped"))
                    .clicked()
                {
                    actions.extend(stoppable.iter().map(|id| Action::Cancel(*id)));
                }
                if ui
                    .add_enabled(connected, egui::Button::new(tr("Update all")))
                    .on_hover_text(tr("Explore the pages again with their scripts run, and add the files that finds to this list"))
                    .clicked()
                {
                    *update = true;
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

/// The list after Update all: the files listed before, with their ticks,
/// then those the new crawl found that were not listed, ticked.
fn merge_update(mut files: Vec<GrabFile>, mut checked: Vec<bool>, found: Vec<GrabFile>) -> (Vec<GrabFile>, Vec<bool>) {
    let mut listed: HashSet<String> = files.iter().map(|f| f.url.clone()).collect();
    for f in found {
        if listed.insert(f.url.clone()) {
            files.push(f);
            checked.push(true);
        }
    }
    (files, checked)
}

/// The downloads in `rows` of the ticked `files` that are running or
/// waiting to run, which the review's Stop stops.
fn stoppable_downloads(files: &[GrabFile], checked: &[bool], rows: &[DownloadEntry]) -> Vec<u64> {
    let ticked: HashSet<&str> = files.iter().zip(checked).filter(|(_, c)| **c).map(|(f, _)| f.url.as_str()).collect();
    rows.iter()
        .filter(|e| matches!(e.status, Status::Queued | Status::Downloading | Status::Confirming | Status::Waiting))
        .filter(|e| ticked.contains(e.url.as_str()))
        .map(|e| e.id)
        .collect()
}

/// A branch of the results tree: a site, folder or page, the found files
/// right under it (their places in `files`) and its branches.
#[derive(Debug, Default, PartialEq)]
struct TreeNode {
    name: String,
    files: Vec<usize>,
    children: Vec<TreeNode>,
}

impl TreeNode {
    /// The places of every file under this branch, its branches' too.
    fn all_files(&self) -> Vec<usize> {
        let mut out = self.files.clone();
        for c in &self.children {
            out.extend(c.all_files());
        }
        out
    }
}

/// The found files as a tree of the pages that link to them, or of the
/// folders of their addresses: site, then each part of the path, in the
/// order first met. A branch with one branch and no files of its own is
/// joined to it, so `site/a/b/` shows as one line.
fn file_tree(files: &[GrabFile], by_page: bool) -> Vec<TreeNode> {
    let mut root = TreeNode::default();
    for (i, f) in files.iter().enumerate() {
        let key =
            if by_page { f.page.as_str() } else { f.url.rsplit_once('/').map_or(f.url.as_str(), |(folder, _)| folder) };
        let path = key.split_once("://").map_or(key, |(_, rest)| rest);
        let mut node = &mut root;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let at = match node.children.iter().position(|c| c.name == part) {
                Some(at) => at,
                None => {
                    node.children.push(TreeNode { name: part.to_string(), ..TreeNode::default() });
                    node.children.len() - 1
                }
            };
            node = &mut node.children[at];
        }
        node.files.push(i);
    }
    fn join(mut node: TreeNode) -> TreeNode {
        while node.files.is_empty() && node.children.len() == 1 {
            let child = node.children.pop().unwrap();
            node.name = format!("{}/{}", node.name, child.name);
            node.files = child.files;
            node.children = child.children;
        }
        node.children = node.children.into_iter().map(join).collect();
        node
    }
    // Files right under the root have no site: shown as a branch of their own.
    let mut top: Vec<TreeNode> = root.children.into_iter().map(join).collect();
    if !root.files.is_empty() {
        top.insert(0, TreeNode { name: "/".to_string(), files: root.files, children: Vec::new() });
    }
    top
}

/// Draw `node` of the results tree and its branches, each with how many of
/// its files are ticked and buttons to tick them all or none; `row` draws a
/// file.
fn tree_ui(
    ui: &mut egui::Ui,
    node: &TreeNode,
    files: &[GrabFile],
    checked: &mut [bool],
    row: &dyn Fn(&mut egui::Ui, &GrabFile, &mut bool),
) {
    let members = node.all_files();
    let ticked = members.iter().filter(|&&i| checked[i]).count();
    let title = trf!("{group} ({ticked} of {count} ticked)", group = node.name, ticked = ticked, count = members.len());
    let id = ui.id().with(("grab_tree", &node.name));
    egui::CollapsingHeader::new(title).id_salt(id).default_open(true).show(ui, |ui| {
        ui.horizontal(|ui| {
            if ui.small_button(tr("Tick all")).clicked() {
                members.iter().for_each(|&i| checked[i] = true);
            }
            if ui.small_button(tr("Tick none")).clicked() {
                members.iter().for_each(|&i| checked[i] = false);
            }
        });
        for child in &node.children {
            tree_ui(ui, child, files, checked, row);
        }
        for &i in &node.files {
            row(ui, &files[i], &mut checked[i]);
        }
    });
}

/// The named filters: picking one adds its masks to the types kept (or,
/// with `skip`, to those left out), as IDM's Add Filter adds to its
/// Download and Don't download lists; Add, Edit and Delete change the
/// list, which is kept for every project.
fn filter_menu(ui: &mut egui::Ui, d: &mut GrabDialog, skip: bool) {
    // Each menu needs its own id, as both carry the same label.
    ui.push_id(skip, |ui| filter_menu_inner(ui, d, skip));
}

fn filter_menu_inner(ui: &mut egui::Ui, d: &mut GrabDialog, skip: bool) {
    ui.menu_button(tr("Filters"), |ui| {
        let mut gone = None;
        for (i, f) in d.filters.iter().enumerate() {
            ui.horizontal(|ui| {
                if ui.button(&f.name).on_hover_text(&f.mask).clicked() {
                    let field = if skip { &mut d.skip_types } else { &mut d.types };
                    *field = uidm_domain::grab::add_masks(field, &f.types());
                    ui.close_menu();
                }
                if ui.small_button(tr("Edit")).clicked() {
                    d.filter_edit = Some((Some(i), f.name.clone(), f.mask.clone()));
                    ui.close_menu();
                }
                if ui.small_button(tr("Delete")).clicked() {
                    gone = Some(i);
                }
            });
        }
        if let Some(i) = gone {
            d.filters.remove(i);
            save_filters(ui.ctx(), d);
        }
        ui.separator();
        if ui.button(tr("Add filter...")).clicked() {
            d.filter_edit = Some((None, String::new(), String::new()));
            ui.close_menu();
        }
    });
}

/// The fields of the filter being added or changed.
fn filter_editor(ui: &mut egui::Ui, d: &mut GrabDialog) {
    let Some((at, name, mask)) = &mut d.filter_edit else {
        return;
    };
    let mut finish = None;
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(name).hint_text(tr("Filter name")).desired_width(110.0));
        ui.add(egui::TextEdit::singleline(mask).hint_text("*.jpg;*.png").desired_width(150.0));
        let ready = !name.trim().is_empty() && !mask.trim().is_empty();
        if ui.add_enabled(ready, egui::Button::new(tr("Keep"))).clicked() {
            finish = Some(Some((
                *at,
                uidm_domain::grab::Filter { name: name.trim().to_string(), mask: mask.trim().to_string() },
            )));
        }
        if ui.button(tr("Cancel")).clicked() {
            finish = Some(None);
        }
    });
    let finished = finish.is_some();
    match finish {
        Some(Some((Some(i), f))) if i < d.filters.len() => d.filters[i] = f,
        Some(Some((_, f))) => d.filters.push(f),
        _ => {}
    }
    if finished {
        d.filter_edit = None;
        save_filters(ui.ctx(), d);
    }
}

/// Save the filters on a worker.
fn save_filters(ctx: &egui::Context, d: &mut GrabDialog) {
    if let Some(lib) = crate::ports::ports().grab.as_deref() {
        let filters = d.filters.clone();
        d.noting.start(ctx, move || match lib.save_filters(&filters) {
            Ok(()) => Note::Keep,
            Err(e) => Note::Set(Some(trf!("Could not save the filters: {error}", error = e))),
        });
    }
}

/// Keep a project's new password in the credential store.
fn keep_password(target: &str, user: &str, password: &str) -> Result<(), String> {
    crate::ports::ports()
        .system
        .save_credential(target, user, password)
        .map_err(|e| trf!("Could not save the password: {error}", error = e))
}

/// Offer to download a copied link: Download or Ignore. Returns false once
/// answered.
pub fn clipboard_offer(ctx: &egui::Context, url: &str, connected: bool, actions: &mut Vec<Action>) -> bool {
    let mut answered = false;
    crate::dialog::Window::new(tr("Download the copied link?"))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn found(url: &str, page: &str) -> GrabFile {
        GrabFile {
            url: url.into(),
            name: url.rsplit('/').next().unwrap().into(),
            page: page.into(),
            size: None,
            text: None,
        }
    }

    #[test]
    fn results_tree_nests_folders_and_joins_lone_branches() {
        let files = [
            found("https://s/docs/a/one.pdf", "https://s/docs/"),
            found("https://s/docs/a/two.pdf", "https://s/docs/a/"),
            found("https://s/docs/b/three.pdf", "https://s/docs/a/"),
            found("https://s/top.zip", "https://s/"),
            found("https://other/x/y/z/four.zip", "https://s/docs/"),
        ];
        let tree = file_tree(&files, false);
        let names: Vec<&str> = tree.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["s", "other/x/y/z"], "a lone chain shows as one line");
        let s = &tree[0];
        assert_eq!(s.files, [3]);
        assert_eq!(s.children.len(), 1);
        let docs = &s.children[0];
        assert_eq!(docs.name, "docs");
        assert_eq!(
            docs.children.iter().map(|n| (n.name.as_str(), n.files.clone())).collect::<Vec<_>>(),
            [("a", vec![0, 1]), ("b", vec![2])]
        );
        assert_eq!(s.all_files(), [3, 0, 1, 2]);

        let by_page = file_tree(&files, true);
        assert_eq!(by_page.len(), 1);
        let site = &by_page[0];
        assert_eq!((site.name.as_str(), site.files.clone()), ("s", vec![3]));
        let docs = &site.children[0];
        assert_eq!((docs.name.as_str(), docs.files.clone()), ("docs", vec![0, 4]));
        assert_eq!((docs.children[0].name.as_str(), docs.children[0].files.clone()), ("a", vec![1, 2]));
    }

    fn row(id: u64, url: &str, status: &str) -> DownloadEntry {
        serde_json::from_value(serde_json::json!({
            "id": id, "url": url, "path": null, "total": null, "done": 0,
            "status": status, "error": null, "resumable": true, "connections": 0,
        }))
        .unwrap()
    }

    #[test]
    fn stop_takes_the_ticked_files_running_or_waiting_downloads() {
        let files = [
            found("https://s/a.pdf", "https://s/"),
            found("https://s/b.pdf", "https://s/"),
            found("https://s/c.pdf", "https://s/"),
        ];
        let checked = [true, true, false];
        let rows = [
            row(1, "https://s/a.pdf", "downloading"),
            row(2, "https://s/b.pdf", "done"),
            row(3, "https://s/b.pdf", "waiting"),
            row(4, "https://s/c.pdf", "queued"),
            row(5, "https://other/a.pdf", "queued"),
            row(6, "https://s/a.pdf", "canceled"),
        ];
        assert_eq!(stoppable_downloads(&files, &checked, &rows), [1, 3], "only ticked files, only running or waiting");
    }

    #[test]
    fn update_all_keeps_the_list_and_adds_new_finds() {
        let file =
            |url: &str| GrabFile { url: url.into(), name: url.into(), page: String::new(), size: None, text: None };
        let (files, checked) = merge_update(vec![file("a"), file("b")], vec![true, false], vec![file("b"), file("c")]);
        assert_eq!(files.iter().map(|f| f.url.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(checked, [true, false, true], "old ticks kept, new finds ticked");
    }

    #[test]
    fn the_form_keeps_the_login_page_subfolders_and_overwrite() {
        let req = GrabRequest {
            start: "https://s/".into(),
            login_url: Some("https://s/login".into()),
            login_cookies: Some("a=1; b=2".into()),
            subfolders: true,
            overwrite: true,
            ..GrabRequest::default()
        };
        assert_eq!(GrabDialog::from_request(&req, uidm_domain::grab::default_filters()).request(), req);
        // An older project's mirror choice to replace files carries over.
        let old = GrabRequest {
            mirror: Some(uidm_domain::grab::Mirror { folder: "/m".into(), pages: false, overwrite: true }),
            ..GrabRequest::default()
        };
        let back = GrabDialog::from_request(&old, uidm_domain::grab::default_filters()).request();
        assert!(back.overwrite && back.mirror.is_some_and(|m| m.overwrite));
        // Cookies without a login page are not kept.
        let mut d = GrabDialog::from_request(&req, uidm_domain::grab::default_filters());
        d.login_page.clear();
        assert_eq!((d.request().login_url, d.request().login_cookies), (None, None));
    }
}
