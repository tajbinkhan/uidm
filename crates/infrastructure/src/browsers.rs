//! The browsers UDM's extension runs in, and the native-messaging
//! registration that lets each one start `udm-host`.
//!
//! On Windows a browser finds the host through a registry key under its
//! own `NativeMessagingHosts` key in `HKEY_CURRENT_USER`, whose default
//! value is the path of a manifest naming the host and the extension
//! allowed to start it. Chromium browsers share one manifest; Firefox names
//! extensions by ID, so it has its own. Setup and `install-host.ps1`
//! register Chrome, Edge and Firefox.
//!
//! On macOS and Linux there is no registry: each browser reads
//! `com.udm.host.json` from its own `NativeMessagingHosts` folder, so the
//! manifest itself is the registration. The packages' `install-host.sh`
//! registers every browser found. Options can register or remove each
//! browser on every system.

use std::io;
use std::path::{Path, PathBuf};

use crate::paths::Root;
#[cfg(windows)]
use crate::registry;

/// The native-messaging host's name, as the extension connects to it.
pub const HOST_NAME: &str = "com.udm.host";
/// The Chromium extension IDs the host lets in. The first is the unpacked
/// copy's, fixed by the key in `extension/chromium/manifest.json`, which
/// developers and Load unpacked keep using. A store assigns a listed copy
/// an ID of its own: add the Chrome Web Store's (and any other store's)
/// after it, here and in the files `every_file_lets_in_the_same_extensions`
/// names (`docs/extension-stores.md`).
pub const CHROMIUM_EXTENSION_IDS: &[&str] = &["epmjakacaejhdhldkpbjnakhfmjjbhfm"];
/// The Firefox extension's ID, from `extension/firefox.json`. addons.mozilla.org
/// keeps the ID the manifest gives, so the signed copy has it too.
pub const FIREFOX_EXTENSION_ID: &str = "udm@udm.local";

/// The key and value of the host manifest that name the extensions allowed
/// to start the host: Firefox's by ID, Chromium's by origin.
fn allowed_extensions(firefox: bool) -> (&'static str, serde_json::Value) {
    if firefox {
        ("allowed_extensions", serde_json::json!([FIREFOX_EXTENSION_ID]))
    } else {
        let origins: Vec<String> =
            CHROMIUM_EXTENSION_IDS.iter().map(|id| format!("chrome-extension://{id}/")).collect();
        ("allowed_origins", serde_json::json!(origins))
    }
}

/// Whether the host manifest `manifest` lets in every extension UDM allows
/// now. One that lets in more stays as it is: an ID passed to
/// `install-host.ps1`, a manifest edited by hand, or another copy of UDM
/// that knows a newer list.
fn lets_in_all(manifest: &serde_json::Value, firefox: bool) -> bool {
    let (key, allowed) = allowed_extensions(firefox);
    let listed = manifest[key].as_array();
    allowed.as_array().is_some_and(|wanted| wanted.iter().all(|w| listed.is_some_and(|l| l.contains(w))))
}

/// A browser UDM can register with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Browser {
    pub name: &'static str,
    /// Where it looks for native-messaging hosts: a key under
    /// `HKEY_CURRENT_USER` on Windows, a folder under the home folder
    /// elsewhere.
    hosts_key: &'static str,
    /// Its profile folder, which shows it is installed for this user: a
    /// base folder and a path inside it.
    profile: (Root, &'static str),
    firefox: bool,
    /// How Options restarts it: its process name and what starts it (a
    /// program found the way Run finds it on Windows, a command on `PATH`
    /// on Linux, the app's name on macOS). `None` when there is no name
    /// that is only its own: Chromium on Windows runs as `chrome.exe`.
    run: Option<(&'static str, &'static str)>,
}

#[cfg(windows)]
pub const BROWSERS: [Browser; 5] = [
    Browser {
        name: "Google Chrome",
        hosts_key: r"Software\Google\Chrome\NativeMessagingHosts",
        profile: (Root::Local, r"Google\Chrome\User Data"),
        firefox: false,
        run: Some(("chrome.exe", "chrome.exe")),
    },
    Browser {
        name: "Microsoft Edge",
        hosts_key: r"Software\Microsoft\Edge\NativeMessagingHosts",
        profile: (Root::Local, r"Microsoft\Edge\User Data"),
        firefox: false,
        run: Some(("msedge.exe", "msedge.exe")),
    },
    Browser {
        name: "Mozilla Firefox",
        hosts_key: r"Software\Mozilla\NativeMessagingHosts",
        profile: (Root::Roaming, r"Mozilla\Firefox"),
        firefox: true,
        run: Some(("firefox.exe", "firefox.exe")),
    },
    Browser {
        name: "Brave",
        hosts_key: r"Software\BraveSoftware\Brave-Browser\NativeMessagingHosts",
        profile: (Root::Local, r"BraveSoftware\Brave-Browser\User Data"),
        firefox: false,
        run: Some(("brave.exe", "brave.exe")),
    },
    Browser {
        name: "Chromium",
        hosts_key: r"Software\Chromium\NativeMessagingHosts",
        profile: (Root::Local, r"Chromium\User Data"),
        firefox: false,
        run: None,
    },
];

/// Chromium browsers that read Chrome's registration instead of keeping
/// their own, with their profile folders.
#[cfg(windows)]
pub const READ_CHROMES: [(&str, (Root, &str)); 2] =
    [("Vivaldi", (Root::Local, r"Vivaldi\User Data")), ("Opera", (Root::Roaming, r"Opera Software\Opera Stable"))];

