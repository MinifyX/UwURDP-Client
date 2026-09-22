//! A vault on one device: created with a master password, unlocked with it,
//! and then sealing and opening secrets under its vault key.
//!
//! ```text
//! master password ──Argon2id(salt, params)──► master key ──wraps──► vault key
//!                                                                     │
//!                         secret ◄──XChaCha20-Poly1305(id, kind)──────┘
//! ```
//!
//! What gets stored is a [`VaultHeader`]: salt, parameters and the wrapped
//! vault key. None of it is secret; all of it is useless without the password.
//! The unwrapped vault key only ever exists inside an [`UnlockedVault`], which
//! wipes it when dropped.

use crate::account::AccountKey;
use crate::crypto::{decrypt_record, decrypt_synced, encrypt_record, encrypt_synced, Sealed};
use crate::kdf::{derive_master_secrets_for, server_auth_key, KdfParams};
use crate::{Result, VaultError};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use uuid::Uuid;
use uwurdp_proto::{EntityKind, Hlc};
use zeroize::Zeroizing;

/// Associated data for wrapping the vault key: a label no record's AAD can
/// collide with (those are 33 bytes of ids and a kind), and the vault's id.
const WRAP_LABEL: &[u8] = b"uwurdp/vault-key/v1";

/// Everything stored about a vault. Not secret — and useless without the
/// master password, and, once a vault is synced, without the account key too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultHeader {
    pub vault_id: Uuid,
    pub kdf: KdfParams,
    pub salt: [u8; 16],
    pub wrapped_key: Sealed,
    /// Whether opening this vault needs the account key as well as the
    /// password. A device without it cannot get in at all, which is the whole
    /// point — and is why this is written down rather than guessed at.
    pub needs_account_key: bool,
}

/// A vault whose key is in memory. Dropping it locks the vault.
pub struct UnlockedVault {
    vault_id: Uuid,
    key: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for UnlockedVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UnlockedVault({}, key redacted)", self.vault_id)
    }
}

/// Create a vault: a fresh random vault key, wrapped under the password.
pub fn create(
    password: &[u8],
    vault_id: Uuid,
    kdf: KdfParams,
) -> Result<(VaultHeader, UnlockedVault)> {
    create_with(password, None, vault_id, kdf)
}

/// The same, for a vault that will be synced: the account key goes into the
/// derivation, so what the server stores is worth nothing without it.
pub fn create_with(
    password: &[u8],
    account_key: Option<&AccountKey>,
    vault_id: Uuid,
    kdf: KdfParams,
) -> Result<(VaultHeader, UnlockedVault)> {
    let mut key = Zeroizing::new([0u8; 32]);
    rand::thread_rng().fill_bytes(key.as_mut());
    let unlocked = UnlockedVault { vault_id, key };
    let header = unlocked.rewrap(password, account_key, kdf)?;
    Ok((header, unlocked))
}

impl VaultHeader {
    /// Unlock with the master password. A wrong password fails the wrapped
    /// key's authentication tag, so there is no separate verifier to attack.
    ///
    /// A vault that needs the account key is refused here rather than failing
    /// as a wrong password: those two are different problems, and telling a
    /// user their password is wrong when it is the kit that is missing sends
    /// them looking in the wrong place.
    pub fn unlock(&self, password: &[u8]) -> Result<UnlockedVault> {
        if self.needs_account_key {
            return Err(VaultError::NeedsAccountKey);
        }
        self.unlock_with(password, None)
    }

    pub fn unlock_with(
        &self,
        password: &[u8],
        account_key: Option<&AccountKey>,
    ) -> Result<UnlockedVault> {
        if self.needs_account_key && account_key.is_none() {
            return Err(VaultError::NeedsAccountKey);
        }
        let master = derive_master_secrets_for(password, &self.salt, self.kdf, account_key)?;
        let unwrapped = unwrap(&master.master_key, self.vault_id, &self.wrapped_key)?;
        let key: [u8; 32] = unwrapped
            .as_slice()
            .try_into()
            .map_err(|_| VaultError::Decrypt)?;
        Ok(UnlockedVault {
            vault_id: self.vault_id,
            key: Zeroizing::new(key),
        })
    }

    /// The key a device proves itself to the server with. Derived from the
    /// same two inputs, two HKDF steps away from anything that opens the
    /// vault, so what the server stores is no shortcut.
    pub fn login_key(
        &self,
        password: &[u8],
        account_key: Option<&AccountKey>,
    ) -> Result<Zeroizing<[u8; 32]>> {
        let master = derive_master_secrets_for(password, &self.salt, self.kdf, account_key)?;
        server_auth_key(&master)
    }
}

impl UnlockedVault {
    pub fn vault_id(&self) -> Uuid {
        self.vault_id
    }

