//! Completion notifications with buttons, and sounds.
//!
//! The daemon shows them, since it is running whenever a download finishes.
//! Each button carries an argument such as `open:12` that the daemon acts on.
//!
//! - Windows: toasts; buttons report back through the toast's `Activated`
//!   event, which Windows raises in this process while it runs.
//! - Linux: `notify-send` with `--action`, which prints the chosen action;
//!   an older one without actions shows the notice alone.
//! - macOS: `osascript`'s `display notification`, which has no buttons.

use std::fmt::Write as _;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt as _;

#[cfg(windows)]
use windows::core::{IInspectable, Interface, HSTRING};
#[cfg(windows)]
use windows::Data::Xml::Dom::XmlDocument;
#[cfg(windows)]
use windows::Foundation::TypedEventHandler;
#[cfg(windows)]
use windows::UI::Notifications::{ToastActivatedEventArgs, ToastNotification, ToastNotificationManager};
#[cfg(windows)]
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

#[cfg(windows)]
use crate::registry::APP_ID;

/// What a toast says and offers.
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub title: String,
    /// Up to two more lines.
    pub lines: Vec<String>,
    /// Button labels and the argument each reports.
    pub buttons: Vec<(String, String)>,
    /// The argument reported when the toast itself is clicked.
    pub launch: String,
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 has no place for most control characters.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// The toast's XML. Toasts are silent; UIDM plays its own sounds.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn toast_xml(t: &Toast) -> String {
    let mut xml = format!(
        r#"<toast launch="{}"><visual><binding template="ToastGeneric"><text>{}</text>"#,
        escape(&t.launch),
        escape(&t.title)
    );
    for line in t.lines.iter().take(2) {
        let _ = write!(xml, "<text>{}</text>", escape(line));
    }
    xml.push_str("</binding></visual>");
    if !t.buttons.is_empty() {
        xml.push_str("<actions>");
        for (label, arg) in t.buttons.iter().take(5) {
            let _ = write!(
                xml,
                r#"<action content="{}" arguments="{}" activationType="foreground"/>"#,
                escape(label),
                escape(arg)
            );
        }
        xml.push_str("</actions>");
    }
    xml.push_str(r#"<audio silent="true"/></toast>"#);
    xml
}

/// A notification shown, kept while its buttons should work.
#[cfg(windows)]
pub type Shown = ToastNotification;
/// A notification shown; its buttons work for as long as it is on screen.
#[cfg(not(windows))]
pub struct Shown;

/// Show `t`. `on_click` gets the argument of whatever was clicked. Keep the
/// returned notification while its buttons should work.
#[cfg(windows)]
pub fn show(t: &Toast, on_click: impl Fn(String) + Send + Sync + 'static) -> Result<Shown, String> {
    show_toast(t, on_click).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn show_toast(
    t: &Toast,
    on_click: impl Fn(String) + Send + Sync + 'static,
) -> windows::core::Result<ToastNotification> {
    // Safe to repeat; an already initialized thread just reports so.
    // SAFETY: plain WinRT initialization of the calling thread.
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(toast_xml(t)))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    toast.Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(
        move |_, args| {
            if let Some(args) = args.as_ref() {
                if let Ok(args) = args.cast::<ToastActivatedEventArgs>() {
                    on_click(args.Arguments()?.to_string());
                }
            }
            Ok(())
        },
    ))?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))?.Show(&toast)?;
    Ok(toast)
}

/// The `notify-send` arguments for `t`: the app name, a button per action
/// and the click on the notice itself as `default`, then the title and the
/// lines. Without `actions`, for a notify-send that has none.
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
fn notify_send_args(t: &Toast, actions: bool) -> Vec<String> {
    let mut args = vec!["--app-name=UIDM".to_string(), "--icon=uidm".to_string()];
    if actions {
        args.push("--wait".into());
        args.push(format!("--action=default={}", t.launch));
        for (label, arg) in t.buttons.iter().take(5) {
            args.push(format!("--action={arg}={label}"));
        }
    }
    args.push(t.title.clone());
    args.push(t.lines.iter().take(2).cloned().collect::<Vec<_>>().join("\n"));
    args
}

/// Show `t`. `on_click` gets the argument of whatever was clicked, while
/// the notice is up.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn show(t: &Toast, on_click: impl Fn(String) + Send + Sync + 'static) -> Result<Shown, String> {
    use std::process::{Command, Stdio};
    let launch = t.launch.clone();
    let with_actions = notify_send_args(t, true);
    let plain = notify_send_args(t, false);
    let child = Command::new("notify-send").args(&with_actions).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
    let child = child.map_err(|e| format!("notify-send: {e}"))?;
    std::thread::spawn(move || {
        let Ok(out) = child.wait_with_output() else {
            return;
        };
        if !out.status.success() {
            // No --action or --wait here: show the notice without buttons.
            let _ = Command::new("notify-send").args(&plain).status();
            return;
        }
        let picked = String::from_utf8_lossy(&out.stdout).trim().to_string();
        match picked.as_str() {
            "" => {}
            "default" => on_click(launch),
            arg => on_click(arg.to_string()),
        }
    });
    Ok(Shown)
}

/// `s` as an AppleScript string literal.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace(['\n', '\r'], " "))
}

/// Show `t` in Notification Center. macOS gives scripts no buttons, so
/// `on_click` is never called.
#[cfg(target_os = "macos")]
pub fn show(t: &Toast, on_click: impl Fn(String) + Send + Sync + 'static) -> Result<Shown, String> {
    let _ = on_click;
    let script = format!(
        "display notification {} with title {}",
        applescript_string(&t.lines.iter().take(2).cloned().collect::<Vec<_>>().join(" — ")),
        applescript_string(&t.title)
    );
    std::process::Command::new("osascript").args(["-e", &script]).spawn().map_err(|e| format!("osascript: {e}"))?;
    Ok(Shown)
}

