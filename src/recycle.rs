//! Sending files and folders to the Recycle Bin (`SHFileOperationW` with
//! undo), so a download removed with its files can still be brought back.

use std::ffi::c_void;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr;

const FO_DELETE: u32 = 3;
const FOF_SILENT: u16 = 0x0004;
const FOF_NOCONFIRMATION: u16 = 0x0010;
const FOF_ALLOWUNDO: u16 = 0x0040;
const FOF_NOERRORUI: u16 = 0x0400;

/// `SHFILEOPSTRUCTW` as laid out on 64-bit Windows.
#[repr(C)]
struct FileOp {
    hwnd: *mut c_void,
    func: u32,
    from: *const u16,
    to: *const u16,
    flags: u16,
    any_aborted: i32,
    name_mappings: *mut c_void,
    progress_title: *const u16,
}

#[link(name = "shell32")]
extern "system" {
    fn SHFileOperationW(op: *mut FileOp) -> i32;
}

/// Move `paths` to the Recycle Bin without asking or showing progress.
/// Paths that do not exist are skipped.
pub fn recycle(paths: &[PathBuf]) -> io::Result<()> {
    let present: Vec<&PathBuf> = paths.iter().filter(|p| p.exists()).collect();
    if present.is_empty() {
        return Ok(());
    }
    // A list of full paths, each ending in a NUL, with one more NUL at the
    // end. The shell only recycles full paths.
    let mut from: Vec<u16> = Vec::new();
    for p in present {
        let full = std::path::absolute(p)?;
        from.extend(full.as_os_str().encode_wide());
        from.push(0);
    }
    from.push(0);
    let mut op = FileOp {
        hwnd: ptr::null_mut(),
        func: FO_DELETE,
        from: from.as_ptr(),
        to: ptr::null(),
        flags: FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI,
        any_aborted: 0,
        name_mappings: ptr::null_mut(),
        progress_title: ptr::null(),
    };
    // SAFETY: `op` and the path list it points to outlive the call.
    let code = unsafe { SHFileOperationW(&mut op) };
    if code != 0 {
        return Err(io::Error::other(format!("the shell could not recycle the files (code {code:#x})")));
    }
    if op.any_aborted != 0 {
        return Err(io::Error::other("recycling was canceled"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recycles_files_and_folders() {
        let dir = std::env::temp_dir().join(format!("uidm_recycle_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("parts")).unwrap();
        let file = dir.join("a.bin");
        std::fs::write(&file, b"x").unwrap();
        std::fs::write(dir.join("parts").join("1.tmp"), b"y").unwrap();
        recycle(&[file.clone(), dir.join("parts"), dir.join("missing")]).unwrap();
        assert!(!file.exists());
        assert!(!dir.join("parts").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
