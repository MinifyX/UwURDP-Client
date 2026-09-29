//! The one-click move from UwUSync to UwULock.
//!
//! Everything UwUSync holds is read, opened with the vault key this device
//! has, sealed again for the UwULock space — same id, kind, clock and
//! tombstone, a new nonce, the space's id and key — and pushed. Then the copy
//! is read back, every record of it opened with the space's key and held
//! against what was read: only when every record is there, opens, and is at
//! least as new, does the caller switch this device over. UwUSync is only ever
//! read from, so until then nothing has changed for it, and a move that stops
//! halfway leaves this device syncing as before.
//!
//! Running it again — after an interruption, or on the next device of the
//! same person — is safe and quick: what the space holds already at the same
//! version is not pushed again, and what another device moved or changed
//! since goes through the merge rule every sync uses.
//!
//! Manifests are not moved: they speak of one vault's devices, and each
//! device writes a fresh one for the space at its first sync there.
//!
//! What is kept while it runs is small and has a ceiling: of UwUSync's records
//! only the header and the opened payload, of the space's only the header.
//! Neither may come to more than a UwULock space holds ([`MAX_RECORDS`],
//! [`MAX_BYTES`]); a server that sends more stops the move instead of filling
//! this device's memory.

use super::api::Lock;
use super::LockError;
use crate::engine::{Transport, TransportError, MAX_ROUNDS};
use serde::Serialize;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
use uwurdp_proto::{
    resolve, EntityKind, Envelope, Hlc, Resolution, SyncCursor, Version, MAX_BATCH,
    MAX_BATCH_BYTES, MAX_BLOB_BYTES,
};
use uwurdp_store::{Space, Store};
use uwurdp_vault::{Sealed, UnlockedVault};

/// The most pages one read goes through, as in a sync pass.
const MAX_PAGES: usize = 1_000;
/// The most records a UwULock space holds: the server's default quota for
/// an account's suite vault (`suite.maxRecords`).
pub const MAX_RECORDS: usize = 50_000;
/// The most bytes of sealed records a UwULock space holds: the server's
/// default quota (`suite.maxMb`, 256 MiB).
pub const MAX_BYTES: usize = 256 * 1024 * 1024;
/// A seal is the payload and a 16-byte tag.
const TAG_BYTES: usize = 16;

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
    /// Not on UwULock at all, or not opening there with the space's key.
    Missing,
    /// On UwULock in an older version than on UwUSync.
    Older,
}

/// One record as it was read from UwUSync and opened: its header and its
/// payload, not the sealed blob it came in.
struct Opened {
    id: Uuid,
    kind: EntityKind,
    updated_at: Hlc,
    deleted: bool,
    payload: zeroize::Zeroizing<Vec<u8>>,
}

impl Opened {
    fn version(&self) -> Version {
        Version::new(self.updated_at, self.deleted)
    }
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
    let mut budget = Budget::new(
        "UwUSync holds more than a UwULock space takes (50 000 records, 256 MiB): \
         nothing was moved",
    );
    each_record(from, |envelope| {
        if envelope.kind == EntityKind::Manifest {
            return Ok(());
        }
        match store.open_envelope(&envelope) {
            Ok(Some(payload)) => {
                budget.take(payload.len() + TAG_BYTES)?;
                source.push(Opened {
                    id: envelope.id,
                    kind: envelope.kind,
                    updated_at: envelope.updated_at,
                    deleted: envelope.deleted,
                    payload,
                });
            }
            Ok(None) => report.unreadable += 1,
            Err(uwurdp_store::StoreError::VaultLocked) => return Err(LockError::VaultLocked),
            Err(error) => return Err(error.into()),
        }
        Ok(())
    })?;
    report.read = source.len();

    // Step 4: sealed again for the space, and pushed where the space does
    // not hold it at least as new.
    let target = UnlockedVault::from_key(space.id, space.key.clone());
    let mut there = headers(to)?;
    let mut waiting: Vec<(usize, u64)> = Vec::new();
    for (index, opened) in source.iter().enumerate() {
        match there.get(&opened.id) {
            None => waiting.push((index, 0)),
            Some(held) => match resolve(opened.version(), held.version) {
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
                    .find(|(index, _)| source[*index].id == conflict.id)
                else {
                    continue;
                };
                let held = Held::of(&conflict);
                match resolve(source[index].version(), held.version) {
                    Resolution::Local => again.push((index, held.seq)),
                    Resolution::Identical => report.already_there += 1,
                    Resolution::Remote => report.newer_there += 1,
                }
                there.insert(conflict.id, held);
            }
        }
        waiting = again;
    }

