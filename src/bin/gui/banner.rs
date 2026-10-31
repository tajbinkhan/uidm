//! Strips under the toolbar for what the window has to say: the daemon
//! connection when it is not up, the last error, notices, the download
//! quota, a newer release and a download waiting for a new link. Each is
//! shown only while it applies, and its text wraps so its buttons stay in
//! view. UIDM's window has no status bar; this is where its messages went.

use eframe::egui::{self, Color32};

use crate::i18n::{tr, trf};
use crate::Action;

/// The connection to the daemon, as the strip shows it.
pub enum Connection<'a> {
    Up,
    Connecting,
    /// Lost, and why.
    Lost(&'a str),
}

/// A newer release on offer.
pub struct Release<'a> {
    pub version: &'a str,
    pub url: &'a str,
    /// It has a setup UIDM can install.
    pub installable: bool,
}

/// Everything the strips need for one frame.
pub struct Notes<'a> {
    pub connection: Connection<'a>,
    /// The download whose next browser download gives it a new link.
    pub refreshing: Option<String>,
    pub error: Option<&'a str>,
    pub notice: Option<&'a str>,
    pub quota: Option<&'a str>,
    pub release: Option<Release<'a>>,
}

impl Notes<'_> {
    /// Whether any strip is shown.
    pub fn any(&self) -> bool {
        !matches!(self.connection, Connection::Up)
            || self.refreshing.is_some()
            || self.error.is_some()
            || self.notice.is_some()
            || self.quota.is_some()
            || self.release.is_some()
    }
}

/// Strips the user closed.
#[derive(Default)]
pub struct Dismissed {
    pub error: bool,
    pub notice: bool,
}

#[derive(Clone, Copy)]
enum Tone {
    Error,
    Warn,
    Info,
}

pub fn show(ui: &mut egui::Ui, notes: &Notes, connected: bool, actions: &mut Vec<Action>) -> Dismissed {
    let mut dismissed = Dismissed::default();
    ui.spacing_mut().item_spacing.y = 3.0;
    match notes.connection {
        Connection::Up => {}
        Connection::Connecting => strip(ui, Tone::Info, |ui| {
            ui.spinner();
            ui.label(tr("Connecting to the UIDM daemon..."));
        }),
        Connection::Lost(why) => strip(ui, Tone::Error, |ui| {
            ui.label(trf!("Disconnected from the UIDM daemon: {reason}", reason = why));
            if ui.button(tr("Retry")).clicked() {
                actions.push(Action::Reconnect);
            }
            if ui
                .button(tr("Restart daemon"))
                .on_hover_text(tr("Stop the daemon if it is running, then start it again. Downloads it was running resume."))
                .clicked()
            {
                actions.push(Action::RestartDaemon);
            }
        }),
    }
    if let Some(error) = notes.error {
        strip(ui, Tone::Warn, |ui| {
            ui.label(error);
            if close(ui) {
                dismissed.error = true;
            }
        });
    }
    if let Some(quota) = notes.quota {
        strip(ui, Tone::Warn, |ui| {
            ui.label(quota);
        });
    }
    if let Some(name) = &notes.refreshing {
        strip(ui, Tone::Info, |ui| {
            ui.label(trf!("Your next browser download gives {name} a new link", name = name));
            if ui.add_enabled(connected, egui::Button::new(tr("Stop waiting"))).clicked() {
                actions.push(Action::CancelRefresh);
            }
        });
    }
    if let Some(notice) = notes.notice {
        strip(ui, Tone::Info, |ui| {
            ui.label(notice);
            if close(ui) {
                dismissed.notice = true;
            }
        });
    }
    if let Some(r) = &notes.release {
        strip(ui, Tone::Info, |ui| {
            ui.hyperlink_to(trf!("UIDM {version} is available", version = r.version), r.url);
            // About checks again and offers Update now.
            if r.installable && ui.add_enabled(connected, egui::Button::new(tr("Update now"))).clicked() {
                actions.push(Action::CheckUpdate);
            }
        });
    }
    dismissed
}

/// One strip: a tinted band whose contents wrap.
fn strip(ui: &mut egui::Ui, tone: Tone, add: impl FnOnce(&mut egui::Ui)) {
    let visuals = ui.visuals();
    let accent = match tone {
        Tone::Error => visuals.error_fg_color,
        Tone::Warn => visuals.warn_fg_color,
        Tone::Info => visuals.hyperlink_color,
    };
    let fill = blend(visuals.panel_fill, accent, 0.14);
    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0_f32, blend(visuals.panel_fill, accent, 0.45)))
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(add);
        });
}

/// A small close box; true when clicked.
fn close(ui: &mut egui::Ui) -> bool {
    ui.add(egui::Button::new("×").frame(false)).on_hover_text(tr("Close")).clicked()
}

/// `a` moved `t` of the way to `b`.
fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}
