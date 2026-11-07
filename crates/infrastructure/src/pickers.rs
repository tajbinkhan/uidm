//! `NativePicker`, the system's file and folder dialogs through `rfd`: the
//! common dialogs on Windows, panels on macOS and GTK's on Linux.

use std::path::{Path, PathBuf};

use uidm_application::window::{FileKind, FilePicker};

/// The system's file and folder dialogs.
pub struct NativePicker;

/// A dialog titled `title` that offers `kinds` and starts in `start`.
fn dialog(title: &str, kinds: &[FileKind], start: Option<&Path>) -> rfd::FileDialog {
    let mut dialog = rfd::FileDialog::new().set_title(title);
    for kind in kinds {
        dialog = dialog.add_filter(&kind.name, &kind.extensions);
    }
    if let Some(start) = start {
        dialog = dialog.set_directory(start);
    }
    dialog
}

impl FilePicker for NativePicker {
    fn pick_folder(&self, title: &str, start: Option<&Path>) -> Option<PathBuf> {
        dialog(title, &[], start).pick_folder()
    }

    fn pick_file(&self, title: &str, kinds: &[FileKind], start: Option<&Path>) -> Option<PathBuf> {
        dialog(title, kinds, start).pick_file()
    }

    fn save_file(&self, title: &str, name: &str, kinds: &[FileKind]) -> Option<PathBuf> {
        dialog(title, kinds, None).set_file_name(name).save_file()
    }
}
