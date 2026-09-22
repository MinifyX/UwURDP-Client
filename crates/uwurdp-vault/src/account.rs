//! The account key: 128 bits that never reach the server.
//!
//! The server has to store the wrapped vault key — otherwise a second device
//! could never get it. That makes the server's database the one place where
//! guessing the master password would pay off: Argon2id makes each guess
//! expensive, but a weak password would still fall. So the key derivation takes
//! a second input that the server never sees.
//!
//! This is 1Password's model, and its cost is stated plainly rather than hidden:
//! **losing every device and the recovery kit means losing the vault.** In
//! exchange, a stolen server database is worth nothing at all.
//!
//! Where it lives: on every paired device (sealed by the operating system, next
//! to nothing else) and in the recovery kit, on paper, in the form below.

use crate::{Result, VaultError};
use rand::RngCore;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Crockford's base32: no letters that can be read as digits (`I`, `L`, `O`)
/// and no `U`, so nothing accidental is ever spelled out.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// 26 characters carry the key, two more catch a typo.
const KEY_CHARS: usize = 26;
const CHECK_CHARS: usize = 2;
/// Four groups of seven, which is what a printed kit reads best as.
const GROUP: usize = 7;

#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct AccountKey([u8; 16]);

impl std::fmt::Debug for AccountKey {
    /// Never printed, not even by a stray `{:?}` in a log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccountKey(redacted)")
    }
}

impl AccountKey {
    /// A new one, from the operating system's randomness.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// How it appears in the recovery kit: four groups of seven characters.
    pub fn to_code(&self) -> String {
        let mut symbols = encode(&self.0);
        symbols.extend_from_slice(&checksum(&self.0));
        let mut code = String::with_capacity(symbols.len() + 3);
        for (index, symbol) in symbols.iter().enumerate() {
            if index > 0 && index % GROUP == 0 {
                code.push('-');
            }
            code.push(ALPHABET[*symbol as usize] as char);
        }
        code
    }

    /// The other direction, forgiving about how it was typed: lower case is
    /// fine, so are spaces and dashes, and so are the three letters people
    /// write for `1` and `0`.
    pub fn from_code(code: &str) -> Result<Self> {
        let symbols: Vec<u8> = code
            .bytes()
            .filter(|byte| !matches!(byte, b'-' | b' ' | b'\t' | b'\r' | b'\n' | b'_'))
            .map(normalise)
            .collect::<Option<Vec<u8>>>()
            .ok_or_else(|| VaultError::Kdf("that is not an account key".into()))?;

        if symbols.len() != KEY_CHARS + CHECK_CHARS {
            return Err(VaultError::Kdf(format!(
                "an account key is {} characters, that one is {}",
                KEY_CHARS + CHECK_CHARS,
                symbols.len()
            )));
        }
        let bytes = decode(&symbols[..KEY_CHARS])
            .ok_or_else(|| VaultError::Kdf("that is not an account key".into()))?;
        if symbols[KEY_CHARS..] != checksum(&bytes) {
            // A mistyped key and a wrong password would both fail to unlock,
            // and the two are worth telling apart.
            return Err(VaultError::Kdf(
                "that account key has a typo in it somewhere".into(),
            ));
        }
        Ok(Self(bytes))
    }
}

/// One symbol as the alphabet's index, taking the letters people substitute.
fn normalise(byte: u8) -> Option<u8> {
    let upper = byte.to_ascii_uppercase();
    let upper = match upper {
        b'I' | b'L' => b'1',
        b'O' => b'0',
        other => other,
    };
    ALPHABET
        .iter()
        .position(|symbol| *symbol == upper)
        .map(|index| index as u8)
}

/// 128 bits as 26 five-bit symbols, the last one carrying two spare zeroes.
/// Symbols are alphabet indices here; they become letters on the way out.
fn encode(bytes: &[u8; 16]) -> Vec<u8> {
    let mut symbols = Vec::with_capacity(KEY_CHARS);
    let mut accumulator = 0u16;
    let mut bits = 0u32;
    for byte in bytes {
        accumulator = accumulator << 8 | u16::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            symbols.push(((accumulator >> bits) & 0b1_1111) as u8);
        }
    }
    if bits > 0 {
        symbols.push(((accumulator << (5 - bits)) & 0b1_1111) as u8);
    }
    symbols
}

/// The other way: 26 alphabet indices back into 16 bytes.
fn decode(indices: &[u8]) -> Option<[u8; 16]> {
    let mut bytes = Vec::with_capacity(16);
    let mut accumulator = 0u16;
    let mut bits = 0u32;
    for index in indices {
        accumulator = accumulator << 5 | u16::from(*index);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            bytes.push((accumulator >> bits) as u8);
        }
    }
    bytes.try_into().ok()
}

/// Ten bits over the key, so a typo is caught before it is blamed on the
/// master password.
fn checksum(bytes: &[u8; 16]) -> [u8; CHECK_CHARS] {
    use sha2::{Digest, Sha256};
    let digest = Sha256::new()
        .chain_update(b"uwurdp/account-key/check/v1")
        .chain_update(bytes)
        .finalize();
    [digest[0] & 0b1_1111, digest[1] & 0b1_1111]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_survives_the_trip_to_paper_and_back() {
        let key = AccountKey::generate();
        let code = key.to_code();
        assert_eq!(code.len(), KEY_CHARS + CHECK_CHARS + 3, "{code}");
        assert_eq!(code.matches('-').count(), 3);
        assert_eq!(AccountKey::from_code(&code).unwrap(), key);
    }

    #[test]
    fn it_is_taken_as_it_was_typed() {
        let key = AccountKey::from_bytes([0x5a; 16]);
        let code = key.to_code();
        for typed in [
            code.to_lowercase(),
            code.replace('-', " "),
            code.replace('-', ""),
            format!(" {code}\n"),
        ] {
            assert_eq!(AccountKey::from_code(&typed).unwrap(), key, "{typed}");
        }
    }

    #[test]
    fn the_letters_people_write_for_one_and_zero_are_understood() {
        let key = AccountKey::from_bytes([0u8; 16]);
        let code = key.to_code();
        // Zeroes read as O, which is not in the alphabet at all.
        let as_letters = code.replace('0', "O").replace('1', "I");
        assert_eq!(AccountKey::from_code(&as_letters).unwrap(), key);
    }

    #[test]
    fn a_typo_is_caught_and_named() {
        let key = AccountKey::generate();
        let code = key.to_code();
        // Swap two neighbouring symbols, the classic typing mistake.
        let mut symbols: Vec<char> = code.chars().filter(|c| *c != '-').collect();
        symbols.swap(3, 4);
        let typo: String = symbols.into_iter().collect();
        if typo != code.replace('-', "") {
            let error = AccountKey::from_code(&typo).expect_err("a typo must not pass");
            assert!(format!("{error}").contains("typo"), "{error}");
        }

        assert!(AccountKey::from_code("too short").is_err());
        assert!(AccountKey::from_code(&format!("{code}Z")).is_err());
        assert!(AccountKey::from_code("!!!!!!!-!!!!!!!-!!!!!!!-!!!!!!!").is_err());
    }

    #[test]
    fn two_keys_are_not_the_same_key() {
        assert_ne!(AccountKey::generate(), AccountKey::generate());
    }

    #[test]
    fn the_key_never_shows_up_in_debug_output() {
        let key = AccountKey::from_bytes([0xab; 16]);
        assert_eq!(format!("{key:?}"), "AccountKey(redacted)");
    }
}
