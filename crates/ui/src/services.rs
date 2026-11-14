//! What Options reaches beyond the settings: the tools UDM can install,
//! remote control and pairing a phone, the browsers UDM is registered with,
//! and the theme files. The state the daemon and the workers report, and
//! what Options shows of it. Plain functions, tested without Tauri.

use udm_application::daemon::{Event, Failure};
use udm_application::themes::ThemesRead;
use udm_application::window::BrowserSlot;
use udm_domain::debrid::DebridCheck;
use udm_domain::pairing::RemoteState;
use udm_domain::settings::{AddedBrowser, Theme};
use udm_domain::tools::{Tool, ToolState};

use crate::contract::{
    Banner, BrowserRowView, Choice, DeviceRow, Id, OptionsView, PairingView, RemoteView, ToolKey, ToolRow,
};
use crate::i18n::{fill, Language};
use crate::projection::{format, Ctx};
use crate::state::AppState;

/// One browser Options lists: one UDM found, or one the user added by its
/// program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserRow {
    /// The browser whose registration it uses.
    pub browser: BrowserSlot,
    pub name: String,
    pub added: Option<AddedBrowser>,
    /// Its place is known rather than guessed from its family.
    pub known: bool,
    pub registered: bool,
    /// UDM can close it and start it again.
    pub can_restart: bool,
}

/// The browsers a worker read, and those that read Chrome's registration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BrowsersRead {
    pub rows: Vec<BrowserRow>,
    pub chrome_readers: Vec<String>,
}

/// What Options shows beyond the settings.
#[derive(Debug, Default)]
pub struct Services {
    pub tools: Vec<ToolState>,
    /// The tools being installed.
    pub tool_busy: Vec<Tool>,
    /// The last install's tool and why it failed, if it did.
    pub tool_done: Option<(Tool, Option<Failure>)>,
    pub remote: Option<RemoteState>,
    /// `None` until a worker read them.
    pub browsers: Option<BrowsersRead>,
    /// What the last browser change said.
    pub browser_note: Option<Banner>,
    /// The theme files, and the system's contrast colors.
    pub themes: ThemesRead,
    /// Check account is waiting for the debrid service.
    pub debrid_checking: bool,
    /// What the last check of the debrid account said.
    pub debrid: Option<DebridCheck>,
}

/// Take what the daemon said of the tools and of remote control.
pub fn take(state: &mut AppState, event: &Event) {
    let s = &mut state.services;
    match event {
        Event::Tools { tools, done, error } => {
            s.tools = tools.clone();
            if let Some(tool) = done {
                s.tool_busy.retain(|t| t != tool);
                s.tool_done = Some((*tool, error.clone()));
            }
        }
        Event::Remote { state } => s.remote = Some(state.clone()),
        Event::Debrid { check } => {
            s.debrid_checking = false;
            s.debrid = Some(check.clone());
        }
        _ => {}
    }
}

fn tool_key(t: Tool) -> ToolKey {
    match t {
        Tool::YtDlp => ToolKey::YtDlp,
        Tool::Ffmpeg => ToolKey::Ffmpeg,
        Tool::GalleryDl => ToolKey::GalleryDl,
    }
}

/// The tool a key names.
pub fn tool_of(k: ToolKey) -> Tool {
    match k {
        ToolKey::YtDlp => Tool::YtDlp,
        ToolKey::Ffmpeg => Tool::Ffmpeg,
        ToolKey::GalleryDl => Tool::GalleryDl,
    }
}

fn tool_rows(lang: &'static Language, s: &Services) -> Vec<ToolRow> {
    s.tools
        .iter()
        .map(|t| {
            let name = t.tool.name();
            let status = match (&t.installed, &t.path) {
                (Some(version), Some(path)) => {
                    fill(lang.tr("Installed by UDM ({version}): {path}"), &[("version", version), ("path", path)])
                }
                (None, Some(path)) => fill(lang.tr("Using {path}"), &[("path", path)]),
                _ => fill(lang.tr("{tool} was not found."), &[("tool", name)]),
            };
            let action = if t.installed.is_some() { "Update {tool}" } else { "Install {tool}" };
            let hint = if !t.installable {
                fill(lang.tr("UDM cannot install it here. Install it with: {command}"), &[("command", &t.command)])
            } else if t.tool == Tool::GalleryDl {
                lang.tr("Downloads the official build from Codeberg and checks it against its published checksum")
                    .to_string()
            } else {
                lang.tr("Downloads the official build from GitHub and checks it against its published checksum")
                    .to_string()
            };
            ToolRow {
                tool: tool_key(t.tool),
                status,
                action: fill(lang.tr(action), &[("tool", name)]),
                hint,
                installable: t.installable,
                busy: s.tool_busy.contains(&t.tool),
            }
        })
        .collect()
}

