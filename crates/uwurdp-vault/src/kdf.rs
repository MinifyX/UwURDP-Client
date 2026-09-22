//! Turning a master password into keys.

use crate::account::AccountKey;
use crate::{Result, VaultError};
use argon2::{Algorithm, Argon2, Block, Params, Version};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

/// 64 MiB. High enough to make offline guessing expensive, low enough that a
/// phone can still unlock the vault — which matters, because a KDF nobody can
/// run on their second device gets lowered until it is useless.
pub const KDF_MEMORY_KIB: u32 = 64 * 1024;
pub const KDF_TIME_COST: u32 = 3;
pub const KDF_PARALLELISM: u32 = 4;

/// The two secrets a master password produces.
///
/// They come from one 64-byte derivation, split in half. Knowing the auth
/// secret tells you nothing about the master key, so handing the server the
/// former never risks the latter.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct MasterSecrets {
    /// Wraps the vault key. Never leaves the device, never touches the network.
    pub master_key: [u8; 32],
    /// Hashed again and sent to the server as proof of who you are.
    pub auth_secret: [u8; 32],
}

impl std::fmt::Debug for MasterSecrets {
    /// Never print key material, not even by accident in a log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterSecrets(redacted)")
    }
}

/// Argon2id's cost parameters. Stored with each vault, so raising the defaults
/// later does not lock anyone out of a vault created with the old ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    pub memory_kib: u32,
    pub time_cost: u32,
    pub parallelism: u32,
}

impl KdfParams {
    pub const RECOMMENDED: Self = Self {
        memory_kib: KDF_MEMORY_KIB,
        time_cost: KDF_TIME_COST,
        parallelism: KDF_PARALLELISM,
    };

    /// Whether these costs are something a device can actually run. A header
    /// is data like any other; a tampered one asking for 4 TiB of memory must
    /// be refused rather than tried.
    pub fn within_limits(&self) -> bool {
        (1..=1024 * 1024).contains(&self.memory_kib)
            && (1..=16).contains(&self.time_cost)
            && (1..=16).contains(&self.parallelism)
    }

    /// The least a password that stands on its own may be derived with: the
    /// OWASP floor for Argon2id. Costs that arrive from somewhere else — a
    /// server handing a joining device its vault's parameters — are checked
    /// against it, so a hostile server cannot ask for a login key made cheap
    /// enough to guess the password from offline.
    pub const FLOOR: Self = Self {
        memory_kib: 19 * 1024,
        time_cost: 2,
        parallelism: 1,
    };

    /// Whether guessing a password through these costs is expensive enough.
    pub fn strong_enough(&self) -> bool {
        self.memory_kib >= Self::FLOOR.memory_kib && self.time_cost >= Self::FLOOR.time_cost
    }

    /// For tests only: fast, and worthless against guessing.
    pub const INSECURE_FOR_TESTS: Self = Self {
        memory_kib: 8,
        time_cost: 1,
        parallelism: 1,
    };
}

/// Derive both secrets from the master password.
///
/// `salt` must be at least 8 bytes and is stored with the vault — it is not
/// secret, it just has to be unique per vault.
pub fn derive_master_secrets(password: &[u8], salt: &[u8]) -> Result<MasterSecrets> {
    derive_master_secrets_with(password, salt, KdfParams::RECOMMENDED)
}

/// The same, with the [`AccountKey`] a synced vault mixes in.
///
/// Argon2id does the work of making a guess expensive; the account key decides
/// whether guessing is worth attempting at all. It goes in through HKDF
/// afterwards rather than into Argon2's own salt, so a vault that gains an
/// account key later keeps its salt, its parameters and its records — only the
/// wrapped key is written again.
pub fn derive_master_secrets_for(
    password: &[u8],
    salt: &[u8],
    kdf: KdfParams,
    account_key: Option<&AccountKey>,
) -> Result<MasterSecrets> {
    let secrets = derive_master_secrets_with(password, salt, kdf)?;
    let Some(account_key) = account_key else {
        return Ok(secrets);
    };

    let mut material = Zeroizing::new([0u8; 64]);
    material[..32].copy_from_slice(&secrets.master_key);
    material[32..].copy_from_slice(&secrets.auth_secret);

    let mut out = Zeroizing::new([0u8; 64]);
    Hkdf::<Sha256>::new(Some(account_key.as_bytes()), material.as_ref())
        .expand(ACCOUNT_INFO, out.as_mut())
        .map_err(|_| VaultError::Kdf("could not mix in the account key".into()))?;

    let mut master_key = [0u8; 32];
    let mut auth_secret = [0u8; 32];
    master_key.copy_from_slice(&out[..32]);
    auth_secret.copy_from_slice(&out[32..]);
    Ok(MasterSecrets {
        master_key,
        auth_secret,
    })
}

/// What the server is told, derived from the auth secret so that what it
/// stores is two steps away from anything that opens the vault.
pub fn server_auth_key(secrets: &MasterSecrets) -> Result<Zeroizing<[u8; 32]>> {
    let mut key = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(None, &secrets.auth_secret)
        .expand(AUTH_INFO, key.as_mut())
        .map_err(|_| VaultError::Kdf("could not derive the login key".into()))?;
    Ok(key)
}

const ACCOUNT_INFO: &[u8] = b"uwurdp/master/v2";
const AUTH_INFO: &[u8] = b"uwussh/server-auth/v1";

