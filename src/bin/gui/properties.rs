//! A download's Properties dialog: what it is and where it stands, and, while
//! it is stopped, its link, referrer, description and own login. A finished
//! download's file can be renamed or moved from here.

use std::path::Path;

use eframe::egui;
use uidm::protocol::{DownloadEntry, DownloadLogin, Properties, Status};

use super::{browse_button, display_name, human_bytes, take_picked, Action, Picked};

pub struct PropertiesDialog {
    pub id: u64,
    url: String,
    referrer: String,
    description: String,
    filename: String,
    folder: String,
    use_login: bool,
    username: String,
    /// Empty keeps the saved password.
    password: String,
    /// The values the dialog opened with, to send only what changed.
    original: [String; 6],
    had_login: bool,
    picked: Picked,
}

impl PropertiesDialog {
    pub fn new(e: &DownloadEntry) -> Self {
        let path = e.path.as_deref().map(Path::new);
        let filename = path.and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let folder = path.and_then(|p| p.parent()).map(|d| d.display().to_string()).unwrap_or_default();
        let url = e.url.clone();
        let referrer = e.referrer.clone().unwrap_or_default();
        let description = e.description.clone().unwrap_or_default();
        let username = e.login_user.clone().unwrap_or_default();
        PropertiesDialog {
            id: e.id,
            original: [
                url.clone(),
                referrer.clone(),
                description.clone(),
                filename.clone(),
                folder.clone(),
                username.clone(),
            ],
            url,
            referrer,
            description,
            filename,
            folder,
            use_login: e.login_user.is_some(),
            had_login: e.login_user.is_some(),
            username,
            password: String::new(),
            picked: Picked::default(),
        }
    }

    /// The fields that changed. `done` decides which ones can.
    fn changes(&self, done: bool) -> Properties {
        let changed = |now: &str, i: usize| (now.trim() != self.original[i].trim()).then(|| now.trim().to_string());
        let mut p = Properties {
            url: changed(&self.url, 0),
            referrer: changed(&self.referrer, 1),
            description: changed(&self.description, 2),
            ..Properties::default()
        };
        if done {
            p.filename = changed(&self.filename, 3);
            p.folder = changed(&self.folder, 4);
        } else if !self.use_login && self.had_login {
            p.login = Some(DownloadLogin::default());
        } else if self.use_login && (changed(&self.username, 5).is_some() || !self.password.is_empty() || !self.had_login) {
            p.login = Some(DownloadLogin {
                username: self.username.trim().to_string(),
                password: Some(self.password.clone()).filter(|p| !p.is_empty()),
            });
        }
        p
    }
}

fn is_empty(p: &Properties) -> bool {
    p.url.is_none()
        && p.referrer.is_none()
        && p.description.is_none()
        && p.login.is_none()
        && p.filename.is_none()
        && p.folder.is_none()
}

fn status_text(s: Status) -> &'static str {
    match s {
        Status::Queued => "Queued",
        Status::Downloading => "Downloading",
        Status::Done => "Finished",
        Status::Failed => "Failed",
        Status::Canceled => "Canceled",
        Status::Interrupted => "Interrupted",
        Status::Confirming => "Waiting for the save-as dialog",
        Status::Waiting => "Waiting in a queue",
    }
}

