//! One pass of syncing: hand the server what we have, then take what it has.
//!
//! Push first, pull second, and the second half is not optional even when
//! nothing came back: the cursor only moves on a pull, so a pass that ended
//! with a push would leave this device believing it had not seen its own
//! writes, and the next pass would download them. Pulling last also means the
//! records this device just pushed come back once and are checked against what
//! is here — cheap, and it catches a server that stored something else.
//!
//! A conflict therefore shows up on the way out, as a refusal carrying the
//! version the server holds. That version is merged straight away, and the
//! round starts over.
//!
//! A pass that got everything out and pulled to the end has one more thing to
//! do: publish this device's manifest if what it holds changed, and hold every
//! device's manifest against what is here — which is how a server that keeps
//! records back, or hands out old versions, is caught (see
//! `uwurdp_store::manifest`).
//!
//! The pass is blocking and belongs on a thread of its own, not in the UI's
//! runtime: every step is either a database transaction or one request, and
//! neither wants to be interleaved with the terminal's data path.

use uwurdp_proto::{EntityKind, Envelope, PullResponse, PushResponse, SyncCursor, MAX_BATCH};
use uwurdp_store::{ApplyReport, Pushed, Store, StoreError, Withheld};

/// What the client needs from a server. The real one speaks HTTP; the tests
/// use [`crate::MemoryServer`], which is the same set of rules without a
/// network.
pub trait Transport {
    /// Records newer than a cursor, in sequence order, at most `limit`.
    fn pull(&self, since: SyncCursor, limit: usize) -> Result<PullResponse, TransportError>;
    /// Offer records. Each one is taken or reported as a conflict, with the
    /// version the server holds instead.
    fn push(&self, envelopes: Vec<Envelope>) -> Result<PushResponse, TransportError>;
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("the server refused the request: {0}")]
    Refused(String),
    #[error("the server could not be reached: {0}")]
    Unreachable(String),
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("syncing needs the vault unlocked")]
    VaultLocked,
    #[error(transparent)]
    Store(StoreError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("gave up after {0} rounds of conflicts")]
    ConflictLoop(u32),
}

impl From<StoreError> for SyncError {
    fn from(error: StoreError) -> Self {
        // A locked vault is not a database problem, and the UI acts on it: it
        // asks for the master password instead of showing an error.
        match error {
            StoreError::VaultLocked => Self::VaultLocked,
            other => Self::Store(other),
        }
    }
}

/// What one pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub pulled: usize,
    pub pushed: usize,
    /// Records the server refused because another device wrote first. They are
    /// merged and offered again, so this is not an error.
    pub conflicts: usize,
    /// How many rounds of pull-then-push it took.
    pub rounds: u32,
    /// Records too large for the protocol, left where they are.
    pub oversized: usize,
    pub apply: ApplyReport,
    pub cursor: u64,
    /// What the other devices' manifests say this one should have and does
    /// not: anything here means the server is keeping records back or handing
    /// out old versions. Carried over from the last complete check when this
    /// pass could not make one.
    pub withheld: Withheld,
}

/// How often a pass may go round before it stops trying. Three is generous:
/// one round settles a conflict with one other device, two settle a race with
/// two, and anything beyond that is a bug rather than a busy household.
pub const MAX_ROUNDS: u32 = 3;

/// The most pages one pass pulls. Half a million records is more than any
/// household of hosts, and a server that always says "there is more" must not
/// keep the sync thread busy forever.
const MAX_PAGES: usize = 1_000;

/// Sync once: everything we have, then everything the server has — and then,
/// with both done, this device's manifest out and everyone's checked.
pub fn sync_once<T: Transport>(store: &Store, transport: &T) -> Result<SyncReport, SyncError> {
    let mut report = SyncReport::default();
    for round in 1..=MAX_ROUNDS {
        report.rounds = round;
        let conflicts = push_all(store, transport, &mut report)?;
        let complete = pull_all(store, transport, &mut report)?;
        if conflicts == 0 {
            let complete = complete && publish_manifest(store, transport, &mut report)?;
            report.withheld = check(store, transport, &mut report, complete)?;
            report.cursor = store.sync_state()?.cursor;
            return Ok(report);
        }
    }
    Err(SyncError::ConflictLoop(MAX_ROUNDS))
}

