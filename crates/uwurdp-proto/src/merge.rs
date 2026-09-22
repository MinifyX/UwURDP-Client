//! Who wins when two devices edited the same record.
//!
//! Last-writer-wins per record, decided by the hybrid logical clock rather than
//! wall time. Per record and not per field: two devices practically never edit
//! the same host at the same moment, and a field-wise merge needs a common
//! ancestor for every field, which costs a second copy of every record for a
//! case that does not happen. Snippet bodies, where text really can collide,
//! are the one place that may earn a three-way merge later.
//!
//! One asymmetry is deliberate: **a delete beats a concurrent edit**. If one
//! device removed a host while another renamed it, resurrecting the host is
//! the worse outcome — the rename is cheap to redo, an unexpectedly returning
//! host you thought you had removed is confusing and, for a bastion you
//! decommissioned, actively wrong.
//!
//! This lives in `uwurdp-proto` rather than in the sync engine because both
//! sides of the engine need it: the store applies the decision inside the same
//! transaction that writes the record, and the engine decides what to push.

use crate::clock::Hlc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Keep ours, push it.
    Local,
    /// Take theirs, write it to the store.
    Remote,
    /// Same version of the same record — nothing to do.
    Identical,
}

/// One side of the comparison: when it was written, and whether it is a
/// tombstone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub updated_at: Hlc,
    pub deleted: bool,
}

impl Version {
    pub fn new(updated_at: Hlc, deleted: bool) -> Self {
        Self {
            updated_at,
            deleted,
        }
    }
}

pub fn resolve(local: Version, remote: Version) -> Resolution {
    if local == remote {
        return Resolution::Identical;
    }

    // Tombstones win over concurrent edits regardless of clock.
    match (local.deleted, remote.deleted) {
        (true, false) => return Resolution::Local,
        (false, true) => return Resolution::Remote,
        _ => {}
    }

    if local.updated_at > remote.updated_at {
        Resolution::Local
    } else {
        Resolution::Remote
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(wall: u64, counter: u32, device: u32, deleted: bool) -> Version {
        Version::new(Hlc::new(wall, counter, device), deleted)
    }

    #[test]
    fn the_later_edit_wins() {
        assert_eq!(
            resolve(v(200, 0, 1, false), v(100, 0, 2, false)),
            Resolution::Local
        );
        assert_eq!(
            resolve(v(100, 0, 1, false), v(200, 0, 2, false)),
            Resolution::Remote
        );
    }

    #[test]
    fn same_millisecond_is_broken_by_the_counter() {
        assert_eq!(
            resolve(v(100, 5, 1, false), v(100, 2, 2, false)),
            Resolution::Local
        );
    }

    #[test]
    fn identical_versions_need_no_work() {
        assert_eq!(
            resolve(v(100, 1, 1, false), v(100, 1, 1, false)),
            Resolution::Identical
        );
    }

    #[test]
    fn a_delete_beats_a_newer_edit() {
        // Remote edited later, but we deleted. The host stays deleted.
        assert_eq!(
            resolve(v(100, 0, 1, true), v(900, 0, 2, false)),
            Resolution::Local
        );
        assert_eq!(
            resolve(v(900, 0, 1, false), v(100, 0, 2, true)),
            Resolution::Remote
        );
    }

    #[test]
    fn two_deletes_fall_back_to_the_clock() {
        assert_eq!(
            resolve(v(200, 0, 1, true), v(100, 0, 2, true)),
            Resolution::Local
        );
    }

    #[test]
    fn a_tie_between_two_devices_goes_the_same_way_on_both() {
        // The same pair, seen from either side: exactly one of them keeps its
        // own version, or the two would flap forever.
        let a = v(100, 0, 1, false);
        let b = v(100, 0, 2, false);
        assert_eq!(resolve(a, b), Resolution::Remote);
        assert_eq!(resolve(b, a), Resolution::Local);
    }
}
