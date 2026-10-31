//! Keeping saved requests, which hold the browser's cookies, and saved
//! passwords encrypted at rest, so only the same user on the same machine
//! can read them.
//!
//! On Windows this is DPAPI (`CryptProtectData`). On macOS and Linux it is
//! AES-256-CBC with an HMAC-SHA256 tag, keyed by a random key in UIDM's data
//! folder that only the user can read (`secret.key`, mode 600).

pub use imp::{protect, unprotect};

#[cfg(windows)]
mod imp {
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
}

#[cfg(unix)]
mod imp {
    use std::io;

    use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
    use hmac::{KeyInit, Mac};
    use sha2::Sha256;

    const MAGIC: &[u8] = b"UIDM-SEAL1";
    const IV: usize = 16;
    const TAG: usize = 32;

    fn bad(why: &str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, why.to_string())
    }

    /// The user's key, made the first time.
    fn master_key() -> io::Result<[u8; 32]> {
        let path = crate::platform::data_dir()?.join("secret.key");
        if let Ok(bytes) = std::fs::read(&path) {
            return bytes.try_into().map_err(|_| bad("secret.key is damaged"));
        }
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).map_err(|e| io::Error::other(format!("getrandom: {e}")))?;
        std::fs::create_dir_all(path.parent().unwrap())?;
        crate::platform::write_private(&path, &key)?;
        Ok(key)
    }

    /// One key for encrypting and one for the tag, from the user's key.
    fn keys() -> io::Result<([u8; 32], [u8; 32])> {
        let master = master_key()?;
        let derive = |label: &[u8]| -> [u8; 32] {
            let mut mac = <hmac::Hmac<Sha256> as KeyInit>::new_from_slice(&master).expect("any key length");
            mac.update(label);
            mac.finalize().into_bytes().into()
        };
        Ok((derive(b"uidm-encrypt"), derive(b"uidm-authenticate")))
    }

    fn tag(key: &[u8; 32], sealed: &[u8]) -> hmac::Hmac<Sha256> {
        let mut mac = <hmac::Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("any key length");
        mac.update(sealed);
        mac
    }

    /// Encrypt `data` for the current user.
    pub fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
        let (enc, auth) = keys()?;
        let mut iv = [0u8; IV];
        getrandom::fill(&mut iv).map_err(|e| io::Error::other(format!("getrandom: {e}")))?;
        let cipher = cbc::Encryptor::<aes::Aes256>::new(&enc.into(), &iv.into());
        let mut buf = vec![0u8; data.len() + 16];
        buf[..data.len()].copy_from_slice(data);
        let n = cipher.encrypt_padded_mut::<Pkcs7>(&mut buf, data.len()).map_err(|_| bad("could not encrypt"))?.len();
        let mut out = Vec::with_capacity(MAGIC.len() + IV + n + TAG);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&iv);
        out.extend_from_slice(&buf[..n]);
        let t = tag(&auth, &out).finalize().into_bytes();
        out.extend_from_slice(&t);
        Ok(out)
    }

    /// Decrypt data that `protect` produced for the current user.
    pub fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
        if data.len() < MAGIC.len() + IV + TAG || !data.starts_with(MAGIC) {
            return Err(bad("not data UIDM sealed"));
        }
        let (enc, auth) = keys()?;
        let (sealed, t) = data.split_at(data.len() - TAG);
        tag(&auth, sealed).verify_slice(t).map_err(|_| bad("sealed data was changed or sealed for another user"))?;
        let iv: [u8; IV] = sealed[MAGIC.len()..MAGIC.len() + IV].try_into().unwrap();
        let mut body = sealed[MAGIC.len() + IV..].to_vec();
        let plain = cbc::Decryptor::<aes::Aes256>::new(&enc.into(), &iv.into())
            .decrypt_padded_mut::<Pkcs7>(&mut body)
            .map_err(|_| bad("could not decrypt"))?;
        Ok(plain.to_vec())
    }
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
