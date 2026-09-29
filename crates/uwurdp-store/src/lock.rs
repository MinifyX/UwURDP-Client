//! Syncing through a UwULock Server instead of UwUSync: what this device
//! keeps of it.
//!
//! On UwULock the vault's key is the key of the account's `rdp` space, and the
//! vault's id is the space's id — so the records are sealed exactly as they
//! are for UwUSync, and only the key comes from somewhere else: the UwULock
//! account, opened with its master password. Joining a space is therefore
//! [`Store::adopt_vault`] with that key, and the vault here is wrapped under
//! the UwULock master password, so one password opens both.
//!
//! What is kept besides: the server, the email, the space, and the refresh
//! token and the "remember this device" token of two-step login, both sealed by
//! the operating system for this user like the UwUSync pairing's secrets. The
//! master password and the keys above the space key are never kept.

use crate::device::never_opens;
use crate::sync::{parse_uuid, Carry};
use crate::vault::{store_header, truncate_wal};
use crate::{now_ms, Result, Store, StoreError};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::Serialize;
use uuid::Uuid;
use uwurdp_proto::{EntityKind, Envelope};
use uwurdp_vault::{KdfParams, Sealed, UnlockedVault};
use zeroize::Zeroizing;

/// Where this device stands with UwULock. Local, never synced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LockState {
    /// UwULock is the backend: the vault is the account's space.
    pub active: bool,
    pub server_url: Option<String>,
    pub email: Option<String>,
    pub space_id: Option<Uuid>,
    /// A refresh token is kept: syncing needs no master password.
    pub signed_in: bool,
    pub signed_in_ms: Option<u64>,
    /// A move from UwUSync began and has not finished.
    pub move_started_ms: Option<u64>,
}

/// A space this device takes as its vault.
pub struct Space {
    pub id: Uuid,
    pub key: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for Space {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Space({}, key redacted)", self.id)
    }
}

/// A sign-in, on its way into the database.
pub struct LockEnrolment<'a> {
    pub server_url: &'a str,
    pub email: &'a str,
    /// Sealed by the operating system already.
    pub protected_refresh_token: Option<Vec<u8>>,
    pub protected_remember_token: Option<Vec<u8>>,
}

/// How the records here come along into the space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Joining {
    /// A device signing in: what it has is new to the account.
    SignIn,
    /// The move from UwUSync copied everything to the space already, and
    /// this device leaves its UwUSync pairing behind in the same step.
    Moved,
}

impl Store {
    pub fn lock_state(&self) -> Result<LockState> {
        let conn = self.conn.lock();
        Ok(conn.query_row(
            "SELECT active, server_url, email, space_id,
                    protected_refresh_token IS NOT NULL, signed_in_ms, move_started_ms
               FROM lock_state WHERE id = 1",
            [],
            |row| {
                let space: Option<String> = row.get(3)?;
                let signed_in_ms: Option<i64> = row.get(5)?;
                let move_started_ms: Option<i64> = row.get(6)?;
                Ok(LockState {
                    active: row.get(0)?,
                    server_url: row.get(1)?,
                    email: row.get(2)?,
                    space_id: space.and_then(|id| Uuid::parse_str(&id).ok()),
                    signed_in: row.get(4)?,
                    signed_in_ms: signed_in_ms.map(|ms| ms as u64),
                    move_started_ms: move_started_ms.map(|ms| ms as u64),
                })
            },
        )?)
    }

