//! Server certificates the user has decided to trust.
//!
//! Stored by `(address, port)` with the SHA-256 fingerprint of the
//! certificate's DER for comparison, and the whole certificate (DER, base64)
//! alongside, so an export can carry it and check it against the fingerprint.
//! Addresses are compared case-insensitively — `Prox-1.lan` and `prox-1.lan`
//! are the same machine, and treating them as two would ask the user to trust
//! the same certificate twice. The names (`host key`) are the ones the shared
//! schema has; the test data below still uses SSH-shaped keys, which the store
//! never looks into.

use crate::manifest::HOST_KEY_KINDS;
use crate::{now_ms, tick, vault_id, Result, Store};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownHostRecord {
    pub address: String,
    pub port: u16,
    pub algorithm: String,
    /// `SHA256:` and the unpadded base64 of the SHA-256 over the certificate's DER.
    pub fingerprint: String,
    pub public_key: String,
    pub first_seen_ms: u64,
}

fn normalise(address: &str) -> String {
    address.trim().to_ascii_lowercase()
}

impl Store {
    /// The key trusted for `address:port`, if any.
    ///
    /// While the last sync check found the server keeping host keys back (see
    /// [`crate::manifest`]), a key another device decided on is not answered
    /// with: it may be the one from before the host's key changed. The host
    /// then counts as new and the next connection asks. A key trusted on this
    /// device stays.
    pub fn known_host(&self, address: &str, port: u16) -> Result<Option<KnownHostRecord>> {
        Ok(self
            .conn
            .lock()
            .query_row(
                &format!(
                    "SELECT address, port, algorithm, fingerprint_sha256, public_key, first_seen_ms
                       FROM known_hosts
                      WHERE address = ?1 AND port = ?2 AND deleted = 0
                        AND (hlc_device = ?3
                             OR NOT EXISTS (SELECT 1 FROM manifest_violations
                                             WHERE kind IN {HOST_KEY_KINDS}))"
                ),
                params![normalise(address), port, self.device],
                |row| {
                    Ok(KnownHostRecord {
                        address: row.get(0)?,
                        port: row.get::<_, i64>(1)? as u16,
                        algorithm: row.get(2)?,
                        fingerprint: row.get(3)?,
                        public_key: row.get(4)?,
                        first_seen_ms: row.get::<_, i64>(5)? as u64,
                    })
                },
            )
            .optional()?)
    }

    /// Trust a key for `address:port`, replacing whatever was trusted before.
    ///
    /// Callers are responsible for only passing keys a server actually
    /// presented; the desktop layer enforces that, so a compromised webview
    /// cannot slip in a key of its own choosing.
    pub fn trust_host_key(
        &self,
        address: &str,
        port: u16,
        algorithm: &str,
        fingerprint: &str,
        public_key: &str,
    ) -> Result<KnownHostRecord> {
        let address = normalise(address);
        {
            let mut conn = self.conn.lock();
            let tx = conn.transaction()?;
            let clock = tick(&tx, self.device)?;
            let vault = vault_id(&tx)?;
            tx.execute(
                "INSERT INTO known_hosts
                    (id, vault_id, address, port, algorithm, fingerprint_sha256, public_key,
                     first_seen_ms, hlc_wall_ms, hlc_counter, hlc_device)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT (address, port) DO UPDATE
                    SET algorithm = excluded.algorithm,
                        fingerprint_sha256 = excluded.fingerprint_sha256,
                        public_key = excluded.public_key,
                        first_seen_ms = excluded.first_seen_ms,
                        dirty = 1,
                        hlc_wall_ms = excluded.hlc_wall_ms,
                        hlc_counter = excluded.hlc_counter,
                        hlc_device = excluded.hlc_device,
                        rev = rev + 1,
                        deleted = 0",
                params![
                    Uuid::now_v7().to_string(),
                    vault,
                    address,
                    port,
                    algorithm,
                    fingerprint,
                    public_key,
                    now_ms() as i64,
                    clock.wall_ms as i64,
                    clock.counter,
                    clock.device,
                ],
            )?;
            tx.commit()?;
        }
        tracing::info!(%address, port, %fingerprint, "host key trusted");

        Ok(self
            .known_host(&address, port)?
            .expect("the row written in the committed transaction above"))
    }

