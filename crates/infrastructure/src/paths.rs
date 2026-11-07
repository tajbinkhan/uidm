//! Every folder UIDM uses, and the only module that asks the system where
//! the user's folders are (AGENTS.md section 6). The base folders come from
//! the `directories` crate; UIDM's own folder names are joined onto them
//! here, so each file stays where earlier versions put it.
//!
//! | | Windows | macOS | Linux |
//! |---|---|---|---|
//! | settings, grabber projects ([`config_dir`]) | `%APPDATA%\UIDM` | `~/Library/Application Support/UIDM` | `$XDG_CONFIG_HOME/uidm` (`~/.config/uidm`) |
//! | list, token, quota, keys ([`data_dir`]) | `%LOCALAPPDATA%\UIDM` | `~/Library/Application Support/UIDM` | `$XDG_DATA_HOME/uidm` (`~/.local/share/uidm`) |
//! | logs ([`daemon_log`], [`host_log`], [`gui_log`]) | in [`data_dir`] | in [`data_dir`] | in [`data_dir`] |
//! | window layout ([`window_dir`]) | `%LOCALAPPDATA%\UIDM` | none | none |
//! | sign-in entry ([`autostart_dir`]) | the registry | `~/Library/LaunchAgents` | `$XDG_CONFIG_HOME/autostart` |
//!
//! With [`INSTANCE_VAR`] set, [`config_dir`] and [`data_dir`] get the
//! instance's name after a dash (`UIDM-test`), and the daemon its own port
//! (see `ipc::daemon_addr`): a second copy, such as a build under test, runs
//! beside the installed one without sharing its list, settings or daemon.
//! The other folders have no instance name, as before.
//!
//! Two variables are read here rather than left to `directories`, so the
//! folders stay where earlier versions resolved them:
//!
//! - On Windows, `APPDATA`, `LOCALAPPDATA` and `USERPROFILE` come first and
//!   the known folders only when they are not set. `directories` asks
//!   Windows for the known folders and ignores the variables, but end-to-end
//!   runs point the variables at a scratch folder to keep a test daemon's
//!   files apart, and a process started with them changed expects UIDM to
//!   follow them, as it always has.
//! - On Linux, any non-empty `XDG_CONFIG_HOME` and `XDG_DATA_HOME` is used
//!   as given. `directories` ignores a relative one, which would move the
//!   files of a user who has one set.

use std::io;
use std::path::PathBuf;

use directories::BaseDirs;

/// The environment variable naming a separate copy of UIDM. Programs UIDM
/// starts inherit it, so a whole copy (window, daemon, host) stays apart.
pub const INSTANCE_VAR: &str = "UIDM_INSTANCE";

/// UIDM's own folder name on this system, before any instance name.
const OWN: &str = if cfg!(any(windows, target_os = "macos")) { "UIDM" } else { "uidm" };

/// The name of this copy of UIDM, from [`INSTANCE_VAR`]; `None` for the
/// usual one. A name is up to 32 letters, digits, `-` and `_`; anything else
/// is ignored, so a typo cannot reach a folder outside UIDM's own.
pub fn instance() -> Option<String> {
    instance_name(std::env::var(INSTANCE_VAR).ok())
}

fn instance_name(value: Option<String>) -> Option<String> {
    let name = value?.trim().to_string();
    let ok =
        !name.is_empty() && name.len() <= 32 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then_some(name)
}

/// UIDM's folder name for instance `name`: `UIDM`, or `UIDM-test`.
fn own_folder(name: Option<&str>) -> String {
    match name {
        Some(name) => format!("{OWN}-{name}"),
        None => OWN.to_string(),
    }
}

fn not_found(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("the {what} folder cannot be found"))
}

/// The folder an environment variable's `value` names, if it names one:
/// anything but empty, used as given.
#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
fn named_dir(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    value.filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The folder environment variable `var` names, if it names one.
#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
fn env_dir(var: &str) -> Option<PathBuf> {
    named_dir(std::env::var_os(var))
}

/// The base folders a user's folders sit in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Root {
    /// The home folder.
    Home,
    /// `%APPDATA%` on Windows, which roams with the user; the home folder
    /// elsewhere.
    Roaming,
    /// `%LOCALAPPDATA%` on Windows, which stays on this machine; the home
    /// folder elsewhere.
    Local,
}

