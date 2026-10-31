//! The tray icon and its menu.
//!
//! The icon and menu items are not `Send`, so they live in a thread local on
//! the thread that made them, and everything that touches them runs there
//! (`with_tray`). On Windows and macOS that is the main thread, whose
//! message loop is the window's. On Linux the icon is a GTK status icon
//! (libappindicator), which needs GTK's own loop: it gets a thread of its
//! own, and the window's calls are posted to it.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tray_icon::menu::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::i18n::tr;

/// What the tray asks the window to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Open,
    AddFromClipboard,
    BatchFromClipboard,
    PauseAll,
    ResumeAll,
    ToggleSpeedLimit,
    ToggleDropTarget,
    Exit,
}

struct Tray {
    icon: TrayIcon,
    speed_limit: CheckMenuItem,
    drop_target: CheckMenuItem,
    /// The other items, and their English text.
    items: Vec<(MenuItem, &'static str)>,
}

const OPEN: &str = "Open UIDM";
const ADD: &str = "Add URL from clipboard";
const ADD_BATCH: &str = "Add batch download from clipboard";
const PAUSE: &str = "Pause all";
const RESUME: &str = "Resume all";
const SPEED_LIMIT: &str = "Speed limit";
const DROP_TARGET: &str = "Drop target";
const EXIT: &str = "Exit";

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

/// Whether the icon exists (it may fail to install).
static INSTALLED: AtomicBool = AtomicBool::new(false);
/// Whether the icon is showing; Options can hide it.
static VISIBLE: AtomicBool = AtomicBool::new(true);

/// Run `f` with the tray, on the thread that owns it.
fn with_tray(f: impl FnOnce(&Tray) + Send + 'static) {
    #[cfg(target_os = "linux")]
    gtk::glib::MainContext::default().invoke(move || TRAY.with(|t| t.borrow().as_ref().map(f)).unwrap_or(()));
    #[cfg(not(target_os = "linux"))]
    TRAY.with(|t| {
        if let Some(tray) = t.borrow().as_ref() {
            f(tray)
        }
    });
}

/// Create the tray icon. `on_command` runs menu choices and clicks on the
/// icon; `tooltip` gives the text shown when the pointer rests on it. Call
/// on the main thread once the window exists.
#[cfg(not(target_os = "linux"))]
pub fn install(
    rgba: Vec<u8>,
    size: u32,
    on_command: Arc<dyn Fn(Command) + Send + Sync>,
    tooltip: Arc<dyn Fn() -> String + Send + Sync>,
) -> Result<(), String> {
    build(rgba, size, on_command, tooltip)
}

/// Create the tray icon on a GTK thread of its own, and wait until it is
/// there or has failed. `on_command` runs menu choices; `tooltip` is unused,
/// since status icons have no tooltip here.
#[cfg(target_os = "linux")]
pub fn install(
    rgba: Vec<u8>,
    size: u32,
    on_command: Arc<dyn Fn(Command) + Send + Sync>,
    tooltip: Arc<dyn Fn() -> String + Send + Sync>,
) -> Result<(), String> {
    let (done, made) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Err(e) = gtk::init() {
            let _ = done.send(Err(format!("GTK could not start: {e}")));
            return;
        }
        // tray-icon panics when no AppIndicator library can be loaded.
        let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build(rgba, size, on_command, tooltip)))
            .unwrap_or_else(|_| Err("install libayatana-appindicator3 for the tray icon".to_string()));
        let ok = built.is_ok();
        let _ = done.send(built);
        if ok {
            gtk::main();
        }
    });
    made.recv().unwrap_or_else(|_| Err("the tray's thread stopped".into()))
}

