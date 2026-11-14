//! What each window shows (AGENTS.md section 7.2): plain functions from the
//! state to a window's view, and the patches between two views.

pub mod dialogs;
pub mod format;
pub mod main;

use udm_domain::theme::appearance;

use crate::contract::{Look, View};
use crate::i18n::{self, Language};
use crate::state::AppState;
use crate::windows::Label;

/// What the projections need beyond the state: the language and the local
/// time's offset from UTC, in minutes.
pub struct Ctx {
    pub lang: &'static Language,
    pub offset: i64,
    /// Today's local date, `YYYY-MM-DD`.
    pub today: String,
    /// Now, for countdowns.
    pub now: std::time::Instant,
    /// Now, in seconds since 1970, for a pairing code's end.
    pub unix: u64,
    /// The system's Downloads folder, for default folders.
    pub downloads: std::path::PathBuf,
    /// Whether the system draws flags from their regional indicator
    /// letters; Windows draws the letters.
    pub flags: bool,
}

/// The language the settings name.
pub fn language(state: &AppState) -> &'static Language {
    let setting = state.settings.as_ref().map(|s| s.language.as_str()).unwrap_or_default();
    i18n::resolve(setting, &state.system_languages)
}

/// The theme and language every window draws with.
pub fn look(state: &AppState) -> Look {
    let lang = language(state);
    let theme = state.settings.as_ref().map(|s| s.theme.clone()).unwrap_or_default();
    Look {
        theme: crate::theme::theme(&appearance(
            &theme,
            state.services.themes.contrast.as_ref(),
            &state.services.themes.themes,
        )),
        language: lang.code.to_string(),
        right_to_left: lang.right_to_left(),
        zoom: state.settings.as_ref().map_or(100, |s| s.text_size),
        text: lang.table().iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
    }
}

/// The view of the window `label`, or `None` when its subject is gone.
pub fn view(ctx: &Ctx, state: &AppState, label: &Label) -> Option<View> {
    Some(match label {
        Label::Main => View::Main(main::view(ctx, state)),
        Label::AddUrl => View::AddUrl(dialogs::add_url(state)),
        Label::SaveAs(id) => View::SaveAs(dialogs::save_as(ctx, state, *id)?),
        Label::Download(id) => View::Download(dialogs::download(ctx, state, *id)?),
        Label::Done(id) => View::Done(dialogs::done(ctx, state, *id)?),
        Label::Remove => View::Remove(dialogs::remove(state)?),
        Label::About => View::About(dialogs::about(ctx, state)),
        Label::Login => View::Login(crate::extras::login_view(state)?),
        Label::Countdown => View::Countdown(crate::extras::countdown_view(ctx, state)?),
        Label::Refresh => View::Refresh(crate::extras::refresh_view(state)?),
        Label::Password => View::Password(crate::extras::password_view(state)?),
        Label::PickLinks => View::PickLinks(crate::extras::links_view(ctx, state)?),
        Label::PickVideos => View::PickVideos(crate::extras::videos_view(ctx, state)?),
        Label::Gallery => View::Gallery(crate::extras::gallery_view(ctx, state, &ctx.downloads)?),
        Label::ZipPreview => View::ZipPreview(crate::filework::zip_view(ctx.lang, state)?),
        Label::Exception => View::Exception(crate::filework::exception_view(state)?),
        Label::Batch => View::Batch(crate::contract::BatchView { connected: state.connected() }),
        Label::Links => View::Links(crate::tools::links_view(ctx, state)?),
        Label::Properties(id) => View::Properties(crate::tools::properties_view(ctx, state, *id)?),
        Label::DropTarget => View::DropTarget(crate::contract::DropView { connected: state.connected() }),
        Label::Grabber => View::Grabber(Box::new(crate::grabber::view(ctx, state))),
        Label::Scheduler => View::Scheduler(crate::scheduler::view(ctx, state)),
        Label::Options => {
            let mut view = crate::options::view(ctx.lang, state.settings.as_ref()?);
            crate::services::fill_view(ctx, state, &mut view);
            View::Options(Box::new(view))
        }
    })
}