/// The base folder `root` on this system.
pub fn root(root: Root) -> Option<PathBuf> {
    #[cfg(windows)]
    return match root {
        Root::Home => env_dir("USERPROFILE").or_else(|| BaseDirs::new().map(|b| b.home_dir().to_path_buf())),
        Root::Roaming => env_dir("APPDATA").or_else(|| BaseDirs::new().map(|b| b.config_dir().to_path_buf())),
        Root::Local => env_dir("LOCALAPPDATA").or_else(|| BaseDirs::new().map(|b| b.data_local_dir().to_path_buf())),
    };
    #[cfg(not(windows))]
    {
        let _ = root;
        BaseDirs::new().map(|b| b.home_dir().to_path_buf())
    }
}

/// The running user's home folder.
pub fn home_dir() -> Option<PathBuf> {
    root(Root::Home)
}

/// The base folder settings go in: `%APPDATA%`, Application Support, or
/// `$XDG_CONFIG_HOME` (`~/.config`).
fn config_base() -> io::Result<PathBuf> {
    #[cfg(windows)]
    return root(Root::Roaming).ok_or_else(|| not_found("APPDATA"));
    // directories puts macOS's config folder in Application Support too.
    #[cfg(target_os = "macos")]
    return BaseDirs::new().map(|b| b.config_dir().to_path_buf()).ok_or_else(|| not_found("config"));
    #[cfg(all(unix, not(target_os = "macos")))]
    return env_dir("XDG_CONFIG_HOME")
        .or_else(|| home_dir().map(|h| h.join(".config")))
        .ok_or_else(|| not_found("config"));
}

/// The base folder machine-local state goes in: `%LOCALAPPDATA%`,
/// Application Support, or `$XDG_DATA_HOME` (`~/.local/share`).
fn data_base() -> io::Result<PathBuf> {
    #[cfg(windows)]
    return root(Root::Local).ok_or_else(|| not_found("LOCALAPPDATA"));
    #[cfg(target_os = "macos")]
    return BaseDirs::new().map(|b| b.data_local_dir().to_path_buf()).ok_or_else(|| not_found("data"));
    #[cfg(all(unix, not(target_os = "macos")))]
    return env_dir("XDG_DATA_HOME")
        .or_else(|| home_dir().map(|h| h.join(".local/share")))
        .ok_or_else(|| not_found("data"));
}

fn config_dir_for(name: Option<&str>) -> io::Result<PathBuf> {
    Ok(config_base()?.join(own_folder(name)))
}

fn data_dir_for(name: Option<&str>) -> io::Result<PathBuf> {
    Ok(data_base()?.join(own_folder(name)))
}

fn grabber_dir_for(name: Option<&str>) -> io::Result<PathBuf> {
    Ok(config_dir_for(name)?.join("grabber"))
}

/// The log called `file` in the data folder, or in the temp folder when the
/// data folder cannot be found.
fn log_for(name: Option<&str>, file: &str) -> PathBuf {
    data_dir_for(name).map(|d| d.join(file)).unwrap_or_else(|_| std::env::temp_dir().join(file))
}

const DAEMON_LOG: &str = "uidm-daemon.log";
const HOST_LOG: &str = "uidm-host.log";
const GUI_LOG: &str = "uidm-gui.log";

/// The folder settings, templates and grabber projects live in, which
/// roams with the user on Windows.
pub fn config_dir() -> io::Result<PathBuf> {
    config_dir_for(instance().as_deref())
}

/// The folder the download list, the daemon's token and other machine-local
/// state live in.
pub fn data_dir() -> io::Result<PathBuf> {
    data_dir_for(instance().as_deref())
}

/// The site grabber's projects, templates and filters, next to the
/// settings file.
pub fn grabber_dir() -> io::Result<PathBuf> {
    grabber_dir_for(instance().as_deref())
}

/// The daemon's log, in [`data_dir`]. It holds download addresses, so it
/// stays out of a shared `/tmp`.
pub fn daemon_log() -> PathBuf {
    log_for(instance().as_deref(), DAEMON_LOG)
}

