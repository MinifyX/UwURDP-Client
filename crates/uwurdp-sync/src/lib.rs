//! Offline-first sync.
//!
//! The rule that shapes this crate: **local writes never wait for the
//! network**. Every change lands in SQLite first and marks its row as waiting
//! in the same transaction; this crate is the background job that drains those
//! rows to a server and brings back what other devices wrote. If the server is
//! down, or you are on a train, nothing about using the app changes.
//!
//! What lives where:
//!
//! - [`uwurdp_proto`] holds the wire format, the clock and the conflict rule —
//!   the three things client and server must agree on exactly.
//! - `uwurdp_store` seals records on the way out, opens them on the way in,
//!   and decides per record who wins, inside the transaction that writes it.
//! - [`engine`] is one pass: pull, then push, then round again if the server
//!   reported a conflict.
//! - [`memory`] is the server's rules as running code, so two devices can be
//!   held against them without a network.
//!
//! - [`http`] is the real transport: one blocking request at a time, against a
//!   server whose certificate this device pinned ([`pin`]).

pub mod engine;
pub mod flow;
pub mod http;
pub mod memory;
pub mod pairing;
pub mod pin;

pub use engine::{sync_once, SyncError, SyncReport, Transport, TransportError, MAX_ROUNDS};
pub use flow::{create_account, join, offer_pairing, parse_setup, reconnect, FlowError, Setup};
pub use http::{from_wire, to_wire, Server};
pub use memory::MemoryServer;
pub use uwurdp_proto::api;

#[cfg(test)]
mod tests;
