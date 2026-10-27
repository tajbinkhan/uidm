//! The link picker: the links (and images) the browser found on a page, or
//! in a selection on it, listed with a box to tick each one. Filters hide web
//! pages, images, repeated links, or keep only some file types. Ticked links
//! are added waiting in a queue, with the page as their referrer and the
//! browser's cookies for their site.

use std::collections::HashSet;

use eframe::egui;
use uidm::protocol::{Download, PageLink};
use uidm::queue::{Queue, MAIN_QUEUE};

use super::{browse_button, take_picked, Action, Picked};

/// Extensions of links that lead to another web page rather than a file.
const PAGE_TYPES: [&str; 9] = ["", "htm", "html", "php", "asp", "aspx", "jsp", "shtml", "cgi"];

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
    folder: String,
    picked: Picked,
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
            folder: String::new(),
            picked: Picked::default(),
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

    /// The ticked links in view, as downloads waiting to start.
    fn downloads(&self, folder: Option<String>) -> Vec<Download> {
        self.visible()
            .into_iter()
            .filter(|&i| self.checked[i])
            .map(|i| {
                let l = &self.links[i];
                let text = l.text.trim();
                Download {
                    url: l.url.clone(),
                    referrer: Some(self.page.clone()),
                    cookies: l.cookies.clone(),
                    user_agent: self.user_agent.clone(),
                    folder: folder.clone(),
                    description: (self.text_as_description && !text.is_empty()).then(|| text.to_string()),
                    start: Some(false),
                    ..Download::default()
                }
            })
            .collect()
    }
}

/// Draw the picker. Returns false once it is closed.
pub fn dialog(ctx: &egui::Context, d: &mut LinksDialog, queues: &[Queue], connected: bool, actions: &mut Vec<Action>) -> bool {
    take_picked(&d.picked, &mut d.folder);
    let mut open = true;
    let mut close = false;
    egui::Window::new("Download links from the page")
        .id(egui::Id::new("links_picker"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .min_width(680.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.add(egui::Label::new(egui::RichText::new(&d.page).weak()).truncate());
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut d.hide_pages, "Hide web pages");
                ui.checkbox(&mut d.hide_images, "Hide images on the page");
                ui.checkbox(&mut d.hide_repeated, "Hide repeated links");
                ui.label("Types");
                ui.add(egui::TextEdit::singleline(&mut d.types).hint_text("all, or zip, pdf").desired_width(120.0));
            });
            let visible = d.visible();
            ui.horizontal(|ui| {
                ui.label(format!("{} of {} links shown", visible.len(), d.links.len()));
                if ui.small_button("Check all").clicked() {
                    visible.iter().for_each(|&i| d.checked[i] = true);
                }
                if ui.small_button("Uncheck all").clicked() {
                    visible.iter().for_each(|&i| d.checked[i] = false);
                }
            });
            egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                egui::Grid::new("links_grid").striped(true).num_columns(4).spacing([10.0, 4.0]).show(ui, |ui| {
                    for &i in &visible {
                        let l = &d.links[i];
                        ui.checkbox(&mut d.checked[i], "");
                        ui.add_sized([220.0, 18.0], egui::Label::new(link_name(l)).truncate())
                            .on_hover_text(if l.text.is_empty() { l.url.as_str() } else { l.text.as_str() });
                        let kind = match (l.image, link_type(l)) {
                            (true, _) => "image".to_string(),
                            (_, t) if PAGE_TYPES.contains(&t.as_str()) => "page".to_string(),
                            (_, t) => t,
                        };
                        ui.weak(kind);
                        ui.add_sized([340.0, 18.0], egui::Label::new(egui::RichText::new(&l.url).monospace().small()).truncate());
                        ui.end_row();
                    }
                });
            });
            ui.add_space(6.0);
            egui::Grid::new("links_target").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Queue");
                ui.horizontal(|ui| {
                    super::tools::queue_combo(ui, "links_queue", &mut d.queue, queues);
                    ui.checkbox(&mut d.start_queue, "Start the queue now");
                });
                ui.end_row();
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
                ui.label("");
                ui.checkbox(&mut d.text_as_description, "Use each link's text as its description");
                ui.end_row();
            });
            if let Some(e) = &d.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            let n = visible.iter().filter(|&&i| d.checked[i]).count();
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(connected && n > 0, egui::Button::new(format!("Add {n}"))).clicked() {
                    let folder = d.folder.trim().to_string();
                    if !folder.is_empty() && !std::path::Path::new(&folder).is_absolute() {
                        d.error = Some(format!("The folder must be a full path: {folder}"));
                        return;
                    }
                    actions.push(Action::AddMany {
                        items: d.downloads((!folder.is_empty()).then_some(folder)),
                        queue: d.queue,
                        start_queue: d.start_queue,
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let items = d.downloads(Some(r"D:\dl".into()));
        assert_eq!(items.len(), 1);
        let a = &items[0];
        assert_eq!(a.referrer.as_deref(), Some("https://x.net/list"));
        assert_eq!(a.cookies.as_deref(), Some("s=1"));
        assert_eq!(a.description.as_deref(), Some("Get it"));
        assert_eq!(a.folder.as_deref(), Some(r"D:\dl"));
        assert_eq!(a.start, Some(false));
    }
}