/// The native-messaging host's log, beside the daemon's.
pub fn host_log() -> PathBuf {
    log_for(instance().as_deref(), HOST_LOG)
}

/// The window's log, beside the daemon's.
pub fn gui_log() -> PathBuf {
    log_for(instance().as_deref(), GUI_LOG)
}

/// The Windows font folder, which the window's fonts come from. Elsewhere
/// the window draws with egui's own.
pub fn fonts_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    return std::env::var_os("WINDIR").map(|w| PathBuf::from(w).join("Fonts"));
    #[cfg(not(windows))]
    None
}

/// The folder the window keeps its layout and the drop target's place in:
/// `%LOCALAPPDATA%\UIDM` on Windows, shared by every instance. Elsewhere
/// the window keeps neither.
pub fn window_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    return root(Root::Local).map(|base| base.join("UIDM"));
    #[cfg(not(windows))]
    return None;
}

/// The folder the entry that starts UIDM at sign-in goes in. Windows keeps
/// it in the registry instead.
#[cfg(unix)]
pub fn autostart_dir() -> io::Result<PathBuf> {
    #[cfg(target_os = "macos")]
    return Ok(home_dir().ok_or_else(|| not_found("home"))?.join("Library/LaunchAgents"));
    #[cfg(not(target_os = "macos"))]
    return Ok(config_base()?.join("autostart"));
}

