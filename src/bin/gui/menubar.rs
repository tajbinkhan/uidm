//! The main window's menu bar: Tasks, File, Downloads, View and Help, in
//! the layout of the download manager UIDM follows.
//!
//! `model` describes the menus for the window's current state. On Windows
//! that becomes a native menu bar on the window (muda), with Alt mnemonics;
//! elsewhere egui draws it (`show`). A choice from the native bar arrives on
//! the window's thread through muda's event handler, is queued here, and the
//! window takes it on its next frame (`take`).

use std::sync::{Arc, Mutex, Once, OnceLock};

use eframe::egui;
use tray_icon::menu::{MenuEvent, MenuId};

use crate::columns::{Column, Sort};
use crate::i18n::{self, tr};

/// What a menu item asks the window to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    AddUrl,
    AddBatch,
    BatchFromClipboard,
    Grabber,
    ToggleDropTarget,
    Export,
    Import,
    Exit,
    /// The File menu, on the ticked downloads.
    StopTicked,
    RemoveTicked,
    StartTicked,
    RedownloadTicked,
    PauseAll,
    ResumeAll,
    DeleteCompleted,
    Find,
    FindNext,
    Scheduler,
    StartQueue(u64),
    StopQueue(u64),
    SpeedLimit(bool),
    Options,
    ToggleCategories,
    Arrange(Option<Sort>),
    ToggleTrayIcon,
    Columns,
    ToggleDarkMode,
    Language(&'static str),
    Help(&'static str),
    Tip,
    CheckUpdate,
    About,
}

/// One entry of a menu.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Item {
        text: String,
        command: Command,
        enabled: bool,
        /// `Some` for an item with a check mark.
        checked: Option<bool>,
    },
    Separator,
    Menu {
        text: String,
        children: Vec<Node>,
    },
}