/// What the last check of the debrid account said.
fn debrid_note(ctx: &Ctx, s: &Services) -> Option<Banner> {
    let lang = ctx.lang;
    let check = s.debrid.as_ref()?;
    let service = check.service.name();
    Some(match &check.outcome {
        Ok(account) => {
            let user = account.username.as_deref().unwrap_or(service);
            let text = match (account.premium, account.premium_until) {
                (true, Some(until)) => fill(
                    lang.tr("The key works: {user}, premium until {date}."),
                    &[("user", user), ("date", &format::date(lang, until, ctx.offset))],
                ),
                (true, None) => fill(lang.tr("The key works: {user}, premium."), &[("user", user)]),
                (false, _) => fill(lang.tr("The key works, but {user} is not a premium account."), &[("user", user)]),
            };
            Banner { text, error: !account.premium }
        }
        Err(e) => Banner { text: format!("{service}: {}", crate::errors::text::debrid(lang, e)), error: true },
    })
}

fn tool_note(lang: &'static Language, s: &Services) -> Option<Banner> {
    let (tool, error) = s.tool_done.as_ref()?;
    Some(match error {
        Some(e) => Banner { text: crate::errors::failure(lang, e), error: true },
        None => Banner { text: fill(lang.tr("{tool} is installed."), &[("tool", tool.name())]), error: false },
    })
}

/// The QR code of `text`, row by row, and its side; `None` when it does
/// not fit in one.
pub fn qr(text: &str) -> Option<(Vec<bool>, u32)> {
    let code = qrcode::QrCode::with_error_correction_level(text, qrcode::EcLevel::M).ok()?;
    let side = code.width() as u32;
    Some((code.to_colors().into_iter().map(|c| c == qrcode::Color::Dark).collect(), side))
}

fn remote_view(ctx: &Ctx, state: &AppState) -> Option<RemoteView> {
    let lang = ctx.lang;
    let r = state.services.remote.as_ref()?;
    let on = state.settings.as_ref().is_some_and(|s| s.remote_enabled);
    let status = match (&r.listener.page, &r.listener.fault, on) {
        (Some(page), _, _) => fill(lang.tr("The page is at {page}"), &[("page", page)]),
        (None, Some(fault), _) => crate::errors::text::remote_fault(lang, fault),
        (None, None, true) => lang.tr("Starting…").to_string(),
        (None, None, false) => lang.tr("Remote control is off.").to_string(),
    };
    let devices = r
        .devices
        .iter()
        .map(|d| {
            let name = if d.name.as_str().is_empty() { lang.tr("Unnamed device") } else { d.name.as_str() };
            let mut detail = fill(lang.tr("Paired {date}"), &[("date", &format::date(lang, d.paired_at, ctx.offset))]);
            if let Some(seen) = d.last_seen {
                detail = format!(
                    "{detail} · {}",
                    fill(lang.tr("Last seen {date}"), &[("date", &format::date(lang, seen, ctx.offset))])
                );
            }
            DeviceRow { id: Id(d.id.get()), name: name.to_string(), detail }
        })
        .collect();
    let pairing = r.pairing.as_ref().map(|p| {
        let (modules, side) = qr(&p.link).unwrap_or_default();
        let page = r.listener.page.clone().unwrap_or_default();
        PairingView {
            link: p.link.clone(),
            code: String::from(p.code.clone()),
            expired: p.expires_at <= ctx.unix,
            modules,
            side,
            type_note: fill(lang.tr("Or open {page} on the phone and type this code:"), &[("page", &page)]),
        }
    });
    Some(RemoteView {
        status,
        error: r.listener.fault.is_some(),
        fingerprint: r.listener.fingerprint.clone(),
        devices,
        pairing,
        can_pair: on && r.listener.page.is_some(),
    })
}

fn browser_rows(lang: &'static Language, b: &BrowsersRead) -> Vec<BrowserRowView> {
    b.rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let detail = match &r.added {
                Some(a) if r.known => fill(
                    lang.tr("{program}; uses {place}'s registration"),
                    &[("program", &a.program), ("place", &r.browser.place)],
                ),
                Some(a) => fill(
                    lang.tr("{program}; registered where {place} looks, which most browsers of its kind read"),
                    &[("program", &a.program), ("place", &r.browser.place)],
                ),
                None => String::new(),
            };
            BrowserRowView {
                index: i as u32,
                name: r.name.clone(),
                detail,
                registered: r.registered,
                added: r.added.is_some(),
                can_restart: r.can_restart,
            }
        })
        .collect()
}

