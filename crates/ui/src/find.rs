//! Finding downloads in the list. Ctrl+F opens a bar above the list, and
//! typing narrows the list to the downloads that match. F3 and Shift+F3 step
//! through them, and Escape closes the bar and shows the whole list again.
//! As in IDM, the file name, the description and the links are each a
//! choice of where to look.

use eframe::egui;
use uidm_domain::download::DownloadEntry;

use super::display_name;
use crate::i18n::{tr, trf};

pub struct FindBar {
    pub text: String,
    /// Look in the file name.
    pub in_name: bool,
    /// Look in the description.
    pub in_description: bool,
    /// Look in the links: the download's address, its referrer and the page
    /// it came from.
    pub in_links: bool,
    pub match_case: bool,
    /// The whole file name must equal the text; in a description or a link
    /// the text must be whole words (see `whole_words`).
    pub whole: bool,
    /// The match F3 moved to.
    pub current: Option<u64>,
    /// Scroll the list to `current` on this frame.
    pub scroll: bool,
    /// Put the cursor in the text box on this frame.
    pub focus: bool,
}

impl FindBar {
    pub fn new() -> Self {
        FindBar {
            text: String::new(),
            in_name: true,
            in_description: true,
            in_links: true,
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
        let (text, fold) = if self.match_case { (text.to_string(), false) } else { (text.to_lowercase(), true) };
        let field = |f: &str| if fold { f.to_lowercase() } else { f.to_string() };
        let words = |f: &str| {
            let f = field(f);
            if self.whole {
                whole_words(&f, &text)
            } else {
                f.contains(&text)
            }
        };
        if self.in_name {
            let name = field(&display_name(e));
            if if self.whole { name == text } else { name.contains(&text) } {
                return true;
            }
        }
        if self.in_description && e.description.as_deref().is_some_and(words) {
            return true;
        }
        self.in_links && [Some(&e.url), e.referrer.as_ref(), e.page.as_ref()].into_iter().flatten().any(|s| words(s))
    }

    /// The places to look, ticked: at least one stays ticked.
    fn places(&mut self) -> [(&mut bool, &'static str); 3] {
        [
            (&mut self.in_name, tr("File name")),
            (&mut self.in_description, tr("Description")),
            (&mut self.in_links, tr("Links (address/referrer/page)")),
        ]
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

/// Whether `text` is in `field` as whole words: each place it is found must
/// not run on into a letter or digit on either side. A link splits into
/// words at everything else (`.`, `/`, `-`, `?`, ...), so "example" is in
/// `https://www.example.com/x` (a whole host label), as are "www.example.com"
/// (the host) and "x" (a path segment), but not in `https://myexample.com`.
fn whole_words(field: &str, text: &str) -> bool {
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    // Only an end of the text that is itself part of a word needs a
    // boundary: ".com" may follow anything.
    let (open, close) = (word(text.chars().next()), word(text.chars().last()));
    !text.is_empty()
        && field.match_indices(text).any(|(at, _)| {
            (!open || !word(field[..at].chars().next_back()))
                && (!close || !word(field[at + text.len()..].chars().next()))
        })
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
        ui.label(tr("Find:"));
        let input = ui.add(
            egui::TextEdit::singleline(&mut bar.text)
                .hint_text(tr("Text to find"))
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
        if ui.add_enabled(!ids.is_empty(), egui::Button::new(tr("Previous"))).on_hover_text(tr("Shift+F3")).clicked() {
            bar.step(ids, true);
        }
        if ui.add_enabled(!ids.is_empty(), egui::Button::new(tr("Next"))).on_hover_text("F3").clicked() {
            bar.step(ids, false);
        }
        ui.label(tr("Look in:"));
        let ticked = [bar.in_name, bar.in_description, bar.in_links].iter().filter(|t| **t).count();
        for (on, label) in bar.places() {
            // The last place ticked stays ticked.
            let last = *on && ticked == 1;
            ui.add_enabled(!last, egui::Checkbox::new(on, label))
                .on_disabled_hover_text(tr("Find looks in one place at least"));
        }
        ui.separator();
        ui.checkbox(&mut bar.match_case, tr("Match case"));
        ui.checkbox(&mut bar.whole, tr("Whole name only")).on_hover_text(tr(
            "The whole file name must be the text. In descriptions and links, whole words: \"example\" finds www.example.com but not myexample.com.",
        ));
        if bar.active() {
            let at = bar.current.and_then(|c| ids.iter().position(|id| *id == c));
            match (ids.len(), at) {
                (0, _) => ui.colored_label(ui.visuals().warn_fg_color, tr("No matches")),
                (n, Some(i)) => ui.weak(trf!("{current} of {count}", current = i + 1, count = n)),
                (1, None) => ui.weak(tr("1 match")),
                (n, None) => ui.weak(trf!("{count} matches", count = n)),
            };
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(tr("Close")).on_hover_text(tr("Escape")).clicked() {
                open = false;
            }
        });
    });
    open
}

#[cfg(test)]
mod tests {
    use super::*;
    use uidm_domain::download::Status;

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
            page: None,
            last_try: None,
            description: None,
            category: None,
            login_user: None,
            speed_limit: None,
            limit_remember: true,
            partial: None,
            from_browser: false,
            on_done: None,
            validator: None,
        }
    }

