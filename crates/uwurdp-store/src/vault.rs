//! The vault's life on this device: create it, unlock it, lock it.
//!
//! The header — salt, KDF parameters, wrapped key — lives in the `vault` table;
//! it is not secret and is useless without the master password. The unlocked
//! key lives only in [`Store`]'s memory, held in a mutex, and is wiped when the
//! store is dropped or the vault is locked. Secrets are sealed and opened
//! against that in-memory key, so the plaintext of a password or a private key
//! never reaches SQLite.

use crate::{tick, vault_id, Result, Store, StoreError};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::Serialize;
use uuid::Uuid;
use uwurdp_vault::{KdfParams, Sealed, UnlockedVault, VaultHeader};
use zeroize::Zeroizing;

/// What the UI needs to know before it can store or reveal a secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VaultStatus {
    /// No master password has ever been set.
    Absent,
    /// A vault exists but the key is not in memory.
    Locked,
    Unlocked,
}

impl Store {
    pub fn vault_status(&self) -> Result<VaultStatus> {
        if self.vault.lock().is_some() {
            return Ok(VaultStatus::Unlocked);
        }
        Ok(if self.load_header()?.is_some() {
            VaultStatus::Locked
        } else {
            VaultStatus::Absent
        })
    }

    /// Create the vault with a master password and leave it unlocked. Uses the
    /// device's existing `vault_id`, so records written before the vault
    /// existed keep the same owner.
    pub fn create_vault(&self, password: &[u8]) -> Result<()> {
        self.create_vault_with(password, KdfParams::RECOMMENDED)
    }

    /// Create the vault for an account that syncs: the account key goes into
    /// the derivation, so what the server stores is worth nothing without it.
    pub fn create_synced_vault(
        &self,
        password: &[u8],
        account_key: &uwurdp_vault::AccountKey,
        kdf: KdfParams,
    ) -> Result<()> {
        self.create_vault_inner(password, Some(account_key), kdf)
    }

    pub fn create_vault_with(&self, password: &[u8], kdf: KdfParams) -> Result<()> {
        self.create_vault_inner(password, None, kdf)
    }

    fn create_vault_inner(
        &self,
        password: &[u8],
        account_key: Option<&uwurdp_vault::AccountKey>,
        kdf: KdfParams,
    ) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        if header_row(&tx)?.is_some() {
            return Err(StoreError::VaultExists);
        }
        let vault_id: String =
            tx.query_row("SELECT vault_id FROM meta WHERE id = 1", [], |r| r.get(0))?;
        let vault_id = Uuid::parse_str(&vault_id).map_err(|_| StoreError::NoVault)?;