    /// Seal a secret, bound to its record id and kind.
    pub fn seal(&self, id: Uuid, kind: EntityKind, plaintext: &[u8]) -> Result<Sealed> {
        encrypt_record(&self.key, id, kind, self.vault_id, plaintext)
    }

    pub fn open(&self, id: Uuid, kind: EntityKind, sealed: &Sealed) -> Result<Zeroizing<Vec<u8>>> {
        decrypt_record(&self.key, id, kind, self.vault_id, sealed).map(Zeroizing::new)
    }

    /// Seal a record for the way to the server, with its whole header — clock
    /// and tombstone flag included — as associated data.
    pub fn seal_synced(
        &self,
        id: Uuid,
        kind: EntityKind,
        updated_at: Hlc,
        deleted: bool,
        plaintext: &[u8],
    ) -> Result<Sealed> {
        encrypt_synced(
            &self.key,
            id,
            kind,
            self.vault_id,
            updated_at,
            deleted,
            plaintext,
        )
    }

    pub fn open_synced(
        &self,
        id: Uuid,
        kind: EntityKind,
        updated_at: Hlc,
        deleted: bool,
        sealed: &Sealed,
    ) -> Result<Zeroizing<Vec<u8>>> {
        decrypt_synced(
            &self.key,
            id,
            kind,
            self.vault_id,
            updated_at,
            deleted,
            sealed,
        )
        .map(Zeroizing::new)
    }

    /// The vault key itself, for the one place that keeps it without the
    /// master password: storage the operating system protects for this user
    /// on this device, so the vault can open on its own.
    pub fn export_key(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(*self.key)
    }

    /// A vault from a key exported earlier. Nothing is checked here: a wrong
    /// key only shows when it fails to open something the real key sealed, so
    /// callers verify it against such a record before trusting it.
    pub fn from_key(vault_id: Uuid, key: Zeroizing<[u8; 32]>) -> Self {
        Self { vault_id, key }
    }

    /// Wrap this vault's key under a password — and, for a synced vault, an
    /// account key — and hand back the header that goes with it.
    ///
    /// This is the whole of changing a master password and of turning sync on:
    /// one 32-byte key is wrapped again, and not one record is touched. A new
    /// salt each time, so the same password never derives the same key twice.
    pub fn rewrap(
        &self,
        password: &[u8],
        account_key: Option<&AccountKey>,
        kdf: KdfParams,
    ) -> Result<VaultHeader> {
        let mut salt = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut salt);
        let master = derive_master_secrets_for(password, &salt, kdf, account_key)?;
        let wrapped_key = wrap(&master.master_key, self.vault_id, &self.key)?;
        Ok(VaultHeader {
            vault_id: self.vault_id,
            kdf,
            salt,
            wrapped_key,
            needs_account_key: account_key.is_some(),
        })
    }
}

fn wrap_aad(vault_id: Uuid) -> Vec<u8> {
    [WRAP_LABEL, vault_id.as_bytes()].concat()
}

fn wrap(master_key: &[u8; 32], vault_id: Uuid, vault_key: &[u8; 32]) -> Result<Sealed> {
    let mut nonce = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce);
    let blob = XChaCha20Poly1305::new(master_key.into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: vault_key,
                aad: &wrap_aad(vault_id),
            },
        )
        .map_err(|e| VaultError::Encrypt(e.to_string()))?;
    Ok(Sealed {
        nonce: nonce.to_vec(),
        blob,
    })
}

