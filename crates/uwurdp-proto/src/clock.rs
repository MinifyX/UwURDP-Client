//! Hybrid logical clock.
//!
//! Wall-clock timestamps across devices are a bug source: laptops sleep, phones
//! drift, and "last write wins" then means "whoever's clock is fastest wins".
//! An HLC keeps the wall clock for human readability but adds a counter that
//! breaks ties deterministically, so two devices can never disagree about which
//! of two edits came later.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// How far ahead of this device another one's clock may push the local clock
/// when its edits arrive: one day, which covers a time zone typo and a
/// forgotten daylight saving change without letting a device stuck in the next
/// century poison every other one.
pub const MAX_DRIFT_MS: u64 = 24 * 60 * 60 * 1000;

/// A hybrid logical clock timestamp: wall time, a tie-break counter, and the
/// device that produced it (so even identical (wall, counter) pairs order
/// stably instead of flapping).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hlc {
    /// Milliseconds since the Unix epoch.
    pub wall_ms: u64,
    /// Incremented when two events share the same millisecond.
    pub counter: u32,
    /// Truncated device id, only used to break exact ties.
    pub device: u32,
}

impl Hlc {
    pub fn new(wall_ms: u64, counter: u32, device: u32) -> Self {
        Self {
            wall_ms,
            counter,
            device,
        }
    }

    /// Produce the next local timestamp.
    ///
    /// `now_ms` normally moves the clock forward. If it did not (same
    /// millisecond, or the system clock went backwards), the counter advances
    /// instead — which is the whole point: time never appears to stand still
    /// or run back as far as ordering is concerned.
    pub fn tick(self, now_ms: u64) -> Self {
        if now_ms > self.wall_ms {
            Self {
                wall_ms: now_ms,
                counter: 0,
                device: self.device,
            }
        } else {
            let (wall_ms, counter) = step(self.wall_ms, self.counter);
            Self {
                wall_ms,
                counter,
                device: self.device,
            }
        }
    }

    /// Merge a timestamp received from another device into the local clock.
    ///
    /// A remote clock further ahead than [`MAX_DRIFT_MS`] is not allowed to
    /// drag the local one with it: a device whose clock says 2099 would
    /// otherwise make every device that ever synced with it claim 2099 too,
    /// and from then on every edit anywhere would only differ in the counter.
    /// The record itself still keeps the timestamp it arrived with, so
    /// ordering does not change — only what this device claims as *now*.
    pub fn merge(self, remote: Hlc, now_ms: u64) -> Self {
        let remote_wall = remote.wall_ms.min(now_ms.saturating_add(MAX_DRIFT_MS));
        let remote = Hlc {
            wall_ms: remote_wall,
            ..remote
        };
        let max_wall = now_ms.max(self.wall_ms).max(remote.wall_ms);

        let (wall_ms, counter) = if max_wall == self.wall_ms && max_wall == remote.wall_ms {
            step(max_wall, self.counter.max(remote.counter))
        } else if max_wall == self.wall_ms {
            step(max_wall, self.counter)
        } else if max_wall == remote.wall_ms {
            step(max_wall, remote.counter)
        } else {
            (max_wall, 0)
        };

        Self {
            wall_ms,
            counter,
            device: self.device,
        }
    }
}

/// The moment right after `(wall_ms, counter)`. A counter at its end — which
/// a record from another device can claim — moves on to the next millisecond
/// instead of wrapping to 0, which would sort before everything it follows.
fn step(wall_ms: u64, counter: u32) -> (u64, u32) {
    match counter.checked_add(1) {
        Some(counter) => (wall_ms, counter),
        None => (wall_ms.saturating_add(1), 0),
    }
}

impl Ord for Hlc {
    fn cmp(&self, other: &Self) -> Ordering {
        self.wall_ms
            .cmp(&other.wall_ms)
            .then(self.counter.cmp(&other.counter))
            .then(self.device.cmp(&other.device))
    }
}

impl PartialOrd for Hlc {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_advances_within_the_same_millisecond() {
        let a = Hlc::new(1_000, 0, 7);
        let b = a.tick(1_000);
        assert_eq!(b.wall_ms, 1_000);
        assert_eq!(b.counter, 1);
        assert!(b > a);
    }

    #[test]
    fn counter_resets_when_wall_time_moves_on() {
        let a = Hlc::new(1_000, 5, 7);
        let b = a.tick(1_001);
        assert_eq!(b.counter, 0);
        assert!(b > a);
    }

    #[test]
    fn a_backwards_system_clock_cannot_reorder_events() {
        let a = Hlc::new(5_000, 0, 7);
        // The laptop woke up and thinks it is earlier than it was.
        let b = a.tick(4_000);
        assert!(
            b > a,
            "an edit made later must never sort before an earlier one"
        );
    }

    #[test]
    fn a_device_whose_clock_says_2099_does_not_take_the_local_clock_with_it() {
        let now = 1_700_000_000_000;
        let local = Hlc::new(now, 0, 1);
        let absurd = Hlc::new(4_000_000_000_000, 0, 2);

        let merged = local.merge(absurd, now);
        assert!(
            merged.wall_ms <= now + MAX_DRIFT_MS,
            "the local clock stays in this decade: {}",
            merged.wall_ms
        );
        // The record that arrived still sorts after ours — only the clock is
        // capped, not the ordering.
        assert!(absurd > local);
    }

    #[test]
    fn a_counter_at_its_end_moves_on_instead_of_wrapping() {
        let now = 1_000;
        let local = Hlc {
            wall_ms: now,
            counter: 0,
            device: 1,
        };
        let remote = Hlc {
            wall_ms: now,
            counter: u32::MAX,
            device: 2,
        };
        let merged = local.merge(remote, now);
        assert!(merged > remote);
        let full = Hlc {
            wall_ms: now,
            counter: u32::MAX,
            device: 1,
        };
        assert!(full.tick(now) > full);
    }

    #[test]
    fn merge_dominates_both_sides() {
        let local = Hlc::new(1_000, 3, 1);
        let remote = Hlc::new(1_000, 9, 2);
        let merged = local.merge(remote, 1_000);
        assert!(merged > local);
        assert!(merged > remote);
    }
}
