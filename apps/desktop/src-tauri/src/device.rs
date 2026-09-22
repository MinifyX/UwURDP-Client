//! Sealing secrets for the signed-in user, so they open on start without the
//! master password.
//!
//! Two things are sealed this way: the vault key, when the vault is remembered
//! on this device, and what a paired device keeps from its pairing (its
//! signing key and the account key). Each purpose has its own label, so a blob
//! made for one can never be opened as the other.
//!
//! - **Windows:** DPAPI ties the blob to the signed-in account: another account
//!   on this machine can't open it and needs the master password. Anything
//!   running as this user can, and so can anyone with the account's password
//!   and a copy of the disk — a remembered vault is as strong as the Windows
//!   account, which is the trust UwURDP already places in the user.
//! - **macOS and Linux:** a random key of UwURDP's own lives in the Keychain or
//!   the Secret Service (GNOME Keyring, KWallet), and seals with
//!   XChaCha20-Poly1305. Where no Secret Service answers — a bare window
//!   manager — the key falls back to a file only this user can read, next to
//!   the database. That is weaker against a stolen disk, and said so in the
//!   docs.
//!
//! See `uwurdp_store::device`.

use zeroize::Zeroizing;

/// Mixed into every seal, so a blob made by UwURDP is only useful to UwURDP's
/// call for the same purpose.
const VAULT_LABEL: &[u8] = b"uwurdp/vault-key/device/v1";
const SYNC_LABEL: &[u8] = b"uwurdp/sync-keys/device/v1";

/// The vault key, remembered on this device.
pub(crate) fn protect(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    os::protect(bytes, VAULT_LABEL)
}

pub(crate) fn unprotect(bytes: &[u8]) -> std::io::Result<Zeroizing<Vec<u8>>> {
    os::unprotect(bytes, VAULT_LABEL)
}

/// What a paired device keeps: its signing key and the account key.
pub(crate) fn protect_sync(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    os::protect(bytes, SYNC_LABEL)
}

pub(crate) fn unprotect_sync(bytes: &[u8]) -> std::io::Result<Zeroizing<Vec<u8>>> {
    os::unprotect(bytes, SYNC_LABEL)
}

#[cfg(windows)]
mod os {
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };
    use zeroize::Zeroizing;

    pub(super) fn protect(bytes: &[u8], label: &[u8]) -> std::io::Result<Vec<u8>> {
        let input = blob(bytes);
        let entropy = blob(label);
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: the input blobs point at live slices for the duration of the
        // call; the output is allocated by DPAPI and freed below.
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(take(output).to_vec())
    }

    pub(super) fn unprotect(bytes: &[u8], label: &[u8]) -> std::io::Result<Zeroizing<Vec<u8>>> {
        let input = blob(bytes);
        let entropy = blob(label);
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: as in `protect`.
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(take(output))
    }

    fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: bytes.len() as u32,
            pbData: bytes.as_ptr() as *mut u8,
        }
    }

    /// Copy DPAPI's output into wiping memory, wipe DPAPI's copy and free it.
    fn take(output: CRYPT_INTEGER_BLOB) -> Zeroizing<Vec<u8>> {
        use windows_sys::Win32::Foundation::LocalFree;
        if output.pbData.is_null() {
            return Zeroizing::new(Vec::new());
        }
        // SAFETY: DPAPI returned `cbData` initialised bytes at `pbData`, which
        // stay valid until LocalFree.
        unsafe {
            let slice = std::slice::from_raw_parts_mut(output.pbData, output.cbData as usize);
            let copy = Zeroizing::new(slice.to_vec());
            zeroize::Zeroize::zeroize(slice);
            LocalFree(output.pbData.cast());
            copy
        }
    }
}