        let (header, unlocked) = uwurdp_vault::create_with(password, account_key, vault_id, kdf)?;
        store_header(&tx, &header)?;
        tx.commit()?;
        // Locks are always taken connection first, vault second, and never
        // the other way round: reveal and import run at the same time.
        drop(conn);
        *self.vault.lock() = Some(unlocked);
        tracing::info!("vault created");
        Ok(())
    }

    /// Unlock an existing vault. A wrong password is [`StoreError::Vault`].
    ///
    /// A vault that also needs its account key says so, rather than failing as
    /// a wrong password: see [`Self::unlock_vault_with`].
    pub fn unlock_vault(&self, password: &[u8]) -> Result<()> {
        let header = self.load_header()?.ok_or(StoreError::NoVault)?;
        let unlocked = header.unlock(password)?;
        *self.vault.lock() = Some(unlocked);
        tracing::info!("vault unlocked");
        Ok(())
    }

    /// Unlock with the password and an account key the caller has.
    pub fn unlock_vault_with(
        &self,
        password: &[u8],
        account_key: Option<&uwurdp_vault::AccountKey>,
    ) -> Result<()> {
        let header = self.load_header()?.ok_or(StoreError::NoVault)?;
        let unlocked = header.unlock_with(password, account_key)?;
        *self.vault.lock() = Some(unlocked);
        tracing::info!("vault unlocked");
        Ok(())
    }

    /// Whether this vault needs the account key at all.
    pub fn vault_needs_account_key(&self) -> Result<bool> {
        Ok(self
            .load_header()?
            .map(|header| header.needs_account_key)
            .unwrap_or(false))
    }

    /// Wrap the vault key again under new inputs and keep the new header.
    ///
    /// This is both turning sync on and changing the master password: one key
    /// is wrapped again, no record is touched. The vault must be unlocked,
    /// since its key is what gets wrapped.
    pub fn rewrap_vault(
        &self,
        password: &[u8],
        account_key: Option<&uwurdp_vault::AccountKey>,
    ) -> Result<VaultHeader> {
        let header = self.wrap_vault_key(password, account_key)?;
        self.keep_vault_header(&header)?;
        Ok(header)
    }

    /// The header [`Self::rewrap_vault`] would keep, without keeping it.
    ///
    /// For a change that only counts once someone else agreed — turning sync
    /// on is the server taking the header first: until then the vault here has
    /// to stay as it was, or a refusal leaves it wrapped under an account key
    /// nobody has.
    pub fn wrap_vault_key(
        &self,
        password: &[u8],
        account_key: Option<&uwurdp_vault::AccountKey>,
    ) -> Result<VaultHeader> {
        let conn = self.conn.lock();
        let guard = self.vault.lock();
        let vault = guard.as_ref().ok_or(StoreError::VaultLocked)?;
        let kdf = self
            .load_header_locked(&conn)?
            .map(|header| header.kdf)
            .unwrap_or(KdfParams::RECOMMENDED);
        Ok(vault.rewrap(password, account_key, kdf)?)
    }

    /// Keep a header made by [`Self::wrap_vault_key`]. It has to wrap the key
    /// of the vault that is open right now, or it is refused.
    pub fn keep_vault_header(&self, header: &VaultHeader) -> Result<()> {
        let mut conn = self.conn.lock();
        let guard = self.vault.lock();
        let vault = guard.as_ref().ok_or(StoreError::VaultLocked)?;
        if vault.vault_id() != header.vault_id {
            return Err(StoreError::NoVault);
        }

        let tx = conn.transaction()?;
        tx.execute("DELETE FROM vault", [])?;
        store_header(&tx, header)?;
        tx.commit()?;
        tracing::info!(
            account_key = header.needs_account_key,
            "the vault key was wrapped again"
        );
        Ok(())
    }

    /// Drop the key from memory. Idempotent.
    pub fn lock_vault(&self) {
        if self.vault.lock().take().is_some() {
            tracing::info!("vault locked");
        }
    }

    /// Open one stored secret by id. Fails if the vault is locked; the
    /// plaintext comes back in memory that wipes itself.
    pub fn reveal_secret(&self, id: Uuid) -> Result<Revealed> {
        // The row first, with only the connection locked; the vault after.
        // Holding the vault while waiting for the connection deadlocked
        // against an import, which takes them in the other order.
        if self.vault.lock().is_none() {
            return Err(StoreError::VaultLocked);
        }
        let sealed = self
            .conn
            .lock()
            .query_row(
                "SELECT nonce, blob FROM secrets WHERE id = ?1 AND deleted = 0",
                [id.to_string()],
                |row| {
                    Ok(Sealed {
                        nonce: row.get(0)?,
                        blob: row.get(1)?,
                    })
                },
            )
            .optional()?
            .ok_or(StoreError::NoVault)?;
        let guard = self.vault.lock();
        let vault = guard.as_ref().ok_or(StoreError::VaultLocked)?;
        Ok(vault.open(id, uwurdp_proto::EntityKind::Secret, &sealed)?)
    }

    /// The vault's header: salt, key derivation costs and the wrapped key.
    ///
    /// None of it is secret and all of it is useless without the master
    /// password, which is what makes it the thing a device uploads when it
    /// creates an account, and the thing a second device downloads before it
    /// asks for that password.
    pub fn vault_header(&self) -> Result<Option<VaultHeader>> {
        self.load_header()
    }

    pub(crate) fn load_header(&self) -> Result<Option<VaultHeader>> {
        header_row(&self.conn.lock())
    }

    fn load_header_locked(&self, conn: &rusqlite::Connection) -> Result<Option<VaultHeader>> {
        header_row(conn)
    }
}

