//! What differs between Windows, macOS and Linux and is not worth a module
//! of its own: where UIDM keeps its files, program names, and starting
//! helper programs without a console window.
//!
//! | | Windows | macOS | Linux |
//! |---|---|---|---|
//! | settings ([`config_dir`]) | `%APPDATA%\UIDM` | `~/Library/Application Support/UIDM` | `$XDG_CONFIG_HOME/uidm` (`~/.config/uidm`) |
//! | list, token, logs ([`data_dir`]) | `%LOCALAPPDATA%\UIDM` | `~/Library/Application Support/UIDM` | `$XDG_DATA_HOME/uidm` (`~/.local/share/uidm`) |

use std::io;
use std::path::PathBuf;
use std::process::Command;

/// The running user's home folder.
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    home.filter(|h| !h.is_empty()).map(PathBuf::from)
}

fn not_found(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{what} is not set"))
}

/// `name` under an XDG base folder: `$var`, else `~/fallback`.
#[cfg(all(unix, not(target_os = "macos")))]
fn xdg(var: &str, fallback: &str) -> io::Result<PathBuf> {
    match std::env::var_os(var).filter(|v| !v.is_empty()) {
        Some(dir) => Ok(PathBuf::from(dir).join("uidm")),
        None => Ok(home_dir().ok_or_else(|| not_found("HOME"))?.join(fallback).join("uidm")),
    }
}

/// The folder settings, templates and grabber projects live in, which
/// roams with the user on Windows.
pub fn config_dir() -> io::Result<PathBuf> {
    #[cfg(windows)]
    return Ok(PathBuf::from(std::env::var_os("APPDATA").ok_or_else(|| not_found("APPDATA"))?).join("UIDM"));
    #[cfg(target_os = "macos")]
    return Ok(home_dir().ok_or_else(|| not_found("HOME"))?.join("Library/Application Support/UIDM"));
    #[cfg(all(unix, not(target_os = "macos")))]
    return xdg("XDG_CONFIG_HOME", ".config");
}

/// The folder the download list, the daemon's token and other machine-local
/// state live in.
pub fn data_dir() -> io::Result<PathBuf> {
    #[cfg(windows)]
    return Ok(PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or_else(|| not_found("LOCALAPPDATA"))?).join("UIDM"));
    #[cfg(target_os = "macos")]
    return config_dir();
    #[cfg(all(unix, not(target_os = "macos")))]
    return xdg("XDG_DATA_HOME", ".local/share");
}

/// The file name of UIDM's program `name` (`uidm-daemon`) on this system.
pub fn exe_name(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// UIDM's program `name`, next to the running one.
pub fn sibling_exe(name: &str) -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the program has no folder"))?;
    Ok(dir.join(exe_name(name)))
}

/// The AppImage UIDM runs from, if it does. Its programs live in a mount
/// that goes away when the process that mounted it ends, so other programs
/// are started through the AppImage itself (its `AppRun` picks the program
/// by the first argument), and browsers are pointed at it.
pub fn appimage() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    std::env::var_os("APPIMAGE").filter(|p| !p.is_empty()).map(PathBuf::from).filter(|p| p.is_file())
}

/// How to start UIDM's program `name` (`uidm-gui`): its file and the
/// arguments that come first.
pub fn program(name: &str) -> io::Result<(PathBuf, Vec<String>)> {
    match appimage() {
        Some(image) => Ok((image, vec![name.to_string()])),
        None => Ok((sibling_exe(name)?, Vec::new())),
    }
}

/// A command that starts UIDM's program `name`.
pub fn program_command(name: &str) -> io::Result<Command> {
    let (file, args) = program(name)?;
    let mut cmd = Command::new(file);
    cmd.args(args);
    Ok(cmd)
}

/// Start `cmd` without a console window flashing up (Windows); elsewhere
/// programs started from a window get none anyway.
pub fn no_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Run `cmd` as a background process of its own, so it outlives the one
/// starting it and gets none of its console or terminal signals.
pub fn detach(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // A session of its own: closing the browser or terminal that started
        // the host does not hang the daemon up.
        cmd.process_group(0);
    }
    cmd
}

/// Write `buf` to `file` at `offset` without moving a cursor other threads
/// share. Returns how many bytes were written, which may be fewer.
pub fn write_at(file: &std::fs::File, buf: &[u8], offset: u64) -> io::Result<usize> {
    #[cfg(windows)]
    return std::os::windows::fs::FileExt::seek_write(file, buf, offset);
    #[cfg(unix)]
    return std::os::unix::fs::FileExt::write_at(file, buf, offset);
}