/// The theme list: the built-in looks, then the theme files' themes, and
/// the theme set even when its file is gone.
fn theme_choices(lang: &'static Language, state: &AppState) -> Vec<Choice> {
    let choice = |value: &str, label: &str| Choice { value: value.to_string(), label: lang.tr(label).to_string() };
    let mut out = vec![
        choice("system", "System"),
        choice("light", "Light"),
        choice("dark", "Dark"),
        choice("highContrast", "High contrast"),
    ];
    for t in &state.services.themes.themes {
        out.push(Choice { value: format!("custom:{}", t.name.as_str()), label: t.name.as_str().to_string() });
    }
    if let Some(Theme::Custom(name)) = state.settings.as_ref().map(|s| &s.theme) {
        let value = format!("custom:{}", name.as_str());
        if !out.iter().any(|c| c.value == value) {
            out.push(Choice { value, label: name.as_str().to_string() });
        }
    }
    out
}

/// Options' view with what the daemon and the workers reported.
pub fn fill_view(ctx: &Ctx, state: &AppState, view: &mut OptionsView) {
    let lang = ctx.lang;
    let s = &state.services;
    view.tools = tool_rows(lang, s);
    view.tool_note = tool_note(lang, s);
    view.debrid.checking = s.debrid_checking;
    view.debrid.check = debrid_note(ctx, s);
    view.remote = remote_view(ctx, state);
    view.browsers = s.browsers.as_ref().map(|b| browser_rows(lang, b));
    view.chrome_note = s
        .browsers
        .as_ref()
        .filter(|b| !b.chrome_readers.is_empty())
        .map(|b| {
            fill(lang.tr("{browsers} use Google Chrome's setting."), &[("browsers", &b.chrome_readers.join(", "))])
        })
        .unwrap_or_default();
    view.browser_note = s.browser_note.clone();
    view.creation_time = state.system.creation_time;
    view.portable = state.system.portable;
    view.themes = theme_choices(lang, state);
    view.theme_notes = s
        .themes
        .broken
        .iter()
        .map(|b| {
            fill(
                lang.tr("{file} is not a theme: {why}"),
                &[("file", &b.file), ("why", &crate::errors::text::theme_file(lang, &b.why))],
            )
        })
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{apply, UiMessage};
    use std::time::Instant;

    fn ctx() -> Ctx {
        Ctx {
            lang: crate::i18n::resolve("en", &[]),
            offset: 0,
            today: String::new(),
            now: Instant::now(),
            unix: 1_700_000_000,
            downloads: std::path::PathBuf::from("/home/u/Downloads"),
        }
    }

    #[test]
    fn an_install_reports_its_tool() {
        let mut s = AppState::default();
        s.services.tool_busy.push(Tool::YtDlp);
        let tools = vec![ToolState {
            tool: Tool::YtDlp,
            path: Some("/t/yt-dlp".into()),
            installed: Some("2026.11.01".into()),
            installable: true,
            command: String::new(),
        }];
        let event = Event::Tools { tools, done: Some(Tool::YtDlp), error: None };
        apply(&mut s, UiMessage::Daemon { event: Box::new(event), at: Instant::now(), unix: 0 });
        assert!(s.services.tool_busy.is_empty());
        let mut view = crate::options::view(ctx().lang, &udm_domain::settings::Settings::default());
        fill_view(&ctx(), &s, &mut view);
        assert_eq!(view.tools[0].status, "Installed by UDM (2026.11.01): /t/yt-dlp");
        assert_eq!(view.tools[0].action, "Update yt-dlp");
        assert_eq!(view.tool_note.map(|n| n.text).as_deref(), Some("yt-dlp is installed."));
    }

    #[test]
    fn a_pairing_code_comes_with_its_qr_code() {
        let (modules, side) = qr("https://192.168.1.5:8765/#pair=ABCD").unwrap();
        assert_eq!(modules.len() as u32, side * side);
        assert!(side >= 21);
    }

    #[test]
    fn custom_themes_join_the_list() {
        let s = AppState {
            settings: Some(udm_domain::settings::Settings {
                theme: Theme::Custom(udm_domain::theme::ThemeName::parse("Gone").unwrap()),
                ..Default::default()
            }),
            ..AppState::default()
        };
        let choices = theme_choices(ctx().lang, &s);
        assert_eq!(choices.last().map(|c| c.value.as_str()), Some("custom:Gone"));
    }
}