/// Seal a secret with the unlocked vault and store it, returning its id.
pub(crate) fn seal_secret(
    tx: &Transaction,
    device: u32,
    vault: &UnlockedVault,
    plaintext: &[u8],
) -> Result<String> {
    let id = Uuid::now_v7();
    let sealed = vault.seal(id, uwurdp_proto::EntityKind::Secret, plaintext)?;
    let clock = tick(tx, device)?;
    tx.execute(
        "INSERT INTO secrets
            (id, vault_id, nonce, blob, hlc_wall_ms, hlc_counter, hlc_device)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id.to_string(),
            vault_id(tx)?,
            sealed.nonce,
            sealed.blob,
            clock.wall_ms as i64,
            clock.counter,
            clock.device,
        ],
    )?;
    Ok(id.to_string())
}

/// Tombstone a secret and drop its ciphertext with it. A replaced password has
/// no reason to stay around, not even encrypted: whoever learns the master
/// password later would learn the old one too.
pub(crate) fn forget_secret(tx: &Transaction, device: u32, id: &str) -> Result<()> {
    let clock = tick(tx, device)?;
    tx.execute(
        "UPDATE secrets
            SET deleted = 1, nonce = x'', blob = x'', rev = rev + 1,
                dirty = 1, hlc_wall_ms = ?2, hlc_counter = ?3, hlc_device = ?4
          WHERE id = ?1 AND deleted = 0",
        params![id, clock.wall_ms as i64, clock.counter, clock.device],
    )?;
    Ok(())
}

/// Fold the write-ahead log back into the database and empty it. After a
/// secret is forgotten, the log still holds the page it was on; with
/// `secure_delete` on, this is what finally overwrites it. Best effort: a
/// reader holding the log open just means it happens next time.
pub(crate) fn truncate_wal(conn: &rusqlite::Connection) {
    let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
}

pub(crate) fn store_header(conn: &rusqlite::Connection, header: &VaultHeader) -> Result<()> {
    conn.execute(
        "INSERT INTO vault
            (id, vault_id, kdf_memory_kib, kdf_time_cost, kdf_parallelism,
             salt, wrapped_nonce, wrapped_blob, needs_account_key)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            header.vault_id.to_string(),
            header.kdf.memory_kib,
            header.kdf.time_cost,
            header.kdf.parallelism,
            header.salt.to_vec(),
            header.wrapped_key.nonce,
            header.wrapped_key.blob,
            header.needs_account_key,
        ],
    )?;
    Ok(())
}

fn header_row(conn: &rusqlite::Connection) -> Result<Option<VaultHeader>> {
    conn.query_row(
        "SELECT vault_id, kdf_memory_kib, kdf_time_cost, kdf_parallelism,
                salt, wrapped_nonce, wrapped_blob, needs_account_key
           FROM vault WHERE id = 1",
        [],
        |row| {
            let vault_id: String = row.get(0)?;
            let salt: Vec<u8> = row.get(4)?;
            Ok((
                vault_id,
                KdfParams {
                    memory_kib: row.get(1)?,
                    time_cost: row.get(2)?,
                    parallelism: row.get(3)?,
                },
                salt,
                Sealed {
                    nonce: row.get(5)?,
                    blob: row.get(6)?,
                },
                row.get::<_, bool>(7)?,
            ))
        },
    )
    .optional()?
    .map(|(vault_id, kdf, salt, wrapped_key, needs_account_key)| {
        let vault_id = Uuid::parse_str(&vault_id).map_err(|_| StoreError::NoVault)?;
        // A header with absurd costs would make every unlock run out of memory.
        if !kdf.within_limits() {
            return Err(StoreError::Vault(uwurdp_vault::VaultError::Kdf(
                "the vault header asks for unreasonable key derivation costs".into(),
            )));
        }
        let salt: [u8; 16] = salt
            .as_slice()
            .try_into()
            .map_err(|_| StoreError::NoVault)?;
        Ok(VaultHeader {
            vault_id,
            kdf,
            salt,
            wrapped_key,
            needs_account_key,
        })
    })
    .transpose()
}

