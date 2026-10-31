//! Questions about many downloads added at once (a batch, an import, the
//! link picker, the site grabber's finds): an address already in the list,
//! or a file name already taken in its folder, when Options says to ask.
//! One prompt asks them in turn, with Yes, Yes to all, No and No to all;
//! "to all" answers the rest of the same kind of question in this batch.

use std::path::Path;

use eframe::egui;
use uidm::protocol::{Download, DownloadEntry};
use uidm::settings::{Duplicate, FileExists, Settings};

use crate::i18n::{tr, trf};

/// What an item needs an answer about.
#[derive(Debug, Clone, PartialEq)]
pub enum Question {
    /// Its address is in the list already, as the download named here.
    /// Yes adds it again (with a new name if the old file is in the way);
    /// No leaves it out.
    InList(String),
    /// A file at this path already has its name. Yes overwrites it; No
    /// saves the new file with a numbered name.
    Exists(String),
}

impl Question {
    fn kind(&self) -> usize {
        match self {
            Question::InList(_) => 0,
            Question::Exists(_) => 1,
        }
    }
}

/// The questions `items` raise against the list in `rows` under
/// `settings`, in order: the item they are about, and the question.
pub fn questions(items: &[Download], rows: &[DownloadEntry], settings: &Settings) -> Vec<(usize, Question)> {
    let mut out = Vec::new();
    for (i, d) in items.iter().enumerate() {
        // Form posts and streams are never duplicates, as in the daemon.
        let plain = uidm::app::post_body(d).is_none() && d.media.is_none();
        let duplicate = d.duplicate.unwrap_or(settings.if_duplicate) == Duplicate::Ask;
        if let (true, true, Some(old)) = (plain, duplicate, rows.iter().rev().find(|e| uidm::app::same_address(&e.url, &d.url))) {
            out.push((i, Question::InList(crate::display_name(old))));
        }
        if d.file_exists.unwrap_or(settings.file_exists) == FileExists::Ask {
            let name = uidm::engine::Inspection::unprobed(&d.url, d.filename.as_deref()).filename;
            let folder = match d.folder.as_deref().map(Path::new).filter(|f| f.is_absolute()) {
                Some(f) => f.to_path_buf(),
                None => settings.folders_for(&d.url).for_name(&name),
            };
            let path = folder.join(&name);
            if path.is_file() {
                out.push((i, Question::Exists(path.display().to_string())));
            }
        }
    }
    out
}

/// Many downloads waiting for answers before they are added.
pub struct Prompt {
    items: Vec<Download>,
    pub queue: u64,
    pub start_queue: bool,
    questions: Vec<(usize, Question)>,
    /// The question on screen.
    at: usize,
    /// Items answered No about being in the list.
    left_out: Vec<bool>,
    /// "to all" answers so far, by kind of question.
    all: [Option<bool>; 2],
}

impl Prompt {
    /// The prompt for `items`, or `None` when they raise no question.
    pub fn new(items: Vec<Download>, queue: u64, start_queue: bool, rows: &[DownloadEntry], settings: &Settings) -> Result<Self, Vec<Download>> {
        let questions = questions(&items, rows, settings);
        if questions.is_empty() {
            return Err(items);
        }
        let left_out = vec![false; items.len()];
        Ok(Prompt {
            items,
            queue,
            start_queue,
            questions,
            at: 0,
            left_out,
            all: [None, None],
        })
    }

    /// The question on screen, skipping those answered already: a "to all"
    /// answer, or an item left out.
    fn current(&mut self) -> Option<&Question> {
        while let Some((i, q)) = self.questions.get(self.at).cloned() {
            if self.left_out[i] {
                self.at += 1;
                continue;
            }
            match self.all[q.kind()] {
                Some(yes) => {
                    self.apply(i, &q, yes);
                    self.at += 1;
                }
                None => break,
            }
        }
        self.questions.get(self.at).map(|(_, q)| q)
    }

    fn apply(&mut self, i: usize, q: &Question, yes: bool) {
        match q {
            Question::InList(_) if yes => self.items[i].duplicate = Some(Duplicate::NewName),
            Question::InList(_) => self.left_out[i] = true,
            Question::Exists(_) => {
                self.items[i].file_exists = Some(if yes { FileExists::Overwrite } else { FileExists::Rename });
            }
        }
    }

    /// Answer the question on screen, and with `to_all` the rest of its
    /// kind too.
    pub fn answer(&mut self, yes: bool, to_all: bool) {
        if self.current().is_none() {
            return;
        }
        let (i, q) = self.questions[self.at].clone();
        self.apply(i, &q, yes);
        if to_all {
            self.all[q.kind()] = Some(yes);
        }
        self.at += 1;
    }

    /// Whether every question has its answer.
    pub fn done(&mut self) -> bool {
        self.current().is_none()
    }

    /// The downloads to add, with their answers.
    pub fn into_items(self) -> Vec<Download> {
        self.items
            .into_iter()
            .zip(self.left_out)
            .filter(|(_, out)| !out)
            .map(|(d, _)| d)
            .collect()
    }
}

