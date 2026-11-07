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
use std::sync::{Arc, Mutex, Once};

use serde::{Deserialize, Serialize};
use tray_icon::menu::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::i18n::tr;

/// How the tray icon looks, from View > Tray icon. Kept with the window's
/// layout, since only the window draws the icon.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Style {
    /// UIDM's own colored icon.
    #[default]
    Color,
    /// A plain white silhouette of it, like the system's own icons on a dark
    /// taskbar.
    Mono,
}

impl Style {
    /// The icon's RGBA pixels in this style, made from the colored icon's
    /// so no second image is bundled. The size is unchanged.
    pub fn apply(self, rgba: &[u8]) -> Vec<u8> {
        match self {
            Style::Color => rgba.to_vec(),
            // Keep each pixel's coverage, paint it white.
            Style::Mono => rgba.as_chunks::<4>().0.iter().flat_map(|p| [255, 255, 255, p[3]]).collect(),
        }
    }
}

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

/// The main icon's ID; the download windows' icon has `PROGRESS_ID`.
const MAIN_ID: &str = "uidm";
const PROGRESS_ID: &str = "uidm-progress";

/// The downloads tray icon, after IDM's: green arrows whose menu lists the
/// hidden download windows by title. It is there only while one is.
struct DownloadsTray {
    icon: TrayIcon,
    menu: Menu,
    /// Each window's item, by download ID, in the order they went.
    items: Vec<(u64, MenuItem)>,
    /// "Show all" and the line under it, at the top while two or more
    /// windows are hidden, as in IDM.
    show_all: MenuItem,
    separator: PredefinedMenuItem,
    show_all_on: bool,
}

thread_local! {
    // The initializers are already const. Clippy 1.99 still flags the macro.
    #[allow(clippy::missing_const_for_thread_local)]
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
    #[allow(clippy::missing_const_for_thread_local)]
    static PROGRESS: RefCell<Option<DownloadsTray>> = const { RefCell::new(None) };
}

/// The download windows' menu items and their downloads, for the menu
/// handler, which runs on another thread on Linux.
static HIDDEN_IDS: Mutex<Vec<(MenuId, u64)>> = Mutex::new(Vec::new());
/// Downloads whose window was picked from that menu, until the window's
/// next frame shows them.
static RESTORE: Mutex<Vec<u64>> = Mutex::new(Vec::new());
/// The "Show all" item's ID, while the icon is there.
static SHOW_ALL_ID: Mutex<Option<MenuId>> = Mutex::new(None);

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
        menu.append_items(&[
            &open,
            &add,
            &add_batch,
            &drop_target,
            &separator,
            &pause,
            &resume,
            &speed_limit,
            &separator2,
            &exit,
        ])
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
        // The download windows' icon opens its menu on either button.
        _ if event.id() != MAIN_ID => {}
        TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } => {
            on_command(Command::Open)
        }
        // Refresh the tooltip just before it shows.
        TrayIconEvent::Enter { .. } | TrayIconEvent::Move { .. } => set_tooltip(tooltip()),
        _ => {}
    }));

    let icon = TrayIconBuilder::new()
        .with_id(MAIN_ID)
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
            items: vec![
                (open, OPEN),
                (add, ADD),
                (add_batch, ADD_BATCH),
                (pause, PAUSE),
                (resume, RESUME),
                (exit, EXIT),
            ],
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

