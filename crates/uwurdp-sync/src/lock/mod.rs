//! Syncing through a UwULock Server instead of UwUSync.
//!
//! What stays the same is everything that decides what is true: the records,
//! their clocks, the merge, the manifests, the seal. UwULock keeps UwUSync's
//! record model for its suite vault (`docs/uwu-api.md` §6 of UwULock-Server),
//! so the engine runs unchanged against [`Lock`] as its [`crate::Transport`].
//! What changes is the transport, the login and where the key comes from:
//!
//! | | UwUSync | UwULock |
//! | --- | --- | --- |
//! | Login | challenge signed by the device key | the account's email, master password and two-step login, as a Bitwarden client ([`sign_in`]) |
//! | Key | vault key wrapped by the Argon2 master key | the space key, under the account's extras key |
//! | Pull / push | `/v1/records` | `/uwu/v1/suite/spaces/rdp/records` |
//! | Live | polling | the realtime channel ([`live`]) |
//! | New device | pairing | signing in with the account |
//!
//! The space key is 32 bytes for XChaCha20-Poly1305, like the UwUSync vault
//! key, and the space id takes the vault id's place in every record's
//! associated data — so a space is simply this device's vault with another key
//! (`uwurdp_store::lock`).
//!
//! And the move: [`moving`] copies an account from UwUSync to UwULock, checks
//! the copy, and only then does this device switch.

mod account;
mod api;
pub mod live;
pub mod moving;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod real;
#[cfg(test)]
mod tests;

pub use account::{
    known, send_email_code, sign_in, AcceptSpace, Known, SignIn, SignInOutcome, SignedIn,
    TwoFactorAnswer, TwoFactorMethod,
};
pub use api::{normalize_server, Lock, LockDevice};
pub use moving::{copy_to_lock, Difference, MoveReport, Problem};

use crate::engine::TransportError;
use uwurdp_store::StoreError;

/// Who this app is to a UwULock Server: the `client_id` of its login, which
/// names the one space its token may touch.
pub const CLIENT_ID: &str = "uwurdp";
/// The suite space this app's records live in.
pub const SPACE: &str = "rdp";
/// How this app is listed among the account's devices.
pub const DEVICE_NAME: &str = "UwURDP";

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// Email or master password refused, or a two-step code — in the
    /// server's own words, when it gave some.
    #[error("{0}")]
    WrongPassword(String),
    /// The account's extras key does not open any more: an official
    /// Bitwarden client replaced the account's key pair. Starting over is the
    /// web vault's business (it needs the full session this app never has).
    #[error("the account's key for UwU apps is lost; start over in the UwULock web vault")]
    KeysLost,
    /// The account never finished registration: no key pair to keep an
    /// extras key for.
    #[error("the account has no key pair yet; log in to the UwULock web vault once")]
    NoKeyPair,
    #[error("{0}")]
    Crypto(String),
    /// The server asks for a cheaper key derivation than this account's
    /// last sign-in used: nothing was derived or sent.
    #[error("refused: {0}")]
    WeakerKdf(String),
    /// The server lists a space this device used on the account before and
    /// moved on from — as a server would that rolls a rekey back. Nothing
    /// was taken or written.
    #[error("the server lists space {0}, which this device left for a newer one")]
    SpaceLeft(uuid::Uuid),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("the vault has to be unlocked first")]
    VaultLocked,
    /// The copy on UwULock is not what was read from UwUSync. Nothing
    /// switched.
    #[error("the copy on UwULock differs in {} records", .0.len())]
    MoveCheck(Vec<Difference>),
}

impl From<uwulock_core::Error> for LockError {
    fn from(error: uwulock_core::Error) -> Self {
        use uwulock_core::Error as E;
        match error {
            E::Network(message) => TransportError::Unreachable(message).into(),
            E::Refused(message) => Self::WrongPassword(message),
            E::SessionExpired => TransportError::SignIn("the session has expired".into()).into(),
            E::Server { message, .. } => TransportError::Refused(message).into(),
            other => Self::Crypto(other.to_string()),
        }
    }
}
