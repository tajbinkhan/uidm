//! The categories tree at the window's left, laid out like the one UIDM's
//! window follows: All Downloads, Unfinished and Finished, each opening to
//! a branch per category, then the saved grabber projects and the queues.
//! Picking a node filters the list. The panel stays up while the daemon is
//! away, drawn from the last settings it sent.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use uidm::protocol::{DownloadEntry, Status};
use uidm::queue::Queue;
use uidm::settings::GENERAL;

use crate::i18n::tr;
use crate::Action;

const ROW_HEIGHT: f32 = 18.0;
const INDENT: f32 = 16.0;
/// The panel's width until it is dragged.
pub const DEFAULT_WIDTH: f32 = 150.0;
/// How often the saved grabber projects are read again.
const PROJECTS_EVERY: Duration = Duration::from_secs(5);

/// Downloads by how far they got.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Progress {
    #[default]
    All,
    Unfinished,
    Finished,
}

impl Progress {
    fn keeps(self, e: &DownloadEntry) -> bool {
        match self {
            Progress::All => true,
            Progress::Unfinished => e.status != Status::Done,
            Progress::Finished => e.status == Status::Done,
        }
    }
}

/// Which downloads the list shows.
#[derive(Clone, PartialEq)]
pub enum Filter {
    /// By progress, and by category when one is picked.
    Downloads { progress: Progress, category: Option<String> },
    /// A queue's downloads, in its order.
    Queue(u64),
}

impl Default for Filter {
    fn default() -> Self {
        Filter::Downloads { progress: Progress::All, category: None }
    }
}