#[cfg(all(unix, not(target_os = "macos")))]
pub const BROWSERS: [Browser; 6] = [
    Browser {
        name: "Google Chrome",
        hosts_key: ".config/google-chrome/NativeMessagingHosts",
        profile: (Root::Home, ".config/google-chrome"),
        firefox: false,
        run: Some(("chrome", "google-chrome")),
    },
    Browser {
        name: "Microsoft Edge",
        hosts_key: ".config/microsoft-edge/NativeMessagingHosts",
        profile: (Root::Home, ".config/microsoft-edge"),
        firefox: false,
        run: Some(("msedge", "microsoft-edge")),
    },
    Browser {
        name: "Mozilla Firefox",
        hosts_key: ".mozilla/native-messaging-hosts",
        profile: (Root::Home, ".mozilla"),
        firefox: true,
        run: Some(("firefox", "firefox")),
    },
    Browser {
        name: "Brave",
        hosts_key: ".config/BraveSoftware/Brave-Browser/NativeMessagingHosts",
        profile: (Root::Home, ".config/BraveSoftware/Brave-Browser"),
        firefox: false,
        run: Some(("brave", "brave-browser")),
    },
    Browser {
        name: "Chromium",
        hosts_key: ".config/chromium/NativeMessagingHosts",
        profile: (Root::Home, ".config/chromium"),
        firefox: false,
        run: Some(("chromium", "chromium")),
    },
    Browser {
        name: "Vivaldi",
        hosts_key: ".config/vivaldi/NativeMessagingHosts",
        profile: (Root::Home, ".config/vivaldi"),
        firefox: false,
        run: Some(("vivaldi-bin", "vivaldi")),
    },
];

#[cfg(target_os = "macos")]
pub const BROWSERS: [Browser; 6] = [
    Browser {
        name: "Google Chrome",
        hosts_key: "Library/Application Support/Google/Chrome/NativeMessagingHosts",
        profile: (Root::Home, "Library/Application Support/Google/Chrome"),
        firefox: false,
        run: Some(("Google Chrome", "Google Chrome")),
    },
    Browser {
        name: "Microsoft Edge",
        hosts_key: "Library/Application Support/Microsoft Edge/NativeMessagingHosts",
        profile: (Root::Home, "Library/Application Support/Microsoft Edge"),
        firefox: false,
        run: Some(("Microsoft Edge", "Microsoft Edge")),
    },
    Browser {
        name: "Mozilla Firefox",
        hosts_key: "Library/Application Support/Mozilla/NativeMessagingHosts",
        profile: (Root::Home, "Library/Application Support/Firefox"),
        firefox: true,
        run: Some(("Firefox", "Firefox")),
    },
    Browser {
        name: "Brave",
        hosts_key: "Library/Application Support/BraveSoftware/Brave-Browser/NativeMessagingHosts",
        profile: (Root::Home, "Library/Application Support/BraveSoftware/Brave-Browser"),
        firefox: false,
        run: Some(("Brave Browser", "Brave Browser")),
    },
    Browser {
        name: "Chromium",
        hosts_key: "Library/Application Support/Chromium/NativeMessagingHosts",
        profile: (Root::Home, "Library/Application Support/Chromium"),
        firefox: false,
        run: Some(("Chromium", "Chromium")),
    },
    Browser {
        name: "Vivaldi",
        hosts_key: "Library/Application Support/Vivaldi/NativeMessagingHosts",
        profile: (Root::Home, "Library/Application Support/Vivaldi"),
        firefox: false,
        run: Some(("Vivaldi", "Vivaldi")),
    },
];

/// Chromium browsers that read Chrome's registration instead of keeping
/// their own, with their profile folders.
#[cfg(all(unix, not(target_os = "macos")))]
pub const READ_CHROMES: [(&str, (Root, &str)); 1] = [("Opera", (Root::Home, ".config/opera"))];
#[cfg(target_os = "macos")]
pub const READ_CHROMES: [(&str, (Root, &str)); 1] =
    [("Opera", (Root::Home, "Library/Application Support/com.operasoftware.Opera"))];

fn profile_exists((base, path): (Root, &str)) -> bool {
    crate::paths::root(base).is_some_and(|base| base.join(path).is_dir())
}

/// The browsers in `READ_CHROMES` installed for this user.
pub fn chrome_readers() -> Vec<&'static str> {
    READ_CHROMES.iter().filter(|(_, p)| profile_exists(*p)).map(|(name, _)| *name).collect()
}

/// The built-in browser named `name`.
fn builtin(name: &str) -> Option<Browser> {
    BROWSERS.iter().find(|b| b.name == name).copied()
}

impl Browser {
    /// Whether `other` is registered in the same place, so registering one
    /// registers both.
    pub fn same_place(&self, other: &Browser) -> bool {
        self.hosts_key == other.hosts_key
    }

    /// How to restart it, when UDM knows its program.
    pub fn restart_plan(&self) -> Option<RestartPlan> {
        let (process, launch) = self.run?;
        let launch = if cfg!(target_os = "macos") { Launch::App(launch) } else { Launch::Named(launch) };
        Some(restart_plan(Os::CURRENT, process, launch))
    }
}

/// The two kinds of browser the extension comes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Chrome and the browsers built on Chromium, which run the Chrome
    /// extension and read Chromium-style registrations.
    Chromium,
    /// Firefox and its forks, which run the Firefox extension.
    Firefox,
}

