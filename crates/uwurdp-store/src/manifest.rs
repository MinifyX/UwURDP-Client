//! Manifests on this device: publishing its own, and checking everyone's.
//!
//! The format and the reasoning are in `uwurdp_proto::manifest`. What lives
//! here is what the store decides:
//!
//! - **What a manifest lists.** Rows the server has confirmed and nothing has
//!   changed since — `dirty = 0` and a server version — and never a
//!   placeholder. A row changed here but not yet pushed is left out: the
//!   version the server has of it is no longer known here, and listing the
//!   local one would claim something the server never saw.
//! - **What counts as missing.** After a pull that reached the end, every
//!   version a manifest lists must be here, or something newer — a later edit,
//!   or a tombstone, which beats every edit anyway. A listed tombstone for a
//!   record this device never had is fine, and so is a host key that arrived
//!   and gave up its slot to a newer one for the same address (see
//!   `superseded`). Kinds this build keeps no table for are not checked.
//! - **What follows from it.** The last check is written down, and while it
//!   found anything about host keys, a host key that came from another device
//!   is not trusted: `known_host` answers as if the host were new, so the next
//!   connection asks. One the user trusted on this device stays trusted.

use crate::sync::{clock_of, parse_uuid, table_of, SYNCED_KINDS};
use crate::{tick, Result, Store};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use uuid::Uuid;
use uwurdp_proto::{EntityKind, Hlc, Manifest, ManifestEntry, MAX_MANIFEST_ENTRIES};

/// The order a manifest lists kinds in, which is also what survives when a
/// vault has more records than one manifest holds: host keys first, since a
/// withheld one is a man in the middle, then what logs in, then the rest.
const PRIORITY: [EntityKind; 7] = [
    EntityKind::KnownHost,
    EntityKind::Key,
    EntityKind::Secret,
    EntityKind::Identity,
    EntityKind::Host,
    EntityKind::Group,
    EntityKind::Snippet,
];

/// The id of a device's manifest in a vault: the same on every device, so each
/// device has exactly one, and it can be named before it has arrived. A
/// version-8 UUID from SHA-256, so it never meets a record's random id.
pub fn manifest_id(vault_id: Uuid, device: u32) -> Uuid {
    let mut hash = Sha256::new();
    hash.update(b"uwurdp/manifest/v1");
    hash.update(vault_id.as_bytes());
    hash.update(device.to_be_bytes());
    let digest = hash.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

/// A manifest a joining device must see before it believes it has everything:
/// the one the device that added it had published, at least that new.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFloor {
    pub id: Uuid,
    pub clock: Hlc,
}

/// What was wrong with one record a manifest listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Problem {
    /// Not here at all.
    Missing,
    /// Here, but only in a version older than the one listed.
    Older,
}

impl Problem {
    fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Older => "older",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "older" => Self::Older,
            _ => Self::Missing,
        }
    }
}

/// One record the server should have handed over and did not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Violation {
    pub kind: EntityKind,
    pub id: Uuid,
    pub problem: Problem,
}

/// What the last check found, as the page shows it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Withheld {
    /// Records — and manifests — missing or older than they should be.
    pub records: usize,
    /// Host keys are among them, or it cannot be told whether they are: host
    /// keys from other devices are not trusted until this clears.
    pub host_keys: bool,
}

impl Withheld {
    pub fn any(&self) -> bool {
        self.records > 0
    }
}

/// Kinds whose violations put host keys in doubt: host keys themselves, and a
/// manifest from the pairing that did not arrive, which could have listed any.
pub(crate) const HOST_KEY_KINDS: &str = "(6, 9)";

/// Forget every manifest and what was found against them — for a device that
/// takes over another vault, whose devices these are not.
pub(crate) fn forget_manifests(tx: &Transaction) -> Result<()> {
    tx.execute_batch(
        "DELETE FROM manifests;
         DELETE FROM superseded;
         DELETE FROM manifest_violations;
         UPDATE sync_state
            SET floor_manifest_id = NULL, floor_wall_ms = NULL,
                floor_counter = NULL, floor_device = NULL
          WHERE id = 1;",
    )?;
    Ok(())
}

