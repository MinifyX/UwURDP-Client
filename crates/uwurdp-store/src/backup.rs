//! Export files: every host, group, login and trusted certificate in one
//! file, and back in.
//!
//! A file without secrets is plain JSON, readable and diffable. A file with
//! them — stored passwords — is sealed as a
//! whole under a password of its own (see `uwurdp_vault::password`): not the
//! master password, because the file may go to another device, another vault,
//! or another person. Nothing is ever written with secrets in the clear.
//!
//! Importing goes through [`Store::import`], so an export read back into the
//! same store adds nothing twice, and one read into another store keeps
//! workspaces, groups, their order and every login.

use crate::credentials::ResolvedLogin;
use crate::hosts::Workspace;
use crate::import::{GroupInput, HostInput, ImportOutcome, ImportSet, KnownHostInput, LoginInput};
use crate::secret::SecretText;
use crate::{now_ms, Result, Store, StoreError};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uwurdp_proto::RdpSettings;
use uwurdp_vault::{KdfParams, PasswordSealed};
use zeroize::Zeroizing;

const FORMAT: &str = "uwurdp-export";
const VERSION: u32 = 1;
/// Larger than any real host list by orders of magnitude; a file bigger than
/// this is not an export.
pub const MAX_EXPORT_BYTES: usize = 64 * 1024 * 1024;

/// The most key derivation work a file may ask for. UwURDP writes 64 MiB and
/// three passes; a file asking for much more wants to stall whoever opens it.
fn reasonable_for_a_file(kdf: &KdfParams) -> bool {
    kdf.within_limits()
        && kdf.memory_kib <= 256 * 1024
        && kdf.time_cost <= 8
        && kdf.parallelism <= 8
}

/// Secrets in a sealed file are base64: without escapes, the JSON reader hands
/// them over in place instead of copying them into a buffer nobody wipes.
mod secret_base64 {
    use super::{SecretText, BASE64};
    use base64::Engine as _;
    use serde::de::{self, Deserializer, Visitor};
    use serde::Serializer;
    use zeroize::Zeroizing;

    pub fn serialize<S: Serializer>(
        value: &Option<SecretText>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(secret) => {
                let encoded = Zeroizing::new(BASE64.encode(secret.expose().as_bytes()));
                serializer.serialize_some(encoded.as_str())
            }
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<SecretText>, D::Error> {
        struct Optional;
        struct Encoded;

        impl<'de> Visitor<'de> for Optional {
            type Value = Option<SecretText>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a base64 secret or null")
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(None)
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(None)
            }
            fn visit_some<D2: Deserializer<'de>>(
                self,
                inner: D2,
            ) -> Result<Self::Value, D2::Error> {
                inner.deserialize_str(Encoded).map(Some)
            }
        }

        impl<'de> Visitor<'de> for Encoded {
            type Value = SecretText;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a base64 secret")
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                let bytes = Zeroizing::new(
                    BASE64
                        .decode(value)
                        .map_err(|_| E::custom("a secret is damaged"))?,
                );
                let text =
                    std::str::from_utf8(&bytes).map_err(|_| E::custom("a secret is damaged"))?;
                Ok(SecretText::new(text.to_string()))
            }
        }

        deserializer.deserialize_option(Optional)
    }
}

