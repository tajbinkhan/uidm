//! The link picker: the links (and images) the browser found on a page, or
//! in a selection on it, listed with a box to tick each one. Filters hide web
//! pages, images, repeated links, or keep only some file types. A link's
//! address can be edited. Ticked links are added waiting in a queue, with the
//! page as their referrer and the browser's cookies for their site, to each
//! one's category, one chosen category or one folder, and optionally renamed
//! by a pattern.

use std::collections::HashSet;

use eframe::egui;
use uidm::protocol::{Download, PageLink};
use uidm::queue::{Queue, MAIN_QUEUE};
use uidm::settings::{Settings, GENERAL};

use super::{browse_button, take_picked, Action, Picked};
use crate::i18n::{tr, trf};

/// Extensions of links that lead to another web page rather than a file.
const PAGE_TYPES: [&str; 9] = ["", "htm", "html", "php", "asp", "aspx", "jsp", "shtml", "cgi"];

/// Where the ticked files are saved.
#[derive(Clone, PartialEq)]
enum Target {
    /// Each in the folder of its own category.
    ByType,
    /// All in one category, listed under it and saved to its folder.
    Category(String),
    /// All in `folder`.
    Folder,
}

pub struct LinksDialog {
    page: String,
    user_agent: Option<String>,
    links: Vec<PageLink>,
    checked: Vec<bool>,
    hide_pages: bool,
    hide_images: bool,
    hide_repeated: bool,
    /// Only these types, comma-separated; empty shows every type.
    types: String,
    text_as_description: bool,
    queue: u64,
    start_queue: bool,
    target: Target,
    folder: String,
    picked: Picked,
    /// Name the files by this pattern, `*` standing for their number in
    /// order; empty keeps their own names.
    rename: String,
    /// The link whose address is being edited, and the new address.
    editing: Option<(usize, String)>,
    error: Option<String>,
}

