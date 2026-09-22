//! Opening the vault without the master password, on this device only.
//!
//! The vault key is handed to the operating system to seal for the signed-in
//! user — DPAPI on Windows — and the sealed blob is kept in `device_unlock`, a
//! table that never syncs. On start the app asks the OS to unseal it and the
//! vault opens on its own, so the master password is needed once per device,
//! not once per start.
//!
//! This trades a little for a lot of comfort, on purpose and in the open: a
//! remembered vault is exactly as strong as the Windows account. Anything
//! running as that user can ask DPAPI too, and whoever has the account's
//! password (or, on a domain, the domain's backup key) can unseal the blob
//! from a copy of the disk. That user is already trusted with everything else
//! UwURDP does (see the security review), and another account on the machine
//! still meets the master password. The store takes the sealing as two
//! closures, so it stays free of OS code and testable.

use crate::{now_ms, Result, Store, StoreError};
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;
use uwurdp_proto::EntityKind;
use uwurdp_vault::{Sealed, UnlockedVault};
use zeroize::Zeroizing;

/// Sealed with the real vault key next to the protected copy: a copy that
/// opens this is the right key, one that doesn't is stale.
const CHECK: &[u8] = b"uwurdp/device-unlock/v1";

/// The stored row: vault id, protected key, and the check value's nonce and blob.
type KeptKey = (String, Vec<u8>, Vec<u8>, Vec<u8>);

/// The record id the check value is sealed under: derived from the vault's id
/// so no real record can share its associated data.
fn check_id(vault_id: Uuid) -> Uuid {
    Uuid::from_u128(vault_id.as_u128() ^ 0x7577_7573_7368_2d64_6576_6963_652d_636b)
}

impl Store {
    /// Keep the unlocked vault's key on this device, sealed by `protect`.
    pub fn remember_vault(
        &self,
        protect: impl FnOnce(&[u8]) -> std::io::Result<Vec<u8>>,
    ) -> Result<()> {
        let (vault_id, key, check) = {
            let guard = self.vault.lock();
            let vault = guard.as_ref().ok_or(StoreError::VaultLocked)?;
            let check = vault.seal(check_id(vault.vault_id()), EntityKind::Secret, CHECK)?;
            (vault.vault_id(), vault.export_key(), check)
        };
        let protected = protect(key.as_ref()).map_err(|e| StoreError::Device(e.to_string()))?;
        self.conn.lock().execute(
            "INSERT INTO device_unlock (id, vault_id, protected, check_nonce, check_blob, created_ms)
             VALUES (1, ?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (id) DO UPDATE SET
                vault_id = excluded.vault_id, protected = excluded.protected,
                check_nonce = excluded.check_nonce, check_blob = excluded.check_blob,
                created_ms = excluded.created_ms",
            params![
                vault_id.to_string(),
                protected,
                check.nonce,
                check.blob,
                now_ms() as i64
            ],
        )?;
        tracing::info!("vault remembered on this device");
        Ok(())
    }

    pub fn forget_remembered_vault(&self) -> Result<()> {
        let conn = self.conn.lock();
        let removed = conn.execute("DELETE FROM device_unlock WHERE id = 1", [])?;
        if removed > 0 {
            crate::vault::truncate_wal(&conn);
            tracing::info!("vault no longer remembered on this device");
        }
        Ok(())
    }

    pub fn vault_is_remembered(&self) -> Result<bool> {
        let count: i64 =
            self.conn
                .lock()
                .query_row("SELECT count(*) FROM device_unlock", [], |row| row.get(0))?;
        Ok(count > 0)
    }

    /// Unlock with the key kept on this device. `Ok(false)` when nothing is
    /// kept — or when what is kept no longer fits this vault or this user, in
    /// which case it is removed, and the master password is the way in again.
    pub fn unlock_remembered_vault(
        &self,
        unprotect: impl FnOnce(&[u8]) -> std::io::Result<Zeroizing<Vec<u8>>>,
    ) -> Result<bool> {
        let row: Option<KeptKey> = self
            .conn
            .lock()
            .query_row(
                "SELECT vault_id, protected, check_nonce, check_blob FROM device_unlock WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((vault_id, protected, nonce, blob)) = row else {
            return Ok(false);
        };

        let header = self.load_header()?;
        let opened = (|| {
            let vault_id = Uuid::parse_str(&vault_id).ok()?;
            if header.as_ref()?.vault_id != vault_id {
                return None;
            }
            let bytes = unprotect(&protected).ok()?;
            let key: [u8; 32] = bytes.as_slice().try_into().ok()?;
            let vault = UnlockedVault::from_key(vault_id, Zeroizing::new(key));
            let check = vault
                .open(
                    check_id(vault_id),
                    EntityKind::Secret,
                    &Sealed { nonce, blob },
                )
                .ok()?;
            (check.as_slice() == CHECK).then_some(vault)
        })();

        match opened {
            Some(vault) => {
                *self.vault.lock() = Some(vault);
                tracing::info!("vault unlocked with this device's key");
                Ok(true)
            }
            None => {
                tracing::warn!("the key kept on this device no longer opens the vault");
                self.forget_remembered_vault()?;
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VaultStatus;
    use uwurdp_vault::KdfParams;

    /// A stand-in for DPAPI: XOR with a per-"user" byte.
    fn protect_as(user: u8) -> impl Fn(&[u8]) -> std::io::Result<Vec<u8>> {
        move |bytes| Ok(bytes.iter().map(|b| b ^ user).collect())
    }

    fn unprotect_as(user: u8) -> impl Fn(&[u8]) -> std::io::Result<Zeroizing<Vec<u8>>> {
        move |bytes| Ok(Zeroizing::new(bytes.iter().map(|b| b ^ user).collect()))
    }

    fn unlocked() -> Store {
        let store = Store::open_in_memory().unwrap();
        store
            .create_vault_with(b"master", KdfParams::INSECURE_FOR_TESTS)
            .unwrap();
        store
    }

    #[test]
    fn a_remembered_vault_opens_without_the_password() {
        let store = unlocked();
        store.remember_vault(protect_as(7)).unwrap();
        assert!(store.vault_is_remembered().unwrap());
        store.lock_vault();
        assert!(store.unlock_remembered_vault(unprotect_as(7)).unwrap());
        assert_eq!(store.vault_status().unwrap(), VaultStatus::Unlocked);
    }

    #[test]
    fn remembering_needs_an_unlocked_vault() {
        let store = unlocked();
        store.lock_vault();
        assert!(matches!(
            store.remember_vault(protect_as(7)),
            Err(StoreError::VaultLocked)
        ));
        assert!(!store.unlock_remembered_vault(unprotect_as(7)).unwrap());
    }

    #[test]
    fn a_key_another_user_unseals_is_refused_and_forgotten() {
        let store = unlocked();
        store.remember_vault(protect_as(7)).unwrap();
        store.lock_vault();
        assert!(!store.unlock_remembered_vault(unprotect_as(9)).unwrap());
        assert_eq!(store.vault_status().unwrap(), VaultStatus::Locked);
        assert!(
            !store.vault_is_remembered().unwrap(),
            "the stale entry is gone"
        );
        // The master password still works.
        store.unlock_vault(b"master").unwrap();
    }

    #[test]
    fn forgetting_means_the_password_again() {
        let store = unlocked();
        store.remember_vault(protect_as(7)).unwrap();
        store.forget_remembered_vault().unwrap();
        store.lock_vault();
        assert!(!store.unlock_remembered_vault(unprotect_as(7)).unwrap());
    }
}