/// JSON in a buffer allocated once, at its final size: a growing buffer
/// leaves copies of what it held behind, and this one holds secrets.
fn json_exact<T: Serialize>(value: &T) -> Result<Zeroizing<Vec<u8>>> {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let failed = |e: serde_json::Error| StoreError::Export(e.to_string());
    let mut count = Count(0);
    serde_json::to_writer(&mut count, value).map_err(failed)?;
    let mut out = Zeroizing::new(Vec::with_capacity(count.0));
    serde_json::to_writer(&mut *out, value).map_err(failed)?;
    Ok(out)
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Backup {
    #[serde(default)]
    pub groups: Vec<BackupGroup>,
    #[serde(default)]
    pub hosts: Vec<BackupHost>,
    #[serde(default)]
    pub known_hosts: Vec<BackupKnownHost>,
}

/// A login inside a file: username, domain and, in a sealed file, the
/// password.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupLogin {
    pub username: String,
    #[serde(default)]
    pub domain: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "secret_base64"
    )]
    pub password: Option<SecretText>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupGroup {
    pub workspace: Workspace,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<BackupLogin>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupHost {
    pub name: String,
    pub address: String,
    pub port: u16,
    #[serde(default)]
    pub workspace: Workspace,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub position: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login: Option<BackupLogin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_login: Option<BackupLogin>,
    #[serde(default)]
    pub rdp: RdpSettings,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub comment: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupKnownHost {
    pub address: String,
    pub port: u16,
    pub algorithm: String,
    pub fingerprint: String,
    pub public_key: String,
}

/// What a file holds, in counts, for the preview. Nothing identifying.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupSummary {
    pub hosts: usize,
    pub groups: usize,
    pub known_hosts: usize,
    pub passwords: usize,
}

impl Backup {
    fn logins(&self) -> impl Iterator<Item = &BackupLogin> {
        self.groups
            .iter()
            .filter_map(|g| g.login.as_ref())
            .chain(self.hosts.iter().filter_map(|h| h.login.as_ref()))
            .chain(self.hosts.iter().filter_map(|h| h.gateway_login.as_ref()))
    }

    pub fn summary(&self) -> BackupSummary {
        BackupSummary {
            hosts: self.hosts.len(),
            groups: self.groups.len(),
            known_hosts: self.known_hosts.len(),
            passwords: self.logins().filter(|l| l.password.is_some()).count(),
        }
    }

    pub fn has_secrets(&self) -> bool {
        self.logins().any(|l| l.password.is_some())
    }
}

impl Store {
    /// A login as a file carries it, its password only with `secrets`.
    fn backup_login(
        &self,
        login: Option<ResolvedLogin>,
        secrets: bool,
    ) -> Result<Option<BackupLogin>> {
        let Some(login) = login else {
            return Ok(None);
        };
        let password = if secrets && login.has_password {
            self.reveal_login_password(&login)?.map(text).transpose()?
        } else {
            None
        };
        Ok(Some(BackupLogin {
            username: login.username,
            domain: login.domain,
            password,
        }))
    }

    fn group_login(&self, workspace: Workspace, name: &str) -> Result<Option<ResolvedLogin>> {
        let conn = self.conn.lock();
        let identity: Option<String> = conn
            .query_row(
                "SELECT identity_id FROM host_groups
                  WHERE workspace = ?1 AND name = ?2 AND deleted = 0",
                rusqlite::params![workspace.as_str(), name],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        drop(conn);
        match identity {
            Some(identity) => self.login_by_identity(&identity),
            None => Ok(None),
        }
    }

    /// Everything, ready to write. With `secrets`, stored passwords come
    /// along, which needs the vault unlocked.
    pub fn export_backup(&self, secrets: bool) -> Result<Backup> {
        if secrets && self.vault.lock().is_none() {
            return Err(StoreError::VaultLocked);
        }
        let mut groups = Vec::new();
        for group in self.list_groups()? {
            let login = self.group_login(group.workspace, &group.name)?;
            groups.push(BackupGroup {
                login: self.backup_login(login, secrets)?,
                workspace: group.workspace,
                name: group.name,
            });
        }

        let mut hosts = Vec::new();
        for host in self.list_hosts()? {
            let own = self.host_login(host.id)?.filter(|l| !l.from_group);
            let gateway = self.host_gateway_login(host.id)?;
            hosts.push(BackupHost {
                login: self.backup_login(own, secrets)?,
                gateway_login: self.backup_login(gateway, secrets)?,
                name: host.name,
                address: host.address,
                port: host.port,
                workspace: host.workspace,
                group: host.group_path,
                position: host.position,
                rdp: host.rdp,
                comment: host.comment,
            });
        }

        let known_hosts = self
            .list_known_hosts()?
            .into_iter()
            .map(|k| BackupKnownHost {
                address: k.address,
                port: k.port,
                algorithm: k.algorithm,
                fingerprint: k.fingerprint,
                public_key: k.public_key,
            })
            .collect();

        Ok(Backup {
            groups,
            hosts,
            known_hosts,
        })
    }

    /// Read a backup into the store, skipping what is already there.
    pub fn import_backup(&self, backup: Backup) -> Result<ImportOutcome> {
        let Backup {
            groups,
            hosts,
            known_hosts,
        } = backup;

        let mut logins = Vec::new();
        let mut login = |value: Option<BackupLogin>| {
            value.map(|l| {
                logins.push(LoginInput {
                    username: l.username,
                    domain: l.domain,
                    password: l.password.map(SecretText::into_inner),
                });
                logins.len() - 1
            })
        };

        let set_groups: Vec<GroupInput> = groups
            .into_iter()
            .map(|g| GroupInput {
                workspace: g.workspace,
                name: g.name,
                login: login(g.login),
            })
            .collect();

        // A file's certificates only for the hosts it brings: a host list
        // must not pre-trust certificates for addresses the user adds some
        // other day.
        let addresses: HashSet<(String, u16)> = hosts
            .iter()
            .map(|h| (h.address.trim().to_ascii_lowercase(), h.port))
            .collect();

        let set_hosts: Vec<HostInput> = hosts
            .into_iter()
            .map(|host| HostInput {
                login: login(host.login),
                gateway_login: login(host.gateway_login),
                name: host.name,
                address: host.address,
                port: host.port,
                group_path: host.group,
                workspace: host.workspace,
                position: None,
                rdp: host.rdp,
                comment: host.comment,
            })
            .collect();

        self.import(ImportSet {
            groups: set_groups,
            hosts: set_hosts,
            logins,
            known_hosts: known_hosts
                .into_iter()
                .filter(|k| addresses.contains(&(k.address.trim().to_ascii_lowercase(), k.port)))
                .map(|k| KnownHostInput {
                    address: k.address,
                    port: k.port,
                    algorithm: k.algorithm,
                    public_key: k.public_key,
                    fingerprint: k.fingerprint,
                })
                .collect(),
        })
    }
}

fn text(bytes: Zeroizing<Vec<u8>>) -> Result<SecretText> {
    std::str::from_utf8(&bytes)
        .map(SecretText::new)
        .map_err(|_| StoreError::Export("a stored secret is not text".into()))
}

// ── The file ────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    format: String,
    version: u32,
    #[serde(default)]
    created_ms: u64,
    #[serde(default)]
    app: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sealed: Option<SealedBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data: Option<Backup>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SealedBody {
    kdf: String,
    memory_kib: u32,
    time_cost: u32,
    parallelism: u32,
    salt: String,
    nonce: String,
    blob: String,
}

/// Write a backup as a file. Secrets need a password; a backup without them is
/// written in the clear, whatever `password` says, so nobody locks away a file
/// that has nothing to hide and then forgets its password.
pub fn encode_export(
    backup: &Backup,
    password: Option<&[u8]>,
    app_version: &str,
) -> Result<Zeroizing<Vec<u8>>> {
    encode_export_with(backup, password, app_version, KdfParams::RECOMMENDED)
}

pub(crate) fn encode_export_with(
    backup: &Backup,
    password: Option<&[u8]>,
    app_version: &str,
    kdf: KdfParams,
) -> Result<Zeroizing<Vec<u8>>> {
    let mut envelope = Envelope {
        format: FORMAT.into(),
        version: VERSION,
        created_ms: now_ms(),
        app: app_version.into(),
        sealed: None,
        data: None,
    };
    let json = |value: &Envelope| {
        serde_json::to_vec_pretty(value)
            .map(Zeroizing::new)
            .map_err(|e| StoreError::Export(e.to_string()))
    };
    // A password always seals, secrets or not: whoever typed one was told
    // the file would be encrypted.
    let Some(password) = password.filter(|p| !p.is_empty()) else {
        if backup.has_secrets() {
            return Err(StoreError::ExportPasswordRequired);
        }
        return serialize_plain(&envelope, backup);
    };
    let inner = json_exact(backup)?;
    let sealed = uwurdp_vault::seal_with_password(password, &inner, kdf)?;
    envelope.sealed = Some(SealedBody {
        kdf: "argon2id".into(),
        memory_kib: sealed.kdf.memory_kib,
        time_cost: sealed.kdf.time_cost,
        parallelism: sealed.kdf.parallelism,
        salt: BASE64.encode(sealed.salt),
        nonce: BASE64.encode(sealed.nonce),
        blob: BASE64.encode(&sealed.blob),
    });
    json(&envelope)
}

fn serialize_plain(envelope: &Envelope, backup: &Backup) -> Result<Zeroizing<Vec<u8>>> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct View<'a> {
        format: &'a str,
        version: u32,
        created_ms: u64,
        app: &'a str,
        data: &'a Backup,
    }
    serde_json::to_vec_pretty(&View {
        format: &envelope.format,
        version: envelope.version,
        created_ms: envelope.created_ms,
        app: &envelope.app,
        data: backup,
    })
    .map(Zeroizing::new)
    .map_err(|e| StoreError::Export(e.to_string()))
}