/// A link's file type: its extension in lower case, or "" for none.
fn link_type(l: &PageLink) -> String {
    let path = url::Url::parse(&l.url).map(|u| u.path().to_string()).unwrap_or_default();
    let name = path.rsplit('/').next().unwrap_or("");
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// The name of file `n` (from 1) of `count` renamed by `pattern`: `*` (or
/// the end of the name, without one) becomes `n`, padded with zeros to the
/// width of `count`. A pattern without its own extension keeps `ext`.
fn pattern_name(pattern: &str, n: usize, count: usize, ext: &str) -> String {
    let width = count.max(1).to_string().len();
    let number = format!("{n:0width$}");
    let pattern = pattern.trim();
    let mut name = if pattern.contains('*') {
        pattern.replacen('*', &number, 1).replace('*', "")
    } else {
        format!("{pattern}{number}")
    };
    // `photo_*.jpg` names its extension; `photo_*` keeps each file's.
    let after_star = pattern.rsplit('*').next().unwrap_or("");
    if !ext.is_empty() && !after_star.contains('.') {
        name = format!("{name}.{ext}");
    }
    name
}

fn is_page(l: &PageLink) -> bool {
    !l.image && PAGE_TYPES.contains(&link_type(l).as_str())
}

/// The name to show for a link: the last part of its path, or the link.
fn link_name(l: &PageLink) -> String {
    let path = url::Url::parse(&l.url).map(|u| u.path().to_string()).unwrap_or_default();
    let tail = path.rsplit('/').next().unwrap_or("");
    let tail = percent_encoding::percent_decode_str(tail).decode_utf8_lossy();
    if tail.is_empty() {
        l.url.clone()
    } else {
        tail.into_owned()
    }
}

impl LinksDialog {
    pub fn new(page: String, links: Vec<PageLink>, user_agent: Option<String>) -> Self {
        // Files start ticked; links to other pages do not.
        let checked = links.iter().map(|l| !is_page(l)).collect();
        LinksDialog {
            page,
            user_agent,
            links,
            checked,
            hide_pages: true,
            hide_images: true,
            hide_repeated: true,
            types: String::new(),
            text_as_description: false,
            queue: MAIN_QUEUE,
            start_queue: false,
            target: Target::ByType,
            folder: String::new(),
            picked: Picked::default(),
            rename: String::new(),
            editing: None,
            error: None,
        }
    }

    /// Indexes of the links the filters leave in view.
    fn visible(&self) -> Vec<usize> {
        let types = uidm::settings::parse_extensions(&self.types);
        let mut seen = HashSet::new();
        (0..self.links.len())
            .filter(|&i| {
                let l = &self.links[i];
                if self.hide_repeated && !seen.insert(l.url.split('#').next().unwrap_or(&l.url).to_string()) {
                    return false;
                }
                if self.hide_images && l.image {
                    return false;
                }
                if self.hide_pages && is_page(l) {
                    return false;
                }
                types.is_empty() || types.contains(&link_type(l))
            })
            .collect()
    }

    /// The ticked links in view, in order.
    fn ticked(&self) -> Vec<usize> {
        self.visible().into_iter().filter(|&i| self.checked[i]).collect()
    }

    /// The ticked links in view, as downloads waiting to start, saved where
    /// the dialog says (`settings` giving a category's folder), and named
    /// by the rename pattern when there is one.
    fn downloads(&self, settings: &Settings) -> Result<Vec<Download>, String> {
        let folder = self.folder.trim().to_string();
        let (folder, category) = match &self.target {
            Target::ByType => (None, None),
            Target::Category(name) => (Some(settings.category_folder(name).display().to_string()), Some(name.clone())),
            Target::Folder if folder.is_empty() => return Err(tr("Choose the folder to save the files to.").into()),
            Target::Folder if !std::path::Path::new(&folder).is_absolute() => {
                return Err(trf!("The folder must be a full path: {folder}", folder = folder));
            }
            Target::Folder => (Some(folder), None),
        };
        if self.rename.trim().chars().any(|c| matches!(c, '\\' | '/' | ':' | '"' | '<' | '>' | '|' | '?')) {
            return Err(tr("A file name cannot contain \\ / : \" < > | or ?").into());
        }
        let ticked = self.ticked();
        Ok(ticked
            .iter()
            .enumerate()
            .map(|(n, &i)| {
                let l = &self.links[i];
                let text = l.text.trim();
                let filename = (!self.rename.trim().is_empty())
                    .then(|| pattern_name(&self.rename, n + 1, ticked.len(), &link_type(l)));
                Download {
                    url: l.url.clone(),
                    referrer: Some(self.page.clone()),
                    cookies: l.cookies.clone(),
                    user_agent: self.user_agent.clone(),
                    folder: folder.clone(),
                    category: category.clone(),
                    filename,
                    description: (self.text_as_description && !text.is_empty()).then(|| text.to_string()),
                    start: Some(false),
                    ..Download::default()
                }
            })
            .collect())
    }
}

/// Draw the picker. Returns false once it is closed.
pub fn dialog(
    ctx: &egui::Context,
    d: &mut LinksDialog,
    queues: &[Queue],
    settings: &Settings,
    connected: bool,
    actions: &mut Vec<Action>,
) -> bool {
    take_picked(&d.picked, &mut d.folder);
    let mut open = true;
    let mut close = false;
    egui::Window::new(tr("Download links from the page"))
        .id(egui::Id::new("links_picker"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .min_width(680.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.add(egui::Label::new(egui::RichText::new(&d.page).weak()).truncate());
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut d.hide_pages, tr("Hide web pages"));
                ui.checkbox(&mut d.hide_images, tr("Hide images on the page"));
                ui.checkbox(&mut d.hide_repeated, tr("Hide repeated links"));
                ui.label(tr("Types"));
                ui.add(egui::TextEdit::singleline(&mut d.types).hint_text(tr("all, or zip, pdf")).desired_width(120.0));
            });
            let visible = d.visible();
            ui.horizontal(|ui| {
                ui.label(trf!("{shown} of {total} links shown", shown = visible.len(), total = d.links.len()));
                if ui.small_button(tr("Check all")).clicked() {
                    visible.iter().for_each(|&i| d.checked[i] = true);
                }
                if ui.small_button(tr("Uncheck all")).clicked() {
                    visible.iter().for_each(|&i| d.checked[i] = false);
                }
            });
            let mut edited = None;
            egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                egui::Grid::new("links_grid").striped(true).num_columns(5).spacing([10.0, 4.0]).show(ui, |ui| {
                    for &i in &visible {
                        let l = &d.links[i];
                        ui.checkbox(&mut d.checked[i], "");
                        ui.add_sized([200.0, 18.0], egui::Label::new(link_name(l)).truncate())
                            .on_hover_text(if l.text.is_empty() { l.url.as_str() } else { l.text.as_str() });
                        let kind = match (l.image, link_type(l)) {
                            (true, _) => tr("image").to_string(),
                            (_, t) if PAGE_TYPES.contains(&t.as_str()) => tr("page").to_string(),
                            (_, t) => t,
                        };
                        ui.weak(kind);
                        match d.editing.as_mut().filter(|(at, _)| *at == i) {
                            Some((_, url)) => {
                                let field = ui.add_sized([320.0, 18.0], egui::TextEdit::singleline(url).font(egui::TextStyle::Monospace));
                                let entered = field.lost_focus() && ui.input(|k| k.key_pressed(egui::Key::Enter));
                                ui.horizontal(|ui| {
                                    if ui.small_button(tr("OK")).clicked() || entered {
                                        edited = Some(true);
                                    }
                                    if ui.small_button(tr("Cancel")).clicked() {
                                        edited = Some(false);
                                    }
                                });
                            }
                            None => {
                                ui.add_sized(
                                    [320.0, 18.0],
                                    egui::Label::new(egui::RichText::new(&l.url).monospace().small()).truncate(),
                                );
                                if ui.small_button(tr("Edit...")).on_hover_text(tr("Change this link's address")).clicked() {
                                    d.editing = Some((i, l.url.clone()));
                                }
                            }
                        }
                        ui.end_row();
                    }
                });
            });
            match edited {
                Some(true) => {
                    if let Some((i, url)) = d.editing.take() {
                        let url = url.trim().to_string();
                        if crate::is_download_url(&url) {
                            d.links[i].url = url;
                            d.error = None;
                        } else {
                            d.error = Some(trf!("{url} is not an http, https or ftp link", url = url));
                            d.editing = Some((i, url));
                        }
                    }
                }
                Some(false) => d.editing = None,
                None => {}
            }
            ui.add_space(6.0);
            egui::Grid::new("links_target").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label(tr("Queue"));
                ui.horizontal(|ui| {
                    super::tools::queue_combo(ui, "links_queue", &mut d.queue, queues);
                    ui.checkbox(&mut d.start_queue, tr("Start the queue now"));
                });
                ui.end_row();
                ui.label(tr("Save to"));
                ui.vertical(|ui| {
                    ui.radio_value(&mut d.target, Target::ByType, tr("The folder of each file's category"));
                    ui.horizontal(|ui| {
                        let one = !matches!(d.target, Target::Category(_));
                        if ui.radio(!one, tr("All files to one category")).clicked() && one {
                            let first = settings.categories.first().map_or(GENERAL.to_string(), |c| c.name.clone());
                            d.target = Target::Category(first);
                        }
                        if let Target::Category(name) = &mut d.target {
                            egui::ComboBox::from_id_salt("links_category").selected_text(name.as_str()).show_ui(ui, |ui| {
                                for c in &settings.categories {
                                    ui.selectable_value(name, c.name.clone(), &c.name);
                                }
                                ui.selectable_value(name, GENERAL.to_string(), GENERAL);
                            });
                            ui.weak(settings.category_folder(name).display().to_string());
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut d.target, Target::Folder, tr("One folder"));
                        ui.add_enabled_ui(d.target == Target::Folder, |ui| {
                            ui.add(egui::TextEdit::singleline(&mut d.folder).desired_width(280.0));
                            browse_button(ui, tr("Save the files to"), &d.folder, &d.picked);
                        });
                    });
                });
                ui.end_row();
                ui.label(tr("Rename"));
                ui.vertical(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut d.rename)
                            .hint_text(tr("Their own names, or a pattern such as photo_*"))
                            .desired_width(280.0),
                    );
                    if !d.rename.trim().is_empty() {
                        let ticked = d.ticked();
                        let names: Vec<String> = ticked
                            .iter()
                            .take(3)
                            .enumerate()
                            .map(|(n, &i)| pattern_name(&d.rename, n + 1, ticked.len(), &link_type(&d.links[i])))
                            .collect();
                        let more = if ticked.len() > 3 { ", ..." } else { "" };
                        ui.weak(format!("{}{more}", names.join(", ")));
                    }
                });
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut d.text_as_description, tr("Use each link's text as its description"));
                ui.end_row();
            });
            if let Some(e) = &d.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            let n = visible.iter().filter(|&&i| d.checked[i]).count();
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(connected && n > 0, egui::Button::new(trf!("Add {count}", count = n))).clicked() {
                    match d.downloads(settings) {
                        Ok(items) => {
                            actions.push(Action::AddMany {
                                items,
                                queue: d.queue,
                                start_queue: d.start_queue,
                            });
                            close = true;
                        }
                        Err(e) => d.error = Some(e),
                    }
                }
                if ui.button(tr("Cancel")).clicked() {
                    close = true;
                }
            });
        });
    open && !close
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full path on this system.
    const DL: &str = if cfg!(windows) { r"D:\dl" } else { "/dl" };

    fn link(url: &str, image: bool) -> PageLink {
        PageLink {
            url: url.into(),
            text: "Get it".into(),
            image,
            cookies: Some("s=1".into()),
        }
    }

    #[test]
    fn filters_hide_pages_images_and_repeats() {
        let links = vec![
            link("https://x.net/a.zip", false),
            link("https://x.net/about.html", false),
            link("https://x.net/logo.png", true),
            link("https://x.net/a.zip#again", false),
            link("https://x.net/docs/", false),
            link("https://x.net/b.pdf", false),
        ];
        let mut d = LinksDialog::new("https://x.net/".into(), links, None);
        assert_eq!(d.visible(), [0, 5]);
        d.types = "pdf".into();
        assert_eq!(d.visible(), [5]);
        d.types.clear();
        d.hide_pages = false;
        d.hide_images = false;
        d.hide_repeated = false;
        assert_eq!(d.visible().len(), 6);
        assert!(!d.checked[1] && !d.checked[4], "pages start unticked");
    }

    #[test]
    fn ticked_links_carry_the_page_and_cookies() {
        let mut d = LinksDialog::new("https://x.net/list".into(), vec![link("https://x.net/a.zip", false)], Some("UA".into()));
        d.text_as_description = true;
        d.target = Target::Folder;
        d.folder = DL.into();
        let items = d.downloads(&Settings::default()).unwrap();
        assert_eq!(items.len(), 1);
        let a = &items[0];
        assert_eq!(a.referrer.as_deref(), Some("https://x.net/list"));
        assert_eq!(a.cookies.as_deref(), Some("s=1"));
        assert_eq!(a.description.as_deref(), Some("Get it"));
        assert_eq!(a.folder.as_deref(), Some(DL));
        assert_eq!(a.filename, None);
        assert_eq!(a.start, Some(false));
        d.folder = "dl".into();
        assert!(d.downloads(&Settings::default()).is_err(), "not a full path");
    }

    #[test]
    fn one_category_takes_every_file() {
        let links = vec![link("https://x.net/a.zip", false), link("https://x.net/b.pdf", false)];
        let mut d = LinksDialog::new("https://x.net/".into(), links, None);
        d.target = Target::Category("Music".into());
        let settings = Settings {
            download_dir: Some(DL.into()),
            ..Settings::default()
        };
        let items = d.downloads(&settings).unwrap();
        let music = std::path::Path::new(DL).join("Music").display().to_string();
        for item in &items {
            assert_eq!(item.category.as_deref(), Some("Music"));
            assert_eq!(item.folder.as_deref(), Some(music.as_str()));
        }
    }

    #[test]
    fn a_pattern_names_the_files_in_order() {
        assert_eq!(pattern_name("photo_*", 3, 12, "jpg"), "photo_03.jpg");
        assert_eq!(pattern_name("photo_*.png", 3, 9, "jpg"), "photo_3.png");
        assert_eq!(pattern_name("part", 2, 2, ""), "part2");

        let links = vec![
            link("https://x.net/a.jpg", false),
            link("https://x.net/b.jpg", false),
            link("https://x.net/c.gif", false),
        ];
        let mut d = LinksDialog::new("https://x.net/".into(), links, None);
        d.checked[1] = false;
        d.rename = "photo_*".into();
        let names: Vec<_> = d.downloads(&Settings::default()).unwrap().into_iter().map(|i| i.filename.unwrap()).collect();
        assert_eq!(names, ["photo_1.jpg", "photo_2.gif"]);
        d.rename = "a/b*".into();
        assert!(d.downloads(&Settings::default()).is_err());
    }
}
