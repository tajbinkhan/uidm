//! Completion notifications: Windows toasts with buttons, and sounds.
//!
//! The daemon shows them, since it is running whenever a download finishes.
//! Buttons report back through the toast's `Activated` event, which Windows
//! raises in this process while it runs; each button carries an argument such
//! as `open:12` that the daemon acts on.

use std::fmt::Write as _;

use windows::core::{IInspectable, Interface, HSTRING};
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{ToastActivatedEventArgs, ToastNotification, ToastNotificationManager};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

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

/// Show `t`. `on_click` gets the argument of whatever was clicked. Keep the
/// returned notification while its buttons should work.
pub fn show(
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

/// Which sound to play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    Done,
    Failed,
}

const MB_ICONHAND: u32 = 0x10;
const MB_ICONASTERISK: u32 = 0x40;

#[link(name = "user32")]
extern "system" {
    fn MessageBeep(kind: u32) -> i32;
}

/// Play the system sound for `sound`, without waiting for it.
pub fn play(sound: Sound) {
    let kind = match sound {
        Sound::Done => MB_ICONASTERISK,
        Sound::Failed => MB_ICONHAND,
    };
    // SAFETY: no pointers; plays asynchronously.
    unsafe {
        MessageBeep(kind);
    }
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
