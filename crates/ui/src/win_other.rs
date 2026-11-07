//! What the window needs outside egui on macOS and Linux: showing and
//! hiding it from the tray's thread, the clipboard for the tray menu and the
//! clipboard offer, opening a file with another program, and the display
//! languages. The Windows version (`win.rs`) calls Win32 for each.
//!
//! "Hiding" minimizes the window here: a hidden window gets no frames on
//! every desktop (and Wayland has no hidden windows at all), so it could not
//! be brought back, while a minimized one always can, from the Dock or the
//! taskbar too.

use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};

use eframe::egui;

/// The window's egui context, once eframe has made it.
static CTX: OnceLock<egui::Context> = OnceLock::new();

/// Keep `ctx`, so other threads can show and hide the window.
pub fn set_context(ctx: &egui::Context) {
    let _ = CTX.set(ctx.clone());
}

/// Show the window, restore it if minimized, and bring it to the front.
/// Safe to call from any thread.
pub fn show() {
    if let Some(ctx) = CTX.get() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }
}

/// A dialog that opens on its own (save-as, download complete) needs the
/// window's frames, which a minimized window may not get here, so the window
/// comes up with it.
pub fn wake() {
    show();
}

/// Whether one of UIDM's own windows is in front. Not known here, so a
/// dialog that opens on its own takes the focus, as before.
pub fn in_front() -> bool {
    true
}

/// Bring a dialog that opened on its own to the front. Here its viewport's
/// own focus request does that, so there is nothing more to do.
pub fn raise_window(_title: &str) -> bool {
    true
}

/// Keep a dialog in front of the main window. Not done here.
pub fn own_window(_title: &str) -> bool {
    true
}

/// The middle of the screen while the main window is put away. The monitor's
/// position is not known here, so dialogs work it out from egui instead.
pub fn screen_center() -> Option<(f32, f32)> {
    None
}

/// Put the window away; it lives on in the tray (and the Dock or taskbar).
pub fn hide() {
    if let Some(ctx) = CTX.get() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        ctx.request_repaint();
    }
}

/// Open `path` with the program the system picks. There is no "Open with"
/// picker to call on these systems, so the file is shown in its folder,
/// whose menu has one.
pub fn open_with(path: &str) {
    crate::ports::ports().opener.reveal(path);
}

/// Dragging a file out of the window is Windows-only for now, so a drag
/// here does nothing.
pub fn drag_file(_path: &str) {}

/// The clipboard, opened once; the system keeps one per process.
fn clipboard() -> &'static Mutex<Option<arboard::Clipboard>> {
    static CLIPBOARD: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();
    CLIPBOARD.get_or_init(|| Mutex::new(arboard::Clipboard::new().ok()))
}

/// The clipboard's text, if it holds any.
pub fn clipboard_text() -> Option<String> {
    clipboard().lock().unwrap_or_else(|e| e.into_inner()).as_mut()?.get_text().ok()
}

/// A number that changes whenever the clipboard's text does. These systems
/// keep no change counter, so it is a hash of the text.
pub fn clipboard_sequence() -> u32 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    clipboard_text().hash(&mut hasher);
    hasher.finish() as u32
}

/// What the system calls files with extension `ext`. Not looked up here, so
/// the extension alone is shown.
pub fn file_type_name(_ext: &str) -> Option<String> {
    None
}

/// The display languages, most preferred first, as tags such as `de-DE`:
/// from `LANGUAGE`, `LC_ALL`, `LC_MESSAGES` and `LANG`, and on macOS the
/// languages set in System Settings.
pub fn display_languages() -> Vec<String> {
    let mut tags = Vec::new();
    #[cfg(target_os = "macos")]
    if let Ok(out) = std::process::Command::new("defaults").args(["read", "-g", "AppleLanguages"]).output() {
        // ( "de-DE", en ) in the old plist text form.
        let text = String::from_utf8_lossy(&out.stdout);
        tags.extend(
            text.split(|c: char| c == ',' || c == '(' || c == ')' || c.is_whitespace())
                .map(|t| t.trim_matches('"'))
                .filter(|t| !t.is_empty())
                .map(str::to_string),
        );
    }
    for var in ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(var) {
            for part in value.split(':') {
                // de_DE.UTF-8@euro to de-DE.
                let tag = part.split(['.', '@']).next().unwrap_or("").replace('_', "-");
                if !tag.is_empty() && tag != "C" && tag != "POSIX" && !tags.contains(&tag) {
                    tags.push(tag);
                }
            }
        }
    }
    tags
}