/// Where Windows keeps app packages' private copies of AppData:
/// `%LOCALAPPDATA%\Packages`.
#[cfg(windows)]
pub fn packages_dir() -> Option<PathBuf> {
    root(Root::Local).map(|base| base.join("Packages"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_instance_name_is_short_and_plain() {
        assert_eq!(instance_name(Some(" test ".into())).as_deref(), Some("test"));
        assert_eq!(instance_name(Some("dev_2-a".into())).as_deref(), Some("dev_2-a"));
        assert_eq!(instance_name(None), None);
        assert_eq!(instance_name(Some("".into())), None);
        assert_eq!(instance_name(Some("../x".into())), None, "no path parts");
        assert_eq!(instance_name(Some("a".repeat(33))), None);
    }

    #[test]
    fn an_instance_gets_folders_of_its_own() {
        let config = config_dir_for(None).unwrap();
        let data = data_dir_for(None).unwrap();
        let base = |dir: &PathBuf| dir.parent().unwrap().to_path_buf();
        let suffix = format!("{OWN}-test");
        assert_eq!(config_dir_for(Some("test")).unwrap(), base(&config).join(&suffix));
        assert_eq!(data_dir_for(Some("test")).unwrap(), base(&data).join(&suffix));
        assert_eq!(grabber_dir_for(None).unwrap(), config.join("grabber"));
        assert_eq!(grabber_dir_for(Some("test")).unwrap(), base(&config).join(&suffix).join("grabber"));
    }

    fn home() -> PathBuf {
        home_dir().expect("tests run with a home folder")
    }

    #[cfg(windows)]
    fn var(name: &str) -> PathBuf {
        PathBuf::from(std::env::var_os(name).expect("the test runs with the variable set"))
    }

    #[cfg(windows)]
    #[test]
    fn windows_folders_sit_in_appdata() {
        let roaming = var("APPDATA");
        let local = var("LOCALAPPDATA");
        assert_eq!(root(Root::Roaming).unwrap(), roaming);
        assert_eq!(root(Root::Local).unwrap(), local);
        assert_eq!(home(), var("USERPROFILE"));
        assert_eq!(config_dir_for(None).unwrap(), roaming.join("UIDM"));
        assert_eq!(config_dir_for(Some("test")).unwrap(), roaming.join("UIDM-test"));
        assert_eq!(data_dir_for(None).unwrap(), local.join("UIDM"));
        assert_eq!(data_dir_for(Some("test")).unwrap(), local.join("UIDM-test"));
        assert_eq!(grabber_dir_for(None).unwrap(), roaming.join(r"UIDM\grabber"));
        assert_eq!(grabber_dir_for(Some("test")).unwrap(), roaming.join(r"UIDM-test\grabber"));
        // The window's files are shared by every instance; the logs are not.
        assert_eq!(window_dir().unwrap(), local.join("UIDM"));
        assert_eq!(log_for(None, DAEMON_LOG), local.join(r"UIDM\uidm-daemon.log"));
        assert_eq!(log_for(Some("test"), DAEMON_LOG), local.join(r"UIDM-test\uidm-daemon.log"));
        assert_eq!(log_for(None, HOST_LOG), local.join(r"UIDM\uidm-host.log"));
        assert_eq!(log_for(None, GUI_LOG), local.join(r"UIDM\uidm-gui.log"));
        assert_eq!(packages_dir().unwrap(), local.join("Packages"));
    }

    #[cfg(windows)]
    #[test]
    fn a_set_variable_is_used_as_given() {
        assert_eq!(named_dir(Some(r"D:\scratch\Roaming".into())), Some(PathBuf::from(r"D:\scratch\Roaming")));
        assert_eq!(named_dir(Some("".into())), None, "an empty variable is not set");
        assert_eq!(named_dir(None), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_folders_sit_in_application_support() {
        let support = home().join("Library/Application Support");
        assert_eq!(config_dir_for(None).unwrap(), support.join("UIDM"));
        assert_eq!(config_dir_for(Some("test")).unwrap(), support.join("UIDM-test"));
        assert_eq!(data_dir_for(None).unwrap(), support.join("UIDM"));
        assert_eq!(data_dir_for(Some("test")).unwrap(), support.join("UIDM-test"));
        assert_eq!(grabber_dir_for(None).unwrap(), support.join("UIDM/grabber"));
        assert_eq!(grabber_dir_for(Some("test")).unwrap(), support.join("UIDM-test/grabber"));
        assert_eq!(log_for(None, DAEMON_LOG), support.join("UIDM/uidm-daemon.log"));
        assert_eq!(log_for(Some("test"), DAEMON_LOG), support.join("UIDM-test/uidm-daemon.log"));
        assert_eq!(log_for(None, HOST_LOG), support.join("UIDM/uidm-host.log"));
        assert_eq!(log_for(None, GUI_LOG), support.join("UIDM/uidm-gui.log"));
        assert_eq!(autostart_dir().unwrap(), home().join("Library/LaunchAgents"));
        assert_eq!(window_dir(), None);
        assert_eq!(root(Root::Local), Some(home()));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_folders_follow_xdg() {
        // Earlier versions used any non-empty XDG value as given, relative
        // ones too.
        assert_eq!(named_dir(Some("rel/config".into())), Some(PathBuf::from("rel/config")));
        assert_eq!(named_dir(Some("/abs/config".into())), Some(PathBuf::from("/abs/config")));
        assert_eq!(named_dir(Some("".into())), None, "an empty variable is not set");
        assert_eq!(named_dir(None), None);

        let config = env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| home().join(".config"));
        let data = env_dir("XDG_DATA_HOME").unwrap_or_else(|| home().join(".local/share"));
        assert_eq!(config_base().unwrap(), config);
        assert_eq!(data_base().unwrap(), data);
        assert_eq!(config_dir_for(None).unwrap(), config.join("uidm"));
        assert_eq!(config_dir_for(Some("test")).unwrap(), config.join("uidm-test"));
        assert_eq!(data_dir_for(None).unwrap(), data.join("uidm"));
        assert_eq!(data_dir_for(Some("test")).unwrap(), data.join("uidm-test"));
        assert_eq!(grabber_dir_for(None).unwrap(), config.join("uidm/grabber"));
        assert_eq!(grabber_dir_for(Some("test")).unwrap(), config.join("uidm-test/grabber"));
        assert_eq!(log_for(None, DAEMON_LOG), data.join("uidm/uidm-daemon.log"));
        assert_eq!(log_for(Some("test"), DAEMON_LOG), data.join("uidm-test/uidm-daemon.log"));
        assert_eq!(log_for(None, HOST_LOG), data.join("uidm/uidm-host.log"));
        assert_eq!(log_for(None, GUI_LOG), data.join("uidm/uidm-gui.log"));
        assert_eq!(autostart_dir().unwrap(), config.join("autostart"));
        assert_eq!(window_dir(), None);
        assert_eq!(root(Root::Roaming), Some(home()));
    }
}
