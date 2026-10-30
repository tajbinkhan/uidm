//! The browsers UIDM's extension runs in, and the native-messaging
//! registration that lets each one start `uidm-host`.
//!
//! On Windows a browser finds the host through a registry key under its
//! own `NativeMessagingHosts` key in `HKEY_CURRENT_USER`, whose default
//! value is the path of a manifest naming the host and the extension
//! allowed to start it. Chromium browsers share one manifest; Firefox names
//! extensions by ID, so it has its own. Setup and `install-host.ps1`
//! register Chrome, Edge and Firefox.
//!
//! On macOS and Linux there is no registry: each browser reads
//! `com.uidm.host.json` from its own `NativeMessagingHosts` folder, so the
//! manifest itself is the registration. The packages' `install-host.sh`
//! registers every browser found. Options can register or remove each
//! browser on every system.

use std::io;
use std::path::{Path, PathBuf};

#[cfg(windows)]
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
    /// Where it looks for native-messaging hosts: a key under
    /// `HKEY_CURRENT_USER` on Windows, a folder under the home folder
    /// elsewhere.
    hosts_key: &'static str,
    /// Its profile folder, which shows it is installed for this user: an
    /// environment variable and a path inside the folder it names.
    profile: (&'static str, &'static str),
    firefox: bool,
}

#[cfg(windows)]
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
#[cfg(windows)]
pub const READ_CHROMES: [(&str, (&str, &str)); 2] = [
    ("Vivaldi", ("LOCALAPPDATA", r"Vivaldi\User Data")),
    ("Opera", ("APPDATA", r"Opera Software\Opera Stable")),
];

#[cfg(all(unix, not(target_os = "macos")))]
pub const BROWSERS: [Browser; 6] = [
    Browser {
        name: "Google Chrome",
        hosts_key: ".config/google-chrome/NativeMessagingHosts",
        profile: ("HOME", ".config/google-chrome"),
        firefox: false,
    },
    Browser {
        name: "Microsoft Edge",
        hosts_key: ".config/microsoft-edge/NativeMessagingHosts",
        profile: ("HOME", ".config/microsoft-edge"),
        firefox: false,
    },
    Browser {
        name: "Mozilla Firefox",
        hosts_key: ".mozilla/native-messaging-hosts",
        profile: ("HOME", ".mozilla"),
        firefox: true,
    },
    Browser {
        name: "Brave",
        hosts_key: ".config/BraveSoftware/Brave-Browser/NativeMessagingHosts",
        profile: ("HOME", ".config/BraveSoftware/Brave-Browser"),
        firefox: false,
    },
    Browser {
        name: "Chromium",
        hosts_key: ".config/chromium/NativeMessagingHosts",
        profile: ("HOME", ".config/chromium"),
        firefox: false,
    },
    Browser {
        name: "Vivaldi",
        hosts_key: ".config/vivaldi/NativeMessagingHosts",
        profile: ("HOME", ".config/vivaldi"),
        firefox: false,
    },
];

#[cfg(target_os = "macos")]
pub const BROWSERS: [Browser; 6] = [
    Browser {
        name: "Google Chrome",
        hosts_key: "Library/Application Support/Google/Chrome/NativeMessagingHosts",
        profile: ("HOME", "Library/Application Support/Google/Chrome"),
        firefox: false,
    },
    Browser {
        name: "Microsoft Edge",
        hosts_key: "Library/Application Support/Microsoft Edge/NativeMessagingHosts",
        profile: ("HOME", "Library/Application Support/Microsoft Edge"),
        firefox: false,
    },
    Browser {
        name: "Mozilla Firefox",
        hosts_key: "Library/Application Support/Mozilla/NativeMessagingHosts",
        profile: ("HOME", "Library/Application Support/Firefox"),
        firefox: true,
    },
    Browser {
        name: "Brave",
        hosts_key: "Library/Application Support/BraveSoftware/Brave-Browser/NativeMessagingHosts",
        profile: ("HOME", "Library/Application Support/BraveSoftware/Brave-Browser"),
        firefox: false,
    },
    Browser {
        name: "Chromium",
        hosts_key: "Library/Application Support/Chromium/NativeMessagingHosts",
        profile: ("HOME", "Library/Application Support/Chromium"),
        firefox: false,
    },
    Browser {
        name: "Vivaldi",
        hosts_key: "Library/Application Support/Vivaldi/NativeMessagingHosts",
        profile: ("HOME", "Library/Application Support/Vivaldi"),
        firefox: false,
    },
];

/// Chromium browsers that read Chrome's registration instead of keeping
/// their own, with their profile folders.
#[cfg(all(unix, not(target_os = "macos")))]
pub const READ_CHROMES: [(&str, (&str, &str)); 1] = [("Opera", ("HOME", ".config/opera"))];
#[cfg(target_os = "macos")]
pub const READ_CHROMES: [(&str, (&str, &str)); 1] = [("Opera", ("HOME", "Library/Application Support/com.operasoftware.Opera"))];

fn profile_exists((var, path): (&str, &str)) -> bool {
    std::env::var_os(var).is_some_and(|base| Path::new(&base).join(path).is_dir())
}

/// The browsers in `READ_CHROMES` installed for this user.
pub fn chrome_readers() -> Vec<&'static str> {
    READ_CHROMES.iter().filter(|(_, p)| profile_exists(*p)).map(|(name, _)| *name).collect()
}