/// The name of the program at `program` as its maker spells it, lowercase:
/// the `.app` bundle's on macOS, else the file's, without `.exe`.
fn program_name(program: &Path) -> String {
    let bundle = program.ancestors().find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")));
    let named = bundle.unwrap_or(program);
    let stem = named.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    stem.trim_end_matches(".exe").to_string()
}

/// The words in program names of the Firefox family.
const FIREFOX_NAMES: [&str; 9] =
    ["firefox", "librewolf", "waterfox", "floorp", "zen", "mullvad", "palemoon", "seamonkey", "tor browser"];
/// The words in program names of the Chromium family.
const CHROMIUM_NAMES: [&str; 12] = [
    "chrome", "msedge", "edge", "brave", "vivaldi", "opera", "chromium", "yandex", "browser", "thorium", "cromite",
    "iron",
];

/// The family of the browser at `program`, told from its program's name,
/// or `None` when the name says nothing UDM knows.
pub fn family_of(program: &Path) -> Option<Family> {
    let name = program_name(program);
    if FIREFOX_NAMES.iter().any(|w| name.contains(w)) {
        Some(Family::Firefox)
    } else if CHROMIUM_NAMES.iter().any(|w| name.contains(w)) {
        Some(Family::Chromium)
    } else {
        None
    }
}

/// Where a browser added by its program registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// The browser whose registration it reads.
    pub browser: Browser,
    /// Whether that is known (the browser is one UDM knows, or a fork
    /// known to read it), not a best guess for a fork UDM does not know.
    pub known: bool,
}

/// Where the browser at `program` finds native-messaging hosts. Each
/// Chromium browser reads its maker's own place; a fork UDM does not know
/// gets Chrome's, which most forks read, and a Firefox fork Mozilla's.
/// `None` when its family cannot be told.
pub fn target_of(program: &Path) -> Option<Target> {
    let name = program_name(program);
    let known = |b: &str| builtin(b).map(|browser| Target { browser, known: true });
    let guess = |b: &str| builtin(b).map(|browser| Target { browser, known: false });
    match family_of(program)? {
        Family::Firefox if name.contains("firefox") => known("Mozilla Firefox"),
        Family::Firefox => guess("Mozilla Firefox"),
        Family::Chromium if name.contains("edge") => known("Microsoft Edge"),
        Family::Chromium if name.contains("brave") => known("Brave"),
        Family::Chromium if name.contains("vivaldi") => known("Vivaldi").or_else(|| known("Google Chrome")),
        // Opera reads Chrome's registration, as `READ_CHROMES` says.
        Family::Chromium if name.contains("opera") => known("Google Chrome"),
        Family::Chromium if name.contains("chromium") => known("Chromium"),
        Family::Chromium if name.contains("chrome") => known("Google Chrome"),
        Family::Chromium => guess("Google Chrome"),
    }
}

