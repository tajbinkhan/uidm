//! Dialogs as windows of their own, like the Win32 dialogs UIDM's layout
//! follows, instead of panels drawn inside the main window. `Window` takes
//! the same builder calls as `egui::Window`, so a dialog moves over by
//! changing one name. Each opens centered on the main window and sizes
//! itself to fit what it shows. Where egui cannot open more windows it
//! falls back to `egui::Window`.

use eframe::egui::{
    self, Id, InnerResponse, Pos2, Rect, Vec2, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId,
};

/// Tahoma 8 pt dialog units at 96 dpi, in points: 1.5 across, 1.625 down.
pub const DLU_X: f32 = 1.5;

/// The gap between a dialog's edge and its controls: 7 dialog units, as in
/// the Windows layout guidelines.
const MARGIN: i8 = 10;
/// The width a dialog opens at when nothing sets one.
const DEFAULT_WIDTH: f32 = 360.0;
/// The height a dialog opens at, before it fits its contents.
const FIRST_HEIGHT: f32 = 160.0;

pub struct Window<'o> {
    title: String,
    id: Option<Id>,
    open: Option<&'o mut bool>,
    resizable: bool,
    width: Option<f32>,
    taskbar: bool,
}

/// Where a dialog was put, kept while it stays open.
#[derive(Clone, Copy)]
struct Placement {
    /// The pass it was last shown in.
    pass: u64,
    /// The main window's center when it opened.
    center: Pos2,
    /// It has been fitted to its contents once.
    fitted: bool,
    /// The main window owns it, so it stays in front of the main window.
    owned: bool,
    /// It has been centered on the main window at its fitted size.
    centered: bool,
}

impl<'o> Window<'o> {
    pub fn new(title: impl Into<String>) -> Self {
        Window {
            title: title.into(),
            id: None,
            open: None,
            resizable: false,
            width: None,
            taskbar: false,
        }
    }

    pub fn id(mut self, id: Id) -> Self {
        self.id = Some(id);
        self
    }

    /// Gives the window a close box, which clears `open`. Escape does too.
    pub fn open(mut self, open: &'o mut bool) -> Self {
        self.open = Some(open);
        self
    }

    pub fn resizable(mut self, resizable: bool) -> Self {
        self.resizable = resizable;
        self
    }

    /// Separate windows cannot collapse; kept so calls match `egui::Window`.
    pub fn collapsible(self, _collapsible: bool) -> Self {
        self
    }

    /// Dialogs always open centered on the main window.
    pub fn anchor(self, _align: egui::Align2, _offset: impl Into<Vec2>) -> Self {
        self
    }

    /// Each dialog is its own window, so there is nothing to stack.
    pub fn order(self, _order: egui::Order) -> Self {
        self
    }

    pub fn default_width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    pub fn min_width(mut self, width: f32) -> Self {
        self.width = Some(self.width.map_or(width, |w| w.max(width)));
        self
    }

    /// The width of the dialog UIDM's layout follows, in dialog units.
    pub fn dlu_width(self, width: f32) -> Self {
        self.min_width(width * DLU_X)
    }

    /// Show a taskbar button for it, as the download window has.
    pub fn taskbar(mut self, taskbar: bool) -> Self {
        self.taskbar = taskbar;
        self
    }

    pub fn show<R>(self, ctx: &egui::Context, add: impl FnOnce(&mut egui::Ui) -> R) -> Option<InnerResponse<Option<R>>> {
        if self.open.as_deref().is_some_and(|open| !*open) {
            return None;
        }
        let id = self.id.unwrap_or_else(|| Id::new(&self.title));
        if ctx.embed_viewports() {
            return self.embedded(ctx, id, add);
        }
        let width = self.width.unwrap_or(DEFAULT_WIDTH);

        let pass = ctx.cumulative_pass_nr();
        let key = id.with("dialog_placement");
        let main = ctx.input(|i| i.viewport().outer_rect).unwrap_or(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0)));
        let mut place = ctx
            .data(|d| d.get_temp::<Placement>(key))
            .filter(|p| p.pass + 1 >= pass)
            .unwrap_or(Placement { pass, center: main.center(), fitted: false, owned: false, centered: false });
        place.pass = pass;
        let first = Vec2::new(width, FIRST_HEIGHT);
        let builder = ViewportBuilder::default()
            .with_title(&self.title)
            .with_inner_size(first)
            .with_position(place.center - first / 2.0)
            .with_resizable(self.resizable)
            .with_minimize_button(false)
            .with_maximize_button(false)
            .with_taskbar(self.taskbar);

        let closable = self.open.is_some();
        let resizable = self.resizable;
        let (title, taskbar) = (&self.title, self.taskbar);
        let mut add = Some(add);
        let mut closed = false;
        let shown = ctx.show_viewport_immediate(ViewportId::from_hash_of(id), builder, |ctx, class| {
            if class == ViewportClass::Embedded {
                return None;
            }
            let (close, escape) = ctx.input(|i| (i.viewport().close_requested(), i.key_pressed(egui::Key::Escape)));
            if close && !closable {
                // The dialog's own buttons close it.
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            }
            closed = closable && (close || escape);

            let frame = egui::Frame::central_panel(&ctx.style()).inner_margin(egui::Margin::same(MARGIN));
            let panel = egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
                // The panel itself fills the window; a scope measures only
                // what the dialog drew.
                let shown = ui.scope(|ui| add.take().map(|add| add(ui)));
                (shown.inner, shown.response.rect.size())
            });
            let (r, content) = panel.inner;

            // Fit the window to what it shows: always for a fixed-size
            // dialog, the first time for one the user can resize.
            let inner = ctx.input(|i| i.viewport().inner_rect).map(|r| r.size());
            let wanted = Vec2::new(
                (content.x + 2.0 * MARGIN as f32).max(width).ceil(),
                (content.y + 2.0 * MARGIN as f32).ceil(),
            );
            if let Some(inner) = inner {
                let off = (wanted - inner).abs();
                let settled = off.x <= 1.0 && off.y <= 1.0;
                if (!resizable || !place.fitted) && !settled {
                    ctx.send_viewport_cmd(ViewportCommand::InnerSize(wanted));
                    // Centered again at its real size. Windows centers it
                    // on the main window below instead.
                    if !place.fitted && !cfg!(windows) {
                        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(place.center - wanted / 2.0));
                    }
                }
                place.fitted = true;
                // Owned by the main window, a dialog stays in front of it
                // and minimizes with it, like a Win32 dialog. The download
                // window keeps its own taskbar button and stays unowned.
                if !taskbar && !place.owned {
                    place.owned = crate::win::own_dialog(title);
                }
                if settled && !place.centered {
                    place.centered = crate::win::center_dialog(title);
                }
            }
            Some(InnerResponse::new(r, panel.response))
        });
        ctx.data_mut(|d| d.insert_temp(key, place));
        if closed {
            if let Some(open) = self.open {
                *open = false;
            }
        }
        shown
    }

    /// Drawn inside the main window, where egui cannot open another.
    fn embedded<R>(self, ctx: &egui::Context, id: Id, add: impl FnOnce(&mut egui::Ui) -> R) -> Option<InnerResponse<Option<R>>> {
        let mut window = egui::Window::new(self.title)
            .id(id)
            .collapsible(false)
            .resizable(self.resizable)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]);
        if let Some(width) = self.width {
            window = window.default_width(width);
        }
        match self.open {
            Some(open) => window.open(open).show(ctx, add),
            None => window.show(ctx, add),
        }
    }
}