/// Whether a file needs a password to read, without reading it all.
pub fn export_is_sealed(bytes: &[u8]) -> Result<bool> {
    Ok(parse_envelope(bytes)?.sealed.is_some())
}

/// Read a file. A sealed one without a password is
/// [`StoreError::ExportPasswordRequired`]; with the wrong one,
/// [`StoreError::ExportPasswordWrong`].
pub fn decode_export(bytes: &[u8], password: Option<&[u8]>) -> Result<Backup> {
    let envelope = parse_envelope(bytes)?;
    match (envelope.sealed, envelope.data) {
        (Some(sealed), _) => {
            let password = password
                .filter(|p| !p.is_empty())
                .ok_or(StoreError::ExportPasswordRequired)?;
            let bytes = |field: &str| {
                BASE64
                    .decode(field)
                    .map_err(|_| StoreError::Export("the sealed part is damaged".into()))
            };
            if sealed.kdf != "argon2id" {
                return Err(StoreError::Export(format!(
                    "unknown key derivation {}",
                    sealed.kdf
                )));
            }
            let salt: [u8; 16] = bytes(&sealed.salt)?
                .try_into()
                .map_err(|_| StoreError::Export("the sealed part is damaged".into()))?;
            let nonce: [u8; 24] = bytes(&sealed.nonce)?
                .try_into()
                .map_err(|_| StoreError::Export("the sealed part is damaged".into()))?;
            let kdf = KdfParams {
                memory_kib: sealed.memory_kib,
                time_cost: sealed.time_cost,
                parallelism: sealed.parallelism,
            };
            if !reasonable_for_a_file(&kdf) {
                return Err(StoreError::Export(
                    "the file asks for far more key derivation work than an export needs".into(),
                ));
            }
            let sealed = PasswordSealed {
                kdf,
                salt,
                nonce,
                blob: bytes(&sealed.blob)?,
            };
            let inner = match uwurdp_vault::open_with_password(password, &sealed) {
                Ok(inner) => inner,
                Err(uwurdp_vault::VaultError::Decrypt) => {
                    return Err(StoreError::ExportPasswordWrong)
                }
                Err(other) => return Err(other.into()),
            };
            serde_json::from_slice(&inner).map_err(|e| StoreError::Export(e.to_string()))
        }
        (None, Some(data)) => Ok(data),
        (None, None) => Err(StoreError::Export("the file holds no data".into())),
    }
}

