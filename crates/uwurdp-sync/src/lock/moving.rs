//! The one-click move from UwUSync to UwULock.
//!
//! Everything UwUSync holds is read, opened with the vault key this device
//! has, sealed again for the UwULock space — same id, kind, clock and
//! tombstone, a new nonce, the space's id and key — and pushed. Then the copy
//! is read back and held against what was read: only when every record is
//! there, at least as new, does the caller switch this device over. UwUSync is
//! only ever read from, so until then nothing has changed for it, and a move
//! that stops halfway leaves this device syncing as before.
//!
//! Running it again — after an interruption, or on the next device of the
//! same person — is safe and quick: what the space holds already at the same
//! version is not pushed again, and what another device moved or changed
//! since goes through the merge rule every sync uses.
//!
//! Manifests are not moved: they speak of one vault's devices, and each
//! device writes a fresh one for the space at its first sync there.

use super::api::Lock;
use super::LockError;
use crate::engine::{Transport, TransportError, MAX_ROUNDS};
use serde::Serialize;
use std::collections::HashMap;
use uuid::Uuid;
use uwurdp_proto::{
    resolve, EntityKind, Envelope, Resolution, SyncCursor, Version, MAX_BATCH, MAX_BATCH_BYTES,
    MAX_BLOB_BYTES,
};
use uwurdp_store::{Space, Store};
use uwurdp_vault::UnlockedVault;

/// The most pages one read goes through, as in a sync pass.
const MAX_PAGES: usize = 1_000;

/// What a move did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveReport {
    /// Records read from UwUSync and opened.
    pub read: usize,
    /// Pushed to UwULock now.
    pub copied: usize,
    /// On UwULock at this version already: an earlier run, or another device.
    pub already_there: usize,
    /// On UwULock in a newer version, which stays.
    pub newer_there: usize,
    /// On UwUSync, but not opening with this device's key: nothing to copy.
    pub unreadable: usize,
}

/// A record the copy does not hold as it should.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Difference {
    pub id: Uuid,
    pub kind: EntityKind,
    pub problem: Problem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Problem {
    /// Not on UwULock at all.
    Missing,
    /// On UwULock in an older version than on UwUSync.
    Older,
}

/// One record as it was read from UwUSync and opened.
struct Opened {
    envelope: Envelope,
    payload: zeroize::Zeroizing<Vec<u8>>,
}

/// Copy everything from `from` (UwUSync, whose vault is open in `store`) to
/// `to` (UwULock, with its space set), and check the copy.
///
/// Nothing on this device changes, and nothing on UwUSync: the caller
/// switches with `Store::join_space` once this came back `Ok`.
pub fn copy_to_lock<T: Transport>(
    store: &Store,
    from: &T,
    to: &Lock,
    space: &Space,
) -> Result<MoveReport, LockError> {
    if to.space() != Some(space.id) {
        return Err(TransportError::Refused("the move needs the space it copies to".into()).into());
    }
    let mut report = MoveReport::default();

    // Step 3 of the contract: everything UwUSync has, opened.
    let mut source = Vec::new();
    for envelope in read_all(from)? {
        if envelope.kind == EntityKind::Manifest {
            continue;
        }
        match store.open_envelope(&envelope) {
            Ok(Some(payload)) => source.push(Opened { envelope, payload }),
            Ok(None) => report.unreadable += 1,
            Err(uwurdp_store::StoreError::VaultLocked) => return Err(LockError::VaultLocked),
            Err(error) => return Err(error.into()),
        }
    }
    report.read = source.len();

    // Step 4: sealed again for the space, and pushed where the space does
    // not hold it at least as new.
    let target = UnlockedVault::from_key(space.id, space.key.clone());
    let mut there = headers(to)?;
    let mut waiting: Vec<(usize, u64)> = Vec::new();
    for (index, opened) in source.iter().enumerate() {
        match there.get(&opened.envelope.id) {
            None => waiting.push((index, 0)),
            Some(held) => match resolve(version(&opened.envelope), held.version) {
                Resolution::Identical => report.already_there += 1,
                Resolution::Remote => report.newer_there += 1,
                Resolution::Local => waiting.push((index, held.seq)),
            },
        }
    }

    for _ in 0..MAX_ROUNDS {
        if waiting.is_empty() {
            break;
        }
        let mut again = Vec::new();
        for batch in batches(&source, &waiting) {
            let envelopes = batch
                .iter()
                .map(|&(index, base_seq)| seal(&target, &source[index], base_seq))
                .collect::<Result<Vec<_>, _>>()?;
            let response = to.push(envelopes)?;
            report.copied += response.accepted.len();
            for conflict in response.conflicts {
                // Someone else wrote this record meanwhile: the merge rule
                // decides, as in every sync.
                let Some(&(index, _)) = batch
                    .iter()
                    .find(|(index, _)| source[*index].envelope.id == conflict.id)
                else {
                    continue;
                };
                let held = Held::of(&conflict);
                match resolve(version(&source[index].envelope), held.version) {
                    Resolution::Local => again.push((index, held.seq)),
                    Resolution::Identical => report.already_there += 1,
                    Resolution::Remote => report.newer_there += 1,
                }
                there.insert(conflict.id, held);
            }
        }
        waiting = again;
    }

    // Step 5: read the copy back and hold it against what was read.
    let there = headers(to)?;
    let mut differences: Vec<Difference> = source
        .iter()
        .filter_map(|opened| {
            let envelope = &opened.envelope;
            let problem = match there.get(&envelope.id) {
                None => Problem::Missing,
                Some(held) => match resolve(version(envelope), held.version) {
                    Resolution::Identical | Resolution::Remote => return None,
                    Resolution::Local => Problem::Older,
                },
            };
            Some(Difference {
                id: envelope.id,
                kind: envelope.kind,
                problem,
            })
        })
        .collect();
    if !differences.is_empty() {
        differences.sort_by_key(|d| (d.kind, d.id));
        return Err(LockError::MoveCheck(differences));
    }
    Ok(report)
}

