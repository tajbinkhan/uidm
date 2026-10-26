//! The few per-user registry values UIDM keeps, all under `HKEY_CURRENT_USER`,
//! so none needs administrator rights:
//!
//! - `Software\Microsoft\Windows\CurrentVersion\Run`, value `UIDM`: starts
//!   the window hidden in the tray at sign-in ("Start with Windows").
//! - `Software\Classes\AppUserModelId\UIDM.DownloadManager`: the name and
//!   icon Windows shows on UIDM's notifications.

use std::io;
use std::path::Path;
use std::ptr;

type Hkey = isize;

const HKEY_CURRENT_USER: Hkey = 0x8000_0001u32 as i32 as isize;
const KEY_QUERY_VALUE: u32 = 0x0001;
const KEY_SET_VALUE: u32 = 0x0002;
const REG_SZ: u32 = 1;
const REG_DWORD: u32 = 4;
const ERROR_SUCCESS: i32 = 0;
const ERROR_FILE_NOT_FOUND: i32 = 2;

#[link(name = "advapi32")]
extern "system" {
    fn RegCreateKeyExW(
        key: Hkey,
        subkey: *const u16,
        reserved: u32,
        class: *const u16,
        options: u32,
        sam: u32,
        security: *const std::ffi::c_void,
        result: *mut Hkey,
        disposition: *mut u32,
    ) -> i32;
    fn RegOpenKeyExW(key: Hkey, subkey: *const u16, options: u32, sam: u32, result: *mut Hkey) -> i32;
    fn RegSetValueExW(key: Hkey, name: *const u16, reserved: u32, kind: u32, data: *const u8, len: u32) -> i32;
    fn RegQueryValueExW(
        key: Hkey,
        name: *const u16,
        reserved: *mut u32,
        kind: *mut u32,
        data: *mut u8,
        len: *mut u32,
    ) -> i32;
    fn RegDeleteValueW(key: Hkey, name: *const u16) -> i32;
    fn RegCloseKey(key: Hkey) -> i32;
}

pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const RUN_VALUE: &str = "UIDM";
/// The AppUserModelID UIDM's notifications are shown under.
pub const APP_ID: &str = "UIDM.DownloadManager";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn check(code: i32, what: &str) -> io::Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::new(
            io::Error::from_raw_os_error(code).kind(),
            format!("{what}: {}", io::Error::from_raw_os_error(code)),
        ))
    }
}

/// An open key, closed when dropped.
struct Key(Hkey);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful open or create.
        unsafe { RegCloseKey(self.0) };
    }
}

fn create(subkey: &str) -> io::Result<Key> {
    let mut key: Hkey = 0;
    let name = wide(subkey);
    // SAFETY: valid strings and out pointer for the call.
    let code = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            name.as_ptr(),
            0,
            ptr::null(),
            0,
            KEY_SET_VALUE | KEY_QUERY_VALUE,
            ptr::null(),
            &mut key,
            ptr::null_mut(),
        )
    };
    check(code, subkey)?;
    Ok(Key(key))
}

/// Open `subkey` if it exists.
fn open(subkey: &str, sam: u32) -> io::Result<Option<Key>> {
    let mut key: Hkey = 0;
    let name = wide(subkey);
    // SAFETY: valid string and out pointer for the call.
    let code = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, name.as_ptr(), 0, sam, &mut key) };
    if code == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    check(code, subkey)?;
    Ok(Some(Key(key)))
}

/// Set a string value, creating the key if needed.
pub fn set_string(subkey: &str, name: &str, value: &str) -> io::Result<()> {
    let key = create(subkey)?;
    let name = wide(name);
    let data = wide(value);
    // SAFETY: `data` is a NUL-terminated UTF-16 buffer of the given byte size.
    let code = unsafe {
        RegSetValueExW(
            key.0,
            name.as_ptr(),
            0,
            REG_SZ,
            data.as_ptr() as *const u8,
            (data.len() * 2) as u32,
        )
    };
    check(code, "set registry value")
}

/// Read a string value; `None` when the key or value does not exist.
pub fn get_string(subkey: &str, name: &str) -> io::Result<Option<String>> {
    let Some(key) = open(subkey, KEY_QUERY_VALUE)? else {
        return Ok(None);
    };
    let name = wide(name);
    let mut kind = 0u32;
    let mut len = 0u32;
    // SAFETY: a size query with a null buffer.
    let code = unsafe {
        RegQueryValueExW(key.0, name.as_ptr(), ptr::null_mut(), &mut kind, ptr::null_mut(), &mut len)
    };
    if code == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    check(code, "read registry value")?;
    let mut buf = vec![0u16; (len as usize).div_ceil(2)];
    // SAFETY: `buf` holds at least `len` bytes.
    let code = unsafe {
        RegQueryValueExW(
            key.0,
            name.as_ptr(),
            ptr::null_mut(),
            &mut kind,
            buf.as_mut_ptr() as *mut u8,
            &mut len,
        )
    };
    check(code, "read registry value")?;
    if kind != REG_SZ {
        return Ok(None);
    }
    let chars = (len as usize / 2).min(buf.len());
    let s = String::from_utf16_lossy(&buf[..chars]);
    Ok(Some(s.trim_end_matches('\0').to_string()))
}

