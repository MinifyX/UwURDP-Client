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

/// Where a credential's password came from.
///
/// A file names whatever addresses it likes. A password written in it as
/// clear text is the file author's to give away; one that only this Windows
/// account could open — a DPAPI blob in the file, or the user's own RDCMan
/// profile the file refers to by name — is the user's, and goes to the file's
/// addresses only when the user saw them and said yes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PasswordOrigin {
    /// Clear text in the file (or no password at all).
    #[default]
    InFile,
    /// A DPAPI blob in the file that this Windows account decrypted.
    Unsealed,
    /// One of the user's own Local-scope profiles from `RDCMan.settings`.
    LocalProfile,
}

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
    pub origin: PasswordOrigin,
}

impl ImportedCredential {
    /// Nothing worth keeping — no user, no domain, no password.
    fn is_empty(&self) -> bool {
        self.username.is_none() && self.domain.is_none() && self.password.is_none()
    }

    /// Whether the password is one this Windows account opened, rather than
    /// one the file carried readable: see [`PasswordOrigin`].
    pub fn password_is_users_own(&self) -> bool {
        self.password.as_ref().is_some_and(|p| !p.is_empty())
            && self.origin != PasswordOrigin::InFile
    }

    /// Identity for deduplication. The password derefs to `&str`, so two
    /// entries that differ only in a wiped-on-drop wrapper still compare equal.
    /// The origin counts: a clear-text copy must not stand in for a sealed one.
    fn dedupe_key(&self) -> DedupeKey<'_> {
        (
            self.label.as_deref(),
            self.username.as_deref(),
            self.domain.as_deref(),
            self.password.as_deref().map(|s| s.as_str()),
            self.origin,
        )
    }
}

type DedupeKey<'a> = (
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
    PasswordOrigin,
);

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

/// Something that would get a password the user's own Windows account opened,
/// as the preview lists it before asking whether to take those passwords.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordRecipient {
    pub kind: RecipientKind,
    /// The host's name, or the group's path.
    pub name: String,
    /// Where the password goes: the host's or the gateway's address. `None`
    /// for a group, which hands its login to every host in it.
    pub address: Option<String>,
    pub port: Option<u16>,
    /// The RDCMan profile the password comes from, when it is one of the
    /// user's own; `None` for a sealed password in the file itself.
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecipientKind {
    Host,
    Group,
    Gateway,
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

    /// Every host, gateway and group that would get a password this Windows
    /// account opened ([`ImportedCredential::password_is_users_own`]), with
    /// the address it would go to — hosts that take it from their group at
    /// connect time included.
    pub fn password_recipients(&self) -> Vec<PasswordRecipient> {
        let own = |index: Option<usize>| {
            index
                .and_then(|i| self.credentials.get(i))
                .filter(|c| c.password_is_users_own())
        };
        let profile = |c: &ImportedCredential| match c.origin {
            PasswordOrigin::LocalProfile => c.label.clone(),
            _ => None,
        };
        let mut out = Vec::new();
        for group in &self.groups {
            if let Some(cred) = own(group.credential) {
                out.push(PasswordRecipient {
                    kind: RecipientKind::Group,
                    name: group.path.clone(),
                    address: None,
                    port: None,
                    profile: profile(cred),
                });
            }
        }
        for host in &self.hosts {
            // A host without a login of its own uses its group's.
            let login = host.credential.or_else(|| {
                let path = host.group_path.as_deref()?;
                self.groups.iter().find(|g| g.path == path)?.credential
            });
            if let Some(cred) = own(login) {
                out.push(PasswordRecipient {
                    kind: RecipientKind::Host,
                    name: host.name.clone(),
                    address: Some(host.address.clone()),
                    port: Some(host.port),
                    profile: profile(cred),
                });
            }
            if let Some(gateway) = &host.settings.gateway {
                // The rule the app stores it by: without a login of its own,
                // or told to share, the gateway gets the host's.
                let gateway_login = if gateway.use_host_credentials || gateway.credential.is_none()
                {
                    login
                } else {
                    gateway.credential
                };
                if let Some(cred) = own(gateway_login) {
                    out.push(PasswordRecipient {
                        kind: RecipientKind::Gateway,
                        name: host.name.clone(),
                        address: Some(gateway.address.clone()),
                        port: gateway.port,
                        profile: profile(cred),
                    });
                }
            }
        }
        out
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

/// Split `"host:3389"` into `("host", Some(3389))`, leaving IPv6/other text as
/// an address with no port.
pub(crate) fn split_host_port(raw: &str) -> (String, Option<u16>) {
    if let Some((host, port)) = raw.rsplit_once(':') {
        if let Ok(p) = port.trim().parse::<u16>() {
            if !host.is_empty() && !host.contains(':') {
                return (host.trim().to_string(), Some(p));
            }
        }
    }
    (raw.trim().to_string(), None)
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
