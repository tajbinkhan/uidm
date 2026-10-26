//! Windows DPAPI (`CryptProtectData`), used to keep saved requests, which hold
//! the browser's cookies, encrypted at rest. Only the same Windows user on the
//! same machine can decrypt them.

use std::ffi::c_void;
use std::io;
use std::ptr;

/// Fixed extra entropy, so another program calling DPAPI as the same user
/// cannot decrypt UIDM's data by accident.
const ENTROPY: &[u8] = b"uidm-saved-requests-v1";
const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

#[repr(C)]
struct DataBlob {
    cb_data: u32,
    pb_data: *mut u8,
}

#[link(name = "crypt32")]
extern "system" {
    fn CryptProtectData(
        data_in: *const DataBlob,
        description: *const u16,
        entropy: *const DataBlob,
        reserved: *mut c_void,
        prompt: *const c_void,
        flags: u32,
        data_out: *mut DataBlob,
    ) -> i32;
    fn CryptUnprotectData(
        data_in: *const DataBlob,
        description: *mut *mut u16,
        entropy: *const DataBlob,
        reserved: *mut c_void,
        prompt: *const c_void,
        flags: u32,
        data_out: *mut DataBlob,
    ) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn LocalFree(mem: *mut c_void) -> *mut c_void;
}

/// Encrypt `data` for the current user.
pub fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
    call(data, true)
}

/// Decrypt data that `protect` produced for the current user.
pub fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
    call(data, false)
}

fn blob(data: &[u8]) -> io::Result<DataBlob> {
    Ok(DataBlob {
        cb_data: data
            .len()
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "data too large for DPAPI"))?,
        // DPAPI only reads input blobs.
        pb_data: data.as_ptr() as *mut u8,
    })
}

fn call(data: &[u8], encrypt: bool) -> io::Result<Vec<u8>> {
    let input = blob(data)?;
    let entropy = blob(ENTROPY)?;
    let mut output = DataBlob {
        cb_data: 0,
        pb_data: ptr::null_mut(),
    };
    // SAFETY: every pointer refers to a live blob for the length of the call;
    // DPAPI allocates `output` with LocalAlloc, freed below after copying.
    let ok = unsafe {
        if encrypt {
            CryptProtectData(
                &input,
                ptr::null(),
                &entropy,
                ptr::null_mut(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                ptr::null_mut(),
                &entropy,
                ptr::null_mut(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success DPAPI set `output` to `cb_data` readable bytes.
    let bytes = unsafe {
        let out = std::slice::from_raw_parts(output.pb_data, output.cb_data as usize).to_vec();
        LocalFree(output.pb_data as *mut c_void);
        out
    };
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_hides_the_plaintext() {
        let secret = b"Cookie: session=abc123; theme=dark";
        let sealed = protect(secret).unwrap();
        assert!(!sealed.windows(secret.len()).any(|w| w == secret));
        assert_eq!(unprotect(&sealed).unwrap(), secret);
    }

    #[test]
    fn tampered_data_is_rejected() {
        let mut sealed = protect(b"hello").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0xff;
        assert!(unprotect(&sealed).is_err());
    }
}