/// Draw the dialog for `entry` (gone from the list closes it). Returns false
/// once closed.
pub fn dialog(
    ctx: &egui::Context,
    d: &mut PropertiesDialog,
    entry: Option<&DownloadEntry>,
    connected: bool,
    actions: &mut Vec<Action>,
) -> bool {
    let Some(e) = entry else {
        return false;
    };
    take_picked(&d.picked, &mut d.folder);
    let done = e.status == Status::Done;
    let stopped = done || e.status.is_retryable();
    let mut open = true;
    let mut close = false;
    egui::Window::new(format!("Properties of {}", display_name(e)))
        .id(egui::Id::new(("properties", d.id)))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            egui::Grid::new("properties_info").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.weak("Status");
                ui.label(status_text(e.status));
                ui.end_row();
                ui.weak("Size");
                ui.label(match e.total {
                    Some(t) if done => human_bytes(t),
                    Some(t) => format!("{} of {}", human_bytes(e.done), human_bytes(t)),
                    None => human_bytes(e.done),
                });
                ui.end_row();
                ui.weak("Last try");
                ui.label(e.last_try.map(super::columns::local_date).unwrap_or_else(|| "Not started yet".into()));
                ui.end_row();
                if let Some(err) = &e.error {
                    ui.weak("Result");
                    ui.colored_label(ui.visuals().error_fg_color, err);
                    ui.end_row();
                }
            });
            ui.separator();
            if !stopped {
                ui.weak("Stop the download to change these.");
            }
            ui.add_enabled_ui(stopped, |ui| {
                egui::Grid::new("properties_edit").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("Link");
                    ui.add(egui::TextEdit::singleline(&mut d.url).desired_width(420.0));
                    ui.end_row();
                    ui.label("Referrer").on_hover_text("The page the download came from");
                    ui.add(
                        egui::TextEdit::singleline(&mut d.referrer)
                            .hint_text("None")
                            .desired_width(420.0),
                    );
                    ui.end_row();
                    ui.label("Description");
                    ui.add(
                        egui::TextEdit::multiline(&mut d.description)
                            .hint_text("A note for yourself")
                            .desired_rows(2)
                            .desired_width(420.0),
                    );
                    ui.end_row();
                    ui.label("File name");
                    ui.add_enabled(done, egui::TextEdit::singleline(&mut d.filename).desired_width(420.0))
                        .on_disabled_hover_text("A finished file can be renamed");
                    ui.end_row();
                    ui.label("Folder");
                    ui.horizontal(|ui| {
                        ui.add_enabled(done, egui::TextEdit::singleline(&mut d.folder).desired_width(340.0))
                            .on_disabled_hover_text("A finished file can be moved");
                        ui.add_enabled_ui(done, |ui| browse_button(ui, "Move the file to", &d.folder, &d.picked));
                    });
                    ui.end_row();
                });
                ui.add_space(4.0);
                ui.add_enabled_ui(!done, |ui| {
                    ui.checkbox(&mut d.use_login, "Log in to this download's site")
                        .on_disabled_hover_text("A finished download keeps no login");
                    if d.use_login {
                        ui.horizontal(|ui| {
                            ui.label("User name");
                            ui.add(egui::TextEdit::singleline(&mut d.username).desired_width(140.0));
                            ui.label("Password");
                            let hint = if d.had_login { "Unchanged" } else { "" };
                            ui.add(
                                egui::TextEdit::singleline(&mut d.password)
                                    .password(true)
                                    .hint_text(hint)
                                    .desired_width(140.0),
                            );
                        });
                    }
                });
            });
            ui.add_space(8.0);
            let changes = d.changes(done);
            let login_ok = !d.use_login || !d.username.trim().is_empty();
            ui.horizontal(|ui| {
                let can_save = connected && stopped && login_ok && !is_empty(&changes);
                if ui.add_enabled(can_save, egui::Button::new("Save")).clicked() {
                    actions.push(Action::SetProperties { id: d.id, changes });
                    close = true;
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
    open && !close
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(status: Status) -> DownloadEntry {
        DownloadEntry {
            id: 3,
            url: "https://x/a.zip".into(),
            path: Some(r"C:\dl\a.zip".into()),
            total: Some(10),
            done: 10,
            status,
            error: None,
            resumable: true,
            connections: 1,
            scan: None,
            parts: None,
            note: None,
            duplicate_of: None,
            referrer: None,
            last_try: None,
            description: None,
            login_user: Some("me".into()),
        }
    }

    #[test]
    fn only_changes_are_sent() {
        let mut d = PropertiesDialog::new(&entry(Status::Failed));
        assert!(is_empty(&d.changes(false)));
        d.description = "note".into();
        d.filename = "b.zip".into();
        let p = d.changes(false);
        assert_eq!(p.description.as_deref(), Some("note"));
        assert_eq!(p.filename, None, "only a finished file is renamed");
        assert!(p.login.is_none(), "the login is unchanged");
    }

    #[test]
    fn unticking_the_login_removes_it() {
        let mut d = PropertiesDialog::new(&entry(Status::Canceled));
        d.use_login = false;
        assert_eq!(d.changes(false).login, Some(DownloadLogin::default()));
        d.use_login = true;
        d.password = "new".into();
        let login = d.changes(false).login.unwrap();
        assert_eq!((login.username.as_str(), login.password.as_deref()), ("me", Some("new")));
    }

    #[test]
    fn a_finished_file_can_move() {
        let mut d = PropertiesDialog::new(&entry(Status::Done));
        d.folder = r"D:\keep".into();
        let p = d.changes(true);
        assert_eq!(p.folder.as_deref(), Some(r"D:\keep"));
        assert!(p.login.is_none());
    }
}
