//! The tray icon and its menu.
//!
//! The icon and menu items are not `Send`, so they live in a thread local on
//! the main thread, which also runs their event handlers (the handlers are
//! called from the window's message loop). Everything that touches them runs
//! there: the handlers below, and the window's frames.

use std::cell::RefCell;
use std::sync::Arc;

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// What the tray asks the window to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Open,
    AddFromClipboard,
    PauseAll,
    ResumeAll,
    ToggleSpeedLimit,
    Exit,
}

struct Tray {
    icon: TrayIcon,
    speed_limit: CheckMenuItem,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

/// Create the tray icon. `on_command` runs menu choices and clicks on the
/// icon; `tooltip` gives the text shown when the pointer rests on it. Call
/// on the main thread once the window exists.
pub fn install(
    rgba: Vec<u8>,
    size: u32,
    on_command: Arc<dyn Fn(Command) + Send + Sync>,
    tooltip: Arc<dyn Fn() -> String + Send + Sync>,
) -> Result<(), String> {
    let open = MenuItem::new("Open UIDM", true, None);
    let add = MenuItem::new("Add URL from clipboard", true, None);
    let pause = MenuItem::new("Pause all", true, None);
    let resume = MenuItem::new("Resume all", true, None);
    let speed_limit = CheckMenuItem::new("Speed limit", true, false, None);
    let exit = MenuItem::new("Exit", true, None);
    let menu = Menu::new();
    let separator = PredefinedMenuItem::separator();
    let separator2 = PredefinedMenuItem::separator();
    menu.append_items(&[&open, &add, &separator, &pause, &resume, &speed_limit, &separator2, &exit])
        .map_err(|e| e.to_string())?;

    let ids: Vec<(MenuId, Command)> = vec![
        (open.id().clone(), Command::Open),
        (add.id().clone(), Command::AddFromClipboard),
        (pause.id().clone(), Command::PauseAll),
        (resume.id().clone(), Command::ResumeAll),
        (speed_limit.id().clone(), Command::ToggleSpeedLimit),
        (exit.id().clone(), Command::Exit),
    ];
    let run = Arc::clone(&on_command);
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if let Some((_, command)) = ids.iter().find(|(id, _)| *id == event.id) {
            run(*command);
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| match event {
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } => on_command(Command::Open),
        // Refresh the tooltip just before it shows.
        TrayIconEvent::Enter { .. } | TrayIconEvent::Move { .. } => set_tooltip(&tooltip()),
        _ => {}
    }));

    let icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("UIDM")
        .with_icon(Icon::from_rgba(rgba, size, size).map_err(|e| e.to_string())?)
        .build()
        .map_err(|e| e.to_string())?;
    TRAY.with(|t| *t.borrow_mut() = Some(Tray { icon, speed_limit }));
    Ok(())
}

/// Whether the tray icon exists (it may fail to install).
pub fn installed() -> bool {
    TRAY.with(|t| t.borrow().is_some())
}

pub fn set_tooltip(text: &str) {
    TRAY.with(|t| {
        if let Some(tray) = t.borrow().as_ref() {
            let _ = tray.icon.set_tooltip(Some(text));
        }
    });
}

/// Show whether the speed limit is on, next to its menu item.
pub fn set_speed_limit_checked(on: bool) {
    TRAY.with(|t| {
        if let Some(tray) = t.borrow().as_ref() {
            if tray.speed_limit.is_checked() != on {
                tray.speed_limit.set_checked(on);
            }
        }
    });
}
