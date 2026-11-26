//! The UwURDP session engine: RDP sessions on [IronRDP], turned into a stream
//! of small binary messages a web page can paint.
//!
//! Like `uwussh-core`, this crate knows nothing about Tauri. The app
//! implements [`FrameSink`] over its IPC channel and calls [`SessionManager`];
//! the tests do the same over a `Vec`.
//!
//! # Data path
//!
//! ```text
//!   server ──TLS──▶ session task ──▶ DecodedImage (RGBA) ──▶ DirtyRegion
//!                        ▲                                       │ ≤ 1 per 16 ms,
//!                        │ fast-path input,                      │ < 2 unacked
//!                        │ resize, clipboard                     ▼
//!   page ──sync calls──▶ SessionManager ──unbounded channel──   FrameSink ──▶ page
//! ```
//!
//! 1. [`SessionManager::connect`] opens TCP, negotiates, upgrades to TLS
//!    (rustls with *ring*), checks the certificate pin **before** CredSSP,
//!    authenticates (NTLM over CredSSP when `nla`), and finishes the RDP
//!    connection sequence. Then it spawns one task per session.
//! 2. The task decodes server updates into an RGBA image (IronRDP's
//!    `ActiveStage`, or the graphics pipeline's output), records what changed, and every 16 ms at most sends the
//!    current pixels of the changed area — but only while fewer than two
//!    `BITMAPS` messages wait for the page's [`SessionManager::ack`]. Nothing
//!    is dropped: while waiting, changes keep accumulating.
//! 3. Input, resize, acks, clipboard and close requests are synchronous calls
//!    that only push onto an unbounded channel, so they work without a tokio
//!    context (Tauri's main thread).
//!
//! # Frame protocol
//!
//! One [`FrameSink::send`] = one message. Little-endian throughout; the first
//! byte is the kind (see [`frame`] for the encoder and exact bounds):
//!
//! | kind | message            | payload |
//! |------|--------------------|---------|
//! | 1    | `BITMAPS`          | u16 count; per rect u16 x, y, w, h, then w·h·4 bytes straight RGBA (A=255), row-major, no padding |
//! | 2    | `DESKTOP_SIZE`     | u16 width, u16 height — first after connecting and after every resize/reactivation, always followed by a full-screen `BITMAPS` |
//! | 3    | `POINTER_BITMAP`   | u16 hot_x, hot_y, w, h, then w·h·4 bytes straight RGBA |
//! | 4    | `POINTER_DEFAULT`  | — |
//! | 5    | `POINTER_HIDDEN`   | — |
//! | 6    | `POINTER_POSITION` | u16 x, u16 y |
//! | 7    | `CLOSED`           | UTF-8 JSON `{"reason":"logoff\|disconnect\|server\|error","message":"…"}`, sent once, right before [`FrameSink::finish`] |
//! | 8    | `CLIPBOARD`        | UTF-8 JSON `{"state":"downloading\|ready\|offered\|sent\|failed",…}`: files through the clipboard |
//!
//! `BITMAPS` count against the ack window; everything else goes out at once.
//! A single `BITMAPS` message is capped at [`frame::MAX_BITMAPS_BYTES`]
//! (32 MiB: a full 4K frame fits); bigger areas are split into horizontal
//! strips over several messages.
//!
//! # What is (not) supported
//!
//! - **NLA**: CredSSP with NTLM. Kerberos is not offered (it would need a KDC
//!   client); `nla = false` means TLS only, with the credentials in the logon
//!   packet.
//! - **Certificates**: trust on first use by SHA-256 fingerprint of the leaf
//!   certificate (`SHA256:<base64>`, like `ssh-keygen`). Handshake
//!   signatures are verified; the chain and host name are not.
//! - **Clipboard**: text, HTML, pictures and files both ways (`arboard`).
//! - **Drives** ([`SessionSettings::drives`]): local folders as
//!   `\\tsclient\<name>` over RDPDR, served by our own file system backend
//!   (`drive.rs`) with `std::fs`, every path confined to its shared folder.
//!   Change notifications are never answered, as in FreeRDP.
//! - **Audio**: with the `audio` feature (default), [`AudioMode::Local`]
//!   plays through cpal as PCM. `Remote` and `Off` both tell the server not
//!   to redirect audio: IronRDP cannot send the "leave it on the server" flag.
//! - **Graphics pipeline** (RDPEGFX, [`SessionSettings::graphics_pipeline`],
//!   on by default): what current Windows servers are fast with. Progressive,
//!   ClearCodec, planar, RemoteFX and uncompressed are decoded here; H.264
//!   (AVC420, AVC444 and AVC444v2) with the `h264` feature and Cisco's
//!   OpenH264 binary, which the app downloads and names in
//!   [`SessionSettings::h264_library`].
//! - **Resize**: through the DisplayControl channel; servers without it keep
//!   their size and the page scales. With the graphics pipeline the server
//!   answers with a `ResetGraphics`.
//! - **Admin/console session**: not requested yet (the cluster data block
//!   stays empty); `admin` is ignored.
//! - **RD Gateway** ([`RdpTarget::gateway`]): our own client for the
//!   gateway's WebSocket transport (MS-TSGU, Windows Server 2016 and later).
//!   The gateway's certificate is checked against the system's roots and
//!   host name, the login is NTLM with a channel binding to that
//!   certificate, and the target's port is the host's. Failures are
//!   [`RdpError::Gateway`]. Not done: the legacy two-connection HTTP
//!   transport, RPC over HTTP, smart card/PAA logins, re-authentication,
//!   and UDP through the gateway. (`ironrdp-mstsgu` 0.0.1 is not used: it
//!   skips the certificate check and only does Basic auth.)
//! - **UDP** ([`SessionSettings::udp`], on by default): on direct
//!   connections the client offers reliable UDP (multitransport with
//!   Soft-Sync); when the server asks, the dynamic channels (graphics
//!   pipeline above all) move onto RDP-UDP2 + TLS, pinned to the same
//!   certificate as TCP. Lossy UDP (DTLS) is not available in IronRDP.
//!   Without an answer within a few seconds the session stays on TCP.
//!
//! [IronRDP]: https://github.com/Devolutions/IronRDP

// Unit-test binaries link the dev-dependencies (see tests/session.rs).
#![cfg_attr(test, allow(linker_messages))]
// `RdpError` carries the observed certificate inline because that is the
// shape the app is written against; it is only ever returned once per
// connection attempt, so its size costs nothing that matters.
#![allow(clippy::result_large_err)]

#[cfg(feature = "audio")]
mod audio;
mod clipboard;
mod config;
mod connect;
pub mod dirty;
mod drive;
mod error;
pub mod frame;
mod gateway;
mod gfx;
mod input;
mod manager;
mod session;
mod sink;
mod tls;
mod transport;
mod udp;

pub use config::{AudioMode, DriveShare, GatewayTarget, RdpTarget, SessionSettings};
pub use connect::{CONNECT_TIMEOUT, HANDSHAKE_TIMEOUT};
pub use error::{ObservedCertificate, RdpError, SessionError};
pub use frame::CloseReason;
pub use input::{InputEvent, MouseButton};
pub use manager::SessionManager;
pub use session::{ACK_WINDOW, FLUSH_INTERVAL};
pub use sink::{FrameSink, SinkError};
pub use tls::certificate_fingerprint;

/// Identifies a session; serializes as the usual hyphenated string.
pub type SessionId = uuid::Uuid;