/// Everything the server has that we have not seen, page by page. Each page is
/// applied and the cursor moved before the next one is asked for, so an
/// interrupted sync resumes where it stopped instead of starting over.
///
/// Returns whether the pull got to the end of what the server offers — which
/// includes a server that stopped making sense along the way: what it did not
/// hand over, it withheld.
fn pull_all<T: Transport>(
    store: &Store,
    transport: &T,
    report: &mut SyncReport,
) -> Result<bool, SyncError> {
    for _ in 0..MAX_PAGES {
        let cursor = store.sync_state()?.cursor;
        let page = transport.pull(SyncCursor(cursor), MAX_BATCH)?;
        if page.envelopes.is_empty() {
            return Ok(true);
        }
        // Records, not the manifests that came with them.
        report.pulled += page
            .envelopes
            .iter()
            .filter(|env| env.kind != EntityKind::Manifest)
            .count();
        let applied = store.apply_envelopes(&page.envelopes)?;
        report.apply.add(applied);
        // A page of nothing but records that fail their seal is a server
        // making things up. The next pass may ask again; this one stops.
        if applied.rejected == page.envelopes.len() {
            return Ok(true);
        }
        // Never move the cursor backwards: a server that answers with a
        // smaller one would make this device forget what it has already seen.
        if page.cursor.0 > cursor {
            store.set_sync_cursor(page.cursor.0)?;
        } else {
            return Ok(true);
        }
        if !page.has_more {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Hold the manifests against what is here.
///
/// Only a pull that reached the end can tell what is missing: until then, a
/// listed record may just be on a page not fetched yet, so an incomplete pass
/// keeps what the last complete one found. And before the alarm goes up for
/// the first time, everything is pulled once more from the start: whatever
/// this device missed by accident — a cursor carried over from somewhere, a
/// page lost along the way — it gets now, and what is still missing after
/// that, the server really is keeping back.
fn check<T: Transport>(
    store: &Store,
    transport: &T,
    report: &mut SyncReport,
    complete: bool,
) -> Result<Withheld, SyncError> {
    if !complete {
        return Ok(store.withheld()?);
    }
    let before = store.withheld()?;
    let found = store.check_manifests()?;
    if !found.any() || before.any() {
        return Ok(found);
    }
    tracing::info!(
        records = found.records,
        "records other devices hold are missing here; pulling everything again"
    );
    store.set_sync_cursor(0)?;
    if pull_all(store, transport, report)? {
        Ok(store.check_manifests()?)
    } else {
        Ok(found)
    }
}

/// Write this device's manifest if what it holds changed, and push it — on
/// its own, after everything it lists is on the server. Returns whether the
/// pull that follows reached the end.
///
/// A manifest that does not get through is left for the next pass rather than
/// failing this one: the records themselves are in, and a server too old to
/// know the kind refuses it every time until it is updated.
fn publish_manifest<T: Transport>(
    store: &Store,
    transport: &T,
    report: &mut SyncReport,
) -> Result<bool, SyncError> {
    store.refresh_manifest()?;
    let Some(manifest) = store.pending_manifest()? else {
        return Ok(true);
    };
    let response = match transport.push(vec![manifest.clone()]) {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, "this device's manifest did not get through");
            return Ok(true);
        }
    };
    let accepted: Vec<Pushed> = response
        .accepted
        .iter()
        .filter(|taken| taken.id == manifest.id)
        .map(|taken| Pushed {
            id: taken.id,
            kind: manifest.kind,
            updated_at: manifest.updated_at,
            seq: taken.seq,
        })
        .collect();
    store.mark_pushed(&accepted)?;
    if !response.conflicts.is_empty() {
        // Someone wrote under this device's manifest id — a copy of this
        // database elsewhere, or a restore. Merged like any record; the next
        // pass tries again.
        report
            .apply
            .add(store.apply_envelopes(&response.conflicts)?);
    }
    pull_all(store, transport, report)
}

/// Everything waiting here, in batches. Returns how many records the server
/// refused as conflicts, which is what decides whether another round is worth
/// it.
fn push_all<T: Transport>(
    store: &Store,
    transport: &T,
    report: &mut SyncReport,
) -> Result<usize, SyncError> {
    let mut conflicts = 0;
    loop {
        let (envelopes, oversized) = store.pending_envelopes(MAX_BATCH)?;
        report.oversized = oversized;
        if envelopes.is_empty() {
            return Ok(conflicts);
        }

        let response = transport.push(envelopes.clone())?;
        let accepted: Vec<Pushed> = response
            .accepted
            .iter()
            .filter_map(|taken| {
                let sent = envelopes.iter().find(|env| env.id == taken.id)?;
                Some(Pushed {
                    id: taken.id,
                    kind: sent.kind,
                    updated_at: sent.updated_at,
                    seq: taken.seq,
                })
            })
            .collect();
        let cleared = store.mark_pushed(&accepted)?;
        report.pushed += accepted.len();

        if !response.conflicts.is_empty() {
            conflicts += response.conflicts.len();
            report.conflicts += response.conflicts.len();
            // The server hands back what it holds; merging it decides who
            // wins and, either way, gives the next push the right version to
            // build on.
            report
                .apply
                .add(store.apply_envelopes(&response.conflicts)?);
        }

        if accepted.is_empty() || cleared == 0 {
            // Nothing moved — either every record conflicted, or every row
            // changed while the request was in flight. Either way the next
            // round starts from a fresh look at the store.
            return Ok(conflicts);
        }
    }
}