/// Swap the icon's picture, for a new style.
pub fn set_icon(rgba: Vec<u8>, size: u32) {
    if !installed() {
        return;
    }
    with_tray(move |tray| {
        if let Ok(icon) = Icon::from_rgba(rgba, size, size) {
            let _ = tray.icon.set_icon(Some(icon));
        }
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

/// Run `f` on the tray's thread (see `with_tray`).
fn on_tray_thread(f: impl FnOnce() + Send + 'static) {
    #[cfg(target_os = "linux")]
    gtk::glib::MainContext::default().invoke(f);
    #[cfg(not(target_os = "linux"))]
    f();
}

/// List the download windows hidden in the tray, as `(download, title)`, in
/// the downloads tray icon's menu, adding the icon for the first and taking
/// it away after the last. `ctx` is woken when one (or Show all) is picked;
/// see `take_restored`.
pub fn set_hidden_windows(windows: Vec<(u64, String)>, ctx: &eframe::egui::Context) {
    if !installed() {
        return;
    }
    static HANDLER: Once = Once::new();
    let wake = ctx.clone();
    HANDLER.call_once(move || {
        crate::menubar::on_menu_event(move |id| {
            let ids = HIDDEN_IDS.lock().unwrap();
            let picked: Vec<u64> = if SHOW_ALL_ID.lock().unwrap().as_ref() == Some(id) {
                ids.iter().map(|(_, d)| *d).collect()
            } else {
                ids.iter().filter(|(i, _)| i == id).map(|(_, d)| *d).collect()
            };
            drop(ids);
            if picked.is_empty() {
                return false;
            }
            RESTORE.lock().unwrap().extend(picked);
            wake.request_repaint();
            true
        });
    });
    on_tray_thread(move || PROGRESS.with(|p| update_downloads_tray(&mut p.borrow_mut(), windows)));
}

fn update_downloads_tray(tray: &mut Option<DownloadsTray>, windows: Vec<(u64, String)>) {
    if windows.is_empty() {
        // Dropping the icon takes it out of the tray.
        *tray = None;
        HIDDEN_IDS.lock().unwrap().clear();
        *SHOW_ALL_ID.lock().unwrap() = None;
        return;
    }
    if tray.is_none() {
        let Ok(picture) = Icon::from_rgba(arrows_icon(PROGRESS_ICON_SIZE), PROGRESS_ICON_SIZE, PROGRESS_ICON_SIZE)
        else {
            return;
        };
        let menu = Menu::new();
        let built = TrayIconBuilder::new()
            .with_id(PROGRESS_ID)
            .with_menu(Box::new(menu.clone()))
            .with_menu_on_left_click(true)
            .with_icon(picture)
            .build();
        let Ok(icon) = built else { return };
        let show_all = MenuItem::new(tr("Show all"), true, None);
        *SHOW_ALL_ID.lock().unwrap() = Some(show_all.id().clone());
        *tray = Some(DownloadsTray {
            icon,
            menu,
            items: Vec::new(),
            show_all,
            separator: PredefinedMenuItem::separator(),
            show_all_on: false,
        });
    }
    let Some(t) = tray.as_mut() else { return };
    // Items for windows no longer there go; the others keep their place.
    let menu = &t.menu;
    t.items.retain(|(download, item)| {
        let keep = windows.iter().any(|(d, _)| d == download);
        if !keep {
            let _ = menu.remove(item);
        }
        keep
    });
    for (download, title) in &windows {
        match t.items.iter().find(|(d, _)| d == download) {
            Some((_, item)) => {
                if item.text() != *title {
                    item.set_text(title);
                }
            }
            None => {
                let item = MenuItem::new(title, true, None);
                if t.menu.append(&item).is_ok() {
                    t.items.push((*download, item));
                }
            }
        }
    }
    let want_show_all = show_all_wanted(windows.len());
    if want_show_all && !t.show_all_on {
        t.show_all.set_text(tr("Show all"));
        t.show_all_on = t.menu.insert(&t.show_all, 0).is_ok() && t.menu.insert(&t.separator, 1).is_ok();
    } else if !want_show_all && t.show_all_on {
        let _ = t.menu.remove(&t.show_all);
        let _ = t.menu.remove(&t.separator);
        t.show_all_on = false;
    }
    *HIDDEN_IDS.lock().unwrap() = t.items.iter().map(|(d, item)| (item.id().clone(), *d)).collect();
    let tip = match windows.as_slice() {
        [(_, title)] => title.clone(),
        _ => crate::i18n::trf!("{count} download windows", count = windows.len()),
    };
    let _ = t.icon.set_tooltip(Some(tip));
}

/// Whether the menu offers "Show all" for `hidden` windows: from two on, as
/// IDM's does.
fn show_all_wanted(hidden: usize) -> bool {
    hidden >= 2
}

/// The downloads whose window was picked from the tray since the last call.
pub fn take_restored() -> Vec<u64> {
    std::mem::take(&mut *RESTORE.lock().unwrap())
}

const PROGRESS_ICON_SIZE: u32 = 32;

/// UIDM's own picture for the download windows' icon: two green arrows
/// pointing down, side by side, the right one lower. RGBA, `size` square.
fn arrows_icon(size: u32) -> Vec<u8> {
    // One arrow in a unit square: a shaft and a head, pointing down.
    fn arrow(x: f32, y: f32) -> bool {
        let shaft = (0.32..=0.68).contains(&x) && (0.0..=0.55).contains(&y);
        let head = (0.45..=1.0).contains(&y) && (x - 0.5).abs() <= (1.0 - y) * 0.9;
        shaft || head
    }
    // Where each arrow sits: left, top, and its side, in icon units.
    let arrows = [(0.02, 0.02, 0.62), (0.36, 0.36, 0.62)];
    let n = size as f32;
    let samples = 4;
    let total = samples * samples;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (mut hit, mut upper) = (0, 0);
            for sy in 0..samples {
                for sx in 0..samples {
                    let x = (px as f32 + (sx as f32 + 0.5) / samples as f32) / n;
                    let y = (py as f32 + (sy as f32 + 0.5) / samples as f32) / n;
                    // The lower arrow is drawn over the upper one.
                    let found = arrows
                        .iter()
                        .enumerate()
                        .rev()
                        .find(|(_, (ax, ay, side))| arrow((x - ax) / side, (y - ay) / side));
                    if let Some((i, _)) = found {
                        hit += 1;
                        if i == 0 {
                            upper += 1;
                        }
                    }
                }
            }
            // The upper arrow is a shade darker, so the two read apart.
            let (r, g, b) = if upper * 2 > hit { (22, 140, 44) } else { (46, 190, 70) };
            rgba.extend_from_slice(&[r, g, b, (hit * 255 / total) as u8]);
        }
    }
    rgba
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_all_comes_with_the_second_hidden_window() {
        assert!(!show_all_wanted(1));
        assert!(show_all_wanted(2));
        assert!(show_all_wanted(5));
    }

    #[test]
    fn the_arrows_icon_is_square_and_partly_clear() {
        let rgba = arrows_icon(PROGRESS_ICON_SIZE);
        assert_eq!(rgba.len(), (PROGRESS_ICON_SIZE * PROGRESS_ICON_SIZE * 4) as usize);
        let alpha: Vec<u8> = rgba.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
        assert!(alpha.contains(&0), "the corners are clear");
        assert!(alpha.contains(&255), "the arrows are solid");
    }

    #[test]
    fn mono_keeps_the_size_and_coverage() {
        let rgba = [10, 20, 30, 0, 200, 100, 50, 128, 1, 2, 3, 255];
        let mono = Style::Mono.apply(&rgba);
        assert_eq!(mono.len(), rgba.len());
        assert_eq!(mono, [255, 255, 255, 0, 255, 255, 255, 128, 255, 255, 255, 255]);
        assert_eq!(Style::Color.apply(&rgba), rgba);
    }

    #[test]
    fn mono_of_the_real_icon_is_64_by_64() {
        let rgba = include_bytes!("../../../assets/icon-64.rgba");
        assert_eq!(Style::Mono.apply(rgba).len(), 64 * 64 * 4);
    }
}