/// Make the icon and its menu on this thread, and keep them in `TRAY`.
fn build(
    rgba: Vec<u8>,
    size: u32,
    on_command: Arc<dyn Fn(Command) + Send + Sync>,
    tooltip: Arc<dyn Fn() -> String + Send + Sync>,
) -> Result<(), String> {
    let open = MenuItem::new(tr(OPEN), true, None);
    let add = MenuItem::new(tr(ADD), true, None);
    let add_batch = MenuItem::new(tr(ADD_BATCH), true, None);
    let pause = MenuItem::new(tr(PAUSE), true, None);
    let resume = MenuItem::new(tr(RESUME), true, None);
    let speed_limit = CheckMenuItem::new(tr(SPEED_LIMIT), true, false, None);
    let drop_target = CheckMenuItem::new(tr(DROP_TARGET), true, false, None);
    let exit = MenuItem::new(tr(EXIT), true, None);
    let menu = Menu::new();
    let separator = PredefinedMenuItem::separator();
    let separator2 = PredefinedMenuItem::separator();
    // The drop target is only on Windows (see `droptarget`).
    if cfg!(windows) {
        menu.append_items(&[&open, &add, &add_batch, &drop_target, &separator, &pause, &resume, &speed_limit, &separator2, &exit])
    } else {
        menu.append_items(&[&open, &add, &add_batch, &separator, &pause, &resume, &speed_limit, &separator2, &exit])
    }
    .map_err(|e| e.to_string())?;

    let ids: Vec<(MenuId, Command)> = vec![
        (open.id().clone(), Command::Open),
        (add.id().clone(), Command::AddFromClipboard),
        (add_batch.id().clone(), Command::BatchFromClipboard),
        (pause.id().clone(), Command::PauseAll),
        (resume.id().clone(), Command::ResumeAll),
        (speed_limit.id().clone(), Command::ToggleSpeedLimit),
        (drop_target.id().clone(), Command::ToggleDropTarget),
        (exit.id().clone(), Command::Exit),
    ];
    let run = Arc::clone(&on_command);
    crate::menubar::on_menu_event(move |id| {
        let found = ids.iter().find(|(i, _)| i == id);
        if let Some((_, command)) = found {
            run(*command);
        }
        found.is_some()
    });
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| match event {
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } => on_command(Command::Open),
        // Refresh the tooltip just before it shows.
        TrayIconEvent::Enter { .. } | TrayIconEvent::Move { .. } => set_tooltip(tooltip()),
        _ => {}
    }));

    let icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("UIDM")
        .with_icon(Icon::from_rgba(rgba, size, size).map_err(|e| e.to_string())?)
        .build()
        .map_err(|e| e.to_string())?;
    TRAY.with(|t| {
        *t.borrow_mut() = Some(Tray {
            icon,
            speed_limit,
            drop_target,
            items: vec![(open, OPEN), (add, ADD), (add_batch, ADD_BATCH), (pause, PAUSE), (resume, RESUME), (exit, EXIT)],
        })
    });
    INSTALLED.store(true, Ordering::Relaxed);
    Ok(())
}

/// Whether the tray icon exists (it may fail to install).
pub fn installed() -> bool {
    INSTALLED.load(Ordering::Relaxed)
}

/// Whether the tray icon is there to click: installed and not hidden.
pub fn shown() -> bool {
    installed() && VISIBLE.load(Ordering::Relaxed)
}

/// Show or hide the tray icon.
pub fn set_visible(on: bool) {
    if !installed() || VISIBLE.swap(on, Ordering::Relaxed) == on {
        return;
    }
    with_tray(move |tray| {
        let _ = tray.icon.set_visible(on);
    });
}

pub fn set_tooltip(text: String) {
    with_tray(move |tray| {
        let _ = tray.icon.set_tooltip(Some(text));
    });
}

/// Show whether the drop target is open, next to its menu item.
pub fn set_drop_target_checked(on: bool) {
    with_tray(move |tray| {
        if tray.drop_target.is_checked() != on {
            tray.drop_target.set_checked(on);
        }
    });
}

/// Show whether the speed limit is on, next to its menu item.
pub fn set_speed_limit_checked(on: bool) {
    with_tray(move |tray| {
        if tray.speed_limit.is_checked() != on {
            tray.speed_limit.set_checked(on);
        }
    });
}

/// Put the menu in the window's current language.
pub fn relabel() {
    // The texts are looked up here: the language is the window's.
    let texts: Vec<String> = [OPEN, ADD, ADD_BATCH, PAUSE, RESUME, EXIT].iter().map(|e| tr(e).to_string()).collect();
    let (speed, drop) = (tr(SPEED_LIMIT).to_string(), tr(DROP_TARGET).to_string());
    with_tray(move |tray| {
        for ((item, _), text) in tray.items.iter().zip(&texts) {
            item.set_text(text);
        }
        tray.speed_limit.set_text(&speed);
        tray.drop_target.set_text(&drop);
    });
}
