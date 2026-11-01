//! The login prompt: a download failed because its site wants a user name
//! and password (HTTP 401, or an FTP server refusing anonymous logins). It
//! shows the site and the realm it named, takes a user name and password,
//! and retries the download with them, as its own login or, with "Save this
//! password", as a site login in Options. Other downloads from the same
//! site waiting for a login are answered with it too.

use eframe::egui;
use uidm::engine::LoginWanted;
use uidm::protocol::{DownloadLogin, Properties};
use uidm::settings::{Settings, SiteLogin};

use super::Action;
use crate::i18n::{tr, trf};

pub struct LoginPrompt {
    /// The downloads waiting for this site's login.
    ids: Vec<u64>,
    wanted: LoginWanted,
    /// Every one of them is on the site that asked, so the login can be
    /// their own. Otherwise (a redirect to another site) it must be saved.
    own_site: bool,
    username: String,
    password: String,
    save: bool,
    /// One of the downloads is on a plain http or FTP address, so a saved
    /// login is allowed on plain connections: that is where it was used.
    pub plain: bool,
}

impl LoginPrompt {
    pub fn new(ids: Vec<u64>, wanted: LoginWanted, own_site: bool, username: String) -> Self {
        LoginPrompt {
            ids,
            wanted,
            own_site,
            username,
            password: String::new(),
            save: !own_site,
            plain: false,
        }
    }

    /// What OK does: save the login to the site logins, or give it to each
    /// download, then retry them.
    fn answer(&self, settings: Option<&Settings>) -> Vec<Action> {
        let username = self.username.trim().to_string();
        let mut actions = Vec::new();
        let save = self.save || !self.own_site;
        if save {
            if let Some(s) = settings {
                let mut s = s.clone();
                let old = s.site_logins.iter_mut().find(|l| l.site.eq_ignore_ascii_case(&self.wanted.host));
                let login = SiteLogin {
                    site: self.wanted.host.clone(),
                    username: username.clone(),
                    password: Some(self.password.clone()),
                    plain: self.plain || old.as_ref().is_some_and(|l| l.plain),
                };
                match old {
                    Some(old) => *old = login,
                    None => s.site_logins.push(login),
                }
                actions.push(Action::QuickSettings(s));
            }
        }
        for &id in &self.ids {
            if !save {
                actions.push(Action::SetProperties {
                    id,
                    changes: Properties {
                        login: Some(DownloadLogin {
                            username: username.clone(),
                            password: Some(self.password.clone()),
                        }),
                        ..Properties::default()
                    },
                });
            }
            actions.push(Action::Retry(id));
        }
        actions
    }
}

/// Draw the prompt. Returns false once it is answered or closed.
pub fn window(ctx: &egui::Context, p: &mut LoginPrompt, settings: Option<&Settings>, connected: bool, actions: &mut Vec<Action>) -> bool {
    let mut open = true;
    let mut done = false;
    crate::dialog::Window::new(tr("Log in to the site"))
        .id(egui::Id::new("login_prompt"))
        .dlu_width(300.0)
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_max_width(420.0);
            let host = p.wanted.host.as_str();
            if p.wanted.refused {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    trf!("{host} refused the login. Check the user name and password.", host = host),
                );
            } else {
                ui.label(trf!("{host} asks for a user name and password.", host = host));
            }
            if p.ids.len() > 1 {
                ui.weak(trf!("{count} downloads from this site are waiting for it.", count = p.ids.len()));
            }
            ui.add_space(4.0);
            let mut entered = false;
            egui::Grid::new("login_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                if let Some(realm) = &p.wanted.realm {
                    ui.label(tr("Realm"));
                    ui.label(realm);
                    ui.end_row();
                }
                ui.label(tr("User name"));
                ui.add(egui::TextEdit::singleline(&mut p.username).desired_width(240.0));
                ui.end_row();
                ui.label(tr("Password"));
                let field = ui.add(egui::TextEdit::singleline(&mut p.password).password(true).desired_width(240.0));
                entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.end_row();
            });
            ui.add_enabled(p.own_site, egui::Checkbox::new(&mut p.save, tr("Save this password")))
                .on_hover_text(tr("Keep it with the site logins in Options, for every download from this site"))
                .on_disabled_hover_text(tr("The file is on another site than its link, so the login is kept with the site logins."));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ready = connected && !p.username.trim().is_empty();
                if (ui.add_enabled(ready, egui::Button::new(tr("OK"))).clicked() || (entered && ready)) && !done {
                    actions.extend(p.answer(settings));
                    done = true;
                }
                if ui.button(tr("Cancel")).clicked() {
                    done = true;
                }
            });
        });
    open && !done
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wanted(host: &str) -> LoginWanted {
        LoginWanted {
            host: host.into(),
            realm: Some("Members".into()),
            refused: false,
        }
    }

    #[test]
    fn ok_gives_each_download_the_login_and_retries() {
        let mut p = LoginPrompt::new(vec![3, 4], wanted("files.example.com"), true, "ann".into());
        p.password = "secret".into();
        let actions = p.answer(Some(&Settings::default()));
        assert_eq!(actions.len(), 4);
        match &actions[0] {
            Action::SetProperties { id: 3, changes } => {
                let login = changes.login.as_ref().unwrap();
                assert_eq!((login.username.as_str(), login.password.as_deref()), ("ann", Some("secret")));
            }
            _ => panic!("expected the download's own login"),
        }
        assert!(matches!(actions[1], Action::Retry(3)));
        assert!(matches!(actions[3], Action::Retry(4)));
    }

    #[test]
    fn a_login_saved_from_plain_http_may_go_there_again() {
        for plain in [false, true] {
            let mut p = LoginPrompt::new(vec![3], wanted("files.example.com"), false, "ann".into());
            p.password = "secret".into();
            p.plain = plain;
            match &p.answer(Some(&Settings::default()))[0] {
                Action::QuickSettings(s) => assert_eq!(s.site_logins[0].plain, plain),
                _ => panic!("expected the login saved"),
            }
        }
    }

    #[test]
    fn save_puts_it_in_the_site_logins() {
        let mut settings = Settings::default();
        settings.site_logins.push(SiteLogin {
            site: "FILES.example.com".into(),
            username: "old".into(),
            password: None,
            plain: false,
        });
        // Another site than the link's: saved whatever the box says.
        let mut p = LoginPrompt::new(vec![3], wanted("files.example.com"), false, "ann".into());
        p.password = "secret".into();
        let actions = p.answer(Some(&settings));
        match &actions[0] {
            Action::QuickSettings(s) => {
                assert_eq!(s.site_logins.len(), 1, "replaced, not added");
                assert_eq!(s.site_logins[0].username, "ann");
                assert_eq!(s.site_logins[0].password.as_deref(), Some("secret"));
            }
            _ => panic!("expected the settings"),
        }
        assert!(matches!(actions[1], Action::Retry(3)));
        assert_eq!(actions.len(), 2);
    }
}
