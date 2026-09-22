//! Writing an imported set into the store, passwords and all.
//!
//! The store defines its own plain input types rather than depending on the
//! importer crate: an importer (RDCMan, `.rdp` files, UwURDP's own export)
//! maps its findings into these, and everything after — sealing passwords, one
//! transaction, skipping hosts that are already there — happens here, in one
//! place, tested here.
//!
//! Passwords are sealed with the unlocked vault as they are written, so one is
//! ciphertext by the time it reaches SQLite. The whole set is one transaction:
//! a failure halfway leaves the store as it was, not half-imported.
//!
//! Every host and group gets a login of its own, even when the source shared
//! one between them (an RDCMan credential profile does): editing one host
//! later must never change another. Importing the same source twice adds
//! nothing twice — a host already there, same address and port, is skipped,
//! and a group that already has a login keeps it.

use crate::hosts::{ensure_group, group_id, next_position, normalize_rdp, Workspace};
use crate::logins::{clean_domain, clean_username, insert_login};
use crate::vault::seal_secret;
use crate::{now_ms, tick, vault_id, Result, Store, StoreError};
use rusqlite::{params, Transaction};
use uuid::Uuid;
use uwurdp_proto::RdpSettings;
use uwurdp_vault::UnlockedVault;
use zeroize::Zeroizing;

/// A secret as it arrives from an importer.
pub type Secret = Zeroizing<String>;

/// A username, a domain and maybe a password, referenced by index.
#[derive(Default)]
pub struct LoginInput {
    pub username: String,
    pub domain: String,
    pub password: Option<Secret>,
}

pub struct HostInput {
    pub name: String,
    pub address: String,
    pub port: u16,
    pub group_path: Option<String>,
    /// Index into [`ImportSet::logins`]. `None`: the host uses its group's
    /// login, or asks.
    pub login: Option<usize>,
    /// Index into [`ImportSet::logins`], for the gateway.
    pub gateway_login: Option<usize>,
    pub workspace: Workspace,
    /// The place within its group, when the source has one (an UwURDP export
    /// does); otherwise the host goes to the end.
    pub position: Option<i64>,
    pub rdp: RdpSettings,
    pub comment: String,
}

/// A group of its own, possibly empty, with the login it hands down.
pub struct GroupInput {
    pub workspace: Workspace,
    pub name: String,
    pub login: Option<usize>,
}

/// A server certificate already trusted, as an UwURDP export carries it.
pub struct KnownHostInput {
    pub address: String,
    pub port: u16,
    pub algorithm: String,
    /// The certificate, base64.
    pub public_key: String,
    pub fingerprint: String,
}

/// Everything one import produced. Indices tie hosts and groups to logins.
#[derive(Default)]
pub struct ImportSet {
    pub groups: Vec<GroupInput>,
    pub hosts: Vec<HostInput>,
    pub logins: Vec<LoginInput>,
    pub known_hosts: Vec<KnownHostInput>,
}

/// What an import changed. Skipped hosts were already there (same address
/// and port) or not something the host form would take.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportOutcome {
    pub hosts_added: usize,
    pub hosts_skipped: usize,
    pub groups_added: usize,
    pub logins_added: usize,
    pub passwords_added: usize,
    pub known_hosts_added: usize,
}

impl ImportSet {
    /// Whether writing this set has to seal anything.
    pub fn has_secrets(&self) -> bool {
        self.logins.iter().any(|l| l.password.is_some())
    }
}

/// The most entries of one kind a single import takes.
const MAX_ITEMS: usize = 50_000;

impl Store {
    /// Write an import into the store. A password it actually writes needs
    /// the vault unlocked, since that is the only place a secret may go;
    /// otherwise the import fails with [`StoreError::VaultLocked`] and writes
    /// nothing. Passwords of hosts that are already there are never written,
    /// so importing the same setup again needs no vault.
    pub fn import(&self, set: ImportSet) -> Result<ImportOutcome> {
        // Far beyond any real setup; an import runs in one transaction that
        // holds the database, so there has to be an end.
        if set.hosts.len() > MAX_ITEMS
            || set.logins.len() > MAX_ITEMS
            || set.known_hosts.len() > MAX_ITEMS
            || set.groups.len() > MAX_ITEMS
        {
            return Err(StoreError::Export(format!(
                "more than {MAX_ITEMS} entries of one kind is not something to import"
            )));
        }
        let mut conn = self.conn.lock();
        let vault_guard = self.vault.lock();
        let tx = conn.transaction()?;
        let vault_uuid = vault_id(&tx)?;
        let writer = Writer {
            tx: &tx,
            device: self.device,
            vault: vault_guard.as_ref(),
            vault_uuid,
        };
        let outcome = writer.write(&set)?;
        tx.commit()?;
        tracing::info!(?outcome, "import written");
        Ok(outcome)
    }
}