impl Filter {
    /// Whether the list shows `e`, listed under `category`. A queue's view
    /// takes its rows from the queue's order instead.
    pub fn keeps(&self, e: &DownloadEntry, category: &str) -> bool {
        match self {
            Filter::Downloads { progress, category: c } => {
                progress.keeps(e) && c.as_deref().is_none_or(|c| c == category)
            }
            Filter::Queue(_) => true,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Branch {
    Progress(ProgressKey),
    Grabber,
    Queues,
}

/// `Progress` as a hash key.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum ProgressKey {
    All,
    Unfinished,
    Finished,
}

impl ProgressKey {
    fn progress(self) -> Progress {
        match self {
            ProgressKey::All => Progress::All,
            ProgressKey::Unfinished => Progress::Unfinished,
            ProgressKey::Finished => Progress::Finished,
        }
    }
}

/// The tree's state between frames.
pub struct Tree {
    open: HashSet<Branch>,
    projects: Vec<String>,
    projects_read: Option<Instant>,
}

impl Default for Tree {
    fn default() -> Self {
        Tree {
            open: [Branch::Progress(ProgressKey::All)].into(),
            projects: Vec::new(),
            projects_read: None,
        }
    }
}

/// Everything the tree needs for one frame.
pub struct Panel<'a> {
    /// The settings' categories, in their order.
    pub categories: Vec<String>,
    /// Each row's category, by index into `rows`.
    pub listed: &'a [String],
    pub queues: &'a [Queue],
    pub connected: bool,
}

/// What the tree asked for besides actions.
#[derive(Default)]
pub struct Outcome {
    /// The close box was clicked.
    pub hide: bool,
}

pub fn show(ui: &mut egui::Ui, panel: &Panel, tree: &mut Tree, filter: &mut Filter, actions: &mut Vec<Action>) -> Outcome {
    let mut outcome = Outcome::default();
    if tree.projects_read.is_none_or(|t| t.elapsed() >= PROJECTS_EVERY) {
        tree.projects = uidm::grab::projects_dir().map(|d| uidm::grab::recent_projects(&d)).unwrap_or_default();
        tree.projects_read = Some(Instant::now());
    }

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tr("Categories")).size(12.5));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let close = ui
                .add_enabled(panel.connected, egui::Button::new("×").frame(false))
                .on_hover_text(tr("Hide categories"));
            if close.clicked() {
                outcome.hide = true;
            }
        });
    });
    ui.separator();

    // General is the files no category takes; it is listed only when some
    // are there.
    let mut names = panel.categories.clone();
    if panel.listed.iter().any(|c| c == GENERAL) {
        names.push(GENERAL.to_string());
    }

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for key in [ProgressKey::All, ProgressKey::Unfinished, ProgressKey::Finished] {
            let progress = key.progress();
            let (text, icon) = match key {
                ProgressKey::All => (tr("All Downloads"), Icon::Folder),
                ProgressKey::Unfinished => (tr("Unfinished"), Icon::Unfinished),
                ProgressKey::Finished => (tr("Finished"), Icon::Finished),
            };
            let branch = Branch::Progress(key);
            let open = tree.open.contains(&branch);
            let here = Filter::Downloads { progress, category: None };
            let r = row(ui, 0, Some(open), icon, text, *filter == here);
            if r.toggle {
                toggle(&mut tree.open, branch);
            }
            if r.pick {
                *filter = here;
            }
            if open {
                for name in &names {
                    let here = Filter::Downloads { progress, category: Some(name.clone()) };
                    let r = row(ui, 1, None, Icon::Category(kind(name)), name, *filter == here);
                    if r.pick {
                        *filter = here;
                    }
                }
            }
        }

        let open = tree.open.contains(&Branch::Grabber);
        let expander = (!tree.projects.is_empty()).then_some(open);
        let r = row(ui, 0, expander, Icon::Grabber, tr("Grabber projects"), false);
        if r.toggle {
            toggle(&mut tree.open, Branch::Grabber);
        }
        if r.pick && tree.projects.is_empty() {
            actions.push(Action::OpenGrabber);
        }
        if open {
            for name in &tree.projects {
                let r = row(ui, 1, None, Icon::Project, name, false);
                if r.pick {
                    actions.push(Action::OpenGrabProject(name.clone()));
                }
            }
        }

        let open = tree.open.contains(&Branch::Queues);
        let r = row(ui, 0, Some(open), Icon::Folder, tr("Queues"), false);
        if r.toggle || r.pick {
            toggle(&mut tree.open, Branch::Queues);
        }
        r.response.context_menu(|ui| {
            if ui.add_enabled(panel.connected, egui::Button::new(tr("Scheduler..."))).clicked() {
                actions.push(Action::OpenQueues(None));
                ui.close_menu();
            }
        });
        if open {
            for q in panel.queues {
                let here = Filter::Queue(q.id);
                let r = row(ui, 1, None, Icon::Queue(q.running), &q.name, *filter == here);
                if r.pick {
                    *filter = here;
                }
                r.response.context_menu(|ui| {
                    ui.add_enabled_ui(panel.connected, |ui| {
                        if q.running {
                            if ui.button(tr("Stop")).clicked() {
                                actions.push(Action::StopQueue(q.id));
                                ui.close_menu();
                            }
                        } else if ui.button(tr("Start")).clicked() {
                            actions.push(Action::StartQueue(q.id));
                            ui.close_menu();
                        }
                        if ui.button(tr("Edit queue...")).clicked() {
                            actions.push(Action::OpenQueues(Some(q.id)));
                            ui.close_menu();
                        }
                    });
                });
            }
        }
    });
    outcome
}

fn toggle(open: &mut HashSet<Branch>, branch: Branch) {
    if !open.remove(&branch) {
        open.insert(branch);
    }
}

struct Row {
    response: egui::Response,
    /// The node was picked: a click on its name.
    pick: bool,
    /// The branch opens or closes: a click on its arrow, or a double-click.
    toggle: bool,
}