/// Where registrations go: on Windows the registry keys under `root`
/// (empty for the real ones; tests use their own) and the manifests in
/// `dir`; elsewhere the manifests in each browser's folder under `root`
/// (the home folder; tests use their own).
#[derive(Debug, Clone)]
pub struct Registrar {
    root: String,
    #[cfg_attr(not(windows), allow(dead_code))]
    dir: PathBuf,
    /// The `uidm-host` the manifests point at.
    host: PathBuf,
}

impl Registrar {
    /// The real registrations for the host next to this program: on
    /// Windows with the manifests in `%LOCALAPPDATA%\UIDM` as
    /// `install-host.ps1` writes them, elsewhere in the home folder.
    pub fn for_this_user() -> io::Result<Self> {
        let dir = crate::platform::data_dir()?;
        // An AppImage's AppRun starts the host when a browser calls it.
        let host = match crate::platform::appimage() {
            Some(image) => image,
            None => crate::platform::sibling_exe("uidm-host")?,
        };
        #[cfg(windows)]
        let root = String::new();
        #[cfg(not(windows))]
        let root = crate::platform::home_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?
            .display()
            .to_string();
        Ok(Registrar { root, dir, host })
    }

    #[cfg(test)]
    fn new(root: &str, dir: PathBuf, host: PathBuf) -> Self {
        Registrar { root: root.to_string(), dir, host }
    }

    #[cfg(windows)]
    fn key(&self, b: &Browser) -> String {
        format!(r"{}{}\{HOST_NAME}", self.root, b.hosts_key)
    }

    #[cfg(windows)]
    fn manifest_path(&self, firefox: bool) -> PathBuf {
        self.dir.join(if firefox { format!("{HOST_NAME}.firefox.json") } else { format!("{HOST_NAME}.json") })
    }

    /// The manifest `b` reads, which is its registration.
    #[cfg(not(windows))]
    fn manifest_path(&self, b: &Browser) -> PathBuf {
        Path::new(&self.root).join(b.hosts_key).join(format!("{HOST_NAME}.json"))
    }

    /// Whether `b` is installed for this user.
    pub fn installed(&self, b: &Browser) -> bool {
        profile_exists(b.profile)
    }

    /// `b`'s manifest, pointing at the host.
    fn manifest(&self, b: &Browser) -> io::Result<Vec<u8>> {
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
        serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)
    }

    /// Whether `b` has a registration: its manifest is there.
    #[cfg(not(windows))]
    pub fn registered(&self, b: &Browser) -> bool {
        self.manifest_path(b).is_file()
    }

    /// Register `b`: write its manifest, pointing at the host.
    #[cfg(not(windows))]
    pub fn register(&self, b: &Browser) -> io::Result<()> {
        let bytes = self.manifest(b)?;
        crate::settings::write_atomic(&self.manifest_path(b), &bytes)
    }

    /// Remove `b`'s registration, its manifest.
    #[cfg(not(windows))]
    pub fn unregister(&self, b: &Browser) -> io::Result<()> {
        match std::fs::remove_file(self.manifest_path(b)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Whether `b` has a registration naming a manifest.
    #[cfg(windows)]
    pub fn registered(&self, b: &Browser) -> bool {
        registry::get_string(&self.key(b), "").ok().flatten().is_some_and(|m| !m.trim().is_empty())
    }

    /// Register `b`: write its family's manifest, pointing at the host, and
    /// its key naming the manifest.
    #[cfg(windows)]
    pub fn register(&self, b: &Browser) -> io::Result<()> {
        let bytes = self.manifest(b)?;
        let path = self.manifest_path(b.firefox);
        crate::settings::write_atomic(&path, &bytes)?;
        registry::set_string(&self.key(b), "", &path.display().to_string())
    }

    /// Remove `b`'s registration, and the manifest it named when that is
    /// UIDM's own and no other browser still names it.
    #[cfg(windows)]
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

    #[cfg(not(windows))]
    #[test]
    fn registering_writes_the_manifest_and_removing_deletes_it() {
        let dir = std::env::temp_dir().join(format!("uidm_browsers_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let host = dir.join("uidm-host");
        std::fs::write(&host, b"").unwrap();
        let r = Registrar::new(&dir.display().to_string(), dir.clone(), host.clone());
        let [chrome, _, firefox, ..] = BROWSERS;
        assert!(!r.registered(&chrome));
        r.register(&chrome).unwrap();
        r.register(&firefox).unwrap();
        assert!(r.registered(&chrome) && r.registered(&firefox));
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join(chrome.hosts_key).join("com.uidm.host.json")).unwrap()).unwrap();
        assert_eq!(manifest["path"], host.display().to_string());
        assert_eq!(manifest["allowed_origins"][0], format!("chrome-extension://{CHROMIUM_EXTENSION_ID}/"));
        let fx: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join(firefox.hosts_key).join("com.uidm.host.json")).unwrap()).unwrap();
        assert_eq!(fx["allowed_extensions"][0], FIREFOX_EXTENSION_ID);
        r.unregister(&chrome).unwrap();
        assert!(!r.registered(&chrome) && r.registered(&firefox));
        r.unregister(&chrome).unwrap();
        let missing = Registrar::new(&dir.display().to_string(), dir.clone(), dir.join("nope"));
        assert!(missing.register(&chrome).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(windows)]
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