/// A plain name for the browser at `program`, from its program's name:
/// `thorium-browser.exe` is "Thorium Browser".
pub fn display_name(program: &Path) -> String {
    let name = program_name(program);
    name.split(['-', '_', ' ', '.'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect::<String>()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The system a restart is planned for, so every system's plan can be
/// tested on any one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    MacOs,
    Linux,
}

impl Os {
    #[cfg(windows)]
    pub const CURRENT: Os = Os::Windows;
    #[cfg(target_os = "macos")]
    pub const CURRENT: Os = Os::MacOs;
    #[cfg(all(unix, not(target_os = "macos")))]
    pub const CURRENT: Os = Os::Linux;
}

/// How a browser to restart is started again.
#[derive(Debug, Clone, Copy)]
pub enum Launch<'a> {
    /// A program found by name: through Windows' registered programs, or
    /// on `PATH` elsewhere.
    Named(&'a str),
    /// The program file itself (on macOS, inside an `.app` it opens the app).
    File(&'a Path),
    /// A macOS app by its name.
    App(&'a str),
}

/// The commands that restart a browser: ask it to close, look whether it
/// still runs (it does while `check`'s output holds `running`), end it with
/// `force` if it is still there after a while, and start it again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartPlan {
    pub close: Vec<String>,
    pub force: Vec<String>,
    pub check: Vec<String>,
    pub running: String,
    pub launch: Vec<String>,
}

/// The restart commands on `os` for a browser running as `process`,
/// started again by `launch`. Closing asks politely first, as closing its
/// window does, so the browser saves its tabs: `taskkill` without `/F` on
/// Windows, AppleScript's quit on macOS, `SIGTERM` on Linux. Chromium keeps
/// windowless helper processes, and with "Continue running background apps"
/// the browser itself, that a polite close leaves running, so `force` ends
/// what is left: `taskkill /F /T` on Windows, `SIGKILL` elsewhere.
pub fn restart_plan(os: Os, process: &str, launch: Launch) -> RestartPlan {
    let v = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    let file = |p: &Path| p.display().to_string();
    match os {
        Os::Windows => RestartPlan {
            close: v(&["taskkill", "/IM", process]),
            force: v(&["taskkill", "/F", "/T", "/IM", process]),
            check: v(&["tasklist", "/FI", &format!("IMAGENAME eq {process}"), "/NH"]),
            running: process.to_lowercase(),
            launch: match launch {
                Launch::Named(n) | Launch::App(n) => v(&["cmd", "/C", "start", "", n]),
                Launch::File(p) => vec![file(p)],
            },
        },
        Os::MacOs => {
            let app = match launch {
                Launch::App(a) => Some((a.to_string(), a.to_string())),
                Launch::File(p) => p
                    .ancestors()
                    .find(|a| a.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")))
                    .map(|a| (a.file_stem().unwrap_or_default().to_string_lossy().into_owned(), file(a))),
                Launch::Named(_) => None,
            };
            match app {
                Some((name, open)) => RestartPlan {
                    close: v(&["osascript", "-e", &format!("tell application \"{name}\" to quit")]),
                    force: unix_plan(process, launch).force,
                    check: v(&["osascript", "-e", &format!("application \"{name}\" is running")]),
                    running: "true".into(),
                    launch: v(&["open", "-a", &open]),
                },
                None => unix_plan(process, launch),
            }
        }
        Os::Linux => unix_plan(process, launch),
    }
}

/// The plan on Linux, and on macOS for a program outside an app: `pkill`
/// and `pgrep` match the name the kernel keeps, its first 15 bytes.
fn unix_plan(process: &str, launch: Launch) -> RestartPlan {
    let mut comm = String::new();
    for c in process.chars() {
        if comm.len() + c.len_utf8() > 15 {
            break;
        }
        comm.push(c);
    }
    RestartPlan {
        close: vec!["pkill".into(), "-TERM".into(), "-x".into(), comm.clone()],
        force: vec!["pkill".into(), "-KILL".into(), "-x".into(), comm.clone()],
        check: vec!["pgrep".into(), "-l".into(), "-x".into(), comm.clone()],
        running: comm.to_lowercase(),
        launch: match launch {
            Launch::Named(n) | Launch::App(n) => vec![n.to_string()],
            Launch::File(p) => vec![p.display().to_string()],
        },
    }
}

/// The plan restarting the browser added by its program `program`.
pub fn restart_plan_for_program(program: &Path) -> RestartPlan {
    let process = program.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    restart_plan(Os::CURRENT, &process, Launch::File(program))
}

/// How long a browser gets to close when asked before it is ended, and how
/// long after that before the restart gives up.
const CLOSE_WAIT: std::time::Duration = std::time::Duration::from_secs(8);
const FORCE_WAIT: std::time::Duration = std::time::Duration::from_secs(7);

fn command(argv: &[String]) -> std::process::Command {
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    crate::platform::no_window(&mut cmd);
    cmd
}

/// Whether `plan`'s browser still runs.
fn still_running(plan: &RestartPlan) -> bool {
    command(&plan.check)
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).to_lowercase().contains(&plan.running))
}

/// Whether `plan`'s browser has gone within `wait`.
fn closed_within(plan: &RestartPlan, wait: std::time::Duration) -> bool {
    let until = std::time::Instant::now() + wait;
    while still_running(plan) {
        if std::time::Instant::now() >= until {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    true
}

/// Carry out `plan`: close the browser if it runs, end what is left of it
/// if it does not go, and start it again. Blocks while the browser closes,
/// so run it on a thread.
pub fn restart(plan: &RestartPlan) -> io::Result<()> {
    if still_running(plan) {
        // A polite close fails for processes without a window, which the
        // force step deals with, so its exit code says nothing.
        command(&plan.close).output()?;
        if !closed_within(plan, CLOSE_WAIT) {
            command(&plan.force).output()?;
            if !closed_within(plan, FORCE_WAIT) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "it did not close; close it yourself and start it again",
                ));
            }
        }
    }
    let mut launch = command(&plan.launch);
    launch.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    crate::platform::spawn_reaped(crate::platform::detach(&mut launch))
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
    /// The `udm-host` the manifests point at.
    host: PathBuf,
}

impl Registrar {
    /// The real registrations for the host next to this program: on
    /// Windows with the manifests in `%LOCALAPPDATA%\UDM` as
    /// `install-host.ps1` writes them, elsewhere in the home folder.
    pub fn for_this_user() -> io::Result<Self> {
        let dir = crate::paths::data_dir()?;
        // An AppImage's AppRun starts the host when a browser calls it.
        let host = match crate::platform::appimage() {
            Some(image) => image,
            None => crate::platform::sibling_exe("udm-host")?,
        };
        #[cfg(windows)]
        let root = String::new();
        #[cfg(not(windows))]
        let root = crate::paths::home_dir()
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
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} is missing", self.host.display())));
        }
        let (key, allowed) = allowed_extensions(b.firefox);
        let mut manifest = serde_json::json!({
            "name": HOST_NAME,
            "description": "UDM download manager native-messaging host",
            "type": "stdio",
            "path": self.host.display().to_string(),
        });
        // An object literal, so an object.
        if let Some(manifest) = manifest.as_object_mut() {
            manifest.insert(key.to_string(), allowed);
        }
        serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)
    }

    /// Whether `b`'s registration names this registrar's host and lets in
    /// every extension UDM allows now, so that a registration made before a
    /// store's ID was added is written again.
    #[cfg_attr(windows, allow(dead_code))]
    fn points_here(&self, b: &Browser) -> bool {
        #[cfg(windows)]
        let path = registry::get_string(&self.key(b), "").ok().flatten().map(PathBuf::from);
        #[cfg(not(windows))]
        let path = Some(self.manifest_path(b));
        let manifest =
            path.and_then(|p| std::fs::read(p).ok()).and_then(|m| serde_json::from_slice::<serde_json::Value>(&m).ok());
        manifest.is_some_and(|m| m["path"] == self.host.display().to_string() && lets_in_all(&m, b.firefox))
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
    /// UDM's own and no other browser still names it.
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

impl Registrar {
    /// Remove `b`'s registration of the host under `old`, a name it had
    /// before UDM got its own. True when there was one.
    fn drop_old(&self, b: &Browser, old: &str) -> io::Result<bool> {
        #[cfg(windows)]
        {
            let key = format!(r"{}{}\{old}", self.root, b.hosts_key);
            if registry::get_string(&key, "")?.is_none() {
                return Ok(false);
            }
            registry::delete_key(&key)?;
            Ok(true)
        }
        #[cfg(not(windows))]
        {
            let manifest = Path::new(&self.root).join(b.hosts_key).join(format!("{old}.json"));
            match std::fs::remove_file(manifest) {
                Ok(()) => Ok(true),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(e),
            }
        }
    }
}

/// The names browsers knew the host by before UDM got its name: Pullbay's
/// in 0.4 and UIDM's up to 0.3.3.
const OLD_HOST_NAMES: [&str; 2] = ["com.pullbay.host", "com.uidm.host"];

/// Replace every browser's registration of the host under an old name
/// ([`OLD_HOST_NAMES`]) with UDM's, so the browsers Pullbay or UIDM was
/// registered with stay registered. Returns those browsers.
pub fn move_old_registrations() -> io::Result<Vec<&'static str>> {
    move_old_registrations_with(&Registrar::for_this_user()?)
}

