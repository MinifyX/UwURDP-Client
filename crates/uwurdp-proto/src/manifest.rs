//! What a device holds, as a record of its own.
//!
//! Every record is sealed with its own header, so a server cannot forge or
//! alter one. What it *can* do is leave some out: hand a newly joined device an
//! old version of a host key and never mention the newer one, or keep a delete
//! to itself. Nothing in a single record can tell a device that there should
//! have been more.
//!
//! A manifest can. Each device publishes one — sealed like any other record,
//! under an id derived from the vault and the device — listing the id and
//! version of every record it holds that the server has confirmed. A device
//! that has pulled to the end checks every manifest it holds against what it
//! has: a listed version it is missing, or only has an older version of, is
//! something the server kept back.
//!
//! The payload is binary rather than JSON: a vault of a few thousand records
//! would otherwise run into [`MAX_BLOB_BYTES`](crate::MAX_BLOB_BYTES). Entries
//! carry the kind as its raw discriminant, so a kind added by a newer build
//! survives a round trip here and is simply not checked.

use crate::clock::Hlc;
use crate::entities::EntityKind;
use uuid::Uuid;

/// The most entries one manifest lists. 34 bytes each, plus a small header and
/// the seal's tag, stays below the 256 KiB a record may have. A vault larger
/// than that publishes a partial manifest — the kinds that matter most first,
/// see the store — rather than none at all.
pub const MAX_MANIFEST_ENTRIES: usize = 7_000;

const MAGIC: &[u8; 3] = b"UWM";
const VERSION: u8 = 1;
const HEADER_BYTES: usize = 4 + 4 + 16 + 1 + 4;
const ENTRY_BYTES: usize = 1 + 1 + 16 + 16;

/// One record as the publishing device holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ManifestEntry {
    /// The kind's discriminant. Kept raw: one this build does not know is
    /// carried, not refused.
    pub kind: u8,
    pub id: Uuid,
    pub updated_at: Hlc,
    pub deleted: bool,
}

impl ManifestEntry {
    pub fn new(kind: EntityKind, id: Uuid, updated_at: Hlc, deleted: bool) -> Self {
        Self {
            kind: kind as u8,
            id,
            updated_at,
            deleted,
        }
    }

    /// The kind, if this build knows it.
    pub fn kind(&self) -> Option<EntityKind> {
        EntityKind::from_discriminant(self.kind)
    }
}

/// Everything one device holds, at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// The device's clock id — the `device` of the clocks it writes.
    pub device: u32,
    /// The device's clock when it wrote this manifest.
    pub created_at: Hlc,
    /// True when the vault had more records than a manifest may list, and
    /// some were left out.
    pub partial: bool,
    pub entries: Vec<ManifestEntry>,
}

impl Manifest {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_BYTES + self.entries.len() * ENTRY_BYTES);
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&self.device.to_be_bytes());
        push_clock(&mut out, self.created_at);
        out.push(u8::from(self.partial));
        out.extend_from_slice(&(self.entries.len() as u32).to_be_bytes());
        for entry in &self.entries {
            out.push(entry.kind);
            out.push(u8::from(entry.deleted));
            out.extend_from_slice(entry.id.as_bytes());
            push_clock(&mut out, entry.updated_at);
        }
        out
    }

    /// `None` for anything that is not exactly a manifest this build can read:
    /// another version, a wrong length, trailing bytes.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < HEADER_BYTES || &bytes[..3] != MAGIC || bytes[3] != VERSION {
            return None;
        }
        let device = u32::from_be_bytes(bytes[4..8].try_into().ok()?);
        let created_at = read_clock(&bytes[8..24])?;
        let partial = match bytes[24] {
            0 => false,
            1 => true,
            _ => return None,
        };
        let count = u32::from_be_bytes(bytes[25..29].try_into().ok()?) as usize;
        let body = &bytes[HEADER_BYTES..];
        if count > MAX_MANIFEST_ENTRIES || body.len() != count.checked_mul(ENTRY_BYTES)? {
            return None;
        }
        let mut entries = Vec::with_capacity(count);
        for chunk in body.as_chunks::<ENTRY_BYTES>().0 {
            let deleted = match chunk[1] {
                0 => false,
                1 => true,
                _ => return None,
            };
            entries.push(ManifestEntry {
                kind: chunk[0],
                id: Uuid::from_bytes(chunk[2..18].try_into().ok()?),
                updated_at: read_clock(&chunk[18..34])?,
                deleted,
            });
        }
        Some(Self {
            device,
            created_at,
            partial,
            entries,
        })
    }
}

fn push_clock(out: &mut Vec<u8>, clock: Hlc) {
    out.extend_from_slice(&clock.wall_ms.to_be_bytes());
    out.extend_from_slice(&clock.counter.to_be_bytes());
    out.extend_from_slice(&clock.device.to_be_bytes());
}

fn read_clock(bytes: &[u8]) -> Option<Hlc> {
    Some(Hlc::new(
        u64::from_be_bytes(bytes[0..8].try_into().ok()?),
        u32::from_be_bytes(bytes[8..12].try_into().ok()?),
        u32::from_be_bytes(bytes[12..16].try_into().ok()?),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(count: usize) -> Manifest {
        Manifest {
            device: 7,
            created_at: Hlc::new(1_700_000_000_000, 3, 7),
            partial: false,
            entries: (0..count)
                .map(|index| ManifestEntry {
                    kind: (index % 12) as u8,
                    id: Uuid::from_u128(index as u128 + 1),
                    updated_at: Hlc::new(1_700_000_000_000 + index as u64, 1, 9),
                    deleted: index % 3 == 0,
                })
                .collect(),
        }
    }

    #[test]
    fn a_manifest_round_trips_kinds_this_build_does_not_know_included() {
        let manifest = sample(40);
        let back = Manifest::decode(&manifest.encode()).unwrap();
        assert_eq!(back, manifest);
        assert!(back.entries.iter().any(|entry| entry.kind().is_none()));
    }

    #[test]
    fn the_largest_manifest_still_fits_in_one_record() {
        let manifest = sample(MAX_MANIFEST_ENTRIES);
        // The seal adds a 16-byte tag.
        assert!(manifest.encode().len() + 16 <= crate::MAX_BLOB_BYTES);
    }

    #[test]
    fn anything_but_an_exact_manifest_is_refused() {
        let bytes = sample(3).encode();
        assert!(Manifest::decode(&bytes[..bytes.len() - 1]).is_none());
        assert!(Manifest::decode(&[bytes.as_slice(), &[0]].concat()).is_none());
        let mut other_version = bytes.clone();
        other_version[3] = 2;
        assert!(Manifest::decode(&other_version).is_none());
        assert!(Manifest::decode(b"").is_none());
        assert!(Manifest::decode(b"{\"entries\":[]}").is_none());
    }
}
