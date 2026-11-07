//! `ThisSystem`, the `SystemServices` adapter: what the window asks of
//! the operating system, from the modules that know it.

use std::path::Path;

use uidm_application::window::{SystemFonts, SystemProxy, SystemServices};
use uidm_domain::ports::FilesError;
use uidm_domain::settings::Sound;

/// The operating system this copy runs on.
pub struct ThisSystem;

impl SystemServices for ThisSystem {
    fn fonts(&self) -> SystemFonts {
        // Segoe UI and Consolas, from the Windows font folder, so nothing
        // is bundled. Elsewhere egui's own fonts are used.
        let read = |file: &str| crate::paths::fonts_dir().and_then(|d| std::fs::read(d.join(file)).ok());
        SystemFonts { text: read("segoeui.ttf"), code: read("consola.ttf") }
    }

    fn fixed_proxy(&self) -> Option<SystemProxy> {
        let rules = crate::net::system_fixed_proxy()?;
        Some(SystemProxy {
            http: rules.http,
            https: rules.https,
            socks: rules.socks,
            ftp: rules.ftp,
            bypass: rules.bypass,
        })
    }

    fn wake_timers_allowed(&self) -> Option<(bool, bool)> {
        crate::power::wake_timers_allowed()
    }

    fn can_elevate(&self) -> bool {
        crate::elevate::supported()
    }

    fn can_set_creation_time(&self) -> bool {
        crate::engine::CAN_SET_CREATION_TIME
    }

    fn play(&self, sound: Sound, file: Option<&Path>) {
        crate::notify::play(sound, file);
    }

    fn save_credential(&self, target: &str, username: &str, password: &str) -> Result<(), FilesError> {
        crate::net::save_credential(target, username, password).map_err(|e| FilesError(e.to_string()))
    }
}