/// Open `target` (a file, folder or web address) with the program the
/// system picks for it.
pub fn open(target: &std::ffi::OsStr) -> io::Result<()> {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("explorer");
        c.arg(target);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(target);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(target);
        c
    };
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    no_window(&mut cmd).spawn().map(|_| ())
}

/// Show `path` selected in the file manager: Explorer, the Finder, or on
/// Linux the file manager's `ShowItems` over D-Bus, else its folder.
pub fn reveal(path: &std::path::Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Explorer parses /select itself, so pass the argument raw with the
        // path quoted. Windows paths cannot contain a double quote.
        no_window(Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display()))).spawn().map(|_| ())
    }
    #[cfg(target_os = "macos")]
    return Command::new("open").arg("-R").arg(path).spawn().map(|_| ());
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let uri = url::Url::from_file_path(path).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "not a full path"))?;
        let shown = Command::new("dbus-send")
            .args([
                "--session",
                "--dest=org.freedesktop.FileManager1",
                "--type=method_call",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
            ])
            .arg(format!("array:string:{uri}"))
            .arg("string:")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if shown.is_ok_and(|s| s.success()) {
            return Ok(());
        }
        open(path.parent().unwrap_or(path).as_os_str())
    }
}

/// Write `bytes` to `path`, readable and writable by the current user only
/// on Unix (Windows' profile folders are private already).
pub fn write_private(path: &std::path::Path, bytes: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        f.write_all(bytes)?;
        // An older file keeps its mode: narrow it too.
        std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    std::fs::write(path, bytes)?;
    Ok(())
}

/// Mark `path` as downloaded from `url`, as browsers do: on Windows its
/// `Zone.Identifier` stream says it came from the internet, so SmartScreen
/// checks a program before it runs and Office opens a document in
/// Protected View. A drive without streams (FAT) keeps no mark, which is
/// not an error. Elsewhere this does nothing.
pub fn mark_from_internet(path: &std::path::Path, url: &str, referrer: Option<&str>) {
    #[cfg(windows)]
    {
        let Some(host_url) = zone_url(url) else {
            return;
        };
        let mut zone = String::from("[ZoneTransfer]\r\nZoneId=3\r\n");
        if let Some(referrer) = referrer.and_then(zone_url) {
            zone.push_str(&format!("ReferrerUrl={referrer}\r\n"));
        }
        zone.push_str(&format!("HostUrl={host_url}\r\n"));
        let mut stream = path.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        let _ = std::fs::write(stream, zone);
    }
    #[cfg(not(windows))]
    let _ = (path, url, referrer);
}

/// `url` as a Zone.Identifier records it: web and FTP addresses only, with
/// any user name and password taken out.
#[cfg(windows)]
fn zone_url(url: &str) -> Option<String> {
    let mut parsed = url::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https" | "ftp") {
        return None;
    }
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    Some(parsed.to_string().replace(['\r', '\n'], ""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn downloads_are_marked_as_from_the_internet() {
        let dir = std::env::temp_dir().join(format!("uidm-motw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.exe");
        std::fs::write(&file, b"x").unwrap();
        mark_from_internet(&file, "https://user:pw@example.com/a.exe", Some("https://example.com/page"));
        let mut stream = file.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        let zone = std::fs::read_to_string(stream).unwrap();
        assert!(zone.contains("ZoneId=3"));
        assert!(zone.contains("HostUrl=https://example.com/a.exe"));
        assert!(zone.contains("ReferrerUrl=https://example.com/page"));
        assert!(!zone.contains("pw"), "no password in the mark");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn programs_are_named_for_the_system() {
        let name = exe_name("uidm-daemon");
        if cfg!(windows) {
            assert_eq!(name, "uidm-daemon.exe");
        } else {
            assert_eq!(name, "uidm-daemon");
        }
    }

    #[test]
    fn folders_end_in_uidm() {
        let config = config_dir().unwrap();
        assert!(config.ends_with("UIDM") || config.ends_with("uidm"), "{}", config.display());
        let data = data_dir().unwrap();
        assert!(data.ends_with("UIDM") || data.ends_with("uidm"), "{}", data.display());
    }
}
