//! Starting UIDM's window, hidden in the tray, when the user signs in:
//!
//! - Windows: the `UIDM` value under `HKCU\...\CurrentVersion\Run`;
//! - macOS: `~/Library/LaunchAgents/com.uidm.gui.plist`, a launchd agent;
//! - Linux: `~/.config/autostart/uidm.desktop`, the freedesktop.org way.

use std::io;
#[cfg(unix)]
use std::path::{Path, PathBuf};

/// Add or remove the entry that starts UIDM's window at sign-in.
pub fn set_autostart(enabled: bool) -> io::Result<()> {
    #[cfg(windows)]
    return crate::registry::set_autostart(enabled, &crate::platform::sibling_exe("uidm-gui")?);
    #[cfg(unix)]
    {
        let (gui, first) = crate::platform::program("uidm-gui")?;
        let path = entry_path()?;
        if !enabled {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, entry(&gui, &first))
    }
}

/// Where the sign-in entry lives.
#[cfg(unix)]
fn entry_path() -> io::Result<PathBuf> {
    let home = crate::platform::home_dir().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    #[cfg(target_os = "macos")]
    return Ok(home.join("Library/LaunchAgents/com.uidm.gui.plist"));
    #[cfg(not(target_os = "macos"))]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map_or_else(|| home.join(".config"), PathBuf::from);
        Ok(base.join("autostart").join("uidm.desktop"))
    }
}

/// The sign-in entry that starts `gui` with `first` (the program's name
/// when `gui` is an AppImage), then `--minimized`.
#[cfg(unix)]
fn entry(gui: &Path, first: &[String]) -> String {
    #[cfg(target_os = "macos")]
    {
        let escape = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n\
             \t<key>Label</key>\n\t<string>com.uidm.gui</string>\n\
             \t<key>ProgramArguments</key>\n\t<array>\n{}\t</array>\n\
             \t<key>RunAtLoad</key>\n\t<true/>\n\
             </dict>\n</plist>\n",
            std::iter::once(gui.display().to_string())
                .chain(first.iter().cloned())
                .chain(["--minimized".to_string()])
                .map(|a| format!("\t\t<string>{}</string>\n", escape(&a)))
                .collect::<String>()
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Exec quotes each argument: a space or quote in it stays part of it.
        let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('`', "\\`").replace('$', "\\$"));
        let exec: Vec<String> = std::iter::once(quote(&gui.display().to_string())).chain(first.iter().map(|a| quote(a))).collect();
        format!(
            "[Desktop Entry]\nType=Application\nName=UIDM\nComment=Download manager\nIcon=uidm\n\
             Exec={} --minimized\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
            exec.join(" ")
        )
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn the_entry_starts_the_window_minimized() {
        let text = entry(Path::new("/opt/UIDM app/uidm-gui"), &[]);
        assert!(text.contains("--minimized"));
        if cfg!(target_os = "macos") {
            assert!(text.contains("<string>/opt/UIDM app/uidm-gui</string>"));
        } else {
            assert!(text.contains("Exec=\"/opt/UIDM app/uidm-gui\" --minimized"));
            let image = entry(Path::new("/home/me/UIDM.AppImage"), &["uidm-gui".to_string()]);
            assert!(image.contains("Exec=\"/home/me/UIDM.AppImage\" \"uidm-gui\" --minimized"), "{image}");
        }
    }
}