fn parse_envelope(bytes: &[u8]) -> Result<Envelope> {
    if bytes.len() > MAX_EXPORT_BYTES {
        return Err(StoreError::Export("the file is too large".into()));
    }
    let envelope: Envelope = serde_json::from_slice(bytes)
        .map_err(|_| StoreError::Export("this is not an UwURDP export".into()))?;
    if envelope.format != FORMAT {
        return Err(StoreError::Export("this is not an UwURDP export".into()));
    }
    if envelope.version > VERSION {
        return Err(StoreError::Export(format!(
            "the file was written by a newer UwURDP (format {})",
            envelope.version
        )));
    }
    Ok(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosts::tests::{draft, set};

    const FAST: KdfParams = KdfParams::INSECURE_FOR_TESTS;

    /// A store with a bit of everything: two workspaces, a group with a
    /// login, a host with its own password and a gateway, a trusted
    /// certificate.
    fn full_store() -> Store {
        let store = Store::open_in_memory().unwrap();
        store.create_vault_with(b"master", FAST).unwrap();
        store.create_group(Workspace::Business, "Clients").unwrap();
        store.create_group(Workspace::Private, "Empty").unwrap();
        store
            .set_group_login(Workspace::Business, "Clients", "svc", "CORP", &set("grp"))
            .unwrap();

        let mut web = draft("web", "10.0.0.5");
        web.workspace = Some(Workspace::Business);
        web.group_path = Some("Clients".into());
        web.password = set("hunter2");
        web.rdp.display = "fixed".into();
        web.rdp.width = 1600;
        web.gateway_username = "gw".into();
        web.gateway_password = set("gw-pw");
        web.rdp.gateway = Some(uwurdp_proto::GatewaySettings {
            address: "gw.example.com".into(),
            port: 443,
            ..Default::default()
        });
        web.comment = "the shop".into();
        store.save_host(web).unwrap();

        let mut inherits = draft("app", "10.0.0.6");
        inherits.workspace = Some(Workspace::Business);
        inherits.group_path = Some("Clients".into());
        inherits.username = String::new();
        store.save_host(inherits).unwrap();

        store
            .trust_host_key("10.0.0.5", 3389, "x509", "SHA256:web", "AAAA")
            .unwrap();
        store
    }

    #[test]
    fn an_export_with_secrets_reads_back_into_a_new_store_whole() {
        let store = full_store();
        let backup = store.export_backup(true).unwrap();
        assert_eq!(
            backup.summary(),
            BackupSummary {
                hosts: 2,
                groups: 2,
                known_hosts: 1,
                passwords: 3,
            }
        );
        let file = encode_export_with(&backup, Some(b"file-pw"), "test", FAST).unwrap();
        assert!(export_is_sealed(&file).unwrap());
        assert!(
            !String::from_utf8_lossy(&file).contains("hunter2"),
            "nothing secret in the clear"
        );

        let other = Store::open_in_memory().unwrap();
        other.create_vault_with(b"other", FAST).unwrap();
        let read = decode_export(&file, Some(b"file-pw")).unwrap();
        let outcome = other.import_backup(read).unwrap();
        assert_eq!(outcome.hosts_added, 2);
        assert_eq!(outcome.known_hosts_added, 1);

        let hosts = other.list_hosts().unwrap();
        let web = hosts.iter().find(|h| h.name == "web").unwrap();
        assert_eq!(web.workspace, Workspace::Business);
        assert_eq!(web.group_path.as_deref(), Some("Clients"));
        assert_eq!(web.rdp.display, "fixed");
        assert_eq!(web.rdp.width, 1600);
        assert_eq!(web.comment, "the shop");
        assert_eq!(web.gateway_username, "gw");
        assert_eq!(&**other.reveal_host_password(web.id).unwrap(), b"hunter2");

        let app = hosts.iter().find(|h| h.name == "app").unwrap();
        let login = other.host_login(app.id).unwrap().unwrap();
        assert!(login.from_group, "the group's login came along");
        assert_eq!(
            &**other.reveal_login_password(&login).unwrap().unwrap(),
            b"grp"
        );
        assert!(other
            .list_groups()
            .unwrap()
            .iter()
            .any(|g| g.name == "Empty"));
    }

    #[test]
    fn a_sealed_file_needs_its_password() {
        let backup = full_store().export_backup(true).unwrap();
        let file = encode_export_with(&backup, Some(b"right"), "test", FAST).unwrap();
        assert!(matches!(
            decode_export(&file, None),
            Err(StoreError::ExportPasswordRequired)
        ));
        assert!(matches!(
            decode_export(&file, Some(b"wrong")),
            Err(StoreError::ExportPasswordWrong)
        ));
    }

    #[test]
    fn secrets_without_a_password_are_refused() {
        let backup = full_store().export_backup(true).unwrap();
        assert!(matches!(
            encode_export_with(&backup, None, "test", FAST),
            Err(StoreError::ExportPasswordRequired)
        ));
    }

    #[test]
    fn a_file_brings_certificates_only_for_its_own_hosts() {
        let store = full_store();
        let mut backup = store.export_backup(false).unwrap();
        backup.known_hosts.push(BackupKnownHost {
            address: "stranger.example.com".into(),
            port: 3389,
            algorithm: "x509".into(),
            fingerprint: "SHA256:evil".into(),
            public_key: "BBBB".into(),
        });
        let other = Store::open_in_memory().unwrap();
        other.import_backup(backup).unwrap();
        assert!(other
            .known_host("stranger.example.com", 3389)
            .unwrap()
            .is_none());
        assert!(other.known_host("10.0.0.5", 3389).unwrap().is_some());
    }

    #[test]
    fn a_file_asking_for_absurd_key_derivation_is_refused() {
        let backup = full_store().export_backup(true).unwrap();
        let file = encode_export_with(&backup, Some(b"pw"), "test", FAST).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&file).unwrap();
        value["sealed"]["memoryKib"] = serde_json::json!(4 * 1024 * 1024);
        let tampered = serde_json::to_vec(&value).unwrap();
        assert!(matches!(
            decode_export(&tampered, Some(b"pw")),
            Err(StoreError::Export(_))
        ));
    }

    #[test]
    fn an_export_without_secrets_is_plain_and_needs_no_vault() {
        let store = full_store();
        let backup = store.export_backup(false).unwrap();
        assert!(!backup.has_secrets());
        let file = encode_export(&backup, None, "test").unwrap();
        assert!(!export_is_sealed(&file).unwrap());
        let text = String::from_utf8(file.to_vec()).unwrap();
        assert!(text.contains("\"uwurdp-export\""));
        assert!(text.contains("10.0.0.5"));
        assert!(!text.contains("hunter2"));

        let other = Store::open_in_memory().unwrap();
        let outcome = other
            .import_backup(decode_export(&file, None).unwrap())
            .unwrap();
        assert_eq!(outcome.hosts_added, 2);
        assert_eq!(outcome.passwords_added, 0);
    }

    #[test]
    fn something_else_is_not_read_as_an_export() {
        assert!(decode_export(b"{}", None).is_err());
        assert!(decode_export(b"not json", None).is_err());
        assert!(decode_export(b"{\"format\":\"uwurdp-export\",\"version\":99}", None).is_err());
    }
}
