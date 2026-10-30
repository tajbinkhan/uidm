//! The browsers UIDM's extension runs in, and the native-messaging
//! registration that lets each one start `uidm-host.exe`.
//!
//! A browser finds the host through a registry key under its own
//! `NativeMessagingHosts` key in `HKEY_CURRENT_USER`, whose default value is
//! the path of a manifest naming the host and the extension allowed to start
//! it. Chromium browsers share one manifest; Firefox names extensions by ID,
//! so it has its own. Setup and `install-host.ps1` register Chrome, Edge and
//! Firefox; Options can register or remove each browser found.

use std::io;
use std::path::{Path, PathBuf};

use crate::registry;

/// The native-messaging host's name, as the extension connects to it.
pub const HOST_NAME: &str = "com.uidm.host";
/// The Chromium extension's ID, fixed by the key in its manifest.
pub const CHROMIUM_EXTENSION_ID: &str = "epmjakacaejhdhldkpbjnakhfmjjbhfm";
/// The Firefox extension's ID, from `extension/firefox.json`.
pub const FIREFOX_EXTENSION_ID: &str = "uidm@uidm.local";

/// A browser UIDM can register with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Browser {
    pub name: &'static str,
    /// Where it looks for native-messaging hosts, under `HKEY_CURRENT_USER`.
    hosts_key: &'static str,
    /// Its profile folder, which shows it is installed for this user: an
    /// environment variable and a path inside the folder it names.
    profile: (&'static str, &'static str),
    firefox: bool,
}

pub const BROWSERS: [Browser; 5] = [
    Browser {
        name: "Google Chrome",
        hosts_key: r"Software\Google\Chrome\NativeMessagingHosts",
        profile: ("LOCALAPPDATA", r"Google\Chrome\User Data"),
        firefox: false,
    },
    Browser {
        name: "Microsoft Edge",
        hosts_key: r"Software\Microsoft\Edge\NativeMessagingHosts",
        profile: ("LOCALAPPDATA", r"Microsoft\Edge\User Data"),
        firefox: false,
    },
    Browser {
        name: "Mozilla Firefox",
        hosts_key: r"Software\Mozilla\NativeMessagingHosts",
        profile: ("APPDATA", r"Mozilla\Firefox"),
        firefox: true,
    },
    Browser {
        name: "Brave",
        hosts_key: r"Software\BraveSoftware\Brave-Browser\NativeMessagingHosts",
        profile: ("LOCALAPPDATA", r"BraveSoftware\Brave-Browser\User Data"),
        firefox: false,
    },
    Browser {
        name: "Chromium",
        hosts_key: r"Software\Chromium\NativeMessagingHosts",
        profile: ("LOCALAPPDATA", r"Chromium\User Data"),
        firefox: false,
    },
];

/// Chromium browsers that read Chrome's registration instead of keeping
/// their own, with their profile folders.
pub const READ_CHROMES: [(&str, (&str, &str)); 2] = [
    ("Vivaldi", ("LOCALAPPDATA", r"Vivaldi\User Data")),
    ("Opera", ("APPDATA", r"Opera Software\Opera Stable")),
];

fn profile_exists((var, path): (&str, &str)) -> bool {
    std::env::var_os(var).is_some_and(|base| Path::new(&base).join(path).is_dir())
}

/// The browsers in `READ_CHROMES` installed for this user.
pub fn chrome_readers() -> Vec<&'static str> {
    READ_CHROMES.iter().filter(|(_, p)| profile_exists(*p)).map(|(name, _)| *name).collect()
}

/// Where registrations go: the registry keys under `root` (empty for the
/// real ones; tests use their own) and the manifests in `dir`.
#[derive(Debug, Clone)]
pub struct Registrar {
    root: String,
    dir: PathBuf,
    /// The `uidm-host.exe` the manifests point at.
    host: PathBuf,
}

impl Registrar {
    /// The real registry, with the manifests in `%LOCALAPPDATA%\UIDM` as
    /// `install-host.ps1` writes them, for the host next to this program.
    pub fn for_this_user() -> io::Result<Self> {
        let dir = crate::history::history_path()?
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no folder for UIDM's data"))?;
        let host = std::env::current_exe()?.with_file_name("uidm-host.exe");
        Ok(Registrar { root: String::new(), dir, host })
    }