    /// The id this install logs in to UwULock with: made once, kept for good,
    /// so the server lists one device however often it signs in.
    pub fn lock_device_identifier(&self) -> Result<Uuid> {
        let text: String = self.conn.lock().query_row(
            "SELECT device_identifier FROM lock_state WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        parse_uuid(&text)
    }

    /// Take a space as this device's vault and UwULock as its backend, in one
    /// transaction.
    ///
    /// The vault is wrapped under `password` — the UwULock master password —
    /// from now on. A device whose vault is this space already (signing in
    /// again, after signing out or after the master password changed on the
    /// server) only gets the new wrapping: nothing is sealed again, and the
    /// vault needs no unlocking first. Any other device brings its records
    /// along as [`Store::adopt_vault`] does, which needs its vault open if it
    /// holds secrets.
    pub fn join_space(
        &self,
        space: Space,
        password: &[u8],
        kdf: KdfParams,
        joining: Joining,
        enrolment: &LockEnrolment<'_>,
    ) -> Result<()> {
        let adopted = UnlockedVault::from_key(space.id, space.key);
        let header = adopted.rewrap(password, None, kdf)?;
        let key = adopted.export_key();
        let current = self.current_vault_id()?;

        let remember = |tx: &Transaction| -> Result<()> {
            tx.execute(
                "UPDATE lock_state
                    SET active = 1, server_url = ?1, email = ?2, space_id = ?3,
                        protected_refresh_token = ?4, protected_remember_token = ?5,
                        signed_in_ms = ?6, move_started_ms = NULL
                  WHERE id = 1",
                params![
                    enrolment.server_url,
                    enrolment.email,
                    space.id.to_string(),
                    enrolment.protected_refresh_token,
                    enrolment.protected_remember_token,
                    now_ms() as i64,
                ],
            )?;
            if joining == Joining::Moved {
                forget_uwusync(tx)?;
            }
            Ok(())
        };

        if current == space.id {
            self.check_space_key(&adopted)?;
            let mut conn = self.conn.lock();
            let mut guard = self.vault.lock();
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM vault", [])?;
            store_header(&tx, &header)?;
            // What this device sealed for itself opens this vault still, but
            // it was sealed next to the old wrapping: the person decides again.
            tx.execute("DELETE FROM device_unlock", [])?;
            remember(&tx)?;
            tx.commit()?;
            *guard = Some(adopted);
            tracing::info!(space = %space.id, "signed in to UwULock again");
            return Ok(());
        }

        let carry = match joining {
            Joining::SignIn => Carry::Push,
            Joining::Moved => Carry::Moved,
        };
        self.adopt(&header, key, carry, remember)?;
        tracing::info!(space = %space.id, ?joining, "this device syncs through UwULock");
        Ok(())
    }

    /// A key for the vault that is here already must open what that vault
    /// sealed: one secret is enough to tell. With no secret to try it on, the
    /// key is taken as the account says — it came authenticated under the
    /// account's own keys.
    fn check_space_key(&self, vault: &UnlockedVault) -> Result<()> {
        let sample: Option<(String, Vec<u8>, Vec<u8>)> = self
            .conn
            .lock()
            .query_row(
                "SELECT id, nonce, blob FROM secrets WHERE deleted = 0 AND length(blob) > 0 LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((id, nonce, blob)) = sample {
            vault
                .open(
                    parse_uuid(&id)?,
                    EntityKind::Secret,
                    &Sealed { nonce, blob },
                )
                .map_err(|_| StoreError::Invalid {
                    field: "space",
                    problem: "the account's key does not open this vault",
                })?;
        }
        Ok(())
    }

    fn current_vault_id(&self) -> Result<Uuid> {
        let text: String =
            self.conn
                .lock()
                .query_row("SELECT vault_id FROM meta WHERE id = 1", [], |row| {
                    row.get(0)
                })?;
        parse_uuid(&text)
    }

    /// A new refresh token: servers hand one out with every refresh, and the
    /// old one stops working. Kept before it is used, or a crash in between
    /// would cost the sign-in.
    pub fn keep_lock_refresh_token(&self, protected: &[u8]) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE lock_state SET protected_refresh_token = ?1 WHERE id = 1 AND active = 1",
            [protected],
        )?;
        Ok(())
    }

    /// The refresh token, opened. `Ok(None)` when none is kept, or what is
    /// kept no longer opens for this user — then it is dropped, and the
    /// master password is the way back in. An error, with it kept, when the
    /// operating system can't tell right now.
    pub fn lock_refresh_token(
        &self,
        unprotect: impl Fn(&[u8]) -> std::io::Result<Zeroizing<Vec<u8>>>,
    ) -> Result<Option<Zeroizing<String>>> {
        self.lock_token("protected_refresh_token", unprotect)
    }

    /// The "remember this device" token of two-step login, opened.
    pub fn lock_remember_token(
        &self,
        unprotect: impl Fn(&[u8]) -> std::io::Result<Zeroizing<Vec<u8>>>,
    ) -> Result<Option<Zeroizing<String>>> {
        self.lock_token("protected_remember_token", unprotect)
    }

    fn lock_token(
        &self,
        column: &str,
        unprotect: impl Fn(&[u8]) -> std::io::Result<Zeroizing<Vec<u8>>>,
    ) -> Result<Option<Zeroizing<String>>> {
        let sealed: Option<Vec<u8>> = self.conn.lock().query_row(
            &format!("SELECT {column} FROM lock_state WHERE id = 1"),
            [],
            |row| row.get(0),
        )?;
        let Some(sealed) = sealed else {
            return Ok(None);
        };
        match unprotect(&sealed) {
            Ok(bytes) => match String::from_utf8(bytes.to_vec()) {
                Ok(text) => Ok(Some(Zeroizing::new(text))),
                Err(_) => {
                    self.drop_lock_token(column)?;
                    Ok(None)
                }
            },
            Err(error) if !never_opens(&error) => Err(StoreError::Device(error.to_string())),
            Err(_) => {
                tracing::warn!("what this device kept of its UwULock sign-in no longer opens");
                self.drop_lock_token(column)?;
                Ok(None)
            }
        }
    }

    fn drop_lock_token(&self, column: &str) -> Result<()> {
        self.conn.lock().execute(
            &format!("UPDATE lock_state SET {column} = NULL WHERE id = 1"),
            [],
        )?;
        Ok(())
    }

    /// The session is over — signed out elsewhere, the device removed, the
    /// refresh token run out: the next sign-in needs the master password.
    /// UwULock stays the backend, and everything here stays as it is.
    pub fn end_lock_session(&self) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE lock_state SET protected_refresh_token = NULL WHERE id = 1",
            [],
        )?;
        tracing::info!("the UwULock session ended");
        Ok(())
    }

    /// Stop syncing through UwULock. The vault stays as it is — the space's
    /// key, under the UwULock master password — and so do its records; the
    /// server and the email stay to fill in the next sign-in.
    pub fn leave_lock(&self) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE lock_state
                SET active = 0, protected_refresh_token = NULL, signed_in_ms = NULL,
                    move_started_ms = NULL
              WHERE id = 1",
            [],
        )?;
        conn.execute(
            "UPDATE sync_state SET cursor = 0, last_sync_ms = NULL WHERE id = 1",
            [],
        )?;
        conn.execute("DELETE FROM manifest_violations", [])?;
        truncate_wal(&conn);
        tracing::info!("this device no longer syncs through UwULock");
        Ok(())
    }

    /// A move from UwUSync began, to this server and account: shown as one to
    /// carry on with until it finishes.
    pub fn note_move_started(&self, server_url: &str, email: &str) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE lock_state
                SET move_started_ms = coalesce(move_started_ms, ?1),
                    server_url = ?2, email = ?3
              WHERE id = 1 AND active = 0",
            params![now_ms() as i64, server_url, email],
        )?;
        Ok(())
    }

    /// Give up on a move that has not finished. UwUSync stays the backend, as
    /// it was all along.
    pub fn forget_move(&self) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE lock_state SET move_started_ms = NULL WHERE id = 1",
            [],
        )?;
        Ok(())
    }

    /// Open a record the way it came from the server, with the vault that is
    /// open here: the payload, or `None` when its seal does not hold or it
    /// belongs to another vault. For the move, which copies records between
    /// servers without going through the tables.
    pub fn open_envelope(&self, envelope: &Envelope) -> Result<Option<Zeroizing<Vec<u8>>>> {
        let guard = self.vault.lock();
        let vault = guard.as_ref().ok_or(StoreError::VaultLocked)?;
        if envelope.vault_id != vault.vault_id() {
            return Ok(None);
        }
        Ok(vault
            .open_synced(
                envelope.id,
                envelope.kind,
                envelope.updated_at,
                envelope.deleted,
                &Sealed {
                    nonce: envelope.nonce.clone(),
                    blob: envelope.blob.clone(),
                },
            )
            .ok())
    }
}

