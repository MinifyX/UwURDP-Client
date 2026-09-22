//! Sealing a blob under a password of its own — for export files.
//!
//! An export leaves the vault, so it cannot lean on the vault key: whoever
//! imports it may be on another device, with another vault or none. It gets
//! its own password instead, through the same Argon2id and XChaCha20-Poly1305
//! the vault uses. The KDF costs and the salt travel with the blob and are part
//! of the associated data, so a file whose header was edited to cheaper costs
//! fails to open instead of opening faster for an attacker.

use crate::kdf::{derive_master_secrets_with, KdfParams};
use crate::{Result, VaultError};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use zeroize::Zeroizing;

const LABEL: &[u8] = b"uwurdp/export/v1";

/// A blob sealed under a password, with everything needed to open it again
/// except the password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordSealed {
    pub kdf: KdfParams,
    pub salt: [u8; 16],
    pub nonce: [u8; 24],
    pub blob: Vec<u8>,
}

fn associated_data(kdf: KdfParams, salt: &[u8; 16]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(LABEL.len() + 12 + 16);
    aad.extend_from_slice(LABEL);
    aad.extend_from_slice(&kdf.memory_kib.to_be_bytes());
    aad.extend_from_slice(&kdf.time_cost.to_be_bytes());
    aad.extend_from_slice(&kdf.parallelism.to_be_bytes());
    aad.extend_from_slice(salt);
    aad
}

pub fn seal_with_password(
    password: &[u8],
    plaintext: &[u8],
    kdf: KdfParams,
) -> Result<PasswordSealed> {
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);
    let mut nonce = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce);
    let secrets = derive_master_secrets_with(password, &salt, kdf)?;
    let blob = XChaCha20Poly1305::new((&secrets.master_key).into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: &associated_data(kdf, &salt),
            },
        )
        .map_err(|e| VaultError::Encrypt(e.to_string()))?;
    Ok(PasswordSealed {
        kdf,
        salt,
        nonce,
        blob,
    })
}

/// A wrong password and a tampered file look the same: [`VaultError::Decrypt`].
pub fn open_with_password(password: &[u8], sealed: &PasswordSealed) -> Result<Zeroizing<Vec<u8>>> {
    let secrets = derive_master_secrets_with(password, &sealed.salt, sealed.kdf)?;
    XChaCha20Poly1305::new((&secrets.master_key).into())
        .decrypt(
            XNonce::from_slice(&sealed.nonce),
            Payload {
                msg: &sealed.blob,
                aad: &associated_data(sealed.kdf, &sealed.salt),
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| VaultError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAST: KdfParams = KdfParams::INSECURE_FOR_TESTS;

    #[test]
    fn the_right_password_opens_what_was_sealed() {
        let sealed = seal_with_password(b"export-pw", b"hosts and keys", FAST).unwrap();
        let opened = open_with_password(b"export-pw", &sealed).unwrap();
        assert_eq!(opened.as_slice(), b"hosts and keys");
    }

    #[test]
    fn the_wrong_password_does_not_open() {
        let sealed = seal_with_password(b"export-pw", b"hosts and keys", FAST).unwrap();
        assert!(matches!(
            open_with_password(b"export-pW", &sealed),
            Err(VaultError::Decrypt)
        ));
    }

    #[test]
    fn a_file_edited_to_cheaper_costs_does_not_open() {
        let kdf = KdfParams {
            memory_kib: 16,
            ..FAST
        };
        let mut sealed = seal_with_password(b"export-pw", b"hosts and keys", kdf).unwrap();
        sealed.kdf = FAST;
        assert!(open_with_password(b"export-pw", &sealed).is_err());
    }

    #[test]
    fn absurd_costs_in_a_file_are_refused() {
        let mut sealed = seal_with_password(b"export-pw", b"x", FAST).unwrap();
        sealed.kdf.memory_kib = u32::MAX;
        assert!(matches!(
            open_with_password(b"export-pw", &sealed),
            Err(VaultError::Kdf(_))
        ));
    }
}
