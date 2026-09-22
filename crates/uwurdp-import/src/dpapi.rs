//! The Windows DPAPI implementation of [`Decrypt`].
//!
//! RDCMan and mstsc seal passwords with `CryptProtectData` under the current
//! user with no extra entropy. Decryption is the mirror call. The plaintext is
//! UTF-16LE, but this type does not decode it: it returns the raw bytes and
//! lets [`crate::decode_dpapi_plaintext`] do the decoding, so the real cipher
//! and the fake one in the tests agree on exactly one place where UTF-16LE and
//! NUL padding are handled.

use crate::{decode_dpapi_plaintext, Decrypt, Secret};

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

/// Decrypts blobs with `CryptUnprotectData` for the current user.
pub struct Dpapi;

impl Dpapi {
    /// The one OS call: unprotect a blob into freshly allocated bytes we copy
    /// out and free. `None` on any failure — wrong user, certificate-sealed
    /// data that is not a DPAPI blob at all, or corruption.
    fn unprotect(blob: &[u8]) -> Option<Vec<u8>> {
        // The API takes a mutable-looking pointer but does not write through it.
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: blob.len() as u32,
            pbData: blob.as_ptr() as *mut u8,
        };
        let mut out_blob = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };

        // SAFETY: both blobs are valid for the call; `in_blob` borrows `blob`
        // for the duration, and the output buffer is copied out and freed
        // before returning.
        let ok = unsafe {
            CryptUnprotectData(
                &in_blob,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                &mut out_blob,
            )
        };

        if ok == 0 || out_blob.pbData.is_null() {
            return None;
        }

        // SAFETY: on success the API set `pbData`/`cbData` to a buffer it owns.
        let bytes = unsafe {
            std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec()
        };
        // SAFETY: the buffer was allocated by the API with LocalAlloc.
        unsafe {
            LocalFree(out_blob.pbData as *mut core::ffi::c_void);
        }
        Some(bytes)
    }
}

impl Decrypt for Dpapi {
    fn decrypt(&self, blob: &[u8]) -> Option<Secret> {
        Self::unprotect(blob).map(|raw| decode_dpapi_plaintext(&raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Security::Cryptography::CryptProtectData;

    /// Seal bytes the way RDCMan does: current user, no entropy.
    fn protect(plaintext: &[u8]) -> Vec<u8> {
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: plaintext.len() as u32,
            pbData: plaintext.as_ptr() as *mut u8,
        };
        let mut out_blob = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: valid input blob; output copied out and freed.
        let ok = unsafe {
            CryptProtectData(
                &in_blob,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                &mut out_blob,
            )
        };
        assert!(
            ok != 0 && !out_blob.pbData.is_null(),
            "CryptProtectData failed"
        );
        let bytes = unsafe {
            std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec()
        };
        unsafe {
            LocalFree(out_blob.pbData as *mut core::ffi::c_void);
        }
        bytes
    }

    #[test]
    fn round_trips_a_real_dpapi_blob() {
        // The plaintext RDCMan stores is UTF-16LE.
        let secret = "R0undTrip!pw";
        let utf16: Vec<u8> = secret
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        let blob = protect(&utf16);

        let got = Dpapi.decrypt(&blob).expect("decrypt");
        assert_eq!(got.as_str(), secret);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Dpapi.decrypt(&[0u8, 1, 2, 3, 4, 5]).is_none());
    }
}