fn version(envelope: &Envelope) -> Version {
    Version::new(envelope.updated_at, envelope.deleted)
}

/// What the space holds of a record: its version and the `seq` to base a
/// push on.
#[derive(Debug, Clone, Copy)]
struct Held {
    version: Version,
    seq: u64,
}

impl Held {
    fn of(envelope: &Envelope) -> Self {
        Self {
            version: Version::new(envelope.updated_at, envelope.deleted),
            seq: envelope.seq.unwrap_or(0),
        }
    }
}

/// The headers of everything in the space. The seals are not opened: whose
/// version is newer is in the header, and a header a server made up fails
/// its seal on the first sync there, like any other.
fn headers(lock: &Lock) -> Result<HashMap<Uuid, Held>, TransportError> {
    Ok(read_all(lock)?
        .into_iter()
        .map(|envelope| (envelope.id, Held::of(&envelope)))
        .collect())
}

/// Every record a server has, from the start, page by page.
fn read_all<T: Transport>(transport: &T) -> Result<Vec<Envelope>, TransportError> {
    let mut all = Vec::new();
    let mut cursor = SyncCursor(0);
    for _ in 0..MAX_PAGES {
        let page = transport.pull(cursor, MAX_BATCH)?;
        if transport.take_reset() {
            return Err(TransportError::Refused(
                "the server started over while it was read".into(),
            ));
        }
        let empty = page.envelopes.is_empty();
        all.extend(page.envelopes);
        if empty || !page.has_more {
            return Ok(all);
        }
        if page.cursor <= cursor {
            return Err(TransportError::Refused(
                "the server's cursor went backwards".into(),
            ));
        }
        cursor = page.cursor;
    }
    Err(TransportError::Refused(
        "the server never came to an end".into(),
    ))
}

/// Records to push in requests the protocol allows: at most `MAX_BATCH` and
/// `MAX_BATCH_BYTES` each, and one record whatever its size.
fn batches(source: &[Opened], waiting: &[(usize, u64)]) -> Vec<Vec<(usize, u64)>> {
    let mut out: Vec<Vec<(usize, u64)>> = Vec::new();
    let mut bytes = 0;
    for &item in waiting {
        // The sealed size is the payload and a 16-byte tag.
        let size = source[item.0].payload.len() + 16;
        let full = out
            .last()
            .is_none_or(|batch| batch.len() >= MAX_BATCH || bytes + size > MAX_BATCH_BYTES);
        if full {
            out.push(Vec::new());
            bytes = 0;
        }
        bytes += size;
        out.last_mut().expect("a batch").push(item);
    }
    out
}

fn seal(target: &UnlockedVault, opened: &Opened, base_seq: u64) -> Result<Envelope, LockError> {
    let envelope = &opened.envelope;
    let sealed = target.seal_synced(
        envelope.id,
        envelope.kind,
        envelope.updated_at,
        envelope.deleted,
        &opened.payload,
    )?;
    if sealed.blob.len() > MAX_BLOB_BYTES {
        // It came through UwUSync, which holds records to the same limit.
        return Err(TransportError::Refused("a record too large to move".into()).into());
    }
    Ok(Envelope {
        id: envelope.id,
        vault_id: target.vault_id(),
        kind: envelope.kind,
        updated_at: envelope.updated_at,
        base_seq,
        deleted: envelope.deleted,
        nonce: sealed.nonce,
        blob: sealed.blob,
        seq: None,
    })
}

impl From<uwurdp_vault::VaultError> for LockError {
    fn from(error: uwurdp_vault::VaultError) -> Self {
        Self::Crypto(error.to_string())
    }
}