    /// Every trusted key, for an export.
    pub fn list_known_hosts(&self) -> Result<Vec<KnownHostRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT address, port, algorithm, fingerprint_sha256, public_key, first_seen_ms
               FROM known_hosts WHERE deleted = 0 ORDER BY address, port",
        )?;
        let keys = stmt
            .query_map([], |row| {
                Ok(KnownHostRecord {
                    address: row.get(0)?,
                    port: row.get::<_, i64>(1)? as u16,
                    algorithm: row.get(2)?,
                    fingerprint: row.get(3)?,
                    public_key: row.get(4)?,
                    first_seen_ms: row.get::<_, i64>(5)? as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(keys)
    }

    /// Stop trusting the key for `address:port`. Returns whether one was trusted.
    pub fn forget_host_key(&self, address: &str, port: u16) -> Result<bool> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let clock = tick(&tx, self.device)?;
        let changed = tx.execute(
            "UPDATE known_hosts
                SET deleted = 1, rev = rev + 1,
                    dirty = 1, hlc_wall_ms = ?3, hlc_counter = ?4, hlc_device = ?5
              WHERE address = ?1 AND port = ?2 AND deleted = 0",
            params![
                normalise(address),
                port,
                clock.wall_ms as i64,
                clock.counter,
                clock.device
            ],
        )?;
        tx.commit()?;
        Ok(changed > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: (&str, &str) = ("SHA256:aaaa", "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIA");
    const KEY_B: (&str, &str) = ("SHA256:bbbb", "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIB");

    #[test]
    fn an_untrusted_host_has_no_record() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.known_host("prox-1", 22).unwrap(), None);
    }

    #[test]
    fn a_trusted_key_is_found_regardless_of_address_case() {
        let store = Store::open_in_memory().unwrap();
        store
            .trust_host_key("Prox-1.LAN", 22, "ssh-ed25519", KEY_A.0, KEY_A.1)
            .unwrap();

        let found = store.known_host("prox-1.lan", 22).unwrap().unwrap();
        assert_eq!(found.fingerprint, KEY_A.0);
        assert_eq!(found.address, "prox-1.lan");
    }

    #[test]
    fn the_port_is_part_of_the_identity() {
        let store = Store::open_in_memory().unwrap();
        store
            .trust_host_key("nas", 22, "ssh-ed25519", KEY_A.0, KEY_A.1)
            .unwrap();
        assert_eq!(store.known_host("nas", 2222).unwrap(), None);
    }

    #[test]
    fn trusting_again_replaces_the_old_key() {
        let store = Store::open_in_memory().unwrap();
        store
            .trust_host_key("nas", 22, "ssh-ed25519", KEY_A.0, KEY_A.1)
            .unwrap();
        store
            .trust_host_key("nas", 22, "ssh-ed25519", KEY_B.0, KEY_B.1)
            .unwrap();
        assert_eq!(
            store.known_host("nas", 22).unwrap().unwrap().fingerprint,
            KEY_B.0
        );
    }

    #[test]
    fn a_forgotten_key_can_be_trusted_again() {
        let store = Store::open_in_memory().unwrap();
        store
            .trust_host_key("nas", 22, "ssh-ed25519", KEY_A.0, KEY_A.1)
            .unwrap();

        assert!(store.forget_host_key("NAS", 22).unwrap());
        assert_eq!(store.known_host("nas", 22).unwrap(), None);
        assert!(
            !store.forget_host_key("nas", 22).unwrap(),
            "nothing left to forget"
        );

        store
            .trust_host_key("nas", 22, "ssh-ed25519", KEY_B.0, KEY_B.1)
            .unwrap();
        assert_eq!(
            store.known_host("nas", 22).unwrap().unwrap().fingerprint,
            KEY_B.0
        );
    }
}