    fn bar(text: &str) -> FindBar {
        FindBar { text: text.into(), ..FindBar::new() }
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
    fn each_place_is_a_choice_of_its_own() {
        let mut e = entry(1, "https://cdn.example.com/f.zip", None, Some("https://blog.example.org/post"));
        e.description = Some("Holiday photos".into());
        assert!(bar("cdn.example").matches(&e));
        assert!(bar("blog.example").matches(&e), "the referrer");
        let only = |name, description, links, text| FindBar {
            in_name: name,
            in_description: description,
            in_links: links,
            ..bar(text)
        };
        assert!(!only(true, true, false, "blog").matches(&e), "links not ticked");
        assert!(only(true, false, false, "f.zip").matches(&e));
        assert!(!only(true, false, false, "holiday").matches(&e), "the description not ticked");
        assert!(only(false, true, false, "holiday").matches(&e));
        assert!(!only(false, true, false, "f.zip").matches(&e));
        e.page = Some("https://shop.example.net/item".into());
        assert!(only(false, false, true, "shop.example").matches(&e), "the page it came from");
    }

    #[test]
    fn whole_name_must_be_equal() {
        let e = entry(1, "https://x/setup.exe", None, None);
        let names = |text| FindBar { whole: true, in_links: false, ..bar(text) };
        assert!(!names("setup").matches(&e));
        assert!(names("SETUP.EXE").matches(&e));
    }

    #[test]
    fn whole_name_finds_a_site_inside_a_link() {
        let e = entry(1, "https://www.example.com/x/report.pdf", None, None);
        let whole = |text| FindBar { whole: true, ..bar(text) };
        assert!(whole("example").matches(&e), "a whole host label");
        assert!(whole("www.example.com").matches(&e), "the host");
        assert!(whole("x").matches(&e), "a path segment");
        assert!(!whole("exam").matches(&e));
        let other = entry(2, "https://myexample.com/a.zip", None, None);
        assert!(!whole("example").matches(&other));
        assert!(bar("example").matches(&other), "without it, anywhere in the link");
        assert!(whole(".com").matches(&other), "an end that is not a word needs no boundary");
    }

    #[test]
    fn whole_words_are_bounded_by_anything_but_letters_and_digits() {
        assert!(whole_words("https://a.example.com/", "example"));
        assert!(whole_words("example", "example"));
        assert!(whole_words("my example photos", "example"));
        assert!(!whole_words("examples", "example"));
        assert!(!whole_words("2example", "example"));
        // A later place can be whole when the first is not.
        assert!(whole_words("myexample.org/example", "example"));
        assert!(!whole_words("abc", ""));
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
