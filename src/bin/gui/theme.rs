//! The window's look, after shadcn/ui: the zinc palette, 1px borders, rounded
//! corners, flat fills and Segoe UI (what shadcn's system font stack picks on
//! Windows). There is a light and a dark set; egui picks one to follow the
//! Windows theme.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle,
    Theme, Vec2, Visuals,
};

/// Tailwind's zinc scale, which shadcn's neutral tokens come from.
mod zinc {
    use eframe::egui::Color32;
    pub const Z50: Color32 = Color32::from_rgb(0xfa, 0xfa, 0xfa);
    pub const Z100: Color32 = Color32::from_rgb(0xf4, 0xf4, 0xf5);
    pub const Z200: Color32 = Color32::from_rgb(0xe4, 0xe4, 0xe7);
    pub const Z300: Color32 = Color32::from_rgb(0xd4, 0xd4, 0xd8);
    pub const Z600: Color32 = Color32::from_rgb(0x52, 0x52, 0x5b);
    pub const Z700: Color32 = Color32::from_rgb(0x3f, 0x3f, 0x46);
    pub const Z800: Color32 = Color32::from_rgb(0x27, 0x27, 0x2a);
    pub const Z900: Color32 = Color32::from_rgb(0x18, 0x18, 0x1b);
    pub const Z950: Color32 = Color32::from_rgb(0x09, 0x09, 0x0b);
}

/// shadcn's tokens for one theme, plus the two status colors egui needs.
struct Palette {
    dark: bool,
    background: Color32,
    foreground: Color32,
    /// Fills for hovered controls, secondary surfaces and tracks.
    accent: Color32,
    /// Pressed controls.
    pressed: Color32,
    /// Separators, windows and menus.
    border: Color32,
    /// Buttons, inputs and checkboxes, a step darker than `border` because
    /// egui has no shadow under them to set them off.
    input: Color32,
    /// Selected items and text selection.
    selected: Color32,
    /// The filled part of a progress bar, light enough for text on top.
    progress: Color32,
    link: Color32,
    destructive: Color32,
    warning: Color32,
    shadow: Color32,
}

const LIGHT: Palette = Palette {
    dark: false,
    background: Color32::WHITE,
    foreground: zinc::Z950,
    accent: zinc::Z100,
    pressed: zinc::Z200,
    border: zinc::Z200,
    input: zinc::Z300,
    selected: zinc::Z200,
    progress: zinc::Z300,
    link: Color32::from_rgb(0x25, 0x63, 0xeb),
    destructive: Color32::from_rgb(0xdc, 0x26, 0x26),
    warning: Color32::from_rgb(0xb4, 0x53, 0x09),
    shadow: Color32::from_black_alpha(20),
};

const DARK: Palette = Palette {
    dark: true,
    background: zinc::Z950,
    foreground: zinc::Z50,
    accent: zinc::Z800,
    pressed: zinc::Z700,
    border: zinc::Z800,
    input: zinc::Z800,
    selected: zinc::Z700,
    progress: zinc::Z600,
    link: Color32::from_rgb(0x60, 0xa5, 0xfa),
    destructive: Color32::from_rgb(0xf8, 0x71, 0x71),
    warning: Color32::from_rgb(0xfb, 0xbf, 0x24),
    shadow: Color32::from_black_alpha(96),
};

/// Buttons, inputs and menus (shadcn's rounded-md).
const CONTROL_RADIUS: u8 = 6;
/// Windows and dialogs (rounded-lg).
const WINDOW_RADIUS: u8 = 8;

/// Sets the fonts, sizes, spacing and both color sets.
pub fn apply(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    for theme in [Theme::Light, Theme::Dark] {
        let palette = if theme == Theme::Dark { &DARK } else { &LIGHT };
        ctx.style_mut_of(theme, |style| {
            style.visuals = visuals(palette);
            style.text_styles = [
                (TextStyle::Small, FontId::proportional(11.0)),
                (TextStyle::Body, FontId::proportional(13.5)),
                (TextStyle::Button, FontId::proportional(13.5)),
                (TextStyle::Heading, FontId::proportional(18.0)),
                (TextStyle::Monospace, FontId::monospace(12.5)),
            ]
            .into();
            let spacing = &mut style.spacing;
            spacing.item_spacing = Vec2::new(8.0, 6.0);
            spacing.button_padding = Vec2::new(10.0, 4.0);
            spacing.interact_size.y = 24.0;
            spacing.window_margin = Margin::same(16);
            spacing.menu_margin = Margin::same(4);
        });
    }
}