fn current_vault(tx: &Transaction) -> Result<Uuid> {
    parse_uuid(&crate::vault_id(tx)?)
}

/// A clock as four columns come out of a row, starting at `first`.
fn clock_at(row: &rusqlite::Row<'_>, first: usize) -> rusqlite::Result<Hlc> {
    Ok(clock_of(
        row.get(first)?,
        row.get(first + 1)?,
        row.get(first + 2)?,
    ))
}

impl Store {
    /// The id this device's manifest has in the current vault.
    pub fn own_manifest_id(&self) -> Result<Uuid> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        Ok(manifest_id(current_vault(&tx)?, self.device))
    }

    /// Write a new manifest for this device if what it holds changed since the
    /// last one. Returns whether there is a new one to push.
    ///
    /// Meant for the end of a sync pass that got everything out and pulled to
    /// the end — that is when what is here is what the server has too.
    pub fn refresh_manifest(&self) -> Result<bool> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let vault = current_vault(&tx)?;
        let id = manifest_id(vault, self.device);

        let mut entries = Vec::new();
        for kind in PRIORITY {
            let Some(table) = table_of(kind) else {
                continue;
            };
            let mut stmt = tx.prepare(&format!(
                "SELECT id, hlc_wall_ms, hlc_counter, hlc_device, deleted FROM {table}
                  WHERE dirty = 0 AND server_seq > 0
                    AND (hlc_wall_ms > 0 OR hlc_counter > 0 OR hlc_device > 0)
                  ORDER BY id"
            ))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, clock_at(row, 1)?, row.get(4)?))
                })?
                .collect::<rusqlite::Result<Vec<(String, Hlc, bool)>>>()?;
            for (row_id, clock, deleted) in rows {
                entries.push(ManifestEntry::new(
                    kind,
                    parse_uuid(&row_id)?,
                    clock,
                    deleted,
                ));
            }
        }
        let partial = entries.len() > MAX_MANIFEST_ENTRIES;
        if partial {
            tracing::warn!(
                records = entries.len(),
                listed = MAX_MANIFEST_ENTRIES,
                "more records than one manifest lists; the rest go unchecked"
            );
            entries.truncate(MAX_MANIFEST_ENTRIES);
        }

        let previous: Option<Vec<u8>> = tx
            .query_row(
                "SELECT entries FROM manifests WHERE id = ?1 AND deleted = 0",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(previous) = previous.as_deref().and_then(Manifest::decode) {
            if previous.device == self.device
                && previous.partial == partial
                && previous.entries == entries
            {
                return Ok(false);
            }
        }

        let clock = tick(&tx, self.device)?;
        let payload = Manifest {
            device: self.device,
            created_at: clock,
            partial,
            entries,
        }
        .encode();
        // The server's version of the row stays, so the push is based on it.
        tx.execute(
            "INSERT INTO manifests
                (id, vault_id, entries, hlc_wall_ms, hlc_counter, hlc_device, dirty)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)
             ON CONFLICT (id) DO UPDATE SET
                entries = excluded.entries, hlc_wall_ms = excluded.hlc_wall_ms,
                hlc_counter = excluded.hlc_counter, hlc_device = excluded.hlc_device,
                deleted = 0, dirty = 1, rev = rev + 1",
            params![
                id.to_string(),
                vault.to_string(),
                payload,
                clock.wall_ms as i64,
                clock.counter,
                clock.device,
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// This device's manifest as the server last confirmed it — what a device
    /// it adds is told to wait for. `None` while there is none, or while a
    /// newer one has not gone up yet: the device being added could otherwise
    /// wait for one that never arrives.
    pub fn published_manifest(&self) -> Result<Option<ManifestFloor>> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let id = manifest_id(current_vault(&tx)?, self.device);
        let clock = tx
            .query_row(
                "SELECT hlc_wall_ms, hlc_counter, hlc_device FROM manifests
                  WHERE id = ?1 AND dirty = 0 AND deleted = 0 AND server_seq > 0",
                [id.to_string()],
                |row| clock_at(row, 0),
            )
            .optional()?;
        Ok(clock.map(|clock| ManifestFloor { id, clock }))
    }

    /// The version of a manifest held here, by id.
    pub fn manifest_clock(&self, id: Uuid) -> Result<Option<Hlc>> {
        Ok(self
            .conn
            .lock()
            .query_row(
                "SELECT hlc_wall_ms, hlc_counter, hlc_device FROM manifests
                  WHERE id = ?1 AND deleted = 0",
                [id.to_string()],
                |row| clock_at(row, 0),
            )
            .optional()?)
    }

    /// Remember the manifest the device that added this one had published.
    pub fn set_manifest_floor(&self, floor: Option<ManifestFloor>) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE sync_state
                SET floor_manifest_id = ?1, floor_wall_ms = ?2,
                    floor_counter = ?3, floor_device = ?4
              WHERE id = 1",
            params![
                floor.map(|f| f.id.to_string()),
                floor.map(|f| f.clock.wall_ms as i64),
                floor.map(|f| f.clock.counter),
                floor.map(|f| f.clock.device),
            ],
        )?;
        Ok(())
    }

    /// The manifest this device was told to wait for when it joined.
    pub fn manifest_floor(&self) -> Result<Option<ManifestFloor>> {
        let row: Option<(String, Hlc)> = self
            .conn
            .lock()
            .query_row(
                "SELECT floor_manifest_id, floor_wall_ms, floor_counter, floor_device
                   FROM sync_state
                  WHERE id = 1 AND floor_manifest_id IS NOT NULL
                    AND floor_wall_ms IS NOT NULL AND floor_counter IS NOT NULL
                    AND floor_device IS NOT NULL",
                [],
                |row| Ok((row.get(0)?, clock_at(row, 1)?)),
            )
            .optional()?;
        row.map(|(id, clock)| {
            Ok(ManifestFloor {
                id: parse_uuid(&id)?,
                clock,
            })
        })
        .transpose()
    }

    /// Hold every manifest against what is here, write down what is missing,
    /// and say what that means.
    ///
    /// Only meaningful right after a pull that reached the end: before that,
    /// what a manifest lists may simply not have been fetched yet.
    pub fn check_manifests(&self) -> Result<Withheld> {
        let floor = self.manifest_floor()?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;

        let manifests = {
            let mut stmt = tx.prepare(
                "SELECT id, entries, hlc_wall_ms, hlc_counter, hlc_device
                   FROM manifests WHERE deleted = 0",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        clock_at(row, 2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };

        let mut found = BTreeMap::<(u8, Uuid), Problem>::new();
        for (_, entries, _) in &manifests {
            let Some(manifest) = Manifest::decode(entries) else {
                continue;
            };
            for entry in &manifest.entries {
                let Some(kind) = entry.kind().filter(|kind| SYNCED_KINDS.contains(kind)) else {
                    continue;
                };
                if let Some(problem) = check_entry(&tx, kind, entry)? {
                    found.insert((kind as u8, entry.id), problem);
                }
            }
        }

        if let Some(floor) = floor {
            let held = manifests
                .iter()
                .find(|(id, _, _)| *id == floor.id.to_string())
                .map(|(_, _, clock)| *clock);
            let problem = match held {
                Some(clock) if clock >= floor.clock => None,
                Some(_) => Some(Problem::Older),
                None => Some(Problem::Missing),
            };
            if let Some(problem) = problem {
                found.insert((EntityKind::Manifest as u8, floor.id), problem);
            }
        }

        tx.execute("DELETE FROM manifest_violations", [])?;
        for ((kind, id), problem) in &found {
            tx.execute(
                "INSERT INTO manifest_violations (kind, id, problem) VALUES (?1, ?2, ?3)",
                params![kind, id.to_string(), problem.as_str()],
            )?;
        }
        tx.commit()?;
        drop(conn);

        let withheld = self.withheld()?;
        if withheld.any() {
            tracing::warn!(
                records = withheld.records,
                host_keys = withheld.host_keys,
                "the server did not hand over everything the other devices hold"
            );
        }
        Ok(withheld)
    }

    /// What the last check found.
    pub fn withheld(&self) -> Result<Withheld> {
        let (records, host_keys): (i64, i64) = self.conn.lock().query_row(
            &format!(
                "SELECT count(*), coalesce(max(kind IN {HOST_KEY_KINDS}), 0)
                   FROM manifest_violations"
            ),
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(Withheld {
            records: records as usize,
            host_keys: host_keys != 0,
        })
    }

    /// Every record the last check found missing or out of date.
    pub fn manifest_violations(&self) -> Result<Vec<Violation>> {
        let conn = self.conn.lock();
        let mut stmt =
            conn.prepare("SELECT kind, id, problem FROM manifest_violations ORDER BY kind, id")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, u8>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut violations = Vec::new();
        for (kind, id, problem) in rows {
            let Some(kind) = EntityKind::from_discriminant(kind) else {
                continue;
            };
            violations.push(Violation {
                kind,
                id: parse_uuid(&id)?,
                problem: Problem::parse(&problem),
            });
        }
        Ok(violations)
    }
}

/// Whether one listed version is accounted for here, and if not, how.
fn check_entry(
    tx: &Transaction,
    kind: EntityKind,
    entry: &ManifestEntry,
) -> Result<Option<Problem>> {
    let Some(table) = table_of(kind) else {
        return Ok(None);
    };
    let local: Option<(Hlc, bool)> = tx
        .prepare_cached(&format!(
            "SELECT hlc_wall_ms, hlc_counter, hlc_device, deleted FROM {table} WHERE id = ?1"
        ))?
        .query_row([entry.id.to_string()], |row| {
            Ok((clock_at(row, 0)?, row.get(3)?))
        })
        .optional()?;
    match local {
        // A tombstone beats every edit, whenever it was made: the record ends
        // deleted here whatever else there is.
        Some((_, true)) => return Ok(None),
        // The listed version or a later one — a local edit not pushed yet
        // included.
        Some((clock, false)) if clock >= entry.updated_at => return Ok(None),
        // A delete of something this device never had.
        None if entry.deleted => return Ok(None),
        _ => {}
    }
    // A host key that arrived and gave up its slot to a newer one.
    let superseded: Option<Hlc> = tx
        .prepare_cached(
            "SELECT hlc_wall_ms, hlc_counter, hlc_device FROM superseded WHERE id = ?1",
        )?
        .query_row([entry.id.to_string()], |row| clock_at(row, 0))
        .optional()?;
    if superseded.is_some_and(|clock| clock >= entry.updated_at) {
        return Ok(None);
    }
    Ok(Some(if local.is_some() {
        Problem::Older
    } else {
        Problem::Missing
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Another device's clock id.
    const OTHER: u32 = 0xdead_beef;

    fn vault(store: &Store) -> Uuid {
        let mut conn = store.conn.lock();
        let tx = conn.transaction().unwrap();
        current_vault(&tx).unwrap()
    }

    fn at(wall: u64) -> Hlc {
        Hlc::new(wall, 0, OTHER)
    }

    /// A snippet here, as if the server had confirmed it.
    fn snippet(store: &Store, id: Uuid, clock: Hlc, deleted: bool) {
        let vault = vault(store).to_string();
        store
            .conn
            .lock()
            .execute(
                "INSERT INTO snippets
                    (id, vault_id, label, body, hlc_wall_ms, hlc_counter, hlc_device,
                     deleted, dirty, server_seq)
                 VALUES (?1, ?2, 's', 'b', ?3, ?4, ?5, ?6, 0, 1)
                 ON CONFLICT (id) DO UPDATE SET
                    hlc_wall_ms = excluded.hlc_wall_ms, hlc_counter = excluded.hlc_counter,
                    hlc_device = excluded.hlc_device, deleted = excluded.deleted",
                params![
                    id.to_string(),
                    vault,
                    clock.wall_ms as i64,
                    clock.counter,
                    clock.device,
                    deleted
                ],
            )
            .unwrap();
    }

    /// Another device's manifest, as if it had arrived.
    fn manifest_from(store: &Store, device: u32, clock: Hlc, entries: Vec<ManifestEntry>) -> Uuid {
        let vault = vault(store);
        let id = manifest_id(vault, device);
        let payload = Manifest {
            device,
            created_at: clock,
            partial: false,
            entries,
        }
        .encode();
        store
            .conn
            .lock()
            .execute(
                "INSERT INTO manifests
                    (id, vault_id, entries, hlc_wall_ms, hlc_counter, hlc_device, server_seq)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)
                 ON CONFLICT (id) DO UPDATE SET
                    entries = excluded.entries, hlc_wall_ms = excluded.hlc_wall_ms",
                params![
                    id.to_string(),
                    vault.to_string(),
                    payload,
                    clock.wall_ms as i64,
                    clock.counter,
                    clock.device
                ],
            )
            .unwrap();
        id
    }

    fn listed(id: Uuid, wall: u64, deleted: bool) -> ManifestEntry {
        ManifestEntry::new(EntityKind::Snippet, id, at(wall), deleted)
    }

    #[test]
    fn every_listed_version_that_is_here_or_newer_counts_as_there() {
        let store = Store::open_in_memory().unwrap();
        let [same, newer, deleted_here, never_had] = [(); 4].map(|_| Uuid::now_v7());
        snippet(&store, same, at(100), false);
        snippet(&store, newer, at(200), false);
        // A tombstone beats an edit whatever the clocks say.
        snippet(&store, deleted_here, at(50), true);

        let mut unknown = listed(Uuid::now_v7(), 100, false);
        unknown.kind = 42;
        manifest_from(
            &store,
            OTHER,
            at(1_000),
            vec![
                listed(same, 100, false),
                listed(newer, 100, false),
                listed(deleted_here, 100, false),
                listed(never_had, 100, true),
                unknown,
                ManifestEntry::new(EntityKind::PortForward, Uuid::now_v7(), at(100), false),
            ],
        );

        assert_eq!(store.check_manifests().unwrap(), Withheld::default());
        assert!(store.manifest_violations().unwrap().is_empty());
    }

    #[test]
    fn an_older_version_or_a_missing_record_is_kept_back() {
        let store = Store::open_in_memory().unwrap();
        let [older, missing, not_deleted, placeholder] = [(); 4].map(|_| Uuid::now_v7());
        snippet(&store, older, at(100), false);
        snippet(&store, not_deleted, at(100), false);
        store
            .conn
            .lock()
            .execute(
                "INSERT INTO host_groups
                    (id, vault_id, workspace, name, position, hlc_wall_ms, hlc_counter,
                     hlc_device, dirty, server_seq)
                 VALUES (?1, ?2, 'private', '', 0, 0, 0, 0, 0, 0)",
                params![placeholder.to_string(), "v"],
            )
            .unwrap();

        manifest_from(
            &store,
            OTHER,
            at(1_000),
            vec![
                listed(older, 200, false),
                listed(missing, 100, false),
                // The delete that should have come, and did not.
                listed(not_deleted, 200, true),
                // A group only a host has named so far: the group itself is
                // missing.
                ManifestEntry::new(EntityKind::Group, placeholder, at(100), false),
            ],
        );

        let withheld = store.check_manifests().unwrap();
        assert_eq!(
            withheld,
            Withheld {
                records: 4,
                host_keys: false
            }
        );
        let problems: Vec<(Uuid, Problem)> = store
            .manifest_violations()
            .unwrap()
            .into_iter()
            .map(|v| (v.id, v.problem))
            .collect();
        for expected in [
            (older, Problem::Older),
            (missing, Problem::Missing),
            (not_deleted, Problem::Older),
            (placeholder, Problem::Older),
        ] {
            assert!(problems.contains(&expected), "{expected:?} in {problems:?}");
        }
        // What was found stays found until the next check says otherwise.
        assert_eq!(store.withheld().unwrap(), withheld);

        snippet(&store, older, at(200), false);
        snippet(&store, missing, at(100), false);
        snippet(&store, not_deleted, at(300), true);
        assert_eq!(store.check_manifests().unwrap().records, 1, "the group");
    }

    #[test]
    fn while_host_keys_are_kept_back_only_this_devices_own_are_trusted() {
        let store = Store::open_in_memory().unwrap();
        store
            .trust_host_key(
                "mine.lan",
                22,
                "ssh-ed25519",
                "SHA256:mine",
                "ssh-ed25519 M",
            )
            .unwrap();
        let theirs = Uuid::now_v7();
        store
            .conn
            .lock()
            .execute(
                "INSERT INTO known_hosts
                    (id, vault_id, address, port, algorithm, fingerprint_sha256, public_key,
                     first_seen_ms, hlc_wall_ms, hlc_counter, hlc_device, dirty, server_seq)
                 VALUES (?1, ?2, 'theirs.lan', 22, 'ssh-ed25519', 'SHA256:theirs',
                         'ssh-ed25519 T', 0, 100, 0, ?3, 0, 1)",
                params![theirs.to_string(), "v", OTHER],
            )
            .unwrap();

        // One host key that arrived and lost its slot to a newer one for the
        // same address: not here, and rightly so.
        let gave_way = Uuid::now_v7();
        {
            let mut conn = store.conn.lock();
            let tx = conn.transaction().unwrap();
            crate::sync::supersede(&tx, EntityKind::KnownHost, gave_way, at(100)).unwrap();
            tx.commit().unwrap();
        }
        let key = |id, wall| ManifestEntry::new(EntityKind::KnownHost, id, at(wall), false);
        manifest_from(&store, OTHER, at(1_000), vec![key(gave_way, 100)]);
        assert_eq!(store.check_manifests().unwrap(), Withheld::default());
        assert!(store.known_host("theirs.lan", 22).unwrap().is_some());

        // A newer version of the key is listed and never came.
        manifest_from(
            &store,
            OTHER,
            at(1_000),
            vec![key(gave_way, 100), key(theirs, 200)],
        );
        let withheld = store.check_manifests().unwrap();
        assert!(withheld.host_keys);
        assert!(
            store.known_host("theirs.lan", 22).unwrap().is_none(),
            "a key from another device is not answered with"
        );
        assert_eq!(
            store
                .known_host("mine.lan", 22)
                .unwrap()
                .unwrap()
                .fingerprint,
            "SHA256:mine",
            "one this device trusted itself still is"
        );
        assert_eq!(
            store.list_known_hosts().unwrap().len(),
            2,
            "nothing is lost"
        );

        // The user connects, sees the key and accepts it: it is theirs now.
        store
            .trust_host_key(
                "theirs.lan",
                22,
                "ssh-ed25519",
                "SHA256:new",
                "ssh-ed25519 N",
            )
            .unwrap();
        assert_eq!(
            store
                .known_host("theirs.lan", 22)
                .unwrap()
                .unwrap()
                .fingerprint,
            "SHA256:new"
        );
        assert_eq!(store.check_manifests().unwrap(), Withheld::default());
    }

    #[test]
    fn the_pairing_floor_needs_that_manifest_at_least_that_new() {
        let store = Store::open_in_memory().unwrap();
        let id = manifest_id(vault(&store), OTHER);
        let floor = |wall| ManifestFloor {
            id,
            clock: at(wall),
        };

        store.set_manifest_floor(Some(floor(5_000))).unwrap();
        assert_eq!(store.manifest_floor().unwrap(), Some(floor(5_000)));
        let missing = store.check_manifests().unwrap();
        assert!(missing.host_keys, "any host key could be the withheld one");
        assert_eq!(
            store.manifest_violations().unwrap()[0].problem,
            Problem::Missing
        );

        manifest_from(&store, OTHER, at(4_000), Vec::new());
        store.check_manifests().unwrap();
        assert_eq!(
            store.manifest_violations().unwrap()[0].problem,
            Problem::Older
        );

        manifest_from(&store, OTHER, at(5_000), Vec::new());
        assert_eq!(store.check_manifests().unwrap(), Withheld::default());

        // Unpairing forgets it.
        store.set_manifest_floor(Some(floor(9_000))).unwrap();
        store.forget_enrolment().unwrap();
        assert_eq!(store.manifest_floor().unwrap(), None);
        assert_eq!(store.withheld().unwrap(), Withheld::default());
    }

    #[test]
    fn a_manifest_lists_what_the_server_confirmed_and_changes_only_with_it() {
        let store = Store::open_in_memory().unwrap();
        let confirmed = Uuid::now_v7();
        snippet(&store, confirmed, at(100), false);
        // Changed here and not pushed; never seen by the server; a
        // placeholder.
        let (dirty, unseen) = (Uuid::now_v7(), Uuid::now_v7());
        snippet(&store, dirty, at(100), false);
        snippet(&store, unseen, at(100), true);
        store
            .conn
            .lock()
            .execute_batch(&format!(
                "UPDATE snippets SET dirty = 1 WHERE id = '{dirty}';
                 UPDATE snippets SET server_seq = 0 WHERE id = '{unseen}';"
            ))
            .unwrap();

        assert!(store.refresh_manifest().unwrap());
        let own = store.own_manifest_id().unwrap();
        let (entries, is_dirty): (Vec<u8>, bool) = store
            .conn
            .lock()
            .query_row(
                "SELECT entries, dirty FROM manifests WHERE id = ?1",
                [own.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let manifest = Manifest::decode(&entries).unwrap();
        assert_eq!(manifest.device, store.device);
        assert_eq!(manifest.entries, vec![listed(confirmed, 100, false)]);
        assert!(is_dirty, "waiting to be pushed");
        assert_eq!(
            store.published_manifest().unwrap(),
            None,
            "not published until the server has it"
        );

        assert!(!store.refresh_manifest().unwrap(), "nothing changed");
        snippet(&store, confirmed, at(300), false);
        assert!(store.refresh_manifest().unwrap());

        store
            .conn
            .lock()
            .execute(
                "UPDATE manifests SET dirty = 0, server_seq = 7 WHERE id = ?1",
                [own.to_string()],
            )
            .unwrap();
        let published = store.published_manifest().unwrap().unwrap();
        assert_eq!(published.id, own);
        assert_eq!(Some(published.clock), store.manifest_clock(own).unwrap());
    }

    #[test]
    fn a_vault_too_large_for_one_manifest_lists_its_host_keys_first() {
        let store = Store::open_in_memory().unwrap();
        {
            let vault = vault(&store).to_string();
            let mut conn = store.conn.lock();
            let tx = conn.transaction().unwrap();
            for index in 0..MAX_MANIFEST_ENTRIES + 5 {
                tx.execute(
                    "INSERT INTO snippets
                        (id, vault_id, label, body, hlc_wall_ms, hlc_counter, hlc_device,
                         dirty, server_seq)
                     VALUES (?1, ?2, 's', 'b', ?3, 0, 1, 0, 1)",
                    params![Uuid::now_v7().to_string(), vault, index as i64 + 1],
                )
                .unwrap();
            }
            tx.execute(
                "INSERT INTO known_hosts
                    (id, vault_id, address, port, algorithm, fingerprint_sha256, public_key,
                     first_seen_ms, hlc_wall_ms, hlc_counter, hlc_device, dirty, server_seq)
                 VALUES (?1, ?2, 'nas.lan', 22, 'ssh-ed25519', 'SHA256:x', 'k', 0, 1, 0, 1, 0, 1)",
                params![Uuid::now_v7().to_string(), vault],
            )
            .unwrap();
            tx.commit().unwrap();
        }
        assert!(store.refresh_manifest().unwrap());
        let entries: Vec<u8> = store
            .conn
            .lock()
            .query_row("SELECT entries FROM manifests", [], |row| row.get(0))
            .unwrap();
        assert!(entries.len() + 16 <= uwurdp_proto::MAX_BLOB_BYTES);
        let manifest = Manifest::decode(&entries).unwrap();
        assert!(manifest.partial);
        assert_eq!(manifest.entries.len(), MAX_MANIFEST_ENTRIES);
        assert_eq!(manifest.entries[0].kind(), Some(EntityKind::KnownHost));
    }

    #[test]
    fn a_manifest_id_belongs_to_one_device_of_one_vault() {
        let vault = Uuid::now_v7();
        let id = manifest_id(vault, 7);
        assert_eq!(id, manifest_id(vault, 7), "the same on every device");
        assert_ne!(id, manifest_id(vault, 8));
        assert_ne!(id, manifest_id(Uuid::now_v7(), 7));
        assert_eq!(id.get_version_num(), 8);
        assert_eq!(id.get_variant(), uuid::Variant::RFC4122);
    }

    #[test]
    fn the_priority_covers_every_synced_kind_once() {
        let mut listed = PRIORITY.to_vec();
        listed.sort();
        let mut synced = SYNCED_KINDS.to_vec();
        synced.sort();
        assert_eq!(listed, synced);
    }
}