/// One node: an arrow when it has children (`expander` says if they are
/// shown), an icon and its name.
fn row(ui: &mut egui::Ui, depth: usize, expander: Option<bool>, icon: Icon, text: &str, selected: bool) -> Row {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), Sense::click());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    let mut x = rect.left() + 2.0 + depth as f32 * INDENT;
    let arrow = Rect::from_center_size(Pos2::new(x + 5.0, rect.center().y), Vec2::splat(10.0));
    x += 12.0;
    let icon_rect = Rect::from_min_size(Pos2::new(x, rect.center().y - 7.0), Vec2::splat(14.0));
    x += 18.0;
    let galley = painter.layout_no_wrap(text.to_string(), egui::FontId::proportional(12.5), visuals.text_color());
    let label = Rect::from_min_size(Pos2::new(x - 2.0, rect.top() + 1.0), Vec2::new(galley.size().x + 4.0, ROW_HEIGHT - 2.0));
    if selected {
        painter.rect_filled(label, 0.0, visuals.selection.bg_fill);
    } else if response.hovered() {
        painter.rect_filled(label, 0.0, visuals.widgets.hovered.weak_bg_fill);
    }
    let color = if selected { visuals.selection.stroke.color } else { visuals.text_color() };
    painter.galley_with_override_text_color(Pos2::new(x, rect.center().y - galley.size().y / 2.0), galley, color);

    let on_arrow = response.interact_pointer_pos().is_some_and(|p| p.x < arrow.right() + 2.0);
    if let Some(open) = expander {
        let c = arrow.center();
        let s = 3.5;
        let points = if open {
            vec![Pos2::new(c.x - s, c.y - s / 2.0), Pos2::new(c.x, c.y + s / 2.0), Pos2::new(c.x + s, c.y - s / 2.0)]
        } else {
            vec![Pos2::new(c.x - s / 2.0, c.y - s), Pos2::new(c.x + s / 2.0, c.y), Pos2::new(c.x - s / 2.0, c.y + s)]
        };
        painter.add(Shape::line(points, Stroke::new(1.2_f32, visuals.weak_text_color())));
    }
    draw_icon(&painter, icon, icon_rect);
    let toggle = expander.is_some() && ((response.clicked() && on_arrow) || response.double_clicked());
    Row {
        pick: response.clicked() && !(expander.is_some() && on_arrow),
        toggle,
        response,
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Compressed,
    Documents,
    Music,
    Programs,
    Video,
    Other,
}

/// The icon a category gets: the built-in ones by name, the rest a page.
fn kind(name: &str) -> Kind {
    match name.to_ascii_lowercase().as_str() {
        "compressed" => Kind::Compressed,
        "documents" => Kind::Documents,
        "music" => Kind::Music,
        "programs" => Kind::Programs,
        "video" => Kind::Video,
        _ => Kind::Other,
    }
}

#[derive(Clone, Copy)]
enum Icon {
    Folder,
    Unfinished,
    Finished,
    Category(Kind),
    Grabber,
    Project,
    /// A queue, and whether it is started.
    Queue(bool),
}

const FOLDER: Color32 = Color32::from_rgb(0xE6, 0xB4, 0x3C);

fn draw_icon(p: &egui::Painter, icon: Icon, r: Rect) {
    let w = r.width();
    let at = |x: f32, y: f32| Pos2::new(r.left() + x * w, r.top() + y * w);
    let thin = |c: Color32| Stroke::new(1.2_f32, c);
    match icon {
        Icon::Folder => folder(p, r),
        Icon::Unfinished => {
            folder(p, r);
            let c = Color32::from_rgb(0x3C, 0x8C, 0xE6);
            p.line_segment([at(0.62, 0.38), at(0.62, 0.86)], Stroke::new(1.6_f32, c));
            p.add(Shape::line(vec![at(0.44, 0.68), at(0.62, 0.88), at(0.8, 0.68)], Stroke::new(1.6_f32, c)));
        }
        Icon::Finished => {
            folder(p, r);
            let c = Color32::from_rgb(0x3C, 0xB4, 0x50);
            p.add(Shape::line(vec![at(0.4, 0.66), at(0.56, 0.82), at(0.9, 0.42)], Stroke::new(1.8_f32, c)));
        }
        Icon::Category(kind) => category(p, kind, r),
        Icon::Grabber => {
            folder(p, r);
            let c = Color32::from_rgb(0x3C, 0x8C, 0xE6);
            p.circle_stroke(at(0.66, 0.66), w * 0.2, thin(c));
            p.line_segment([at(0.46, 0.66), at(0.86, 0.66)], thin(c));
        }
        Icon::Project => {
            let c = Color32::from_rgb(0x3C, 0x8C, 0xE6);
            p.circle_stroke(at(0.5, 0.5), w * 0.38, thin(c));
            p.line_segment([at(0.12, 0.5), at(0.88, 0.5)], thin(c));
            p.line_segment([at(0.5, 0.12), at(0.5, 0.88)], thin(c));
        }
        Icon::Queue(running) => {
            let c = if running { Color32::from_rgb(0x3C, 0xB4, 0x50) } else { Color32::GRAY };
            for y in [0.25, 0.5, 0.75] {
                p.circle_filled(at(0.18, y), w * 0.07, c);
                p.line_segment([at(0.34, y), at(0.9, y)], Stroke::new(1.6_f32, c));
            }
        }
    }
}

