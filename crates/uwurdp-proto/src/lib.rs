//! Types shared by the UwURDP client and the sync server.
//!
//! This crate is the single definition of what a host, an identity or a key
//! *is*, and of what travels between a device and its server.
//!
//! UwURDP syncs through the same server as UwUSSH — an
//! [UwUSSH-Server](https://github.com/MinifyX/UwUSSH-Server), with an account of
//! its own. So [`api`] and [`sync`] are UwUSSH's wire format, byte for byte,
//! and must stay that way: the server depends on `uwussh-proto` from the
//! UwUSSH-Client repository, not on this crate. The payloads in [`entities`]
//! are free to differ; the server only ever sees them sealed.
//!
//! Nothing in here touches the network, the database or the filesystem — it is
//! pure data and the two decisions that must be identical on both sides of the
//! wire: how records are ordered ([`clock`]) and who wins a conflict
//! ([`merge`]).

pub mod api;
pub mod clock;
pub mod entities;
pub mod manifest;
pub mod merge;
pub mod sync;

pub use api::{Admitted, NewDevice, WireVault, WireVaultParams};
pub use clock::{Hlc, MAX_DRIFT_MS};
pub use entities::*;
pub use manifest::{Manifest, ManifestEntry, MAX_MANIFEST_ENTRIES};
pub use merge::{resolve, Resolution, Version};
pub use sync::{
    Accepted, Envelope, PullResponse, PushRequest, PushResponse, SyncCursor, MAX_BATCH,
    MAX_BATCH_BYTES, MAX_BLOB_BYTES,
};

/// Bumped whenever the wire format changes in a way older peers cannot read.
/// The server rejects envelopes carrying a schema it does not know.
///
/// 1 → 2: the header of a record is authenticated along with its payload, the
/// version of a record is the server's sequence number rather than a
/// device-local counter, and payloads carry only the fields that mean
/// something on another device.
pub const SCHEMA_VERSION: u32 = 2;