/// What the menus depend on.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    pub connected: bool,
    /// Settings-backed checks, once the daemon has sent its settings.
    pub settings: Option<Checks>,
    /// What the ticked downloads allow.
    pub ticked: Ticked,
    /// Every queue: id, name and whether it is running.
    pub queues: Vec<(u64, String, bool)>,
    pub sort: Option<Sort>,
    pub dark: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Checks {
    pub drop_target: bool,
    pub speed_limit: bool,
    pub categories: bool,
    pub tray_icon: bool,
    /// Whether the theme follows Windows (or is dark) rather than light.
    pub dark_mode: bool,
    pub language: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ticked {
    pub any: bool,
    /// Some ticked download is running or about to.
    pub running: bool,
    /// Some ticked download is waiting, stopped or failed.
    pub startable: bool,
    /// Some ticked download is finished or stopped.
    pub finished: bool,
}

fn item(text: &str, command: Command, enabled: bool) -> Node {
    Node::Item {
        text: text.to_string(),
        command,
        enabled,
        checked: None,
    }
}

fn check(text: &str, command: Command, enabled: bool, checked: bool) -> Node {
    Node::Item {
        text: text.to_string(),
        command,
        enabled,
        checked: Some(checked),
    }
}

fn menu(text: &str, children: Vec<Node>) -> Node {
    Node::Menu {
        text: text.to_string(),
        children,
    }
}

/// The menus for `s`. `&` marks the Alt key letter.
pub fn model(s: &State) -> Vec<Node> {
    let on = s.connected;
    let set = s.settings.as_ref().filter(|_| on);
    let checked = |f: fn(&Checks) -> bool| set.is_some_and(f);

    let mut tasks = vec![
        item(tr("&Add new download"), Command::AddUrl, on),
        item(tr("Add &batch download"), Command::AddBatch, on),
        item(tr("Add batch download from &clipboard"), Command::BatchFromClipboard, on),
        item(tr("Run site &grabber"), Command::Grabber, on),
    ];
    // The drop target is only on Windows (see `droptarget`).
    if cfg!(windows) {
        tasks.push(Node::Separator);
        tasks.push(check(tr("Show &drop target"), Command::ToggleDropTarget, set.is_some(), checked(|c| c.drop_target)));
    }
    tasks.extend([
        Node::Separator,
        item(tr("E&xport..."), Command::Export, on),
        item(tr("&Import..."), Command::Import, on),
        Node::Separator,
        item(tr("&Exit"), Command::Exit, true),
    ]);

    let t = &s.ticked;
    let file = vec![
        item(tr("&Stop Download"), Command::StopTicked, on && t.running),
        item(tr("&Remove"), Command::RemoveTicked, on && t.any),
        item(tr("&Download Now"), Command::StartTicked, on && t.startable),
        item(tr("R&edownload"), Command::RedownloadTicked, on && t.finished),
    ];

    let queues = |start: bool| -> Vec<Node> {
        if s.queues.is_empty() {
            return vec![item(tr("(no queues)"), Command::Scheduler, false)];
        }
        s.queues
            .iter()
            .map(|(id, name, running)| {
                let command = if start { Command::StartQueue(*id) } else { Command::StopQueue(*id) };
                item(name, command, on && *running != start)
            })
            .collect()
    };
    let limit = checked(|c| c.speed_limit);
    let downloads = vec![
        item(tr("&Pause All"), Command::PauseAll, on),
        item(tr("&Resume All"), Command::ResumeAll, on),
        Node::Separator,
        item(tr("&Delete All Completed"), Command::DeleteCompleted, on),
        Node::Separator,
        item(tr("Find (Ctrl-F)"), Command::Find, true),
        item(tr("Find Next (F3)"), Command::FindNext, true),
        Node::Separator,
        item(tr("S&cheduler"), Command::Scheduler, on),
        menu(tr("Start &queue"), queues(true)),
        menu(tr("S&top queue"), queues(false)),
        Node::Separator,
        menu(
            tr("Speed &Limiter"),
            vec![
                check(tr("Turn o&n"), Command::SpeedLimit(true), set.is_some() && !limit, limit),
                check(tr("Turn of&f"), Command::SpeedLimit(false), set.is_some() && limit, set.is_some() && !limit),
                Node::Separator,
                item(tr("S&ettings..."), Command::Options, set.is_some()),
            ],
        ),
        Node::Separator,
        item(tr("&Options"), Command::Options, set.is_some()),
    ];

    let arrange = |text: &str, column: Option<Column>| {
        let sort = column.map(|column| Sort {
            column,
            descending: false,
        });
        let current = s.sort.map(|x| x.column) == column;
        check(text, Command::Arrange(sort), true, current)
    };
    let languages = i18n::LANGUAGES
        .iter()
        .map(|l| {
            let current = set.is_some_and(|c| i18n::resolve(&c.language).code == l.code);
            check(l.name, Command::Language(l.code), set.is_some(), current)
        })
        .collect();
    let view = vec![
        check(tr("Hide &categories"), Command::ToggleCategories, set.is_some(), set.is_some_and(|c| !c.categories)),
        menu(
            tr("&Arrange files"),
            vec![
                arrange(tr("By &Order Of Addition"), None),
                arrange(tr("By &File Name"), Some(Column::Name)),
                arrange(tr("By &Size"), Some(Column::Size)),
                arrange(tr("By St&atus"), Some(Column::Status)),
                arrange(tr("By &Time Left"), Some(Column::TimeLeft)),
                arrange(tr("By Transfer &Rate"), Some(Column::Speed)),
                arrange(tr("By &Last Try Date"), Some(Column::LastTry)),
                arrange(tr("By &Description"), Some(Column::Description)),
                arrange(tr("By Save &Path"), Some(Column::SaveTo)),
                arrange(tr("By Parent &Web Page"), Some(Column::Referrer)),
            ],
        ),
        check(tr("Do&n't show the tray icon"), Command::ToggleTrayIcon, set.is_some(), set.is_some_and(|c| !c.tray_icon)),
        item(tr("C&ustomize URL List..."), Command::Columns, true),
        check(tr("Dark Mode support"), Command::ToggleDarkMode, set.is_some(), checked(|c| c.dark_mode)),
        Node::Separator,
        menu(tr("&Language"), languages),
    ];

    let help = vec![
        item(tr("H&elp contents"), Command::Help(crate::help::MANUAL), true),
        item(tr("&Scheduler and queues"), Command::Help(crate::help::SCHEDULER), true),
        item(tr("&Grabber Help"), Command::Help(crate::help::GRABBER), true),
        item(tr("Tip of the &Day..."), Command::Tip, true),
        Node::Separator,
        item(tr("UIDM &Home Page"), Command::Help(crate::help::HOME), true),
        item(tr("&Contact Support"), Command::Help(crate::help::SUPPORT), true),
        Node::Separator,
        item(tr("Check for &updates..."), Command::CheckUpdate, on),
        Node::Separator,
        item(tr("&About UIDM"), Command::About, true),
    ];

    vec![
        menu(tr("&Tasks"), tasks),
        menu(tr("&File"), file),
        menu(tr("&Downloads"), downloads),
        menu(tr("&View"), view),
        menu(tr("&Help"), help),
    ]
}

/// Choices from the native bar, waiting for the next frame.
static PENDING: Mutex<Vec<Command>> = Mutex::new(Vec::new());
/// The native items' IDs and what each one does.
static IDS: Mutex<Vec<(MenuId, Command)>> = Mutex::new(Vec::new());
/// For waking the window when a choice arrives.
static CONTEXT: OnceLock<egui::Context> = OnceLock::new();

type Handler = Arc<dyn Fn(&MenuId) -> bool + Send + Sync>;
static HANDLERS: Mutex<Vec<Handler>> = Mutex::new(Vec::new());

/// Run `handler` for menu choices, from the tray menu and the menu bar,
/// which share muda's one event handler. It returns whether the ID was its
/// own.
pub fn on_menu_event(handler: impl Fn(&MenuId) -> bool + Send + Sync + 'static) {
    HANDLERS.lock().unwrap().push(Arc::new(handler));
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        MenuEvent::set_event_handler(Some(|event: MenuEvent| {
            let handlers = HANDLERS.lock().unwrap().clone();
            for handler in handlers {
                if handler(&event.id) {
                    break;
                }
            }
        }));
    });
}

