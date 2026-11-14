//! What differs between Windows, macOS and Linux and is not worth a module
//! of its own: program names, starting helper programs without a console
//! window, and opening files. Where UDM keeps its files is in `paths`.

use std::io;
use std::path::PathBuf;
use std::process::Command;

/// The file name of UDM's program `name` (`udm-daemon`) on this system.
pub fn exe_name(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// UDM's program `name`, next to the running one.
pub fn sibling_exe(name: &str) -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the program has no folder"))?;
    Ok(dir.join(exe_name(name)))
}

/// The AppImage UDM runs from, if it does. Its programs live in a mount
/// that goes away when the process that mounted it ends, so other programs
/// are started through the AppImage itself (its `AppRun` picks the program
/// by the first argument), and browsers are pointed at it.
pub fn appimage() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    std::env::var_os("APPIMAGE").filter(|p| !p.is_empty()).map(PathBuf::from).filter(|p| p.is_file())
}

/// How to start UDM's program `name` (`udm-gui`): its file and the
/// arguments that come first.
pub fn program(name: &str) -> io::Result<(PathBuf, Vec<String>)> {
    match appimage() {
        Some(image) => Ok((image, vec![name.to_string()])),
        None => Ok((sibling_exe(name)?, Vec::new())),
    }
}

/// A command that starts UDM's program `name`.
pub fn program_command(name: &str) -> io::Result<Command> {
    let (file, args) = program(name)?;
    let mut cmd = Command::new(file);
    cmd.args(args);
    Ok(cmd)
}

/// Start `cmd` and wait for it on a thread of its own, for a program UDM
/// starts and does not watch (a file's viewer, the window, the daemon).
/// Unix keeps an ended child as a zombie until its parent waits for it, and
/// the window and the daemon live long enough to collect many.
pub fn spawn_reaped(cmd: &mut Command) -> io::Result<()> {
    let mut child = cmd.spawn()?;
    // The program runs either way; without the thread it is only left
    // unwaited, as before.
    let _ = std::thread::Builder::new().name("reap-child".into()).spawn(move || {
        let _ = child.wait();
    });
    Ok(())
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
    spawn_reaped(no_window(&mut cmd))
}

/// Show `path` selected in the file manager: Explorer, the Finder, or on
/// Linux the file manager's `ShowItems` over D-Bus, else its folder.
pub fn reveal(path: &std::path::Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Explorer parses /select itself, so pass the argument raw with the
        // path quoted. Windows paths cannot contain a double quote.
        spawn_reaped(no_window(Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display()))))
    }
    #[cfg(target_os = "macos")]
    return spawn_reaped(Command::new("open").arg("-R").arg(path));
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let uri = url::Url::from_file_path(path)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "not a full path"))?;
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

/// Mark `path` as downloaded from the internet with no address to name, as
/// a torrent's files are: only `ZoneId=3` in its `Zone.Identifier` stream on
/// Windows. Elsewhere this does nothing.
pub fn mark_from_internet_unnamed(path: &std::path::Path) {
    #[cfg(windows)]
    {
        let mut stream = path.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        let _ = std::fs::write(stream, "[ZoneTransfer]\r\nZoneId=3\r\n");
    }
    #[cfg(not(windows))]
    let _ = path;
}

/// `url` as a Zone.Identifier records it: web and FTP addresses only, with
/// any user name and password taken out.
#[cfg(windows)]
fn zone_url(url: &str) -> Option<String> {
    let mut parsed = url::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https" | "ftp" | "ftps") {
        return None;
    }
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    Some(parsed.to_string().replace(['\r', '\n'], ""))
}

/// The `FileOpener` adapter: opens files with the desktop's default program.
pub struct Desktop;

impl udm_domain::ports::FileOpener for Desktop {
    fn open(&self, file: &str) {
        tracing::info!(file, "opening");
        if let Err(e) = open(file.as_ref()) {
            tracing::warn!(file, error = %e, "could not open");
        }
    }

    fn reveal(&self, path: &str) {
        if let Err(e) = reveal(std::path::Path::new(path)) {
            tracing::warn!(path, error = %e, "could not show the file");
        }
    }

    fn can_open_with(&self) -> bool {
        cfg!(windows)
    }

    fn open_with(&self, file: &str) {
        open_with::show(file);
    }
}

/// Windows' Open with picker; other systems have no such picker.
#[cfg(windows)]
mod open_with {
    #[repr(C)]
    struct OpenAsInfo {
        file: *const u16,
        class: *const u16,
        flags: u32,
    }

    const OAIF_ALLOW_REGISTRATION: u32 = 0x1;
    const OAIF_REGISTER_EXT: u32 = 0x2;
    const OAIF_EXEC: u32 = 0x4;

    #[link(name = "shell32")]
    extern "system" {
        fn SHOpenWithDialog(parent: isize, info: *const OpenAsInfo) -> i32;
    }

    pub fn show(path: &str) {
        let file: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let info = OpenAsInfo {
            file: file.as_ptr(),
            class: std::ptr::null(),
            flags: OAIF_ALLOW_REGISTRATION | OAIF_REGISTER_EXT | OAIF_EXEC,
        };
        // SAFETY: `info` and the path it points to live until the call
        // returns.
        let result = unsafe { SHOpenWithDialog(0, &info) };
        if result < 0 {
            tracing::warn!(file = path, result, "the Open with picker did not open");
        }
    }
}

#[cfg(not(windows))]
mod open_with {
    pub fn show(_path: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This process's ended children that nobody waited for, named `comm`.
    #[cfg(target_os = "linux")]
    fn zombies(comm: &str) -> usize {
        let me = std::process::id().to_string();
        let Ok(dir) = std::fs::read_dir("/proc") else { return 0 };
        dir.filter_map(|e| std::fs::read_to_string(e.ok()?.path().join("stat")).ok())
            .filter(|stat| {
                // pid (comm) state ppid ...
                let Some((name, rest)) = stat.split_once(") ") else { return false };
                let fields: Vec<&str> = rest.split_whitespace().collect();
                name.ends_with(&format!("({comm}"))
                    && fields.first() == Some(&"Z")
                    && fields.get(1) == Some(&me.as_str())
            })
            .count()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_program_started_and_forgotten_is_waited_for() {
        // `true` is not otherwise started by these tests.
        for _ in 0..3 {
            spawn_reaped(&mut Command::new("true")).unwrap();
        }
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while zombies("true") > 0 {
            assert!(std::time::Instant::now() < until, "ended programs left as zombies");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // A child nobody has waited for yet does show: the check can see one.
        let mut unwaited = Command::new("true").spawn().unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while zombies("true") == 0 {
            assert!(std::time::Instant::now() < until, "the zombie check sees nothing");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        unwaited.wait().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn downloads_are_marked_as_from_the_internet() {
        let dir = std::env::temp_dir().join(format!("udm-motw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.exe");
        std::fs::write(&file, b"x").unwrap();
        mark_from_internet(&file, "https://user:pw@example.com/a.exe", Some("https://example.com/page"));
        let unnamed = file.with_extension("torrent-file");
        std::fs::write(&unnamed, b"x").unwrap();
        mark_from_internet_unnamed(&unnamed);
        let mut zone = unnamed.as_os_str().to_owned();
        zone.push(":Zone.Identifier");
        assert_eq!(std::fs::read_to_string(zone).unwrap(), "[ZoneTransfer]\r\nZoneId=3\r\n");
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
        let name = exe_name("udm-daemon");
        if cfg!(windows) {
            assert_eq!(name, "udm-daemon.exe");
        } else {
            assert_eq!(name, "udm-daemon");
        }
    }
}
