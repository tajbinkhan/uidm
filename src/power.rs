//! What a finished queue can do to the computer: sleep, hibernate or shut
//! down. The daemon calls these only after the 60-second countdown the window
//! shows has run out without being canceled.

use std::io;
use std::process::Command;

use crate::queue::WhenDone;

#[cfg(windows)]
#[link(name = "powrprof")]
extern "system" {
    fn SetSuspendState(hibernate: u8, force: u8, wakeup_events_disabled: u8) -> u8;
}

/// Carry out a power action. `Nothing` and `Exit` are not power actions and
/// do nothing here. `force_close` shuts down without waiting for programs
/// that hold it up (such as one asking to save a file); sleep and hibernate
/// never wait for programs.
pub fn perform(action: WhenDone, force_close: bool) -> io::Result<()> {
    #[cfg(windows)]
    if matches!(action, WhenDone::Sleep | WhenDone::Hibernate) {
        let hibernate = (action == WhenDone::Hibernate) as u8;
        // SAFETY: no pointers; returns 0 on failure.
        if unsafe { SetSuspendState(hibernate, 0, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        return Ok(());
    }
    let Some((program, args)) = command(action, force_close) else {
        return Ok(());
    };
    // System32's own copy, not one found on the PATH.
    #[cfg(windows)]
    let path = std::path::PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())).join("System32").join(program);
    #[cfg(not(windows))]
    let path = program;
    let status = Command::new(path).args(&args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{program} failed: {status}")))
    }
}

/// The program and arguments that carry out `action`.
///
/// - Windows: shutdown.exe, which holds the privilege dance ExitWindowsEx
///   would need; `/f` closes programs without waiting.
/// - macOS: pmset to sleep (the Mac's hibernate mode decides whether it
///   hibernates), and System Events to shut down the way the Apple menu does.
/// - Linux: systemd; forcing ignores the programs that ask to hold it up.
fn command(action: WhenDone, force_close: bool) -> Option<(&'static str, Vec<&'static str>)> {
    if matches!(action, WhenDone::Nothing | WhenDone::Exit) {
        return None;
    }
    #[cfg(windows)]
    {
        let _ = action;
        let mut args = vec!["/s", "/t", "0"];
        if force_close {
            args.push("/f");
        }
        Some(("shutdown.exe", args))
    }
    #[cfg(target_os = "macos")]
    {
        let _ = force_close;
        match action {
            WhenDone::ShutDown => Some(("osascript", vec!["-e", "tell application \"System Events\" to shut down"])),
            _ => Some(("pmset", vec!["sleepnow"])),
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let verb = match action {
            WhenDone::Sleep => "suspend",
            WhenDone::Hibernate => "hibernate",
            _ => "poweroff",
        };
        let mut args = vec![verb];
        if force_close {
            args.push("--ignore-inhibitors");
        }
        Some(("systemctl", args))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forcing_skips_waiting_for_programs() {
        assert_eq!(command(WhenDone::Nothing, true), None);
        assert_eq!(command(WhenDone::Exit, false), None);
        let (_, gentle) = command(WhenDone::ShutDown, false).unwrap();
        let (_, forced) = command(WhenDone::ShutDown, true).unwrap();
        if cfg!(windows) {
            assert_eq!(gentle, ["/s", "/t", "0"]);
            assert_eq!(forced, ["/s", "/t", "0", "/f"]);
        } else if cfg!(target_os = "linux") {
            assert_eq!(gentle, ["poweroff"]);
            assert_eq!(forced, ["poweroff", "--ignore-inhibitors"]);
            assert_eq!(command(WhenDone::Hibernate, false).unwrap(), ("systemctl", vec!["hibernate"]));
        }
    }
}
