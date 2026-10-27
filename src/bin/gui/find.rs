//! Finding downloads in the list. Ctrl+F opens a bar above the list, and
//! typing narrows the list to the downloads that match. F3 and Shift+F3 step
//! through them, and Escape closes the bar and shows the whole list again.

use eframe::egui;
use uidm::protocol::DownloadEntry;

use super::display_name;

/// What the text is looked for in.
#[derive(Clone, Copy, PartialEq)]
pub enum Scope {
    /// The file name and description.
    Name,
    /// The file name and description, the download link and the page it
    /// came from.
    NameAndAddress,
}

pub struct FindBar {
    pub text: String,
    pub scope: Scope,
    pub match_case: bool,
    /// The whole name (or link) must equal the text.
    pub whole: bool,
    /// The match F3 moved to.
    pub current: Option<u64>,
    /// Scroll the list to `current` on this frame.
    pub scroll: bool,
    /// Put the cursor in the text box on this frame.
    focus: bool,
}

impl FindBar {
    pub fn new() -> Self {
        FindBar {
            text: String::new(),
            scope: Scope::NameAndAddress,
            match_case: false,
            whole: false,
            current: None,
            scroll: false,
            focus: true,
        }
    }

    /// True when there is text to look for.
    pub fn active(&self) -> bool {
        !self.text.trim().is_empty()
    }

    pub fn matches(&self, e: &DownloadEntry) -> bool {
        let text = self.text.trim();
        if text.is_empty() {
            return true;
        }
        let hit = |field: &str| field_matches(field, text, self.match_case, self.whole);
        if hit(&display_name(e)) || e.description.as_deref().is_some_and(hit) {
            return true;
        }
        self.scope == Scope::NameAndAddress && (hit(&e.url) || e.referrer.as_deref().is_some_and(hit))
    }

    /// Move to the match after the current one in `ids` (before it when
    /// `back`), wrapping around at either end.
    pub fn step(&mut self, ids: &[u64], back: bool) {
        if ids.is_empty() {
            self.current = None;
            return;
        }
        let at = self.current.and_then(|c| ids.iter().position(|id| *id == c));
        let next = match (at, back) {
            (None, false) => 0,
            (None, true) => ids.len() - 1,
            (Some(i), false) => (i + 1) % ids.len(),
            (Some(i), true) => (i + ids.len() - 1) % ids.len(),
        };
        self.current = Some(ids[next]);
        self.scroll = true;
    }
}

fn field_matches(field: &str, text: &str, match_case: bool, whole: bool) -> bool {
    let (field, text) = if match_case {
        (field.to_string(), text.to_string())
    } else {
        (field.to_lowercase(), text.to_lowercase())
    };
    if whole {
        field == text
    } else {
        field.contains(&text)
    }
}

/// Open the bar on Ctrl+F (or F3 while it is closed), and close it on
/// Escape.
pub fn shortcuts(ctx: &egui::Context, bar: &mut Option<FindBar>) {
    let (open, escape) = ctx.input_mut(|i| {
        let open = i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::F));
        (open, i.key_pressed(egui::Key::Escape))
    });
    if open {
        bar.get_or_insert_with(FindBar::new).focus = true;
    } else if escape {
        *bar = None;
    } else if bar.is_none() && ctx.input(|i| i.key_pressed(egui::Key::F3)) {
        *bar = Some(FindBar::new());
    }
}

/// Step through `ids`, the matches in list order, on F3 and Shift+F3. The
/// current match stays one of them.
pub fn step_keys(ctx: &egui::Context, bar: &mut FindBar, ids: &[u64]) {
    let (next, back) = ctx.input(|i| (i.key_pressed(egui::Key::F3), i.modifiers.shift));
    if bar.current.is_some_and(|c| !ids.contains(&c)) {
        bar.current = None;
    }
    if next && bar.active() {
        bar.step(ids, back);
    }
}