/// The UwUSync pairing, forgotten as `Store::forget_enrolment` does it — but
/// inside the transaction that makes UwULock the backend.
fn forget_uwusync(tx: &Transaction) -> Result<()> {
    tx.execute(
        "UPDATE sync_state
            SET server_url = NULL, account_id = NULL, device_id = NULL,
                tls_fingerprint = NULL, protected_device_key = NULL,
                protected_account_key = NULL, paired_ms = NULL
          WHERE id = 1",
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosts::tests::draft;
    use crate::{PasswordChange, SecretText};

    const PASSWORD: &[u8] = b"uwulock master password";

    fn enrolment<'a>(token: &'a [u8]) -> LockEnrolment<'a> {
        LockEnrolment {
            server_url: "https://lock.example.com",
            email: "nyu@example.com",
            protected_refresh_token: Some(token.to_vec()),
            protected_remember_token: None,
        }
    }

    fn space() -> Space {
        Space {
            id: Uuid::new_v4(),
            key: Zeroizing::new([7u8; 32]),
        }
    }

    fn same(space: &Space) -> Space {
        Space {
            id: space.id,
            key: space.key.clone(),
        }
    }

    fn open(bytes: &[u8]) -> std::io::Result<Zeroizing<Vec<u8>>> {
        Ok(Zeroizing::new(bytes.to_vec()))
    }

    fn dirty(store: &Store) -> i64 {
        store
            .conn
            .lock()
            .query_row("SELECT count(*) FROM hosts WHERE dirty = 1", [], |r| {
                r.get(0)
            })
            .unwrap()
    }

    #[test]
    fn a_fresh_install_is_on_no_backend_and_keeps_one_identifier() {
        let store = Store::open_in_memory().unwrap();
        let state = store.lock_state().unwrap();
        assert!(!state.active && !state.signed_in);
        let id = store.lock_device_identifier().unwrap();
        assert_eq!(store.lock_device_identifier().unwrap(), id);
    }

    #[test]
    fn signing_in_brings_hosts_and_secrets_along_and_the_uwulock_password_opens_the_vault() {
        let store = Store::open_in_memory().unwrap();
        store
            .create_vault_with(b"old local password", KdfParams::INSECURE_FOR_TESTS)
            .unwrap();
        let mut host = draft("prox-1", "192.0.2.10");
        host.password = PasswordChange::Set {
            value: SecretText::from(Zeroizing::new("hunter2".to_string())),
        };
        let id = store.save_host(host).unwrap().id;
        let space = space();

        store
            .join_space(
                same(&space),
                PASSWORD,
                KdfParams::INSECURE_FOR_TESTS,
                Joining::SignIn,
                &enrolment(b"refresh-1"),
            )
            .unwrap();

        let state = store.lock_state().unwrap();
        assert!(state.active && state.signed_in);
        assert_eq!(state.space_id, Some(space.id));
        assert_eq!(dirty(&store), 1, "the host is new to the account");
        assert_eq!(
            store.lock_refresh_token(open).unwrap().unwrap().as_str(),
            "refresh-1"
        );

        store.lock_vault();
        assert!(store.unlock_vault(b"old local password").is_err());
        store.unlock_vault(PASSWORD).unwrap();
        assert_eq!(&**store.reveal_host_password(id).unwrap(), b"hunter2");
    }

    #[test]
    fn signing_in_again_to_the_same_space_only_wraps_the_vault_again() {
        let store = Store::open_in_memory().unwrap();
        let space = space();
        store
            .join_space(
                same(&space),
                PASSWORD,
                KdfParams::INSECURE_FOR_TESTS,
                Joining::SignIn,
                &enrolment(b"refresh-1"),
            )
            .unwrap();
        let mut host = draft("prox-1", "192.0.2.10");
        host.password = PasswordChange::Set {
            value: SecretText::from(Zeroizing::new("hunter2".to_string())),
        };
        let id = store.save_host(host).unwrap().id;
        store.end_lock_session().unwrap();
        store.lock_vault();

        // The master password changed on the server; the vault here is locked
        // and still wrapped under the old one.
        store
            .join_space(
                same(&space),
                b"a new master password",
                KdfParams::INSECURE_FOR_TESTS,
                Joining::SignIn,
                &enrolment(b"refresh-2"),
            )
            .unwrap();
        assert_eq!(dirty(&store), 1, "nothing was sealed again or marked");
        assert_eq!(&**store.reveal_host_password(id).unwrap(), b"hunter2");
        store.lock_vault();
        store.unlock_vault(b"a new master password").unwrap();

        // A key that does not open what is here is refused.
        let wrong = Space {
            id: space.id,
            key: Zeroizing::new([8u8; 32]),
        };
        assert!(store
            .join_space(
                wrong,
                PASSWORD,
                KdfParams::INSECURE_FOR_TESTS,
                Joining::SignIn,
                &enrolment(b"x"),
            )
            .is_err());
    }

    #[test]
    fn a_move_leaves_uwusync_and_marks_nothing_new() {
        let store = Store::open_in_memory().unwrap();
        store
            .create_vault_with(PASSWORD, KdfParams::INSECURE_FOR_TESTS)
            .unwrap();
        store.save_host(draft("prox-1", "192.0.2.10")).unwrap();
        store
            .conn
            .lock()
            .execute_batch(
                "UPDATE hosts SET dirty = 0, server_seq = 4;
                 UPDATE sync_state SET server_url = 'https://nas.example.net',
                    account_id = 'a', device_id = 'd', cursor = 9 WHERE id = 1;",
            )
            .unwrap();
        store
            .note_move_started("https://lock.example.com", "nyu@example.com")
            .unwrap();
        assert!(store.lock_state().unwrap().move_started_ms.is_some());

        store
            .join_space(
                space(),
                PASSWORD,
                KdfParams::INSECURE_FOR_TESTS,
                Joining::Moved,
                &enrolment(b"refresh-1"),
            )
            .unwrap();

        assert_eq!(dirty(&store), 0);
        let sync = store.sync_state().unwrap();
        assert!(!sync.paired());
        assert_eq!(sync.cursor, 0);
        let lock = store.lock_state().unwrap();
        assert!(lock.active && lock.move_started_ms.is_none());
    }

    #[test]
    fn leaving_keeps_the_vault_and_what_to_fill_in_next_time() {
        let store = Store::open_in_memory().unwrap();
        store
            .join_space(
                space(),
                PASSWORD,
                KdfParams::INSECURE_FOR_TESTS,
                Joining::SignIn,
                &enrolment(b"refresh-1"),
            )
            .unwrap();
        store.leave_lock().unwrap();
        let state = store.lock_state().unwrap();
        assert!(!state.active && !state.signed_in);
        assert_eq!(state.email.as_deref(), Some("nyu@example.com"));
        assert!(store.lock_refresh_token(open).unwrap().is_none());
        store.lock_vault();
        store.unlock_vault(PASSWORD).unwrap();
    }

    #[test]
    fn a_token_that_no_longer_opens_is_dropped_and_one_that_cannot_open_now_is_kept() {
        let store = Store::open_in_memory().unwrap();
        store
            .join_space(
                space(),
                PASSWORD,
                KdfParams::INSECURE_FOR_TESTS,
                Joining::SignIn,
                &enrolment(b"refresh-1"),
            )
            .unwrap();
        let later = |_: &[u8]| -> std::io::Result<Zeroizing<Vec<u8>>> {
            Err(std::io::Error::other("the keychain is locked"))
        };
        assert!(store.lock_refresh_token(later).is_err());
        assert!(store.lock_state().unwrap().signed_in);
        let never = |_: &[u8]| -> std::io::Result<Zeroizing<Vec<u8>>> {
            Err(std::io::Error::from(std::io::ErrorKind::InvalidData))
        };
        assert!(store.lock_refresh_token(never).unwrap().is_none());
        assert!(!store.lock_state().unwrap().signed_in);
    }
}