    // Step 5: read the copy back, open it, and hold it against what was read.
    let wanted: HashSet<Uuid> = source.iter().map(|opened| opened.id).collect();
    let there = opened_copy(to, &target, &wanted)?;
    let mut differences: Vec<Difference> = source
        .iter()
        .filter_map(|opened| {
            let problem = match there.get(&opened.id) {
                None => Problem::Missing,
                Some(held) => match resolve(opened.version(), held.version) {
                    Resolution::Identical | Resolution::Remote => return None,
                    Resolution::Local => Problem::Older,
                },
            };
            Some(Difference {
                id: opened.id,
                kind: opened.kind,
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

/// How much of a server's records this device takes in before it stops.
struct Budget {
    /// What the refusal says.
    too_much: &'static str,
    records: usize,
    bytes: usize,
}

impl Budget {
    fn new(too_much: &'static str) -> Self {
        Self {
            too_much,
            records: 0,
            bytes: 0,
        }
    }

    /// One more record of this many bytes.
    fn take(&mut self, bytes: usize) -> Result<(), TransportError> {
        self.records += 1;
        self.bytes = self.bytes.saturating_add(bytes);
        if self.records > MAX_RECORDS || self.bytes > MAX_BYTES {
            return Err(TransportError::Refused(self.too_much.into()));
        }
        Ok(())
    }
}

const TOO_MUCH_THERE: &str =
    "UwULock sent more than a space holds (50 000 records, 256 MiB): nothing switched";

/// The headers of everything in the space, to decide what to push. The
/// seals are not opened here: a header a server made up only changes what is
/// pushed, and the check at the end ([`opened_copy`]) finds it out.
fn headers(lock: &Lock) -> Result<HashMap<Uuid, Held>, LockError> {
    let mut budget = Budget::new(TOO_MUCH_THERE);
    let mut held = HashMap::new();
    each_record(lock, |envelope| {
        budget.take(envelope.blob.len())?;
        held.insert(envelope.id, Held::of(&envelope));
        Ok(())
    })?;
    Ok(held)
}

/// The versions of the `wanted` records the space holds, as far as they open
/// with the space's key under their own header: a record a server made up,
/// or whose header it changed, is left out, and so counts as missing.
fn opened_copy(
    lock: &Lock,
    target: &UnlockedVault,
    wanted: &HashSet<Uuid>,
) -> Result<HashMap<Uuid, Held>, LockError> {
    let mut budget = Budget::new(TOO_MUCH_THERE);
    let mut copy: HashMap<Uuid, Held> = HashMap::new();
    each_record(lock, |envelope| {
        budget.take(envelope.blob.len())?;
        if !wanted.contains(&envelope.id) {
            return Ok(());
        }
        let (id, held) = (envelope.id, Held::of(&envelope));
        if !opens(target, envelope) {
            return Ok(());
        }
        match copy.entry(id) {
            Entry::Vacant(entry) => {
                entry.insert(held);
            }
            // The same record twice: the newer one that opens counts.
            Entry::Occupied(mut entry) => {
                if resolve(held.version, entry.get().version) == Resolution::Local {
                    entry.insert(held);
                }
            }
        }
        Ok(())
    })?;
    Ok(copy)
}

/// Whether a record opens with the space's key, its header bound in.
fn opens(target: &UnlockedVault, envelope: Envelope) -> bool {
    envelope.vault_id == target.vault_id()
        && target
            .open_synced(
                envelope.id,
                envelope.kind,
                envelope.updated_at,
                envelope.deleted,
                &Sealed {
                    nonce: envelope.nonce,
                    blob: envelope.blob,
                },
            )
            .is_ok()
}

/// Every record a server has, from the start, page by page, handed to `each`
/// and not kept here: only one page is held at a time.
fn each_record<T: Transport>(
    transport: &T,
    mut each: impl FnMut(Envelope) -> Result<(), LockError>,
) -> Result<(), LockError> {
    let mut cursor = SyncCursor(0);
    for _ in 0..MAX_PAGES {
        let page = transport.pull(cursor, MAX_BATCH)?;
        if transport.take_reset() {
            return Err(TransportError::Refused(
                "the server started over while it was read".into(),
            )
            .into());
        }
        let done = page.envelopes.is_empty() || !page.has_more;
        for envelope in page.envelopes {
            each(envelope)?;
        }
        if done {
            return Ok(());
        }
        if page.cursor <= cursor {
            return Err(
                TransportError::Refused("the server's cursor went backwards".into()).into(),
            );
        }
        cursor = page.cursor;
    }
    Err(TransportError::Refused("the server never came to an end".into()).into())
}

/// Records to push in requests the protocol allows: at most `MAX_BATCH` and
/// `MAX_BATCH_BYTES` each, and one record whatever its size.
fn batches(source: &[Opened], waiting: &[(usize, u64)]) -> Vec<Vec<(usize, u64)>> {
    let mut out: Vec<Vec<(usize, u64)>> = Vec::new();
    let mut bytes = 0;
    for &item in waiting {
        let size = source[item.0].payload.len() + TAG_BYTES;
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
    let sealed = target.seal_synced(
        opened.id,
        opened.kind,
        opened.updated_at,
        opened.deleted,
        &opened.payload,
    )?;
    if sealed.blob.len() > MAX_BLOB_BYTES {
        // It came through UwUSync, which holds records to the same limit.
        return Err(TransportError::Refused("a record too large to move".into()).into());
    }
    Ok(Envelope {
        id: opened.id,
        vault_id: target.vault_id(),
        kind: opened.kind,
        updated_at: opened.updated_at,
        base_seq,
        deleted: opened.deleted,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_stops_at_the_quota_of_a_space() {
        let mut budget = Budget::new("too much");
        for _ in 0..MAX_RECORDS {
            budget.take(1).unwrap();
        }
        assert!(matches!(budget.take(1), Err(TransportError::Refused(m)) if m == "too much"));

        let mut budget = Budget::new("too much");
        budget.take(MAX_BYTES - 1).unwrap();
        budget.take(1).unwrap();
        assert!(budget.take(1).is_err(), "one byte over");
        assert!(Budget::new("x").take(usize::MAX).is_err());
    }
}
