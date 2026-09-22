//! Record encryption.
//!
//! XChaCha20-Poly1305 with a fresh 24-byte nonce per record. The associated
//! data is `record_id || kind || vault_id`, which is the part that actually
//! matters against a hostile server: without it, a server could hand back the
//! blob of record A when the client asked for record B, and the decryption
//! would succeed. With it, that swap fails the authentication tag.

use crate::{Result, VaultError};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use uuid::Uuid;
use uwurdp_proto::{EntityKind, Hlc};

/// An encrypted record, ready to go into an envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub nonce: Vec<u8>,
    pub blob: Vec<u8>,
}

/// Bind a ciphertext to the identity of the record it belongs to.
fn associated_data(id: Uuid, kind: EntityKind, vault_id: Uuid) -> Vec<u8> {
    let mut aad = Vec::with_capacity(16 + 1 + 16);
    aad.extend_from_slice(id.as_bytes());
    aad.push(kind as u8);
    aad.extend_from_slice(vault_id.as_bytes());
    aad
}

/// A label the record associated data above cannot collide with: that one is
/// 33 bytes of ids and a kind, this one starts with text.
const SYNCED_LABEL: &[u8] = b"uwurdp/record/v2";

/// Everything about a record a server must not be able to change: which record
/// it is, what kind, whose vault, when it was written and whether it is a
/// tombstone.
///
/// The last two are the point. A server that can flip `deleted` can remove a
/// host from every device, because a tombstone beats a concurrent edit; a
/// server that can rewrite the clock can make an old version look like the
/// newest one. Covering both means such an envelope fails its authentication
/// tag instead of being applied.
fn synced_associated_data(
    id: Uuid,
    kind: EntityKind,
    vault_id: Uuid,
    updated_at: Hlc,
    deleted: bool,
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(SYNCED_LABEL.len() + 16 + 1 + 16 + 16 + 1);
    aad.extend_from_slice(SYNCED_LABEL);
    aad.extend_from_slice(id.as_bytes());
    aad.push(kind as u8);
    aad.extend_from_slice(vault_id.as_bytes());
    aad.extend_from_slice(&updated_at.wall_ms.to_be_bytes());
    aad.extend_from_slice(&updated_at.counter.to_be_bytes());
    aad.extend_from_slice(&updated_at.device.to_be_bytes());
    aad.push(u8::from(deleted));
    aad
}

/// Seal a record for the way to the server. A tombstone seals an empty
/// payload rather than nothing, so its header is authenticated as well.
pub fn encrypt_synced(
    vault_key: &[u8; 32],
    id: Uuid,
    kind: EntityKind,
    vault_id: Uuid,
    updated_at: Hlc,
    deleted: bool,
    plaintext: &[u8],
) -> Result<Sealed> {
    seal(
        vault_key,
        &synced_associated_data(id, kind, vault_id, updated_at, deleted),
        plaintext,
    )
}

pub fn decrypt_synced(
    vault_key: &[u8; 32],
    id: Uuid,
    kind: EntityKind,
    vault_id: Uuid,
    updated_at: Hlc,
    deleted: bool,
    sealed: &Sealed,
) -> Result<Vec<u8>> {
    open(
        vault_key,
        &synced_associated_data(id, kind, vault_id, updated_at, deleted),
        sealed,
    )
}