#[cfg(not(windows))]
mod os {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};
    use std::io::{Error, ErrorKind};
    use std::path::PathBuf;
    use zeroize::Zeroizing;

    const SERVICE: &str = "app.uwurdp.desktop";
    const ACCOUNT: &str = "device-seal-key";
    /// Marks a blob sealed this way, so a format change later can tell.
    const VERSION: u8 = 1;

    pub(super) fn protect(bytes: &[u8], label: &[u8]) -> std::io::Result<Vec<u8>> {
        use rand::RngCore;
        let key = seal_key()?;
        let cipher = XChaCha20Poly1305::new(key.as_slice().into());
        let mut nonce = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut nonce);
        let sealed = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: bytes,
                    aad: label,
                },
            )
            .map_err(|_| Error::other("sealing failed"))?;
        let mut out = Vec::with_capacity(1 + nonce.len() + sealed.len());
        out.push(VERSION);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    pub(super) fn unprotect(bytes: &[u8], label: &[u8]) -> std::io::Result<Zeroizing<Vec<u8>>> {
        let (Some(&VERSION), Some(rest)) = (bytes.first(), bytes.get(1..)) else {
            return Err(Error::new(ErrorKind::InvalidData, "not sealed by UwURDP"));
        };
        if rest.len() < 24 + 16 {
            return Err(Error::new(ErrorKind::InvalidData, "too short"));
        }
        let (nonce, sealed) = rest.split_at(24);
        let key = seal_key()?;
        let cipher = XChaCha20Poly1305::new(key.as_slice().into());
        cipher
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: sealed,
                    aad: label,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| Error::new(ErrorKind::InvalidData, "does not open for this user"))
    }

    /// UwURDP's own key for this user: from the Keychain or the Secret Service,
    /// made on first use; from a private file where neither answers.
    fn seal_key() -> std::io::Result<Zeroizing<Vec<u8>>> {
        match keychain_key() {
            Ok(key) => Ok(key),
            Err(error) => {
                tracing::warn!(%error, "no keychain; keeping the seal key in a private file");
                file_key()
            }
        }
    }

    fn keychain_key() -> Result<Zeroizing<Vec<u8>>, keyring::Error> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT)?;
        match entry.get_secret() {
            Ok(secret) if secret.len() == 32 => Ok(Zeroizing::new(secret)),
            Ok(_) | Err(keyring::Error::NoEntry) => {
                let key = fresh_key();
                entry.set_secret(&key)?;
                // Read back, so a keychain that silently drops writes is not
                // mistaken for one that keeps them.
                let stored = Zeroizing::new(entry.get_secret()?);
                if stored.as_slice() == key.as_slice() {
                    Ok(key)
                } else {
                    Err(keyring::Error::NoEntry)
                }
            }
            Err(error) => Err(error),
        }
    }

    fn fresh_key() -> Zeroizing<Vec<u8>> {
        use rand::RngCore;
        let mut key = Zeroizing::new(vec![0u8; 32]);
        rand::thread_rng().fill_bytes(&mut key);
        key
    }

    fn fallback_path() -> std::io::Result<PathBuf> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| Error::other("no home folder"))?;
        let base = if cfg!(target_os = "macos") {
            home.join("Library/Application Support")
        } else {
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .filter(|dir| dir.is_absolute())
                .unwrap_or_else(|| home.join(".local/share"))
        };
        Ok(base.join(SERVICE).join("device-seal.key"))
    }

    fn file_key() -> std::io::Result<Zeroizing<Vec<u8>>> {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let path = fallback_path()?;
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            // Only a plain file of ours, readable by nobody else, counts.
            if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
                return Err(Error::other("the seal key file is not private"));
            }
            let key = Zeroizing::new(std::fs::read(&path)?);
            return if key.len() == 32 {
                Ok(key)
            } else {
                Err(Error::new(
                    ErrorKind::InvalidData,
                    "the seal key file is damaged",
                ))
            };
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        let key = fresh_key();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(&key)?;
        file.sync_all()?;
        Ok(key)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn what_this_user_protects_this_user_can_open() {
        let key = [42u8; 32];
        let sealed = protect(&key).unwrap();
        assert_ne!(sealed.as_slice(), key.as_slice());
        assert_eq!(unprotect(&sealed).unwrap().as_slice(), key.as_slice());
    }

    #[test]
    fn a_tampered_blob_does_not_open() {
        let mut sealed = protect(&[1u8; 32]).unwrap();
        let middle = sealed.len() / 2;
        sealed[middle] ^= 0xff;
        assert!(unprotect(&sealed).is_err());
    }

    #[test]
    fn a_blob_for_one_purpose_does_not_open_as_another() {
        let sealed = protect(&[3u8; 32]).unwrap();
        assert!(unprotect_sync(&sealed).is_err());
        let sealed = protect_sync(&[4u8; 32]).unwrap();
        assert!(unprotect(&sealed).is_err());
        assert_eq!(unprotect_sync(&sealed).unwrap().as_slice(), &[4u8; 32]);
    }
}