/// Draws a progress bar on shadcn's muted track instead of the input
/// background, which matches the panel and would hide the empty part.
pub fn progress_bar(ui: &mut egui::Ui, bar: egui::ProgressBar) -> egui::Response {
    let p = if ui.visuals().dark_mode { &DARK } else { &LIGHT };
    ui.scope(|ui| {
        ui.visuals_mut().extreme_bg_color = p.accent;
        ui.add(bar.fill(p.progress))
    })
    .inner
}

/// Segoe UI for text and Consolas for code, read from the Windows font folder
/// so nothing is bundled. egui's own fonts stay behind them for any glyph they
/// lack, and take over if the files are missing.
fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let dir = std::env::var_os("WINDIR").map(|w| std::path::Path::new(&w).join("Fonts"));
    for (name, file, family) in [
        ("segoe-ui", "segoeui.ttf", FontFamily::Proportional),
        ("consolas", "consola.ttf", FontFamily::Monospace),
    ] {
        let Some(bytes) = dir.as_ref().and_then(|d| std::fs::read(d.join(file)).ok()) else {
            continue;
        };
        fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_owned(bytes)));
        fonts.families.entry(family).or_default().insert(0, name.to_owned());
    }
    fonts
}

fn visuals(p: &Palette) -> Visuals {
    let mut v = if p.dark { Visuals::dark() } else { Visuals::light() };
    let radius = CornerRadius::same(CONTROL_RADIUS);
    let border = line(p.border);
    let text = line(p.foreground);

    let w = &mut v.widgets;
    // Labels, separators and frames. weak_bg_fill is also what weak text fades
    // toward, so it has to be the background.
    w.noninteractive.bg_fill = p.background;
    w.noninteractive.weak_bg_fill = p.background;
    w.noninteractive.bg_stroke = border;
    w.noninteractive.fg_stroke = text;
    w.noninteractive.corner_radius = radius;
    // Outline buttons fill with weak_bg_fill: the background, then the accent
    // on hover. Checkboxes fill with bg_fill, one step stronger, so an
    // unticked box still shows on the background.
    for (state, weak_fill, fill) in [
        (&mut w.inactive, p.background, p.accent),
        (&mut w.hovered, p.accent, p.pressed),
        (&mut w.active, p.pressed, p.pressed),
        (&mut w.open, p.accent, p.accent),
    ] {
        state.weak_bg_fill = weak_fill;
        state.bg_fill = fill;
        state.bg_stroke = line(p.input);
        state.fg_stroke = text;
        state.corner_radius = radius;
        state.expansion = 0.0;
    }

    v.selection.bg_fill = p.selected;
    // Also the text on selected items and progress bars, and the ring around
    // the focused text field.
    v.selection.stroke = text;
    v.text_cursor.stroke = Stroke { width: 1.5, color: p.foreground };

    v.hyperlink_color = p.link;
    v.error_fg_color = p.destructive;
    v.warn_fg_color = p.warning;
    v.faint_bg_color = if p.dark { zinc::Z900 } else { zinc::Z50 };
    v.extreme_bg_color = p.background;
    v.code_bg_color = p.accent;

    v.panel_fill = p.background;
    v.window_fill = p.background;
    v.window_stroke = border;
    v.window_corner_radius = CornerRadius::same(WINDOW_RADIUS);
    // egui fills the title bar of the front window; shadcn dialogs are one
    // flat surface.
    v.window_highlight_topmost = false;
    v.menu_corner_radius = radius;
    v.window_shadow = Shadow { offset: [0, 8], blur: 24, spread: 0, color: p.shadow };
    v.popup_shadow = Shadow { offset: [0, 4], blur: 12, spread: 0, color: p.shadow };
    v
}

/// A 1px stroke. (`Stroke::new` takes `impl Into<f32>`, which leaves a float
/// literal's type to a fallback the compiler is phasing out.)
fn line(color: Color32) -> Stroke {
    Stroke { width: 1.0, color }
}
