//! Help: links to the README's sections, and the tip of the day. The Help
//! menu itself is in `menubar`.

use eframe::egui;

use crate::i18n::tr;

/// The README on GitHub, which is UIDM's manual.
pub const MANUAL: &str = "https://github.com/tajbinkhan/uidm#readme";
/// Its section on queues and their schedules.
pub const SCHEDULER: &str = "https://github.com/tajbinkhan/uidm#queues-and-the-scheduler";
/// Its section on the site grabber.
pub const GRABBER: &str = "https://github.com/tajbinkhan/uidm#streams-batches-and-the-site-grabber";
/// UIDM's home page.
pub const HOME: &str = "https://github.com/tajbinkhan/uidm";
/// Where to report a problem or ask for help.
pub const SUPPORT: &str = "https://github.com/tajbinkhan/uidm/issues";

/// The tips, one shown at a time.
pub const TIPS: [&str; 10] = [
    "Hold Alt while clicking a link to keep its download in the browser, or Ctrl to send any link to UIDM. Options > Browser capture sets the keys.",
    "Right-click a link in the browser and choose Download later with UIDM to add it to a queue without starting it.",
    "A queue can start on a schedule and put the computer to sleep or shut it down when it is done. Downloads > Scheduler sets one up.",
    "Tasks > Run site grabber finds the files of a type that a site links to, such as every PDF in its manuals.",
    "The video button over a playing video lists the qualities the page loaded. Pick one, and UIDM downloads it as one file.",
    "Tasks > Add batch download downloads a numbered series, such as photo[001-120].jpg, or a list of addresses you paste.",
    "Ctrl+F finds a download in the list by its name or address.",
    "Options > Connection sets a proxy, and the logins for sites that ask for one.",
    "A category can remember the folder you last chose for it in the save-as dialog: tick its box under Options > Categories.",
    "Options > When done can scan each finished download for viruses, with Microsoft Defender or a scanner you choose.",
];

/// The tip window's state: the tip shown, and the startup box.
pub struct TipWindow {
    pub index: usize,
    pub on_startup: bool,
}

/// Draw the tip window. Returns `Some` with the tip to show next time and
/// the startup choice once it is closed.
pub fn tip_window(ctx: &egui::Context, t: &mut TipWindow) -> Option<(usize, bool)> {
    let mut open = true;
    let mut close = false;
    crate::dialog::Window::new(tr("Tip of the day"))
        .id(egui::Id::new("tip_of_the_day"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(380.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_max_width(380.0);
            ui.label(tr(TIPS[t.index % TIPS.len()]));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.checkbox(&mut t.on_startup, tr("Show tips on startup"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(tr("Close")).clicked() {
                        close = true;
                    }
                    if ui.button(tr("Next tip")).clicked() {
                        t.index = (t.index + 1) % TIPS.len();
                    }
                });
            });
        });
    (!open || close).then(|| ((t.index + 1) % TIPS.len(), t.on_startup))
}