/// A closed folder: a tab and a body.
fn folder(p: &egui::Painter, r: Rect) {
    let w = r.width();
    let at = |x: f32, y: f32| Pos2::new(r.left() + x * w, r.top() + y * w);
    p.rect_filled(Rect::from_min_max(at(0.05, 0.16), at(0.45, 0.32)), 1.0, FOLDER.gamma_multiply(0.8));
    p.rect_filled(Rect::from_min_max(at(0.05, 0.26), at(0.95, 0.86)), 1.5, FOLDER);
}

fn category(p: &egui::Painter, kind: Kind, r: Rect) {
    let w = r.width();
    let at = |x: f32, y: f32| Pos2::new(r.left() + x * w, r.top() + y * w);
    let page = Rect::from_min_max(at(0.18, 0.06), at(0.82, 0.94));
    match kind {
        Kind::Compressed => {
            let c = Color32::from_rgb(0xC8, 0x96, 0x3C);
            p.rect_filled(page, 1.0, c);
            for y in [0.2, 0.36, 0.52] {
                p.rect_filled(Rect::from_min_max(at(0.44, y), at(0.56, y + 0.08)), 0.0, Color32::from_gray(40));
            }
            p.rect_filled(Rect::from_min_max(at(0.4, 0.66), at(0.6, 0.82)), 1.0, Color32::from_gray(40));
        }
        Kind::Documents | Kind::Other => {
            p.rect_filled(page, 1.0, Color32::from_gray(235));
            let c = if matches!(kind, Kind::Documents) { Color32::from_rgb(0x3C, 0x6E, 0xC8) } else { Color32::GRAY };
            for y in [0.3, 0.48, 0.66] {
                p.line_segment([at(0.3, y), at(0.7, y)], Stroke::new(1.2_f32, c));
            }
        }
        Kind::Music => {
            let c = Color32::from_rgb(0x3C, 0x8C, 0xE6);
            p.circle_filled(at(0.32, 0.76), w * 0.14, c);
            p.circle_filled(at(0.74, 0.66), w * 0.14, c);
            let s = Stroke::new(1.6_f32, c);
            p.line_segment([at(0.44, 0.76), at(0.44, 0.16)], s);
            p.line_segment([at(0.86, 0.66), at(0.86, 0.08)], s);
            p.line_segment([at(0.44, 0.16), at(0.86, 0.08)], Stroke::new(2.4_f32, c));
        }
        Kind::Programs => {
            let c = Color32::from_rgb(0x50, 0x78, 0xA0);
            let window = Rect::from_min_max(at(0.06, 0.16), at(0.94, 0.84));
            p.rect_filled(window, 1.0, Color32::from_gray(235));
            p.rect_filled(Rect::from_min_max(window.min, Pos2::new(window.right(), at(0.0, 0.32).y)), 1.0, c);
        }
        Kind::Video => {
            let c = Color32::from_rgb(0x3C, 0x8C, 0xE6);
            let frame = Rect::from_min_max(at(0.06, 0.18), at(0.94, 0.82));
            p.rect_filled(frame, 1.0, c);
            let screen = frame.shrink2(Vec2::new(w * 0.14, w * 0.1));
            p.rect_filled(screen, 0.0, Color32::from_rgb(0xD2, 0xE6, 0xFA));
        }
    }
}
