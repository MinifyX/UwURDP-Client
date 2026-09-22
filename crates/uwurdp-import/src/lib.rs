//! Getting an existing Remote Desktop setup into UwURDP.
//!
//! Two sources, one shape. Remote Desktop Connection Manager (`.rdg`) keeps a
//! tree of groups and servers with credentials and per-node settings that
//! inherit down the tree; a single `.rdp` file is one host mstsc saved. Both
//! land in the same [`ImportBundle`] so the preview, the duplicate check and
//! the writer downstream never learn which tool the data came from.
//!
//! Imported data is deliberately its own shape, not the app's stored model:
//! half of it needs a decision from the user, none of it has ids yet, and the
//! secrets in it must never reach the webview. Converting happens once, after
//! the preview.

pub mod rdg;
pub mod rdp;
pub mod settings;

#[cfg(windows)]
mod dpapi;
#[cfg(windows)]
pub use dpapi::Dpapi;

pub use settings::{rdcman_settings, RdcManSettings};

use serde::{Deserialize, Serialize};

/// Where an entry came from, so the preview can label the pile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    RdcMan,
    RdpFile,
}

/// Secret text from an import: wiped from memory when dropped, and not
/// serializable, so it cannot end up in the preview that goes to the webview.
pub type Secret = zeroize::Zeroizing<String>;

/// A username/domain/password triple as a source recorded it.
///
/// Not [`Serialize`], because of the password. Credentials are deduplicated
/// into [`ImportBundle::credentials`] and referred to by index, so one profile
/// shared by forty hosts arrives once.
#[derive(Debug, Default)]
pub struct ImportedCredential {
    /// The profile name, when the source named it (RDCMan credential profiles).
    pub label: Option<String>,
    pub username: Option<String>,
    pub domain: Option<String>,
    pub password: Option<Secret>,
}

impl ImportedCredential {
    /// Nothing worth keeping — no user, no domain, no password.
    fn is_empty(&self) -> bool {
        self.username.is_none() && self.domain.is_none() && self.password.is_none()
    }

    /// Identity for deduplication. The password derefs to `&str`, so two
    /// entries that differ only in a wiped-on-drop wrapper still compare equal.
    fn dedupe_key(&self) -> (Option<&str>, Option<&str>, Option<&str>, Option<&str>) {
        (
            self.label.as_deref(),
            self.username.as_deref(),
            self.domain.as_deref(),
            self.password.as_deref().map(|s| s.as_str()),
        )
    }
}

/// A group in the source's tree.
///
/// Nested groups are flattened into a single `/`-joined path (`"Parent / Child"`)
/// because the app's own grouping is flat; the preview still shows the shape.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedGroup {
    pub path: String,
    /// Index into [`ImportBundle::credentials`]: the group's *effective*
    /// credentials, resolved through inheritance. `None` when the group has
    /// none anywhere up its chain.
    pub credential: Option<usize>,
    pub comment: Option<String>,
}

/// How the remote session is sized on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ImportedDisplay {
    FitWindow,
    FullScreen,
    Fixed { width: u16, height: u16 },
}

/// Where remote audio plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImportedAudio {
    Local,
    Remote,
    Off,
}

/// An RD Gateway in front of a host.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedGateway {
    pub address: String,
    pub port: Option<u16>,
    /// Index into [`ImportBundle::credentials`], for a gateway with its own login.
    pub credential: Option<usize>,
    /// The gateway reuses the host's credentials instead of its own.
    pub use_host_credentials: bool,
    /// Skip the gateway for addresses that are already local.
    pub bypass_local: bool,
}

/// The connection settings an importer understands well enough to map.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedSettings {
    pub display: Option<ImportedDisplay>,
    pub color_depth: Option<u32>,
    /// Connect to the admin/console session.
    pub admin: Option<bool>,
    pub audio: Option<ImportedAudio>,
    pub clipboard: Option<bool>,
    pub gateway: Option<ImportedGateway>,
}

/// One host, with its group, credentials and settings fully resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedHost {
    pub name: String,
    pub address: String,
    /// Default 3389 when the source did not say.
    pub port: u16,
    pub group_path: Option<String>,
    /// Index into [`ImportBundle::credentials`]. `None` means the host inherits
    /// from its group at connect time (or is asked), rather than having none.
    pub credential: Option<usize>,
    pub comment: Option<String>,
    pub settings: ImportedSettings,
    /// Recognised but unmapped settings, kept visible in the preview so nothing
    /// a user configured disappears without a trace.
    pub extras: Vec<(String, String)>,
}