    #[cfg(test)]
    fn new(root: &str, dir: PathBuf, host: PathBuf) -> Self {
        Registrar { root: root.to_string(), dir, host }
    }

    fn key(&self, b: &Browser) -> String {
        format!(r"{}{}\{HOST_NAME}", self.root, b.hosts_key)
    }

    fn manifest_path(&self, firefox: bool) -> PathBuf {
        self.dir.join(if firefox { format!("{HOST_NAME}.firefox.json") } else { format!("{HOST_NAME}.json") })
    }

    /// Whether `b` is installed for this user.
    pub fn installed(&self, b: &Browser) -> bool {
        profile_exists(b.profile)
    }

    /// Whether `b` has a registration naming a manifest.
    pub fn registered(&self, b: &Browser) -> bool {
        registry::get_string(&self.key(b), "").ok().flatten().is_some_and(|m| !m.trim().is_empty())
    }

    /// Register `b`: write its family's manifest, pointing at the host, and
    /// its key naming the manifest.
    pub fn register(&self, b: &Browser) -> io::Result<()> {
        if !self.host.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} is missing", self.host.display()),
            ));
        }
        let allowed = if b.firefox {
            serde_json::json!({ "allowed_extensions": [FIREFOX_EXTENSION_ID] })
        } else {
            serde_json::json!({ "allowed_origins": [format!("chrome-extension://{CHROMIUM_EXTENSION_ID}/")] })
        };
        let mut manifest = serde_json::json!({
            "name": HOST_NAME,
            "description": "UIDM download manager native-messaging host",
            "type": "stdio",
            "path": self.host.display().to_string(),
        });
        manifest.as_object_mut().unwrap().extend(allowed.as_object().unwrap().clone());
        let path = self.manifest_path(b.firefox);
        let bytes = serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?;
        crate::settings::write_atomic(&path, &bytes)?;
        registry::set_string(&self.key(b), "", &path.display().to_string())
    }

    /// Remove `b`'s registration, and the manifest it named when that is
    /// UIDM's own and no other browser still names it.
    pub fn unregister(&self, b: &Browser) -> io::Result<()> {
        let key = self.key(b);
        let named = registry::get_string(&key, "").ok().flatten();
        registry::delete_key(&key)?;
        let ours = self.manifest_path(b.firefox);
        let still_named = BROWSERS.iter().any(|other| {
            registry::get_string(&self.key(other), "").ok().flatten().is_some_and(|m| Path::new(&m) == ours)
        });
        if named.is_some_and(|m| Path::new(&m) == ours) && !still_named {
            match std::fs::remove_file(&ours) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registering_writes_the_key_and_manifest_and_removing_undoes_them() {
        let dir = std::env::temp_dir().join(format!("uidm_browsers_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let host = dir.join("uidm-host.exe");
        std::fs::write(&host, b"").unwrap();
        let r = Registrar::new(r"Software\UIDM-test-browsers\", dir.clone(), host.clone());
        let [chrome, edge, firefox, ..] = BROWSERS;

        assert!(!r.registered(&chrome));
        r.register(&chrome).unwrap();
        r.register(&edge).unwrap();
        r.register(&firefox).unwrap();
        assert!(r.registered(&chrome) && r.registered(&edge) && r.registered(&firefox));
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("com.uidm.host.json")).unwrap()).unwrap();
        assert_eq!(manifest["path"], host.display().to_string());
        assert_eq!(manifest["allowed_origins"][0], format!("chrome-extension://{CHROMIUM_EXTENSION_ID}/"));
        let fx: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("com.uidm.host.firefox.json")).unwrap()).unwrap();
        assert_eq!(fx["allowed_extensions"][0], FIREFOX_EXTENSION_ID);

        // Edge still names the shared manifest, so it stays.
        r.unregister(&chrome).unwrap();
        assert!(!r.registered(&chrome));
        assert!(dir.join("com.uidm.host.json").exists());
        r.unregister(&edge).unwrap();
        assert!(!dir.join("com.uidm.host.json").exists());
        r.unregister(&firefox).unwrap();
        assert!(!dir.join("com.uidm.host.firefox.json").exists());
        // Removing what is not there is fine.
        r.unregister(&firefox).unwrap();

        let missing = Registrar::new(r"Software\UIDM-test-browsers\", dir.clone(), dir.join("nope.exe"));
        assert!(missing.register(&chrome).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