/// Get ready to take choices from the native bar.
pub fn init(ctx: &egui::Context) {
    let _ = CONTEXT.set(ctx.clone());
    on_menu_event(|id| {
        let command = IDS.lock().unwrap().iter().find(|(i, _)| i == id).map(|(_, c)| c.clone());
        let Some(command) = command else { return false };
        PENDING.lock().unwrap().push(command);
        if let Some(ctx) = CONTEXT.get() {
            ctx.request_repaint();
        }
        true
    });
}

/// The choices made since the last call.
pub fn take() -> Vec<Command> {
    std::mem::take(&mut *PENDING.lock().unwrap())
}

/// Whether the menus are a native bar on the window rather than drawn by egui.
pub fn native() -> bool {
    cfg!(windows)
}

#[cfg(windows)]
mod native_bar {
    use std::cell::RefCell;

    use tray_icon::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};

    use super::{Command, Node, IDS};

    struct Bar {
        hwnd: isize,
        model: Vec<Node>,
        dark: bool,
        menu: Option<Menu>,
    }

    thread_local! {
        static BAR: RefCell<Option<Bar>> = const { RefCell::new(None) };
    }

    pub fn attach(hwnd: isize) {
        BAR.with(|b| {
            *b.borrow_mut() = Some(Bar {
                hwnd,
                model: Vec::new(),
                dark: false,
                menu: None,
            })
        });
    }

    fn build(nodes: &[Node], ids: &mut Vec<(tray_icon::menu::MenuId, Command)>) -> Vec<Box<dyn IsMenuItem>> {
        nodes
            .iter()
            .map(|node| match node {
                Node::Separator => Box::new(PredefinedMenuItem::separator()) as Box<dyn IsMenuItem>,
                Node::Item {
                    text,
                    command,
                    enabled,
                    checked,
                } => {
                    let id = format!("bar.{}", ids.len());
                    ids.push((id.as_str().into(), command.clone()));
                    match checked {
                        Some(on) => Box::new(CheckMenuItem::with_id(id, text, *enabled, *on, None)) as Box<dyn IsMenuItem>,
                        None => Box::new(MenuItem::with_id(id, text, *enabled, None)),
                    }
                }
                Node::Menu { text, children } => {
                    let items = build(children, ids);
                    let refs: Vec<&dyn IsMenuItem> = items.iter().map(|i| i.as_ref()).collect();
                    let id = format!("bar.menu.{}", ids.len());
                    Box::new(Submenu::with_id_and_items(id, text, true, &refs).expect("a new submenu takes its items"))
                }
            })
            .collect()
    }

    /// Put `model` on the window, if it differs from what is there.
    pub fn sync(model: Vec<Node>, dark: bool) {
        BAR.with(|b| {
            let mut b = b.borrow_mut();
            let Some(bar) = b.as_mut() else { return };
            if bar.menu.is_some() && bar.model == model && bar.dark == dark {
                return;
            }
            let mut ids = Vec::new();
            let items = build(&model, &mut ids);
            let refs: Vec<&dyn IsMenuItem> = items.iter().map(|i| i.as_ref()).collect();
            let Ok(menu) = Menu::with_items(&refs) else { return };
            // The old menu takes itself off the window when dropped; that
            // has to happen before the new one goes on, or it would take
            // the new one off too.
            drop(bar.menu.take());
            let theme = if dark {
                tray_icon::menu::MenuTheme::Dark
            } else {
                tray_icon::menu::MenuTheme::Light
            };
            // SAFETY: `hwnd` is the main window's, which outlives the menu.
            if unsafe { menu.init_for_hwnd_with_theme(bar.hwnd, theme) }.is_ok() {
                *IDS.lock().unwrap() = ids;
                bar.menu = Some(menu);
                bar.model = model;
                bar.dark = dark;
            }
        });
    }
}