/// Which sound to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    Done,
    Failed,
    QueueDone,
}

#[cfg(windows)]
const MB_ICONHAND: u32 = 0x10;
#[cfg(windows)]
const MB_ICONEXCLAMATION: u32 = 0x30;
#[cfg(windows)]
const MB_ICONASTERISK: u32 = 0x40;
#[cfg(windows)]
const SND_ASYNC: u32 = 0x0001;
#[cfg(windows)]
const SND_NODEFAULT: u32 = 0x0002;
#[cfg(windows)]
const SND_FILENAME: u32 = 0x0002_0000;

#[cfg(windows)]
#[link(name = "user32")]
extern "system" {
    fn MessageBeep(kind: u32) -> i32;
}

#[cfg(windows)]
#[link(name = "winmm")]
extern "system" {
    fn PlaySoundW(sound: *const u16, module: isize, flags: u32) -> i32;
}

/// Play `file` (a WAV file) for `sound`, or the system sound for it when
/// there is no file or it cannot be played, without waiting for it.
#[cfg(windows)]
pub fn play(sound: Sound, file: Option<&std::path::Path>) {
    if let Some(file) = file.filter(|f| f.is_file()) {
        let wide: Vec<u16> = file.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        // SAFETY: a NUL-terminated path, read before the call returns.
        if unsafe { PlaySoundW(wide.as_ptr(), 0, SND_FILENAME | SND_ASYNC | SND_NODEFAULT) } != 0 {
            return;
        }
    }
    let kind = match sound {
        Sound::Done => MB_ICONASTERISK,
        Sound::Failed => MB_ICONHAND,
        Sound::QueueDone => MB_ICONEXCLAMATION,
    };
    // SAFETY: no pointers; plays asynchronously.
    unsafe {
        MessageBeep(kind);
    }
}

/// The system's own sound for `sound`.
#[cfg(unix)]
fn system_sound(sound: Sound) -> &'static str {
    #[cfg(target_os = "macos")]
    return match sound {
        Sound::Done => "/System/Library/Sounds/Glass.aiff",
        Sound::Failed => "/System/Library/Sounds/Basso.aiff",
        Sound::QueueDone => "/System/Library/Sounds/Hero.aiff",
    };
    #[cfg(not(target_os = "macos"))]
    return match sound {
        Sound::Done => "/usr/share/sounds/freedesktop/stereo/complete.oga",
        Sound::Failed => "/usr/share/sounds/freedesktop/stereo/dialog-warning.oga",
        Sound::QueueDone => "/usr/share/sounds/freedesktop/stereo/message.oga",
    };
}

/// Play `file` for `sound`, or the system sound for it when there is no
/// file, without waiting for it: with afplay on macOS, and on Linux with
/// paplay (PulseAudio and PipeWire), else aplay for WAV files.
#[cfg(unix)]
pub fn play(sound: Sound, file: Option<&std::path::Path>) {
    use std::process::{Command, Stdio};
    let path = match file.filter(|f| f.is_file()) {
        Some(f) => f.to_path_buf(),
        None => std::path::PathBuf::from(system_sound(sound)),
    };
    if !path.is_file() {
        return;
    }
    let players: &[&str] = if cfg!(target_os = "macos") { &["afplay"] } else { &["paplay", "aplay"] };
    std::thread::spawn(move || {
        for player in players {
            let played = Command::new(player).arg(&path).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
            if played.is_ok_and(|s| s.success()) {
                return;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_escapes_names_and_carries_buttons() {
        let t = Toast {
            title: "Download complete".into(),
            lines: vec!["a<b>&\"c'.pdf".into(), "4.0 MB\u{1}".into(), "dropped".into()],
            buttons: vec![("Open".into(), "open:7".into()), ("Show in folder".into(), "show:7".into())],
            launch: "window".into(),
        };
        let xml = toast_xml(&t);
        assert_eq!(
            xml,
            concat!(
                r#"<toast launch="window"><visual><binding template="ToastGeneric">"#,
                r#"<text>Download complete</text><text>a&lt;b&gt;&amp;&quot;c&apos;.pdf</text>"#,
                r#"<text>4.0 MB</text></binding></visual><actions>"#,
                r#"<action content="Open" arguments="open:7" activationType="foreground"/>"#,
                r#"<action content="Show in folder" arguments="show:7" activationType="foreground"/>"#,
                r#"</actions><audio silent="true"/></toast>"#
            )
        );
    }

    #[test]
    fn notify_send_gets_a_button_per_action() {
        let t = Toast {
            title: "Download complete".into(),
            lines: vec!["a.pdf".into(), "4.0 MB".into()],
            buttons: vec![("Open".into(), "open:7".into())],
            launch: "window".into(),
        };
        assert_eq!(
            notify_send_args(&t, true),
            ["--app-name=UIDM", "--icon=uidm", "--wait", "--action=default=window", "--action=open:7=Open", "Download complete", "a.pdf\n4.0 MB"]
        );
        assert_eq!(notify_send_args(&t, false), ["--app-name=UIDM", "--icon=uidm", "Download complete", "a.pdf\n4.0 MB"]);
        assert_eq!(applescript_string("say \"hi\"\\"), r#""say \"hi\"\\""#);
    }

    #[test]
    fn no_actions_without_buttons() {
        let t = Toast {
            title: "x".into(),
            lines: vec![],
            buttons: vec![],
            launch: "window".into(),
        };
        assert!(!toast_xml(&t).contains("<actions>"));
    }
}
