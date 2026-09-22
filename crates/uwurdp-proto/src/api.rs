//! What the server's HTTP surface speaks.
//!
//! These types live next to the wire format for records rather than on either
//! side of it, for the same reason: a field named `authKey` here and
//! `auth_key` there is a bug nobody notices until pairing fails on a stranger's
//! NAS. Both halves deserialize the same structs.
//!
//! Bytes travel as base64 without padding, because a JSON body is something
//! people read while debugging. Nothing in here is secret except the two keys
//! a device proves itself with, and those only ever go out over TLS.

use crate::entities::Extra;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Whether the server is alive, and what it speaks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub ok: bool,
    pub schema: u32,
}

/// The vault header as it travels: the same salt, costs and wrapped key the
/// device keeps, with the bytes in base64.
///
/// None of it is secret and all of it is useless without the master password —
/// and, when `needs_account_key` is set, without the account key as well.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireVault {
    pub vault_id: Uuid,
    pub kdf_memory_kib: u32,
    pub kdf_time_cost: u32,
    pub kdf_parallelism: u32,
    pub salt: String,
    pub wrapped_nonce: String,
    pub wrapped_blob: String,
    /// Whether opening this vault also needs the account key. A joining device
    /// is told, so it can say "the kit is missing" instead of "wrong
    /// password".
    #[serde(default)]
    pub needs_account_key: bool,
    /// Room for a later build to add a field without an older one dropping it.
    #[serde(flatten, default, skip_serializing_if = "Extra::is_empty")]
    pub extra: Extra,
}

/// What a device joining an account may see before it has proved anything:
/// enough to turn a master password into keys, and nothing it could attack
/// offline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireVaultParams {
    pub vault_id: Uuid,
    pub kdf_memory_kib: u32,
    pub kdf_time_cost: u32,
    pub kdf_parallelism: u32,
    pub salt: String,
    #[serde(default)]
    pub needs_account_key: bool,
}

impl WireVault {
    pub fn params(&self) -> WireVaultParams {
        WireVaultParams {
            vault_id: self.vault_id,
            kdf_memory_kib: self.kdf_memory_kib,
            kdf_time_cost: self.kdf_time_cost,
            kdf_parallelism: self.kdf_parallelism,
            salt: self.salt.clone(),
            needs_account_key: self.needs_account_key,
        }
    }
}

/// A device on its way in: what to call it, and the key it will sign with.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewDevice {
    #[serde(default)]
    pub name: String,
    /// Ed25519, 32 bytes, base64.
    pub public_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAccount {
    #[serde(default)]
    pub invite: String,
    pub vault: WireVault,
    /// Derived from the master password and the account key. The server keeps
    /// only its hash.
    pub auth_key: String,
    pub device: NewDevice,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrolDevice {
    /// The one-time token the device that is already in handed over through
    /// the pairing channel.
    pub enrolment: String,
    pub auth_key: String,
    pub device: NewDevice,
}

/// What a device gets when it is let in.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Admitted {
    pub account_id: Uuid,
    pub device_id: Uuid,
    pub token: String,
    pub expires_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangePassword {
    /// Proof that whoever asks knows the password being replaced: a device
    /// token alone must not be able to lock everyone else out.
    pub current_auth_key: String,
    pub vault: WireVault,
    pub auth_key: String,
}

/// Shutting a device out.
///
/// A device may always take itself out. Taking out **another** one needs the
/// same proof a password change does: whoever holds a stolen laptop has its
/// token, and a token alone must not be enough to lock the owner's other
/// devices out, one after the other.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevokeDevice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_auth_key: Option<String>,
}

/// What a joining device asks for the vault's parameters with. In a body and
/// not in the address, because addresses end up in the logs of every proxy
/// in between.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultParamsRequest {
    pub enrolment: String,
}