/// Draw the prompt. Returns `Some(true)` once every question is answered,
/// `Some(false)` when the batch is canceled, `None` while it waits.
pub fn window(ctx: &egui::Context, p: &mut Prompt) -> Option<bool> {
    let left = {
        let at = p.at;
        p.questions.len().saturating_sub(at)
    };
    let question = p.current()?.clone();
    let name = |path: &str| Path::new(path).file_name().map_or(path.to_string(), |n| n.to_string_lossy().into_owned());
    let mut result = None;
    crate::dialog::Window::new(tr("Adding downloads"))
        .id(egui::Id::new("batch_answers"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_max_width(460.0);
            match &question {
                Question::InList(old) => {
                    ui.label(trf!("This address is already in the list as {name}.", name = old));
                    ui.strong(tr("Add it again?"));
                    ui.weak(tr("Yes adds it, with a new name if the old file is in the way. No leaves it out."));
                }
                Question::Exists(path) => {
                    ui.label(trf!("{name} already exists in this folder.", name = name(path)));
                    ui.weak(path.as_str());
                    ui.strong(tr("Overwrite it?"));
                    ui.weak(tr("Yes overwrites the file. No saves the new one with a numbered name."));
                }
            }
            if left > 1 {
                ui.weak(trf!("{count} questions left in this batch.", count = left));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for (label, yes, to_all) in [
                    (tr("Yes"), true, false),
                    (tr("Yes to all"), true, true),
                    (tr("No"), false, false),
                    (tr("No to all"), false, true),
                ] {
                    if ui.button(label).clicked() {
                        p.answer(yes, to_all);
                    }
                }
                ui.add_space(12.0);
                if ui.button(tr("Cancel")).on_hover_text(tr("Add none of them")).clicked() {
                    result = Some(false);
                }
            });
        });
    if result.is_none() && p.done() {
        result = Some(true);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("uidm_answers_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(id: u64, url: &str) -> DownloadEntry {
        serde_json::from_value(serde_json::json!({
            "id": id, "url": url, "path": format!("{}{id}.zip", if cfg!(windows) { "C:\\dl\\" } else { "/dl/" }), "total": null, "done": 0,
            "status": "done", "error": null, "resumable": false, "connections": 0,
        }))
        .unwrap()
    }

    fn item(url: &str) -> Download {
        Download {
            url: url.into(),
            ..Download::default()
        }
    }

    #[test]
    fn only_settings_that_ask_raise_questions() {
        let dir = temp_dir("raise");
        std::fs::write(dir.join("b.zip"), b"old").unwrap();
        let mut s = Settings {
            download_dir: Some(dir.display().to_string()),
            ..Settings::default()
        };
        s.categories.clear();
        let rows = [entry(1, "https://x.net/a.zip")];
        let items = [item("https://x.net/a.zip#again"), item("https://x.net/b.zip"), item("https://x.net/c.zip")];
        // By default addresses ask and taken names get new ones.
        assert_eq!(questions(&items, &rows, &s), [(0, Question::InList("1.zip".into()))]);
        s.file_exists = FileExists::Ask;
        s.if_duplicate = Duplicate::NewName;
        assert_eq!(
            questions(&items, &rows, &s),
            [(1, Question::Exists(dir.join("b.zip").display().to_string()))]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn to_all_answers_the_rest_of_its_kind() {
        let dir = temp_dir("to_all");
        for n in ["a.zip", "b.zip", "c.zip"] {
            std::fs::write(dir.join(n), b"old").unwrap();
        }
        let mut s = Settings {
            download_dir: Some(dir.display().to_string()),
            file_exists: FileExists::Ask,
            ..Settings::default()
        };
        s.categories.clear();
        let rows = [entry(1, "https://x.net/a.zip"), entry(2, "https://x.net/b.zip")];
        let items = vec![item("https://x.net/a.zip"), item("https://x.net/b.zip"), item("https://x.net/c.zip")];
        let mut p = Prompt::new(items, 1, false, &rows, &s).ok().unwrap();
        // a: in the list (No to all leaves out b too); then c's name.
        assert!(matches!(p.current(), Some(Question::InList(_))));
        p.answer(false, true);
        assert!(matches!(p.current(), Some(Question::Exists(path)) if path.ends_with("c.zip")));
        p.answer(true, false);
        assert!(p.done());
        let items = p.into_items();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].url, "https://x.net/c.zip");
        assert_eq!(items[0].file_exists, Some(FileExists::Overwrite));

        // Nothing to ask: the items come straight back.
        let s = Settings::default();
        assert!(Prompt::new(vec![item("https://y.net/z.bin")], 1, false, &[], &s).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn yes_adds_a_duplicate_with_a_new_name() {
        let rows = [entry(1, "https://x.net/a.zip")];
        let mut p = Prompt::new(vec![item("https://x.net/a.zip")], 1, false, &rows, &Settings::default()).ok().unwrap();
        p.answer(true, false);
        assert!(p.done());
        assert_eq!(p.into_items()[0].duplicate, Some(Duplicate::NewName));
    }
}