/// Use a native menu bar on this window.
#[cfg(windows)]
pub fn attach(hwnd: isize) {
    native_bar::attach(hwnd);
}

/// Bring the menus up to date with `state`: the native bar on Windows. Call
/// every frame; nothing is rebuilt unless something changed.
pub fn sync(state: &State) {
    #[cfg(windows)]
    native_bar::sync(model(state), state.dark);
    #[cfg(not(windows))]
    let _ = state;
}

/// Draw the menus with egui, where there is no native bar. Returns the
/// choice made, if any.
pub fn show(ui: &mut egui::Ui, state: &State) -> Option<Command> {
    fn plain(text: &str) -> String {
        text.replace('&', "")
    }
    fn draw(ui: &mut egui::Ui, nodes: &[Node], chosen: &mut Option<Command>) {
        for node in nodes {
            match node {
                Node::Separator => {
                    ui.separator();
                }
                Node::Item {
                    text,
                    command,
                    enabled,
                    checked,
                } => {
                    let text = plain(text);
                    let clicked = match checked {
                        Some(on) => {
                            let mut on = *on;
                            ui.add_enabled(*enabled, egui::Checkbox::new(&mut on, text)).clicked()
                        }
                        None => ui.add_enabled(*enabled, egui::Button::new(text)).clicked(),
                    };
                    if clicked {
                        *chosen = Some(command.clone());
                        ui.close_menu();
                    }
                }
                Node::Menu { text, children } => {
                    ui.menu_button(plain(text), |ui| draw(ui, children, chosen));
                }
            }
        }
    }
    let mut chosen = None;
    egui::menu::bar(ui, |ui| draw(ui, &model(state), &mut chosen));
    chosen
}