/// The header a joining device claims side `b` of a pairing session with: a
/// random secret it makes up and sends with every request. The first one to
/// arrive holds the side, so nobody who merely guessed the session id can
/// post as the joining device or read in its place. Side `a` is the device
/// that opened the session, and it proves that with its token.
pub const PAIR_CLAIM_HEADER: &str = "x-uwussh-pair-claim";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeRequest {
    pub device_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeResponse {
    pub challenge: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginRequest {
    pub device_id: Uuid,
    /// The challenge, signed with the device's key.
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginResponse {
    pub account_id: Uuid,
    pub token: String,
    pub expires_ms: u64,
}

/// A device as the list shows it. No key material: this is for people.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSummary {
    pub id: Uuid,
    pub name: String,
    pub created_ms: u64,
    pub last_seen_ms: Option<u64>,
    pub revoked_ms: Option<u64>,
}

/// A one-time token for a device about to join.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrolmentToken {
    pub token: String,
    pub expires_ms: u64,
}

/// A pairing session, as the server names it. The words that go with the id
/// are the SPAKE2 password and never reach the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairOpened {
    pub id: String,
    pub expires_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairMessage {
    /// `a` for the device that opened the session, `b` for the one joining.
    pub side: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairMessages {
    pub messages: Vec<String>,
    /// What to pass as `after` next time.
    pub next: usize,
}

/// What a device signs to prove that it is itself.
///
/// The label keeps the signature from meaning anything anywhere else, and the
/// two ids tie it to one device of one account — so a signature made for one
/// server cannot be replayed at another account, or for another device.
///
/// It lives here because both halves have to build the same bytes: a client
/// that signs something slightly different is a client that cannot log in, and
/// the two would be debugged separately for an afternoon.
pub fn session_material(account: Uuid, device: Uuid, challenge: &[u8]) -> Vec<u8> {
    let mut material = Vec::with_capacity(SESSION_LABEL.len() + 16 + 16 + challenge.len());
    material.extend_from_slice(SESSION_LABEL);
    material.extend_from_slice(account.as_bytes());
    material.extend_from_slice(device.as_bytes());
    material.extend_from_slice(challenge);
    material
}

const SESSION_LABEL: &[u8] = b"uwussh/session/v1";

/// What the server says when it refuses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    /// A short word for the client to act on: `unauthorized`, `schema`,
    /// `rate-limited`, …
    pub error: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_names_on_the_wire_are_the_ones_the_server_writes() {
        let json = serde_json::to_value(&CreateAccount {
            invite: "ABCDE".into(),
            vault: WireVault {
                vault_id: Uuid::nil(),
                kdf_memory_kib: 65536,
                kdf_time_cost: 3,
                kdf_parallelism: 4,
                salt: "c2FsdA".into(),
                wrapped_nonce: "bm9uY2U".into(),
                wrapped_blob: "d3JhcA".into(),
                needs_account_key: true,
                extra: Extra::new(),
            },
            auth_key: "a2V5".into(),
            device: NewDevice {
                name: "LVDesk1".into(),
                public_key: "cHVi".into(),
            },
        })
        .unwrap();

        assert_eq!(json["authKey"], "a2V5");
        assert_eq!(json["vault"]["kdfMemoryKib"], 65536);
        assert_eq!(json["vault"]["wrappedBlob"], "d3JhcA");
        assert_eq!(json["vault"]["needsAccountKey"], true);
        assert_eq!(json["device"]["publicKey"], "cHVi");
    }

    #[test]
    fn a_header_from_a_server_that_says_nothing_about_the_account_key_needs_none() {
        let header: WireVault = serde_json::from_str(
            r#"{"vaultId":"00000000-0000-0000-0000-000000000000","kdfMemoryKib":65536,
                "kdfTimeCost":3,"kdfParallelism":4,"salt":"c2FsdA","wrappedNonce":"bm9uY2U",
                "wrappedBlob":"d3JhcA"}"#,
        )
        .unwrap();
        assert!(!header.needs_account_key);
        assert!(header.params().salt == "c2FsdA");
    }

    #[test]
    fn what_is_signed_names_exactly_one_device_of_one_account() {
        let account = Uuid::from_u128(1);
        let device = Uuid::from_u128(2);
        let challenge = [7u8; 32];
        let material = session_material(account, device, &challenge);

        assert!(material.starts_with(b"uwussh/session/v1"));
        assert_eq!(material.len(), 17 + 16 + 16 + 32);
        assert_ne!(material, session_material(device, account, &challenge));
        assert_ne!(material, session_material(account, device, &[8u8; 32]));
    }

    #[test]
    fn the_parameters_a_joining_device_sees_hold_no_wrapped_key() {
        let params = WireVaultParams {
            vault_id: Uuid::nil(),
            kdf_memory_kib: 8,
            kdf_time_cost: 1,
            kdf_parallelism: 1,
            salt: "c2FsdA".into(),
            needs_account_key: true,
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(!json.contains("wrapped"), "{json}");
        assert!(json.contains("needsAccountKey"));
    }
}