fn move_old_registrations_with(registrar: &Registrar) -> io::Result<Vec<&'static str>> {
    let mut moved = Vec::new();
    for b in BROWSERS.iter() {
        let mut had = false;
        for old in OLD_HOST_NAMES {
            had |= registrar.drop_old(b, old)?;
        }
        if !had {
            continue;
        }
        if !registrar.registered(b) {
            registrar.register(b)?;
        }
        moved.push(b.name);
    }
    Ok(moved)
}

/// On macOS and Linux, and for a portable copy on Windows, where no Setup
/// registers UDM: register every browser found the first time UDM
/// runs (noted in `browsers.done` in the data folder, so one the user turns
/// off in Options stays off), and point registrations at this copy of the
/// host when UDM has moved, as an AppImage does or a USB stick that gets
/// another drive letter, or when it lets in other extensions than UDM does
/// now (a store's ID was added). Returns the browsers registered.
///
/// A copy named by `UDM_INSTANCE` registers nothing: every copy shares the
/// browsers' one `com.udm.host` manifest, so a test build would take the
/// browsers away from the installed UDM. Options can still register it.
pub fn register_found() -> io::Result<Vec<&'static str>> {
    if !registers_found(cfg!(windows), crate::paths::is_portable(), crate::paths::instance().as_deref()) {
        return Ok(Vec::new());
    }
    let registrar = Registrar::for_this_user()?;
    let done = crate::paths::data_dir()?.join("browsers.done");
    let first = !done.exists();
    let mut registered = Vec::new();
    for b in BROWSERS.iter().filter(|b| registrar.installed(b)) {
        if (first && !registrar.registered(b)) || (registrar.registered(b) && !registrar.points_here(b)) {
            registrar.register(b)?;
            registered.push(b.name);
        }
    }
    if first {
        crate::settings::write_atomic(&done, b"")?;
    }
    Ok(registered)
}

/// Whether `register_found` registers anything: not on Windows, where Setup
/// does, unless the copy is `portable` (which no Setup installed, and whose
/// drive letter can change between computers), and not for the copy named
/// `instance`.
fn registers_found(windows: bool, portable: bool, instance: Option<&str>) -> bool {
    (!windows || portable) && instance.is_none()
}

/// The `BrowserRegistry` adapter: the browsers above, registered for this
/// user.
pub struct Registry;

fn slot(b: &Browser) -> udm_application::window::BrowserSlot {
    udm_application::window::BrowserSlot { name: b.name.to_string(), place: b.hosts_key.to_string() }
}

/// The browser `slot` stands for.
fn browser_of(slot: &udm_application::window::BrowserSlot) -> Option<Browser> {
    BROWSERS.iter().find(|b| b.hosts_key == slot.place).copied()
}

fn files_error(e: impl std::fmt::Display) -> udm_domain::ports::FilesError {
    udm_domain::ports::FilesError(e.to_string())
}

/// How `slot`, or the added browser at `program`, restarts.
fn plan_for(slot: &udm_application::window::BrowserSlot, program: Option<&Path>) -> Option<RestartPlan> {
    match program {
        Some(p) => Some(restart_plan_for_program(p)),
        None => browser_of(slot)?.restart_plan(),
    }
}

impl udm_application::window::BrowserRegistry for Registry {
    fn known(&self) -> Vec<(udm_application::window::BrowserSlot, bool)> {
        let Ok(r) = Registrar::for_this_user() else {
            return Vec::new();
        };
        BROWSERS
            .iter()
            .map(|b| (b, r.registered(b), r.installed(b)))
            .filter(|(_, registered, installed)| *registered || *installed)
            .map(|(b, registered, _)| (slot(b), registered))
            .collect()
    }

    fn identify(&self, program: &Path) -> Option<udm_application::window::IdentifiedBrowser> {
        let t = target_of(program)?;
        Some(udm_application::window::IdentifiedBrowser { browser: slot(&t.browser), known: t.known })
    }

    fn registered(&self, browser: &udm_application::window::BrowserSlot) -> bool {
        let (Ok(r), Some(b)) = (Registrar::for_this_user(), browser_of(browser)) else {
            return false;
        };
        r.registered(&b)
    }