/// Read a DWORD value; `None` when the key or value does not exist or is
/// another type.
pub fn get_dword(subkey: &str, name: &str) -> io::Result<Option<u32>> {
    let Some(key) = open(subkey, KEY_QUERY_VALUE)? else {
        return Ok(None);
    };
    let name = wide(name);
    let mut kind = 0u32;
    let mut value = 0u32;
    let mut len = 4u32;
    // SAFETY: a 4-byte buffer for a DWORD.
    let code = unsafe {
        RegQueryValueExW(
            key.0,
            name.as_ptr(),
            ptr::null_mut(),
            &mut kind,
            &mut value as *mut u32 as *mut u8,
            &mut len,
        )
    };
    if code == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    check(code, "read registry value")?;
    Ok((kind == REG_DWORD).then_some(value))
}

/// Set a DWORD value, creating the key if needed.
pub fn set_dword(subkey: &str, name: &str, value: u32) -> io::Result<()> {
    let key = create(subkey)?;
    let name = wide(name);
    // SAFETY: points at 4 bytes for a DWORD.
    let code = unsafe {
        RegSetValueExW(key.0, name.as_ptr(), 0, REG_DWORD, &value as *const u32 as *const u8, 4)
    };
    check(code, "set registry value")
}

/// Delete a value; a missing key or value is not an error.
pub fn delete_value(subkey: &str, name: &str) -> io::Result<()> {
    let Some(key) = open(subkey, KEY_SET_VALUE)? else {
        return Ok(());
    };
    let name = wide(name);
    // SAFETY: valid key and string.
    let code = unsafe { RegDeleteValueW(key.0, name.as_ptr()) };
    if code == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    check(code, "delete registry value")
}

/// The Run command that starts `gui` hidden in the tray.
pub fn autostart_command(gui: &Path) -> String {
    format!("\"{}\" --minimized", gui.display())
}

/// Add or remove the "Start with Windows" entry for `gui`.
pub fn set_autostart(enabled: bool, gui: &Path) -> io::Result<()> {
    if enabled {
        set_string(RUN_KEY, RUN_VALUE, &autostart_command(gui))
    } else {
        delete_value(RUN_KEY, RUN_VALUE)
    }
}

/// Register the name and icon Windows shows on UIDM's notifications.
pub fn register_app_id(icon: &Path) -> io::Result<()> {
    let key = format!(r"Software\Classes\AppUserModelId\{APP_ID}");
    set_string(&key, "DisplayName", "UIDM")?;
    set_string(&key, "IconUri", &icon.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_KEY: &str = r"Software\UIDM-test";

    #[test]
    fn set_read_and_delete_values() {
        set_string(TEST_KEY, "value", "C:\\Program Files\\x — ü").unwrap();
        assert_eq!(get_string(TEST_KEY, "value").unwrap().as_deref(), Some("C:\\Program Files\\x — ü"));
        delete_value(TEST_KEY, "value").unwrap();
        assert_eq!(get_string(TEST_KEY, "value").unwrap(), None);
        // Deleting again, or from a key that does not exist, is fine.
        delete_value(TEST_KEY, "value").unwrap();
        delete_value(r"Software\UIDM-test-missing", "value").unwrap();
        assert_eq!(get_string(r"Software\UIDM-test-missing", "value").unwrap(), None);

        set_dword(TEST_KEY, "flag", 1).unwrap();
        assert_eq!(get_dword(TEST_KEY, "flag").unwrap(), Some(1));
        set_string(TEST_KEY, "text", "x").unwrap();
        assert_eq!(get_dword(TEST_KEY, "text").unwrap(), None, "a string is not a DWORD");
        delete_value(TEST_KEY, "flag").unwrap();
        delete_value(TEST_KEY, "text").unwrap();
        assert_eq!(get_dword(TEST_KEY, "flag").unwrap(), None);
    }

    #[test]
    fn autostart_command_quotes_the_path() {
        let gui = Path::new(r"C:\Program Files\UIDM\uidm-gui.exe");
        assert_eq!(autostart_command(gui), r#""C:\Program Files\UIDM\uidm-gui.exe" --minimized"#);
    }
}