/// The bar itself. Returns false when it was closed.
pub fn show(ui: &mut egui::Ui, bar: &mut FindBar, ids: &[u64]) -> bool {
    let mut open = true;
    ui.horizontal(|ui| {
        ui.label("Find:");
        let input = ui.add(
            egui::TextEdit::singleline(&mut bar.text)
                .hint_text("File name or link")
                .desired_width(220.0),
        );
        if std::mem::take(&mut bar.focus) {
            input.request_focus();
        }
        // Enter in the box goes to the next match, as F3 does.
        if input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let back = ui.input(|i| i.modifiers.shift);
            bar.step(ids, back);
            input.request_focus();
        }
        if ui.add_enabled(!ids.is_empty(), egui::Button::new("Previous")).on_hover_text("Shift+F3").clicked() {
            bar.step(ids, true);
        }
        if ui.add_enabled(!ids.is_empty(), egui::Button::new("Next")).on_hover_text("F3").clicked() {
            bar.step(ids, false);
        }
        egui::ComboBox::from_id_salt("find_scope")
            .selected_text(scope_label(bar.scope))
            .show_ui(ui, |ui| {
                for s in [Scope::Name, Scope::NameAndAddress] {
                    ui.selectable_value(&mut bar.scope, s, scope_label(s));
                }
            });
        ui.checkbox(&mut bar.match_case, "Match case");
        ui.checkbox(&mut bar.whole, "Whole name only")
            .on_hover_text("The whole file name or link must be the text");
        if bar.active() {
            let at = bar.current.and_then(|c| ids.iter().position(|id| *id == c));
            match (ids.len(), at) {
                (0, _) => ui.colored_label(ui.visuals().warn_fg_color, "No matches"),
                (n, Some(i)) => ui.weak(format!("{} of {n}", i + 1)),
                (1, None) => ui.weak("1 match"),
                (n, None) => ui.weak(format!("{n} matches")),
            };
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Close").on_hover_text("Escape").clicked() {
                open = false;
            }
        });
    });
    open
}

fn scope_label(s: Scope) -> &'static str {
    match s {
        Scope::Name => "In names and descriptions",
        Scope::NameAndAddress => "In names, descriptions and links",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uidm::protocol::Status;

    fn entry(id: u64, url: &str, path: Option<&str>, referrer: Option<&str>) -> DownloadEntry {
        DownloadEntry {
            id,
            url: url.into(),
            path: path.map(str::to_string),
            total: None,
            done: 0,
            status: Status::Done,
            error: None,
            resumable: false,
            connections: 0,
            scan: None,
            parts: None,
            note: None,
            duplicate_of: None,
            referrer: referrer.map(str::to_string),
            last_try: None,
            description: None,
            login_user: None,
        }
    }

    fn bar(text: &str) -> FindBar {
        FindBar {
            text: text.into(),
            ..FindBar::new()
        }
    }

    #[test]
    fn matches_the_name_ignoring_case() {
        let e = entry(1, "https://cdn.example.com/a?id=9", Some(r"C:\dl\Report.PDF"), None);
        assert!(bar("report").matches(&e));
        assert!(!FindBar { match_case: true, ..bar("report") }.matches(&e));
        assert!(FindBar { match_case: true, ..bar("Report") }.matches(&e));
        assert!(bar("").matches(&e), "no text matches everything");
    }

    #[test]
    fn scope_decides_whether_links_count() {
        let e = entry(1, "https://cdn.example.com/f.zip", None, Some("https://blog.example.org/post"));
        assert!(bar("cdn.example").matches(&e));
        assert!(bar("blog.example").matches(&e), "the page it came from");
        let names = FindBar { scope: Scope::Name, ..bar("blog") };
        assert!(!names.matches(&e));
        assert!(FindBar { scope: Scope::Name, ..bar("f.zip") }.matches(&e));
    }

    #[test]
    fn whole_name_must_be_equal() {
        let e = entry(1, "https://x/setup.exe", None, None);
        assert!(!FindBar { whole: true, ..bar("setup") }.matches(&e));
        assert!(FindBar { whole: true, ..bar("SETUP.EXE") }.matches(&e));
    }

    #[test]
    fn steps_wrap_both_ways() {
        let mut b = bar("x");
        let ids = [4, 7, 9];
        b.step(&ids, false);
        assert_eq!(b.current, Some(4));
        b.step(&ids, false);
        b.step(&ids, false);
        b.step(&ids, false);
        assert_eq!(b.current, Some(4), "past the last goes back to the first");
        b.step(&ids, true);
        assert_eq!(b.current, Some(9));
        b.step(&[], false);
        assert_eq!(b.current, None);
    }
}
