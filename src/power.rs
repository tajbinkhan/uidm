//! What a finished queue can do to the computer: sleep, hibernate or shut
//! down. The daemon calls these only after the 60-second countdown the window
//! shows has run out without being canceled.

use std::io;
use std::path::PathBuf;
use std::process::Command;

use crate::queue::WhenDone;

#[link(name = "powrprof")]
extern "system" {
    fn SetSuspendState(hibernate: u8, force: u8, wakeup_events_disabled: u8) -> u8;
}

/// Carry out a power action. `Nothing` and `Exit` are not power actions and
/// do nothing here.
pub fn perform(action: WhenDone) -> io::Result<()> {
    match action {
        WhenDone::Sleep | WhenDone::Hibernate => {
            let hibernate = (action == WhenDone::Hibernate) as u8;
            // SAFETY: no pointers; returns 0 on failure.
            if unsafe { SetSuspendState(hibernate, 0, 0) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        WhenDone::ShutDown => {
            // shutdown.exe holds the privilege dance ExitWindowsEx would need.
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
            let exe = PathBuf::from(root).join("System32").join("shutdown.exe");
            let status = Command::new(exe).args(["/s", "/t", "0"]).status()?;
            if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!("shutdown.exe failed: {status}")))
            }
        }
        WhenDone::Nothing | WhenDone::Exit => Ok(()),
    }
}