/// A secret revealed from the vault: wiped from memory when dropped.
pub type Revealed = Zeroizing<Vec<u8>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn create(store: &Store) {
        store
            .create_vault_with(b"open sesame", KdfParams::INSECURE_FOR_TESTS)
            .unwrap();
    }

    #[test]
    fn status_walks_absent_locked_unlocked() {
        let store = store();
        assert_eq!(store.vault_status().unwrap(), VaultStatus::Absent);
        create(&store);
        assert_eq!(store.vault_status().unwrap(), VaultStatus::Unlocked);
        store.lock_vault();
        assert_eq!(store.vault_status().unwrap(), VaultStatus::Locked);
    }

    #[test]
    fn a_vault_cannot_be_created_twice() {
        let store = store();
        create(&store);
        assert!(matches!(
            store.create_vault_with(b"again", KdfParams::INSECURE_FOR_TESTS),
            Err(StoreError::VaultExists)
        ));
    }

    #[test]
    fn the_right_password_unlocks_a_reopened_vault() {
        let path = std::env::temp_dir().join(format!("uwurdp-vault-{}.db", Uuid::now_v7()));
        {
            let store = Store::open(&path).unwrap();
            create(&store);
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.vault_status().unwrap(), VaultStatus::Locked);
        assert!(store.unlock_vault(b"wrong").is_err());
        assert_eq!(store.vault_status().unwrap(), VaultStatus::Locked);
        store.unlock_vault(b"open sesame").unwrap();
        assert_eq!(store.vault_status().unwrap(), VaultStatus::Unlocked);

        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn a_synced_vault_says_that_it_needs_its_account_key() {
        let store = store();
        let account_key = uwurdp_vault::AccountKey::generate();
        store
            .create_synced_vault(b"master", &account_key, KdfParams::INSECURE_FOR_TESTS)
            .unwrap();
        assert!(store.vault_needs_account_key().unwrap());

        store.lock_vault();
        assert!(matches!(
            store.unlock_vault(b"master"),
            Err(StoreError::Vault(uwurdp_vault::VaultError::NeedsAccountKey))
        ));
        assert!(store.unlock_vault_with(b"master", None).is_err());
        store
            .unlock_vault_with(b"master", Some(&account_key))
            .unwrap();
        assert_eq!(store.vault_status().unwrap(), VaultStatus::Unlocked);
    }

    #[test]
    fn turning_sync_on_keeps_the_secrets_that_are_already_sealed() {
        let store = store();
        create(&store);
        // A password, sealed under the vault as it is now.
        let host = store
            .save_host(crate::hosts::tests::draft("nas", "10.0.0.9"))
            .unwrap();
        store
            .set_host_login(
                host.id,
                "admin",
                "",
                Some(crate::SecretText::new("hunter2")),
            )
            .unwrap();

        let account_key = uwurdp_vault::AccountKey::generate();
        let header = store
            .rewrap_vault(b"open sesame", Some(&account_key))
            .unwrap();
        assert!(header.needs_account_key);
        assert_eq!(
            store.reveal_host_password(host.id).unwrap().as_slice(),
            b"hunter2",
            "the vault key did not change, so nothing had to be re-encrypted"
        );

        // And it is the new inputs that open it from now on.
        store.lock_vault();
        assert!(store.unlock_vault_with(b"open sesame", None).is_err());
        store
            .unlock_vault_with(b"open sesame", Some(&account_key))
            .unwrap();
        assert_eq!(
            store.reveal_host_password(host.id).unwrap().as_slice(),
            b"hunter2"
        );
    }

    #[test]
    fn a_header_with_absurd_kdf_costs_is_refused() {
        let store = store();
        create(&store);
        store.lock_vault();
        store
            .conn
            .lock()
            .execute("UPDATE vault SET kdf_memory_kib = 4294967295", [])
            .unwrap();
        assert!(matches!(
            store.unlock_vault(b"open sesame"),
            Err(StoreError::Vault(uwurdp_vault::VaultError::Kdf(_)))
        ));
    }

    #[test]
    fn revealing_and_importing_at_once_does_not_deadlock() {
        use crate::import::ImportSet;
        let store = std::sync::Arc::new(store());
        create(&store);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(400);
        let revealer = {
            let store = std::sync::Arc::clone(&store);
            std::thread::spawn(move || {
                while std::time::Instant::now() < deadline {
                    let _ = store.reveal_secret(Uuid::now_v7());
                }
            })
        };
        while std::time::Instant::now() < deadline {
            store.import(ImportSet::default()).unwrap();
        }
        revealer.join().unwrap();
    }

    #[test]
    fn locking_without_a_vault_is_harmless() {
        let store = store();
        store.lock_vault();
        assert!(store.unlock_vault(b"x").is_err());
    }
}