struct Writer<'a> {
    tx: &'a Transaction<'a>,
    device: u32,
    /// `None` when the vault is locked; fine for a set without passwords.
    vault: Option<&'a UnlockedVault>,
    vault_uuid: String,
}

/// Whether an imported host is one the host form would have let through: an
/// address without spaces or control characters, a name without control
/// characters. What isn't is skipped, not written.
fn plausible(host: &HostInput) -> bool {
    let address = host.address.trim();
    !address.is_empty()
        && address.len() <= 253
        && !address.chars().any(|c| c.is_whitespace() || c.is_control())
        && !host.name.chars().any(char::is_control)
        && host.name.len() <= 256
        && host.port > 0
}

impl Writer<'_> {
    fn write(&self, set: &ImportSet) -> Result<ImportOutcome> {
        let mut outcome = ImportOutcome::default();

        for group in &set.groups {
            let Some(name) = crate::hosts::group_name(Some(group.name.clone()))? else {
                continue;
            };
            let existed = group_id(self.tx, group.workspace, &name)?.is_some();
            let id = ensure_group(
                self.tx,
                self.device,
                &self.vault_uuid,
                group.workspace,
                &name,
            )?;
            if !existed {
                outcome.groups_added += 1;
            }
            let has_login: bool = self.tx.query_row(
                "SELECT identity_id IS NOT NULL FROM host_groups WHERE id = ?1",
                [&id],
                |row| row.get(0),
            )?;
            if has_login {
                continue;
            }
            if let Some(login) = self.login(set, group.login, &mut outcome)? {
                let clock = tick(self.tx, self.device)?;
                self.tx.execute(
                    "UPDATE host_groups
                        SET identity_id = ?2, rev = rev + 1,
                            dirty = 1, hlc_wall_ms = ?3, hlc_counter = ?4, hlc_device = ?5
                      WHERE id = ?1",
                    params![id, login, clock.wall_ms as i64, clock.counter, clock.device],
                )?;
            }
        }

        let mut added = std::collections::HashSet::new();
        for host in &set.hosts {
            if !plausible(host) || self.host_exists(&host.address, host.port)? {
                outcome.hosts_skipped += 1;
                continue;
            }
            let login = self.login(set, host.login, &mut outcome)?;
            let gateway_login = self.login(set, host.gateway_login, &mut outcome)?;
            self.write_host(host, login.as_deref(), gateway_login.as_deref())?;
            added.insert((host.address.trim().to_ascii_lowercase(), host.port));
            outcome.hosts_added += 1;
        }

        // Certificates only for the hosts this import just added. A file names
        // whatever addresses it likes: one that lists `dc.corp` beside a
        // certificate of its own must not become the one trusted for the
        // `dc.corp` the user already has, or adds by hand tomorrow — that is a
        // man-in-the-middle waiting for the first connection.
        for known in &set.known_hosts {
            let address = (known.address.trim().to_ascii_lowercase(), known.port);
            if added.contains(&address) && self.write_known_host(known)? {
                outcome.known_hosts_added += 1;
            }
        }

        Ok(outcome)
    }

    /// A fresh login for one owner, from the set's entry at `index`. A login
    /// without a username is none; a username the host form would refuse is
    /// dropped too, and the host asks on connect.
    fn login(
        &self,
        set: &ImportSet,
        index: Option<usize>,
        outcome: &mut ImportOutcome,
    ) -> Result<Option<String>> {
        let Some(input) = index.and_then(|i| set.logins.get(i)) else {
            return Ok(None);
        };
        let (Ok(username), Ok(domain)) = (
            clean_username("username", &input.username),
            clean_domain("domain", &input.domain),
        ) else {
            return Ok(None);
        };
        if username.is_empty() {
            return Ok(None);
        }
        let secret = match &input.password {
            Some(password) if !password.is_empty() => {
                let vault = self.vault.ok_or(StoreError::VaultLocked)?;
                outcome.passwords_added += 1;
                Some(seal_secret(
                    self.tx,
                    self.device,
                    vault,
                    password.as_bytes(),
                )?)
            }
            _ => None,
        };
        let label = if domain.is_empty() {
            username.clone()
        } else {
            format!("{domain}\\{username}")
        };
        let clock = tick(self.tx, self.device)?;
        let id = insert_login(
            self.tx,
            &self.vault_uuid,
            &label,
            &username,
            &domain,
            secret.as_deref(),
            clock,
        )?;
        outcome.logins_added += 1;
        Ok(Some(id))
    }

    fn host_exists(&self, address: &str, port: u16) -> Result<bool> {
        let count: i64 = self.tx.query_row(
            "SELECT count(*) FROM hosts
              WHERE deleted = 0 AND lower(address) = lower(?1) AND port = ?2",
            params![address.trim(), port],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    fn write_host(
        &self,
        host: &HostInput,
        login: Option<&str>,
        gateway_login: Option<&str>,
    ) -> Result<()> {
        let group = match crate::hosts::group_name(host.group_path.clone())? {
            Some(name) => Some(ensure_group(
                self.tx,
                self.device,
                &self.vault_uuid,
                host.workspace,
                &name,
            )?),
            None => None,
        };
        let position = match host.position {
            Some(position) => position,
            None => next_position(self.tx, host.workspace, group.as_deref())?,
        };
        let name = match host.name.trim() {
            "" => host.address.trim().to_string(),
            name => name.to_string(),
        };
        let rdp = serde_json::to_string(&normalize_rdp(host.rdp.clone())).map_err(|_| {
            StoreError::Invalid {
                field: "rdp",
                problem: "unserialisable",
            }
        })?;
        let comment: String = host.comment.trim().chars().take(4000).collect();
        let clock = tick(self.tx, self.device)?;
        self.tx.execute(
            "INSERT INTO hosts
                (id, vault_id, name, address, port, identity_id, group_id, workspace, position,
                 rdp, comment, gateway_identity_id, hlc_wall_ms, hlc_counter, hlc_device)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                Uuid::now_v7().to_string(),
                self.vault_uuid,
                name,
                host.address.trim(),
                host.port,
                login,
                group,
                host.workspace.as_str(),
                position,
                rdp,
                comment,
                gateway_login,
                clock.wall_ms as i64,
                clock.counter,
                clock.device,
            ],
        )?;
        Ok(())
    }

    /// Trust an imported certificate, unless that address already has one —
    /// an import must never quietly replace a certificate the user relies on,
    /// nor bring back one the user removed.
    fn write_known_host(&self, known: &KnownHostInput) -> Result<bool> {
        let address = known.address.trim().to_ascii_lowercase();
        let taken: i64 = self.tx.query_row(
            "SELECT count(*) FROM known_hosts WHERE address = ?1 AND port = ?2",
            params![address, known.port],
            |row| row.get(0),
        )?;
        if taken > 0 {
            return Ok(false);
        }
        let clock = tick(self.tx, self.device)?;
        self.tx.execute(
            "INSERT INTO known_hosts
                (id, vault_id, address, port, algorithm, fingerprint_sha256, public_key,
                 first_seen_ms, hlc_wall_ms, hlc_counter, hlc_device)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT (address, port) DO NOTHING",
            params![
                Uuid::now_v7().to_string(),
                self.vault_uuid,
                address,
                known.port,
                known.algorithm,
                known.fingerprint,
                known.public_key,
                now_ms() as i64,
                clock.wall_ms as i64,
                clock.counter,
                clock.device,
            ],
        )?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosts::tests::{count, unlocked};

    fn secret(text: &str) -> Secret {
        Secret::new(text.to_owned())
    }

    fn host(name: &str, address: &str, group: Option<&str>, login: Option<usize>) -> HostInput {
        HostInput {
            name: name.into(),
            address: address.into(),
            port: 3389,
            group_path: group.map(str::to_string),
            login,
            gateway_login: None,
            workspace: Workspace::Business,
            position: None,
            rdp: RdpSettings::default(),
            comment: String::new(),
        }
    }

    /// What an RDCMan file typically brings: a group with a login its hosts
    /// inherit, one host with its own, one without any.
    fn sample() -> ImportSet {
        ImportSet {
            groups: vec![GroupInput {
                workspace: Workspace::Business,
                name: "Domain Controllers".into(),
                login: Some(0),
            }],
            logins: vec![
                LoginInput {
                    username: "admin".into(),
                    domain: "CORP".into(),
                    password: Some(secret("group-pw")),
                },
                LoginInput {
                    username: "local".into(),
                    domain: String::new(),
                    password: Some(secret("own-pw")),
                },
            ],
            hosts: vec![
                host("dc-1", "10.0.0.1", Some("Domain Controllers"), None),
                host("dc-2", "10.0.0.2", Some("Domain Controllers"), Some(1)),
                host("kiosk", "10.0.0.3", None, None),
            ],
            known_hosts: Vec::new(),
        }
    }

    #[test]
    fn an_import_lands_with_group_logins_and_own_logins() {
        let store = unlocked();
        let outcome = store.import(sample()).unwrap();
        assert_eq!(outcome.hosts_added, 3);
        assert_eq!(outcome.groups_added, 1);
        assert_eq!(outcome.logins_added, 2);
        assert_eq!(outcome.passwords_added, 2);

        let hosts = store.list_hosts().unwrap();
        let find = |name: &str| hosts.iter().find(|h| h.name == name).unwrap().clone();
        let dc1 = store.host_login(find("dc-1").id).unwrap().unwrap();
        assert!(dc1.from_group);
        assert_eq!(
            (dc1.username.as_str(), dc1.domain.as_str()),
            ("admin", "CORP")
        );
        let dc2 = store.host_login(find("dc-2").id).unwrap().unwrap();
        assert!(!dc2.from_group);
        assert_eq!(
            &**store.reveal_login_password(&dc2).unwrap().unwrap(),
            b"own-pw"
        );
        assert!(store.host_login(find("kiosk").id).unwrap().is_none());
        assert_eq!(find("kiosk").workspace, Workspace::Business);
    }

    #[test]
    fn importing_the_same_set_twice_adds_nothing_twice() {
        let store = unlocked();
        store.import(sample()).unwrap();
        let again = store.import(sample()).unwrap();
        assert_eq!(again.hosts_added, 0);
        assert_eq!(again.hosts_skipped, 3);
        assert_eq!(again.groups_added, 0);
        assert_eq!(again.logins_added, 0, "the group keeps the login it has");
        assert_eq!(
            count(&store, "SELECT count(*) FROM identities WHERE deleted = 0"),
            2
        );
    }

    #[test]
    fn a_login_several_hosts_share_arrives_once_per_host() {
        let store = unlocked();
        let mut set = sample();
        set.groups.clear();
        set.hosts = vec![
            host("a", "10.0.0.1", None, Some(1)),
            host("b", "10.0.0.2", None, Some(1)),
        ];
        store.import(set).unwrap();
        assert_eq!(
            count(&store, "SELECT count(*) FROM identities WHERE deleted = 0"),
            2,
            "one login each, so editing one never changes the other"
        );
    }

    #[test]
    fn importing_passwords_into_a_locked_vault_is_refused_and_writes_nothing() {
        let store = Store::open_in_memory().unwrap();
        assert!(matches!(
            store.import(sample()),
            Err(StoreError::VaultLocked)
        ));
        assert!(store.list_hosts().unwrap().is_empty());
        assert!(store.list_groups().unwrap().is_empty());
    }

    #[test]
    fn a_passwordless_import_needs_no_vault() {
        let store = Store::open_in_memory().unwrap();
        let mut set = sample();
        for login in &mut set.logins {
            login.password = None;
        }
        let outcome = store.import(set).unwrap();
        assert_eq!(outcome.hosts_added, 3);
        assert_eq!(outcome.passwords_added, 0);
    }

    #[test]
    fn hosts_the_form_would_refuse_are_skipped() {
        let store = unlocked();
        let mut set = sample();
        set.hosts.push(host("spaced", "10.0.0 .9", None, None));
        set.hosts
            .push(host("control\u{7}", "10.0.0.10", None, None));
        let outcome = store.import(set).unwrap();
        assert_eq!(outcome.hosts_added, 3);
        assert_eq!(outcome.hosts_skipped, 2);
    }

    #[test]
    fn a_certificate_comes_along_only_for_a_host_the_import_added() {
        let store = unlocked();
        store
            .trust_host_key("10.0.0.1", 3389, "x509", "SHA256:mine", "AAAA")
            .unwrap();
        let mut set = sample();
        set.known_hosts = vec![
            KnownHostInput {
                address: "10.0.0.1".into(),
                port: 3389,
                algorithm: "x509".into(),
                public_key: "BBBB".into(),
                fingerprint: "SHA256:theirs".into(),
            },
            KnownHostInput {
                address: "10.0.0.2".into(),
                port: 3389,
                algorithm: "x509".into(),
                public_key: "CCCC".into(),
                fingerprint: "SHA256:new".into(),
            },
            KnownHostInput {
                address: "elsewhere.example.com".into(),
                port: 3389,
                algorithm: "x509".into(),
                public_key: "DDDD".into(),
                fingerprint: "SHA256:stranger".into(),
            },
        ];
        let outcome = store.import(set).unwrap();
        assert_eq!(outcome.known_hosts_added, 1);
        assert_eq!(
            store
                .known_host("10.0.0.1", 3389)
                .unwrap()
                .unwrap()
                .fingerprint,
            "SHA256:mine",
            "an import never replaces a trusted certificate"
        );
        assert!(store
            .known_host("elsewhere.example.com", 3389)
            .unwrap()
            .is_none());
    }

    #[test]
    fn rdp_settings_arrive_brought_into_range() {
        let store = unlocked();
        let mut set = sample();
        set.hosts[0].rdp.display = "fullscreen".into();
        set.hosts[0].rdp.color_depth = 3;
        set.hosts[0].comment = "  first DC ".into();
        store.import(set).unwrap();
        let dc1 = store
            .list_hosts()
            .unwrap()
            .into_iter()
            .find(|h| h.name == "dc-1")
            .unwrap();
        assert_eq!(dc1.rdp.display, "fullscreen");
        assert_eq!(dc1.rdp.color_depth, 32);
        assert_eq!(dc1.comment, "first DC");
    }
}