/// Everything one source had.
#[derive(Debug, Default)]
pub struct ImportBundle {
    pub source: Option<Source>,
    pub groups: Vec<ImportedGroup>,
    pub hosts: Vec<ImportedHost>,
    pub credentials: Vec<ImportedCredential>,
    /// What was found and left out (or partially imported), with the reason —
    /// shown in the preview. A silent partial import is worse than a visible
    /// incomplete one.
    pub skipped: Vec<(String, String)>,
}

impl ImportBundle {
    /// Store a credential once, returning its index. Empty credentials are not
    /// stored — they carry nothing a host or group could use.
    fn intern_credential(&mut self, cred: ImportedCredential) -> Option<usize> {
        if cred.is_empty() {
            return None;
        }
        if let Some(i) = self
            .credentials
            .iter()
            .position(|c| c.dedupe_key() == cred.dedupe_key())
        {
            return Some(i);
        }
        self.credentials.push(cred);
        Some(self.credentials.len() - 1)
    }
}

/// Decrypts a stored password blob.
///
/// On Windows the app passes a DPAPI ([`Dpapi`]) implementation backed by
/// `CryptUnprotectData` for the current user. Off Windows, or when the app has
/// no way to decrypt, it passes something whose [`decrypt`](Decrypt::decrypt)
/// always returns `None`; the importer then keeps the username and drops the
/// password rather than failing the whole import.
pub trait Decrypt {
    /// Return the plaintext for a decrypted blob, or `None` if it cannot be
    /// read (wrong user, certificate encryption, corrupt data).
    fn decrypt(&self, blob: &[u8]) -> Option<Secret>;
}

/// Decode the plaintext DPAPI hands back for an RDCMan/`.rdp` password.
///
/// The plaintext is UTF-16LE, often with trailing NUL code units. Every
/// [`Decrypt`] implementation ends here so the encoding lives in one place:
/// [`Dpapi`] does the OS call and passes the raw bytes straight through, and
/// tests do the same with a fake cipher.
// Off Windows only the tests reach this (the app supplies its own `Decrypt`),
// so a non-test build there would otherwise flag it as unused.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn decode_dpapi_plaintext(raw: &[u8]) -> Secret {
    // Drop a dangling odd byte, then read UTF-16LE code units and stop at the
    // first NUL — RDCMan pads with NULs it does not mean as content.
    let mut units = Vec::with_capacity(raw.len() / 2);
    let (pairs, _) = raw.as_chunks::<2>();
    for pair in pairs {
        let unit = u16::from_le_bytes(*pair);
        if unit == 0 {
            break;
        }
        units.push(unit);
    }
    Secret::new(String::from_utf16_lossy(&units))
}

/// A named credential the app already has, used to resolve `scope="Local"`
/// profile references in a `.rdg` file (RDCMan's own credential profiles live
/// outside the file).
#[derive(Debug)]
pub struct NamedCredential {
    pub name: String,
    pub credential: ImportedCredential,
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("could not read the file: {0}")]
    Read(String),
    #[error("nothing importable found")]
    Empty,
}

pub use rdg::parse_rdg;
pub use rdp::parse_rdp_file;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_utf16le_and_trims_nuls() {
        // "Hi" as UTF-16LE, then two NUL code units of padding.
        let raw = [b'H', 0, b'i', 0, 0, 0, 0, 0];
        assert_eq!(decode_dpapi_plaintext(&raw).as_str(), "Hi");
    }

    #[test]
    fn intern_dedupes_and_skips_empty() {
        let mut bundle = ImportBundle::default();
        assert_eq!(
            bundle.intern_credential(ImportedCredential::default()),
            None
        );

        let a = bundle.intern_credential(ImportedCredential {
            username: Some("root".into()),
            ..Default::default()
        });
        let b = bundle.intern_credential(ImportedCredential {
            username: Some("root".into()),
            ..Default::default()
        });
        assert_eq!(a, Some(0));
        assert_eq!(b, Some(0));
        assert_eq!(bundle.credentials.len(), 1);
    }
}