fn unwrap(master_key: &[u8; 32], vault_id: Uuid, wrapped: &Sealed) -> Result<Zeroizing<Vec<u8>>> {
    if wrapped.nonce.len() != 24 {
        return Err(VaultError::Decrypt);
    }
    XChaCha20Poly1305::new(master_key.into())
        .decrypt(
            XNonce::from_slice(&wrapped.nonce),
            Payload {
                msg: &wrapped.blob,
                aad: &wrap_aad(vault_id),
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| VaultError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAST: KdfParams = KdfParams::INSECURE_FOR_TESTS;

    fn vault() -> (VaultHeader, UnlockedVault) {
        create(b"correct horse battery staple", Uuid::from_u128(7), FAST).unwrap()
    }

    #[test]
    fn the_right_password_unlocks_what_creating_sealed() {
        let (header, created) = vault();
        let id = Uuid::from_u128(1);
        let sealed = created.seal(id, EntityKind::Secret, b"nyu").unwrap();
        drop(created);

        let unlocked = header.unlock(b"correct horse battery staple").unwrap();
        assert_eq!(
            unlocked
                .open(id, EntityKind::Secret, &sealed)
                .unwrap()
                .as_slice(),
            b"nyu"
        );
    }

    #[test]
    fn the_wrong_password_does_not_unlock() {
        let (header, _) = vault();
        assert!(matches!(
            header.unlock(b"correct horse battery stapler"),
            Err(VaultError::Decrypt)
        ));
    }

    #[test]
    fn a_header_moved_to_another_vault_id_does_not_unlock() {
        let (mut header, _) = vault();
        header.vault_id = Uuid::from_u128(8);
        assert!(header.unlock(b"correct horse battery staple").is_err());
    }

    #[test]
    fn a_secret_is_bound_to_its_id() {
        let (_, unlocked) = vault();
        let sealed = unlocked
            .seal(Uuid::from_u128(1), EntityKind::Secret, b"nyu")
            .unwrap();
        assert!(unlocked
            .open(Uuid::from_u128(2), EntityKind::Secret, &sealed)
            .is_err());
    }

    #[test]
    fn a_synced_vault_needs_both_the_password_and_the_kit() {
        let account_key = AccountKey::generate();
        let (header, created) = create_with(
            b"correct horse battery staple",
            Some(&account_key),
            Uuid::from_u128(7),
            FAST,
        )
        .unwrap();
        let id = Uuid::from_u128(1);
        let sealed = created.seal(id, EntityKind::Secret, b"nyu").unwrap();
        drop(created);

        assert!(header.needs_account_key);
        // The password alone is turned away, and not as a wrong password.
        assert!(matches!(
            header.unlock(b"correct horse battery staple"),
            Err(VaultError::NeedsAccountKey)
        ));
        assert!(matches!(
            header.unlock_with(b"correct horse battery staple", None),
            Err(VaultError::NeedsAccountKey)
        ));
        // Somebody else's kit is a wrong key, which is a different answer.
        assert!(matches!(
            header.unlock_with(
                b"correct horse battery staple",
                Some(&AccountKey::generate())
            ),
            Err(VaultError::Decrypt)
        ));
        // Both together open it.
        let unlocked = header
            .unlock_with(b"correct horse battery staple", Some(&account_key))
            .unwrap();
        assert_eq!(
            unlocked
                .open(id, EntityKind::Secret, &sealed)
                .unwrap()
                .as_slice(),
            b"nyu"
        );
    }

    #[test]
    fn turning_sync_on_rewraps_one_key_and_leaves_the_records_alone() {
        let (header, unlocked) = vault();
        let id = Uuid::from_u128(1);
        let sealed = unlocked.seal(id, EntityKind::Secret, b"nyu").unwrap();

        // Sync is switched on: same vault key, new wrapping, account key now.
        let account_key = AccountKey::generate();
        let synced = unlocked
            .rewrap(b"correct horse battery staple", Some(&account_key), FAST)
            .unwrap();
        drop(unlocked);

        assert_ne!(synced.salt, header.salt, "a fresh salt every time");
        assert_eq!(synced.vault_id, header.vault_id);
        let reopened = synced
            .unlock_with(b"correct horse battery staple", Some(&account_key))
            .unwrap();
        assert_eq!(
            reopened
                .open(id, EntityKind::Secret, &sealed)
                .unwrap()
                .as_slice(),
            b"nyu",
            "what was sealed before still opens: no record was re-encrypted"
        );

        // And the old header still opens the same vault with the old inputs.
        assert!(header.unlock(b"correct horse battery staple").is_ok());
    }

    #[test]
    fn the_login_key_follows_the_password_and_the_kit() {
        let account_key = AccountKey::generate();
        let (header, _) = create_with(b"pw", Some(&account_key), Uuid::from_u128(7), FAST).unwrap();

        let right = header.login_key(b"pw", Some(&account_key)).unwrap();
        assert_eq!(
            header
                .login_key(b"pw", Some(&account_key))
                .unwrap()
                .as_slice(),
            right.as_slice(),
            "the same every time, or no device could log in twice"
        );
        assert_ne!(
            header
                .login_key(b"other", Some(&account_key))
                .unwrap()
                .as_slice(),
            right.as_slice()
        );
        assert_ne!(
            header
                .login_key(b"pw", Some(&AccountKey::generate()))
                .unwrap()
                .as_slice(),
            right.as_slice()
        );
        assert_ne!(
            header.login_key(b"pw", None).unwrap().as_slice(),
            right.as_slice()
        );
    }

    #[test]
    fn two_vaults_with_the_same_password_have_different_keys() {
        let (a_header, a) = vault();
        let (b_header, _) = vault();
        assert_ne!(a_header.salt, b_header.salt);

        let sealed = a
            .seal(Uuid::from_u128(1), EntityKind::Secret, b"nyu")
            .unwrap();
        let b = b_header.unlock(b"correct horse battery staple").unwrap();
        assert!(b
            .open(Uuid::from_u128(1), EntityKind::Secret, &sealed)
            .is_err());
    }

    #[test]
    fn the_vault_key_never_shows_up_in_debug_output() {
        let (_, unlocked) = vault();
        let printed = format!("{unlocked:?}");
        assert!(printed.contains("redacted"), "{printed}");
    }
}