pub fn derive_master_secrets_with(
    password: &[u8],
    salt: &[u8],
    kdf: KdfParams,
) -> Result<MasterSecrets> {
    if !kdf.within_limits() {
        return Err(VaultError::Kdf("unreasonable key derivation costs".into()));
    }
    let params = Params::new(kdf.memory_kib, kdf.time_cost, kdf.parallelism, Some(64))
        .map_err(|e| VaultError::Kdf(e.to_string()))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params.clone());

    // Argon2's working memory, owned here so it can be wiped: its last blocks
    // are enough to recompute the key.
    let mut memory = vec![Block::new(); params.block_count()];
    let mut out = [0u8; 64];
    let hashed = argon.hash_password_into_with_memory(password, salt, &mut out, &mut memory);
    memory.zeroize();
    hashed.map_err(|e| VaultError::Kdf(e.to_string()))?;

    let mut master_key = [0u8; 32];
    let mut auth_secret = [0u8; 32];
    master_key.copy_from_slice(&out[..32]);
    auth_secret.copy_from_slice(&out[32..]);
    out.zeroize();

    Ok(MasterSecrets {
        master_key,
        auth_secret,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // A cheap parameter set, because the real one takes 64 MiB per call and
    // these tests run on every commit.
    fn fast(password: &[u8], salt: &[u8]) -> [u8; 64] {
        let params = Params::new(8, 1, 1, Some(64)).unwrap();
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut out = [0u8; 64];
        argon.hash_password_into(password, salt, &mut out).unwrap();
        out
    }

    #[test]
    fn the_two_halves_are_different() {
        let out = fast(b"correct horse battery staple", b"uwurdp-salt-1234");
        assert_ne!(
            &out[..32],
            &out[32..],
            "master key and auth secret must not coincide"
        );
    }

    #[test]
    fn a_different_salt_gives_different_keys() {
        let a = fast(b"same password", b"salt-one-aaaaaaa");
        let b = fast(b"same password", b"salt-two-bbbbbbb");
        assert_ne!(a, b);
    }

    #[test]
    fn derivation_is_deterministic() {
        let a = fast(b"pw", b"salt-one-aaaaaaa");
        let b = fast(b"pw", b"salt-one-aaaaaaa");
        assert_eq!(a, b, "unlocking on a second device depends on this");
    }

    #[test]
    fn the_defaults_clear_the_floor_and_the_test_costs_do_not() {
        assert!(KdfParams::RECOMMENDED.strong_enough());
        assert!(KdfParams::FLOOR.strong_enough());
        assert!(!KdfParams::INSECURE_FOR_TESTS.strong_enough());
        let one_pass = KdfParams {
            time_cost: 1,
            ..KdfParams::RECOMMENDED
        };
        assert!(!one_pass.strong_enough());
    }

    #[test]
    fn absurd_costs_are_refused_before_anything_is_allocated() {
        let greedy = KdfParams {
            memory_kib: u32::MAX,
            ..KdfParams::RECOMMENDED
        };
        assert!(!greedy.within_limits());
        assert!(derive_master_secrets_with(b"pw", b"salt-one-aaaaaaa", greedy).is_err());
        assert!(KdfParams::RECOMMENDED.within_limits());
        assert!(KdfParams::INSECURE_FOR_TESTS.within_limits());
    }

    #[test]
    fn the_account_key_changes_everything_the_password_alone_would_give() {
        let salt = b"salt-one-aaaaaaa";
        let fast = KdfParams::INSECURE_FOR_TESTS;
        let one = AccountKey::from_bytes([1u8; 16]);
        let two = AccountKey::from_bytes([2u8; 16]);

        let without = derive_master_secrets_for(b"pw", salt, fast, None).unwrap();
        let with_one = derive_master_secrets_for(b"pw", salt, fast, Some(&one)).unwrap();
        let with_one_again = derive_master_secrets_for(b"pw", salt, fast, Some(&one)).unwrap();
        let with_two = derive_master_secrets_for(b"pw", salt, fast, Some(&two)).unwrap();

        assert_eq!(
            with_one.master_key, with_one_again.master_key,
            "the same inputs must open the same vault on another device"
        );
        assert_ne!(
            without.master_key, with_one.master_key,
            "the password alone is not the key any more"
        );
        assert_ne!(
            with_one.master_key, with_two.master_key,
            "and whoever has the wrong account key has nothing"
        );
        assert_ne!(with_one.master_key, with_one.auth_secret);
    }

    #[test]
    fn what_the_server_learns_says_nothing_about_the_vault() {
        let secrets = derive_master_secrets_for(
            b"pw",
            b"salt-one-aaaaaaa",
            KdfParams::INSECURE_FOR_TESTS,
            None,
        )
        .unwrap();
        let login = server_auth_key(&secrets).unwrap();
        assert_ne!(login.as_slice(), secrets.master_key);
        assert_ne!(login.as_slice(), secrets.auth_secret);
        assert_eq!(
            server_auth_key(&secrets).unwrap().as_slice(),
            login.as_slice(),
            "but it is the same every time this device logs in"
        );
    }

    #[test]
    fn real_parameters_produce_two_keys() {
        let secrets = derive_master_secrets(b"pw", b"salt-one-aaaaaaa").expect("derive");
        assert_ne!(secrets.master_key, secrets.auth_secret);
    }
}