    fn set_registered(
        &self,
        browser: &udm_application::window::BrowserSlot,
        on: bool,
    ) -> Result<(), udm_domain::ports::FilesError> {
        let r = Registrar::for_this_user().map_err(files_error)?;
        let b = browser_of(browser).ok_or_else(|| files_error(format!("UDM does not know {}", browser.name)))?;
        if on { r.register(&b) } else { r.unregister(&b) }.map_err(files_error)
    }

    fn display_name(&self, program: &Path) -> String {
        display_name(program)
    }

    fn chrome_readers(&self) -> Vec<String> {
        chrome_readers().into_iter().map(str::to_string).collect()
    }

    fn can_restart(&self, browser: &udm_application::window::BrowserSlot, program: Option<&Path>) -> bool {
        plan_for(browser, program).is_some()
    }

    fn restart(
        &self,
        browser: &udm_application::window::BrowserSlot,
        program: Option<&Path>,
    ) -> Result<(), udm_domain::ports::FilesError> {
        let plan = plan_for(browser, program)
            .ok_or_else(|| files_error(format!("UDM does not know how to restart {}", browser.name)))?;
        restart(&plan).map_err(files_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_usual_copy_registers_the_browsers_it_finds() {
        assert!(registers_found(false, false, None));
        assert!(!registers_found(false, false, Some("test")), "a named copy leaves the installed one's browsers alone");
        assert!(!registers_found(true, false, None), "Setup registers on Windows");
        assert!(registers_found(true, true, None), "no Setup installed a portable copy");
        assert!(registers_found(false, true, None));
        assert!(!registers_found(true, true, Some("test")));
    }

    #[test]
    fn a_program_tells_its_family_and_where_it_registers() {
        let fam = |p: &str| family_of(Path::new(p));
        assert_eq!(fam(r"C:\Program Files\Google\Chrome\Application\chrome.exe"), Some(Family::Chromium));
        assert_eq!(fam("/usr/bin/librewolf"), Some(Family::Firefox));
        assert_eq!(fam("/Applications/Floorp.app/Contents/MacOS/floorp"), Some(Family::Firefox));
        assert_eq!(fam("/opt/yandex/browser/yandex_browser"), Some(Family::Chromium));
        assert_eq!(fam("/usr/bin/gimp"), None);

        let target = |p: &str| target_of(Path::new(p)).map(|t| (t.browser.name, t.known));
        assert_eq!(target("/usr/bin/microsoft-edge"), Some(("Microsoft Edge", true)));
        assert_eq!(target("/usr/bin/brave-browser"), Some(("Brave", true)));
        assert_eq!(target("/usr/bin/firefox"), Some(("Mozilla Firefox", true)));
        assert_eq!(target("/usr/bin/waterfox"), Some(("Mozilla Firefox", false)));
        assert_eq!(target("/usr/bin/opera"), Some(("Google Chrome", true)));
        assert_eq!(target("/usr/bin/thorium-browser"), Some(("Google Chrome", false)));
        assert_eq!(target("/usr/bin/gimp"), None);
        assert_eq!(display_name(Path::new("/usr/bin/thorium-browser")), "Thorium Browser");
        assert_eq!(
            display_name(Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")),
            "Google Chrome"
        );
    }

    #[test]
    fn a_restart_asks_the_browser_to_close_then_starts_it() {
        let win = restart_plan(Os::Windows, "chrome.exe", Launch::Named("chrome.exe"));
        assert_eq!(win.close, ["taskkill", "/IM", "chrome.exe"]);
        assert_eq!(win.force, ["taskkill", "/F", "/T", "/IM", "chrome.exe"]);
        assert_eq!(win.check, ["tasklist", "/FI", "IMAGENAME eq chrome.exe", "/NH"]);
        assert_eq!(win.launch, ["cmd", "/C", "start", "", "chrome.exe"]);
        let file = Path::new(r"C:\Thorium\thorium.exe");
        assert_eq!(restart_plan(Os::Windows, "thorium.exe", Launch::File(file)).launch, [file.display().to_string()]);

        let mac = restart_plan(Os::MacOs, "Google Chrome", Launch::App("Google Chrome"));
        assert_eq!(mac.close, ["osascript", "-e", "tell application \"Google Chrome\" to quit"]);
        assert_eq!(mac.check, ["osascript", "-e", "application \"Google Chrome\" is running"]);
        assert_eq!(mac.running, "true");
        assert_eq!(mac.launch, ["open", "-a", "Google Chrome"]);
        let bundled = restart_plan(
            Os::MacOs,
            "floorp",
            Launch::File(Path::new("/Applications/Floorp.app/Contents/MacOS/floorp")),
        );
        assert_eq!(bundled.close[2], "tell application \"Floorp\" to quit");
        assert_eq!(bundled.launch, ["open", "-a", "/Applications/Floorp.app"]);

        let linux = restart_plan(Os::Linux, "chrome", Launch::Named("google-chrome"));
        assert_eq!(linux.close, ["pkill", "-TERM", "-x", "chrome"]);
        assert_eq!(linux.force, ["pkill", "-KILL", "-x", "chrome"]);
        assert_eq!(linux.check, ["pgrep", "-l", "-x", "chrome"]);
        assert_eq!(linux.launch, ["google-chrome"]);
        // The kernel keeps 15 bytes of a process's name.
        let long =
            restart_plan(Os::Linux, "thorium-browser-avx2", Launch::File(Path::new("/opt/thorium-browser-avx2")));
        assert_eq!(long.close[3], "thorium-browser");
        assert_eq!(long.launch, ["/opt/thorium-browser-avx2"]);
    }

    #[cfg(not(windows))]
    #[test]
    fn old_registrations_become_udms() {
        let dir = std::env::temp_dir().join(format!("udm_old_browsers_{}", std::process::id()));
        let host = dir.join("udm-host");
        let r = Registrar::new(&dir.display().to_string(), dir.clone(), host.clone());
        let [chrome, edge, firefox, ..] = BROWSERS;
        for (b, old) in [(chrome, "com.pullbay.host.json"), (firefox, "com.uidm.host.json")] {
            std::fs::create_dir_all(dir.join(b.hosts_key)).unwrap();
            std::fs::write(dir.join(b.hosts_key).join(old), b"{}").unwrap();
        }
        std::fs::write(&host, b"").unwrap();
        assert_eq!(move_old_registrations_with(&r).unwrap(), [chrome.name, firefox.name]);
        assert!(r.registered(&chrome) && r.registered(&firefox) && !r.registered(&edge));
        assert!(!dir.join(chrome.hosts_key).join("com.pullbay.host.json").exists());
        assert!(!dir.join(firefox.hosts_key).join("com.uidm.host.json").exists());
        assert!(r.points_here(&firefox));
        assert!(move_old_registrations_with(&r).unwrap().is_empty(), "nothing is left to move");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The strings in double quotes in `text`.
    fn quoted(text: &str) -> Vec<String> {
        text.split('"').skip(1).step_by(2).map(str::to_string).collect()
    }

    /// The value of `#define <name> ...` in Setup's script `setup`, worked
    /// out as Inno Setup's preprocessor does for what it holds: strings in
    /// double quotes and the names of other defines, joined with `+`.
    fn setup_define(setup: &str, name: &str) -> String {
        let prefix = format!("#define {name} ");
        let line = setup.lines().find(|l| l.starts_with(&prefix)).unwrap_or_else(|| panic!("no #define {name}"));
        line[prefix.len()..]
            .split('+')
            .map(|part| {
                let part = part.trim();
                match part.strip_prefix('"').and_then(|p| p.strip_suffix('"')) {
                    Some(text) => text.to_string(),
                    None => setup_define(setup, part),
                }
            })
            .collect()
    }

    #[test]
    fn a_manifest_that_lets_in_more_is_kept_and_one_that_lacks_an_id_is_not() {
        let (_, allowed) = allowed_extensions(false);
        let mut origins = allowed.as_array().unwrap().clone();
        assert!(lets_in_all(&serde_json::json!({ "allowed_origins": origins }), false));
        origins.push(serde_json::json!("chrome-extension://another/"));
        assert!(lets_in_all(&serde_json::json!({ "allowed_origins": origins }), false));
        origins.remove(0);
        assert!(!lets_in_all(&serde_json::json!({ "allowed_origins": origins }), false));
        assert!(!lets_in_all(&serde_json::json!({}), false));
        assert!(!lets_in_all(&serde_json::json!({ "allowed_origins": "x" }), false));
        assert!(lets_in_all(&serde_json::json!({ "allowed_extensions": [FIREFOX_EXTENSION_ID, "other@x"] }), true));
        assert!(!lets_in_all(&serde_json::json!({ "allowed_extensions": ["other@x"] }), true));
    }

    /// A file of the repository, by its path from the root.
    fn repo_file(path: &str) -> String {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        std::fs::read_to_string(root.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    // Every file that registers the host lets in the same extensions as
    // `register`, so a store's ID added in one place cannot be missed in
    // another (docs/extension-stores.md).
    #[test]
    fn every_file_lets_in_the_same_extensions() {
        let ids: std::collections::BTreeSet<&str> = CHROMIUM_EXTENSION_IDS.iter().copied().collect();
        assert_eq!(ids.len(), CHROMIUM_EXTENSION_IDS.len(), "an ID is listed twice");

        let host: serde_json::Value = serde_json::from_str(&repo_file("host/com.udm.host.json")).unwrap();
        assert_eq!(host["allowed_origins"], allowed_extensions(false).1, "host/com.udm.host.json");
        let firefox_host: serde_json::Value =
            serde_json::from_str(&repo_file("host/com.udm.host.firefox.json")).unwrap();
        assert_eq!(firefox_host["allowed_extensions"], allowed_extensions(true).1, "host/com.udm.host.firefox.json");

        let listed: Vec<String> = CHROMIUM_EXTENSION_IDS.iter().map(|id| id.to_string()).collect();
        let setup = repo_file("installer/windows/udm.iss");
        let setup_ids: Vec<String> =
            setup_define(&setup, "ChromiumIds").split_whitespace().map(str::to_string).collect();
        assert_eq!(setup_ids, listed, "ChromiumIds in udm.iss");
        assert_eq!(setup_define(&setup, "ExtensionId"), CHROMIUM_EXTENSION_IDS[0], "ExtensionId in udm.iss");
        assert_eq!(setup_define(&setup, "FirefoxId"), FIREFOX_EXTENSION_ID, "FirefoxId in udm.iss");

        let script = repo_file("install-host.ps1");
        let param = |name: &str| {
            let line = script
                .lines()
                .map(str::trim)
                .find(|l| l.contains(&format!("]${name} =")))
                .unwrap_or_else(|| panic!("no ${name} in install-host.ps1"));
            quoted(line)
        };
        assert_eq!(param("ExtensionId"), listed, "$ExtensionId in install-host.ps1");
        assert_eq!(param("FirefoxId"), [FIREFOX_EXTENSION_ID], "$FirefoxId in install-host.ps1");
        assert!(
            repo_file("installer/linux/AppRun").contains(&format!("\"$2\" = \"{FIREFOX_EXTENSION_ID}\"")),
            "installer/linux/AppRun"
        );

        let changes: serde_json::Value = serde_json::from_str(&repo_file("extension/firefox.json")).unwrap();
        assert_eq!(changes["browser_specific_settings"]["gecko"]["id"], FIREFOX_EXTENSION_ID);

        // The unpacked copy's ID is the one its manifest's key gives: the
        // first 32 hex digits of the key's SHA-256, written with a to p.
        use base64::Engine as _;
        use sha2::Digest as _;
        let manifest: serde_json::Value = serde_json::from_str(&repo_file("extension/chromium/manifest.json")).unwrap();
        let key = base64::engine::general_purpose::STANDARD.decode(manifest["key"].as_str().unwrap()).unwrap();
        let unpacked: String = sha2::Sha256::digest(&key)[..16]
            .iter()
            .flat_map(|b| [b >> 4, b & 15])
            .map(|n| char::from(b'a' + n))
            .collect();
        assert_eq!(CHROMIUM_EXTENSION_IDS[0], unpacked);
    }

    #[cfg(not(windows))]
    #[test]
    fn registering_writes_the_manifest_and_removing_deletes_it() {
        let dir = std::env::temp_dir().join(format!("udm_browsers_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let host = dir.join("udm-host");
        std::fs::write(&host, b"").unwrap();
        let r = Registrar::new(&dir.display().to_string(), dir.clone(), host.clone());
        let [chrome, _, firefox, ..] = BROWSERS;
        assert!(!r.registered(&chrome));
        r.register(&chrome).unwrap();
        r.register(&firefox).unwrap();
        assert!(r.registered(&chrome) && r.registered(&firefox));
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join(chrome.hosts_key).join("com.udm.host.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["path"], host.display().to_string());
        assert_eq!(manifest["allowed_origins"], allowed_extensions(false).1);
        assert_eq!(manifest["allowed_origins"][0], format!("chrome-extension://{}/", CHROMIUM_EXTENSION_IDS[0]));
        let fx: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join(firefox.hosts_key).join("com.udm.host.json")).unwrap())
                .unwrap();
        assert_eq!(fx["allowed_extensions"][0], FIREFOX_EXTENSION_ID);
        assert!(r.points_here(&chrome));
        // A manifest written before a store's ID was added is written again.
        let path = dir.join(chrome.hosts_key).join("com.udm.host.json");
        let mut older = manifest.clone();
        older["allowed_origins"] = serde_json::json!(["chrome-extension://older/"]);
        std::fs::write(&path, serde_json::to_vec(&older).unwrap()).unwrap();
        assert!(!r.points_here(&chrome));
        r.register(&chrome).unwrap();
        assert!(r.points_here(&chrome));
        // One that lets in more, such as an ID given to install-host.ps1,
        // stays as it is.
        let mut more = manifest.clone();
        more["allowed_origins"].as_array_mut().unwrap().push(serde_json::json!("chrome-extension://more/"));
        std::fs::write(&path, serde_json::to_vec(&more).unwrap()).unwrap();
        assert!(r.points_here(&chrome));
        r.register(&chrome).unwrap();
        let moved = Registrar::new(&dir.display().to_string(), dir.clone(), dir.join("elsewhere"));
        assert!(!moved.points_here(&chrome));
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
        let dir = std::env::temp_dir().join(format!("udm_browsers_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let host = dir.join("udm-host.exe");
        std::fs::write(&host, b"").unwrap();
        let r = Registrar::new(r"Software\UDM-test-browsers\", dir.clone(), host.clone());
        let [chrome, edge, firefox, ..] = BROWSERS;

        assert!(!r.registered(&chrome));
        r.register(&chrome).unwrap();
        r.register(&edge).unwrap();
        r.register(&firefox).unwrap();
        assert!(r.registered(&chrome) && r.registered(&edge) && r.registered(&firefox));
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("com.udm.host.json")).unwrap()).unwrap();
        assert_eq!(manifest["path"], host.display().to_string());
        assert_eq!(manifest["allowed_origins"], allowed_extensions(false).1);
        assert_eq!(manifest["allowed_origins"][0], format!("chrome-extension://{}/", CHROMIUM_EXTENSION_IDS[0]));
        let fx: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("com.udm.host.firefox.json")).unwrap()).unwrap();
        assert_eq!(fx["allowed_extensions"][0], FIREFOX_EXTENSION_ID);

        // Edge still names the shared manifest, so it stays.
        r.unregister(&chrome).unwrap();
        assert!(!r.registered(&chrome));
        assert!(dir.join("com.udm.host.json").exists());
        r.unregister(&edge).unwrap();
        assert!(!dir.join("com.udm.host.json").exists());
        r.unregister(&firefox).unwrap();
        assert!(!dir.join("com.udm.host.firefox.json").exists());
        // Removing what is not there is fine.
        r.unregister(&firefox).unwrap();

        let missing = Registrar::new(r"Software\UDM-test-browsers\", dir.clone(), dir.join("nope.exe"));
        assert!(missing.register(&chrome).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