fn seal(vault_key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Result<Sealed> {
    let cipher = XChaCha20Poly1305::new(vault_key.into());
    let mut nonce_bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let blob = cipher
        .encrypt(
            XNonce::from_slice(&nonce_bytes),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|e| VaultError::Encrypt(e.to_string()))?;
    Ok(Sealed {
        nonce: nonce_bytes.to_vec(),
        blob,
    })
}

fn open(vault_key: &[u8; 32], aad: &[u8], sealed: &Sealed) -> Result<Vec<u8>> {
    if sealed.nonce.len() != 24 {
        return Err(VaultError::Decrypt);
    }
    XChaCha20Poly1305::new(vault_key.into())
        .decrypt(
            XNonce::from_slice(&sealed.nonce),
            Payload {
                msg: &sealed.blob,
                aad,
            },
        )
        .map_err(|_| VaultError::Decrypt)
}

pub fn encrypt_record(
    vault_key: &[u8; 32],
    id: Uuid,
    kind: EntityKind,
    vault_id: Uuid,
    plaintext: &[u8],
) -> Result<Sealed> {
    seal(vault_key, &associated_data(id, kind, vault_id), plaintext)
}

pub fn decrypt_record(
    vault_key: &[u8; 32],
    id: Uuid,
    kind: EntityKind,
    vault_id: Uuid,
    sealed: &Sealed,
) -> Result<Vec<u8>> {
    open(vault_key, &associated_data(id, kind, vault_id), sealed)
}

/// Seal something under a key that is already strong — a key two devices just
/// agreed on, say, rather than one derived from a password.
///
/// The label is the associated data, so a blob sealed for one purpose cannot
/// be opened as another. There is no key derivation here on purpose: the
/// caller brings 32 bytes of real key material.
pub fn encrypt_keyed(key: &[u8; 32], label: &[u8], plaintext: &[u8]) -> Result<Sealed> {
    seal(key, label, plaintext)
}

pub fn decrypt_keyed(key: &[u8; 32], label: &[u8], sealed: &Sealed) -> Result<Vec<u8>> {
    open(key, label, sealed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> [u8; 32] {
        [7u8; 32]
    }

    #[test]
    fn round_trip() {
        let id = Uuid::from_u128(1);
        let vault = Uuid::from_u128(99);
        let sealed = encrypt_record(&key(), id, EntityKind::Host, vault, b"prox-1").unwrap();
        let out = decrypt_record(&key(), id, EntityKind::Host, vault, &sealed).unwrap();
        assert_eq!(out, b"prox-1");
    }

    #[test]
    fn a_swapped_record_id_fails() {
        let vault = Uuid::from_u128(99);
        let sealed = encrypt_record(
            &key(),
            Uuid::from_u128(1),
            EntityKind::Host,
            vault,
            b"secret",
        )
        .unwrap();

        // The server hands this blob back as if it belonged to another record.
        let out = decrypt_record(&key(), Uuid::from_u128(2), EntityKind::Host, vault, &sealed);
        assert!(
            out.is_err(),
            "AAD must bind the ciphertext to its record id"
        );
    }

    #[test]
    fn a_reinterpreted_kind_fails() {
        let id = Uuid::from_u128(1);
        let vault = Uuid::from_u128(99);
        let sealed = encrypt_record(&key(), id, EntityKind::Snippet, vault, b"secret").unwrap();

        let out = decrypt_record(&key(), id, EntityKind::Key, vault, &sealed);
        assert!(out.is_err(), "a snippet must not be readable as a key");
    }

    #[test]
    fn a_tampered_blob_fails() {
        let id = Uuid::from_u128(1);
        let vault = Uuid::from_u128(99);
        let mut sealed = encrypt_record(&key(), id, EntityKind::Host, vault, b"secret").unwrap();
        sealed.blob[0] ^= 0xff;

        assert!(decrypt_record(&key(), id, EntityKind::Host, vault, &sealed).is_err());
    }

    /// The four things a hostile server must not be able to change about a
    /// record on its way back to a device.
    #[test]
    fn a_synced_record_is_bound_to_its_whole_header() {
        let id = Uuid::from_u128(1);
        let vault = Uuid::from_u128(99);
        let clock = Hlc::new(1_700_000_000_000, 2, 7);
        let sealed =
            encrypt_synced(&key(), id, EntityKind::Host, vault, clock, false, b"prox-1").unwrap();

        let opened =
            decrypt_synced(&key(), id, EntityKind::Host, vault, clock, false, &sealed).unwrap();
        assert_eq!(opened, b"prox-1");

        // Marked deleted: a tombstone beats a concurrent edit, so this one bit
        // would remove the host from every device.
        assert!(
            decrypt_synced(&key(), id, EntityKind::Host, vault, clock, true, &sealed).is_err(),
            "the tombstone flag must be authenticated"
        );
        // Handed back with a newer clock, to make an old version win.
        let forward = Hlc::new(clock.wall_ms + 60_000, 0, 7);
        assert!(
            decrypt_synced(&key(), id, EntityKind::Host, vault, forward, false, &sealed).is_err(),
            "the clock must be authenticated"
        );
        // Handed back as another record, or as another kind.
        assert!(decrypt_synced(
            &key(),
            Uuid::from_u128(2),
            EntityKind::Host,
            vault,
            clock,
            false,
            &sealed
        )
        .is_err());
        assert!(decrypt_synced(&key(), id, EntityKind::Key, vault, clock, false, &sealed).is_err());
    }

    #[test]
    fn a_tombstone_carries_a_sealed_payload_of_its_own() {
        let id = Uuid::from_u128(1);
        let vault = Uuid::from_u128(99);
        let clock = Hlc::new(5, 0, 1);
        let sealed = encrypt_synced(&key(), id, EntityKind::Host, vault, clock, true, b"").unwrap();
        assert!(
            !sealed.blob.is_empty(),
            "an empty payload still has an authentication tag"
        );
        assert_eq!(
            decrypt_synced(&key(), id, EntityKind::Host, vault, clock, true, &sealed).unwrap(),
            Vec::<u8>::new()
        );
    }

    /// A record sealed for storage must not open as a synced one, or the
    /// header the synced form authenticates could be bypassed by presenting
    /// the stored form instead.
    #[test]
    fn the_two_forms_do_not_open_each_other() {
        let id = Uuid::from_u128(1);
        let vault = Uuid::from_u128(99);
        let clock = Hlc::new(5, 0, 1);
        let stored = encrypt_record(&key(), id, EntityKind::Secret, vault, b"nyu").unwrap();
        assert!(
            decrypt_synced(&key(), id, EntityKind::Secret, vault, clock, false, &stored).is_err()
        );

        let synced =
            encrypt_synced(&key(), id, EntityKind::Secret, vault, clock, false, b"nyu").unwrap();
        assert!(decrypt_record(&key(), id, EntityKind::Secret, vault, &synced).is_err());
    }

    #[test]
    fn nonces_do_not_repeat() {
        let id = Uuid::from_u128(1);
        let vault = Uuid::from_u128(99);
        let a = encrypt_record(&key(), id, EntityKind::Host, vault, b"same").unwrap();
        let b = encrypt_record(&key(), id, EntityKind::Host, vault, b"same").unwrap();
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(
            a.blob, b.blob,
            "identical plaintext must not produce identical ciphertext"
        );
    }
}
