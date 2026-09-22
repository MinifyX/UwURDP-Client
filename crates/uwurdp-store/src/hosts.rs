//! Hosts: a Windows machine (or anything else that speaks RDP), how its
//! desktop is shown, and the login it uses.
//!
//! A host lives in a workspace (private or business) and optionally a group,
//! at a position the user dragged it to. Its login is its own — a username, a
//! domain and maybe a password in the vault — or, when it has none, the one
//! its group hands down, like RDCMan's "inherit from parent". With neither,
//! connecting asks. A gateway in front of it can have a login of its own too.
//! See [`crate::logins`] for how logins are kept.

use crate::logins::{apply_login, read_login, release_login, LoginChange};
use crate::secret::SecretText;
use crate::vault::truncate_wal;
use crate::{now_ms, tick, vault_id, Result, Store, StoreError};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use uwurdp_proto::RdpSettings;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Workspace {
    #[default]
    Private,
    Business,
}

impl Workspace {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Business => "business",
        }
    }

    pub(crate) fn parse(value: &str) -> Self {
        match value {
            "business" => Self::Business,
            _ => Self::Private,
        }
    }
}

/// A host as the UI sees it, with its own login and its gateway's flattened
/// in. Nothing in here is secret; `has_password` only says that a password is
/// stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostRecord {
    pub id: Uuid,
    pub name: String,
    pub address: String,
    pub port: u16,
    /// The host's own login; empty when it uses its group's.
    pub username: String,
    pub domain: String,
    /// A password for the host's own login is sealed in the vault.
    pub has_password: bool,
    pub group_path: Option<String>,
    pub last_connected_ms: Option<u64>,
    pub workspace: Workspace,
    pub position: i64,
    pub rdp: RdpSettings,
    pub comment: String,
    pub gateway_username: String,
    pub gateway_domain: String,
    pub has_gateway_password: bool,
}

/// What happens to a stored password when a form is saved.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PasswordChange {
    #[default]
    Keep,
    Set {
        value: SecretText,
    },
    Forget,
}

/// What the host form submits. No `id` means a new host.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostDraft {
    pub id: Option<Uuid>,
    pub name: String,
    pub address: String,
    pub port: u16,
    /// Empty: no login of its own, the group's is used (or connecting asks).
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub domain: String,
    pub group_path: Option<String>,
    /// `None` keeps an edited host where it is and puts a new one in private.
    #[serde(default)]
    pub workspace: Option<Workspace>,
    #[serde(default)]
    pub password: PasswordChange,
    #[serde(default)]
    pub rdp: RdpSettings,
    #[serde(default)]
    pub comment: String,
    #[serde(default)]
    pub gateway_username: String,
    #[serde(default)]
    pub gateway_domain: String,
    #[serde(default)]
    pub gateway_password: PasswordChange,
}

fn invalid(field: &'static str, problem: &'static str) -> StoreError {
    StoreError::Invalid { field, problem }
}

pub(crate) fn blank_to_none(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// A group name as typed: trimmed, not empty, one line, not a novel.
pub(crate) fn group_name(value: Option<String>) -> Result<Option<String>> {
    match blank_to_none(value) {
        None => Ok(None),
        Some(name) if name.chars().any(char::is_control) => Err(invalid("groupPath", "control")),
        Some(name) if name.chars().count() > 80 => Err(invalid("groupPath", "too-long")),
        Some(name) => Ok(Some(name)),
    }
}

/// Settings as a form or a file had them, brought into range: an unknown
/// display mode is `fit`, sizes stay within what RDP can carry, and a gateway
/// without an address is no gateway.
pub fn normalize_rdp(mut rdp: RdpSettings) -> RdpSettings {
    if !matches!(rdp.display.as_str(), "fit" | "fixed" | "fullscreen") {
        rdp.display = "fit".into();
    }
    rdp.width = rdp.width.clamp(200, 8192);
    rdp.height = rdp.height.clamp(200, 8192);
    if !matches!(rdp.color_depth, 15 | 16 | 24 | 32) {
        rdp.color_depth = 32;
    }
    if !matches!(rdp.audio.as_str(), "local" | "remote" | "off") {
        rdp.audio = "local".into();
    }
    if let Some(gateway) = &mut rdp.gateway {
        gateway.address = gateway.address.trim().to_string();
        if gateway.port == 0 {
            gateway.port = 443;
        }
    }
    if rdp
        .gateway
        .as_ref()
        .is_some_and(|gateway| gateway.address.is_empty())
    {
        rdp.gateway = None;
    }
    rdp
}

fn rdp_to_text(rdp: &RdpSettings) -> Result<String> {
    serde_json::to_string(rdp).map_err(|_| invalid("rdp", "unserialisable"))
}

pub(crate) fn rdp_from_text(text: Option<String>) -> RdpSettings {
    text.and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

impl HostDraft {
    fn validated(self) -> Result<Self> {
        let address = self.address.trim().to_string();
        if address.is_empty() {
            return Err(invalid("address", "required"));
        }
        if address.chars().any(char::is_whitespace) {
            return Err(invalid("address", "whitespace"));
        }
        if self.port == 0 {
            return Err(invalid("port", "out-of-range"));
        }
        let name = match self.name.trim() {
            "" => address.clone(),
            name => name.to_string(),
        };
        if self.comment.chars().count() > 4000 {
            return Err(invalid("comment", "too-long"));
        }
        let rdp = normalize_rdp(self.rdp);
        if rdp
            .gateway
            .as_ref()
            .is_some_and(|g| g.address.chars().any(char::is_whitespace))
        {
            return Err(invalid("gatewayAddress", "whitespace"));
        }
        let comment = self.comment.trim().to_string();
        Ok(Self {
            name,
            address,
            group_path: group_name(self.group_path)?,
            rdp,
            comment,
            ..self
        })
    }
}

impl Store {
    pub fn list_hosts(&self) -> Result<Vec<HostRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&format!(
            "{HOST_SELECT} WHERE h.deleted = 0
              ORDER BY h.workspace, lower(coalesce(g.name, '')), h.position, lower(h.name)"
        ))?;
        let rows = stmt
            .query_map([], host_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        host_from_row_list(&conn, rows)
    }

    pub fn get_host(&self, id: Uuid) -> Result<Option<HostRecord>> {
        read_host(&self.conn.lock(), id)
    }

    /// Insert a new host, or update an existing one when the draft has an id.
    ///
    /// Setting a password seals it in the vault, so that needs the vault
    /// unlocked — checked before anything is written.
    pub fn save_host(&self, draft: HostDraft) -> Result<HostRecord> {
        let draft = draft.validated()?;
        // Connection first, vault second: the order every other path takes.
        let mut conn = self.conn.lock();
        let vault_guard = self.vault.lock();
        let sets_password = matches!(draft.password, PasswordChange::Set { .. })
            || matches!(draft.gateway_password, PasswordChange::Set { .. });
        if sets_password && vault_guard.is_none() {
            return Err(StoreError::VaultLocked);
        }

        let tx = conn.transaction()?;
        let clock = tick(&tx, self.device)?;
        let vault = vault_id(&tx)?;

        // Where the host is now, if it exists: an edit that keeps workspace
        // and group keeps its position too.
        #[allow(clippy::type_complexity)]
        let existing: Option<(
            Option<String>,
            Option<String>,
            String,
            Option<String>,
            i64,
        )> = match draft.id {
            Some(id) => Some(
                tx.query_row(
                    "SELECT identity_id, gateway_identity_id, workspace, group_id, position
                           FROM hosts WHERE id = ?1 AND deleted = 0",
                    [id.to_string()],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .optional()?
                .ok_or(StoreError::UnknownHost(draft.id.unwrap_or_default()))?,
            ),
            None => None,
        };

        let workspace = draft
            .workspace
            .or_else(|| existing.as_ref().map(|(_, _, w, _, _)| Workspace::parse(w)))
            .unwrap_or_default();
        let group = match &draft.group_path {
            Some(name) => Some(ensure_group(&tx, self.device, &vault, workspace, name)?),
            None => None,
        };
        let position = match &existing {
            Some((_, _, w, g, position)) if Workspace::parse(w) == workspace && *g == group => {
                *position
            }
            _ => next_position(&tx, workspace, group.as_deref())?,
        };

        let login = apply_login(
            &tx,
            self.device,
            vault_guard.as_ref(),
            &vault,
            existing.as_ref().and_then(|(i, ..)| i.as_deref()),
            LoginChange {
                username: &draft.username,
                domain: &draft.domain,
                password: &draft.password,
            },
            "username",
            clock,
        )?;
        let gateway_login = apply_login(
            &tx,
            self.device,
            vault_guard.as_ref(),
            &vault,
            existing.as_ref().and_then(|(_, g, ..)| g.as_deref()),
            LoginChange {
                username: &draft.gateway_username,
                domain: &draft.gateway_domain,
                password: &draft.gateway_password,
            },
            "gatewayUsername",
            clock,
        )?;
        drop(vault_guard);

        let rdp = rdp_to_text(&draft.rdp)?;
        let id = match (draft.id, existing.is_some()) {
            (Some(id), true) => {
                tx.execute(
                    "UPDATE hosts
                        SET name = ?2, address = ?3, port = ?4, identity_id = ?5, group_id = ?6,
                            workspace = ?7, position = ?8, rdp = ?9, comment = ?10,
                            gateway_identity_id = ?11,
                            dirty = 1, hlc_wall_ms = ?12, hlc_counter = ?13, hlc_device = ?14,
                            rev = rev + 1
                      WHERE id = ?1",
                    params![
                        id.to_string(),
                        draft.name,
                        draft.address,
                        draft.port,
                        login.identity,
                        group,
                        workspace.as_str(),
                        position,
                        rdp,
                        draft.comment,
                        gateway_login.identity,
                        clock.wall_ms as i64,
                        clock.counter,
                        clock.device,
                    ],
                )?;
                id
            }
            _ => {
                let id = Uuid::now_v7();
                tx.execute(
                    "INSERT INTO hosts
                        (id, vault_id, name, address, port, identity_id, group_id,
                         workspace, position, rdp, comment, gateway_identity_id,
                         hlc_wall_ms, hlc_counter, hlc_device)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                    params![
                        id.to_string(),
                        vault,
                        draft.name,
                        draft.address,
                        draft.port,
                        login.identity,
                        group,
                        workspace.as_str(),
                        position,
                        rdp,
                        draft.comment,
                        gateway_login.identity,
                        clock.wall_ms as i64,
                        clock.counter,
                        clock.device,
                    ],
                )?;
                id
            }
        };
        for released in [login.release, gateway_login.release].into_iter().flatten() {
            release_login(&tx, self.device, &released, clock)?;
        }

        let record = read_host(&tx, id)?.ok_or(StoreError::UnknownHost(id))?;
        tx.commit()?;
        let touched_secrets = !matches!(draft.password, PasswordChange::Keep)
            || !matches!(draft.gateway_password, PasswordChange::Keep);
        if touched_secrets {
            truncate_wal(&conn);
        }
        Ok(record)
    }

    /// Tombstone, not a hard delete: a device that was offline when this
    /// happened must learn about it on the next sync instead of resurrecting
    /// the host. Its logins, and their passwords, go with it.
    pub fn delete_host(&self, id: Uuid) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let clock = tick(&tx, self.device)?;

        let logins: Option<(Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT identity_id, gateway_identity_id FROM hosts
                  WHERE id = ?1 AND deleted = 0",
                [id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((login, gateway)) = logins else {
            return Err(StoreError::UnknownHost(id));
        };
        tx.execute(
            "UPDATE hosts
                SET deleted = 1, rev = rev + 1,
                    dirty = 1, hlc_wall_ms = ?2, hlc_counter = ?3, hlc_device = ?4
              WHERE id = ?1 AND deleted = 0",
            params![
                id.to_string(),
                clock.wall_ms as i64,
                clock.counter,
                clock.device
            ],
        )?;
        for identity in [login, gateway].into_iter().flatten() {
            release_login(&tx, self.device, &identity, clock)?;
        }
        tx.commit()?;
        truncate_wal(&conn);
        Ok(())
    }

    /// Give a host a login of its own after the connect dialog asked for one
    /// and the user ticked "save": username, domain and — with `Some` — the
    /// password. The rest of the host stays as it is.
    pub fn set_host_login(
        &self,
        id: Uuid,
        username: &str,
        domain: &str,
        password: Option<SecretText>,
    ) -> Result<HostRecord> {
        let change = match password {
            Some(value) => PasswordChange::Set { value },
            None => PasswordChange::Keep,
        };
        let mut conn = self.conn.lock();
        let vault_guard = self.vault.lock();
        if matches!(change, PasswordChange::Set { .. }) && vault_guard.is_none() {
            return Err(StoreError::VaultLocked);
        }
        let tx = conn.transaction()?;
        let current: Option<String> = tx
            .query_row(
                "SELECT identity_id FROM hosts WHERE id = ?1 AND deleted = 0",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StoreError::UnknownHost(id))?;
        let clock = tick(&tx, self.device)?;
        let vault = vault_id(&tx)?;
        let login = apply_login(
            &tx,
            self.device,
            vault_guard.as_ref(),
            &vault,
            current.as_deref(),
            LoginChange {
                username,
                domain,
                password: &change,
            },
            "username",
            clock,
        )?;
        drop(vault_guard);
        tx.execute(
            "UPDATE hosts
                SET identity_id = ?2, rev = rev + 1,
                    dirty = 1, hlc_wall_ms = ?3, hlc_counter = ?4, hlc_device = ?5
              WHERE id = ?1",
            params![
                id.to_string(),
                login.identity,
                clock.wall_ms as i64,
                clock.counter,
                clock.device
            ],
        )?;
        if let Some(released) = login.release {
            release_login(&tx, self.device, &released, clock)?;
        }
        let record = read_host(&tx, id)?.ok_or(StoreError::UnknownHost(id))?;
        tx.commit()?;
        truncate_wal(&conn);
        Ok(record)
    }

    /// Forget the password of a host's own login, keeping the username.
    pub fn forget_host_password(&self, id: Uuid) -> Result<HostRecord> {
        let host = self.get_host(id)?.ok_or(StoreError::UnknownHost(id))?;
        self.save_host(HostDraft {
            id: Some(id),
            name: host.name,
            address: host.address,
            port: host.port,
            username: host.username,
            domain: host.domain,
            group_path: host.group_path,
            workspace: Some(host.workspace),
            password: PasswordChange::Forget,
            rdp: host.rdp,
            comment: host.comment,
            gateway_username: host.gateway_username,
            gateway_domain: host.gateway_domain,
            gateway_password: PasswordChange::Keep,
        })
    }

    /// Record a successful connection. Local bookkeeping, so it neither bumps
    /// the revision nor ticks the sync clock.
    pub fn mark_connected(&self, id: Uuid) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE hosts SET last_connected_ms = ?2 WHERE id = ?1",
            params![id.to_string(), now_ms() as i64],
        )?;
        Ok(())
    }
}

/// A host with its group name. `nullif(g.name, '')` is for a group whose
/// record has not arrived from the server yet: it is a placeholder with an
/// empty name, and until the real one lands the host is simply shown without a
/// group.
pub(crate) const HOST_SELECT: &str = "SELECT h.id, h.name, h.address, h.port,
            h.identity_id, nullif(g.name, ''), h.last_connected_ms, h.workspace, h.position,
            h.rdp, h.comment, h.gateway_identity_id
       FROM hosts h
       LEFT JOIN host_groups g ON g.id = h.group_id AND g.deleted = 0";

pub(crate) fn read_host(conn: &Connection, id: Uuid) -> Result<Option<HostRecord>> {
    let row = conn
        .query_row(
            &format!("{HOST_SELECT} WHERE h.id = ?1 AND h.deleted = 0"),
            [id.to_string()],
            host_row,
        )
        .optional()?;
    row.map(|row| row.into_record(conn)).transpose()
}

fn parse_uuid(index: usize, text: &str) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// One row of [`HOST_SELECT`], before its logins are looked up.
pub(crate) struct HostRow {
    id: Uuid,
    name: String,
    address: String,
    port: u16,
    identity: Option<String>,
    group_path: Option<String>,
    last_connected_ms: Option<u64>,
    workspace: Workspace,
    position: i64,
    rdp: Option<String>,
    comment: String,
    gateway_identity: Option<String>,
}

pub(crate) fn host_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HostRow> {
    let id: String = row.get(0)?;
    let port: i64 = row.get(3)?;
    let last: Option<i64> = row.get(6)?;
    let workspace: String = row.get(7)?;
    Ok(HostRow {
        id: parse_uuid(0, &id)?,
        name: row.get(1)?,
        address: row.get(2)?,
        port: port as u16,
        identity: row.get(4)?,
        group_path: row.get(5)?,
        last_connected_ms: last.map(|v| v as u64),
        workspace: Workspace::parse(&workspace),
        position: row.get(8)?,
        rdp: row.get(9)?,
        comment: row.get(10)?,
        gateway_identity: row.get(11)?,
    })
}

impl HostRow {
    pub(crate) fn into_record(self, conn: &Connection) -> Result<HostRecord> {
        let login = read_login(conn, self.identity.as_deref())?;
        let gateway = read_login(conn, self.gateway_identity.as_deref())?;
        Ok(HostRecord {
            id: self.id,
            name: self.name,
            address: self.address,
            port: self.port,
            username: login.username,
            domain: login.domain,
            has_password: login.has_password,
            group_path: self.group_path,
            last_connected_ms: self.last_connected_ms,
            workspace: self.workspace,
            position: self.position,
            rdp: rdp_from_text(self.rdp),
            comment: self.comment,
            gateway_username: gateway.username,
            gateway_domain: gateway.domain,
            has_gateway_password: gateway.has_password,
        })
    }
}

fn host_from_row_list(conn: &Connection, rows: Vec<HostRow>) -> Result<Vec<HostRecord>> {
    rows.into_iter().map(|row| row.into_record(conn)).collect()
}

/// The id of a live group by workspace and name.
pub(crate) fn group_id(
    tx: &Transaction,
    workspace: Workspace,
    name: &str,
) -> Result<Option<String>> {
    Ok(tx
        .query_row(
            "SELECT id FROM host_groups
              WHERE workspace = ?1 AND name = ?2 AND deleted = 0
              ORDER BY id",
            params![workspace.as_str(), name],
            |row| row.get(0),
        )
        .optional()?)
}

/// The id of the group a host names, creating the record if it is new.
///
/// Hosts point at this id rather than at the name, so renaming a group is one
/// record rather than one per host — and, on two devices at once, one conflict
/// rather than one per host.
pub(crate) fn ensure_group(
    tx: &Transaction,
    device: u32,
    vault: &str,
    workspace: Workspace,
    name: &str,
) -> Result<String> {
    if let Some(id) = group_id(tx, workspace, name)? {
        return Ok(id);
    }
    let position: i64 = tx.query_row(
        "SELECT coalesce(max(position) + 1, 0) FROM host_groups
          WHERE workspace = ?1 AND deleted = 0",
        [workspace.as_str()],
        |row| row.get(0),
    )?;
    let clock = tick(tx, device)?;
    let id = Uuid::now_v7().to_string();
    tx.execute(
        "INSERT INTO host_groups
            (id, vault_id, workspace, name, position, hlc_wall_ms, hlc_counter, hlc_device)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            id,
            vault,
            workspace.as_str(),
            name,
            position,
            clock.wall_ms as i64,
            clock.counter,
            clock.device,
        ],
    )?;
    Ok(id)
}

/// The position after the last host of a group.
pub(crate) fn next_position(
    tx: &Transaction,
    workspace: Workspace,
    group: Option<&str>,
) -> Result<i64> {
    Ok(tx.query_row(
        "SELECT coalesce(max(position) + 1, 0) FROM hosts
          WHERE deleted = 0 AND workspace = ?1 AND group_id IS ?2",
        params![workspace.as_str(), group],
        |row| row.get(0),
    )?)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use uwurdp_vault::KdfParams;

    pub(crate) fn draft(name: &str, address: &str) -> HostDraft {
        HostDraft {
            id: None,
            name: name.into(),
            address: address.into(),
            port: 3389,
            username: "admin".into(),
            domain: String::new(),
            group_path: None,
            workspace: None,
            password: PasswordChange::Keep,
            rdp: RdpSettings::default(),
            comment: String::new(),
            gateway_username: String::new(),
            gateway_domain: String::new(),
            gateway_password: PasswordChange::Keep,
        }
    }

    pub(crate) fn set(value: &str) -> PasswordChange {
        PasswordChange::Set {
            value: SecretText::new(value.to_string()),
        }
    }

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    pub(crate) fn unlocked() -> Store {
        let store = store();
        store
            .create_vault_with(b"master", KdfParams::INSECURE_FOR_TESTS)
            .unwrap();
        store
    }

    pub(crate) fn count(store: &Store, sql: &str) -> i64 {
        store.conn.lock().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn live_secrets(store: &Store) -> i64 {
        count(store, "SELECT count(*) FROM secrets WHERE deleted = 0")
    }

    fn live_logins(store: &Store) -> i64 {
        count(store, "SELECT count(*) FROM identities WHERE deleted = 0")
    }

    #[test]
    fn a_saved_host_comes_back_as_saved() {
        let store = store();
        let saved = store.save_host(draft("dc-1", "10.0.0.12")).unwrap();
        assert_eq!(store.list_hosts().unwrap(), vec![saved.clone()]);
        assert_eq!(store.get_host(saved.id).unwrap(), Some(saved.clone()));
        assert_eq!(saved.workspace, Workspace::Private);
        assert_eq!(saved.username, "admin");
        assert_eq!(saved.rdp, RdpSettings::default());
        assert!(!saved.has_password);
    }

    #[test]
    fn an_empty_name_defaults_to_the_address() {
        let saved = store().save_host(draft("  ", "ts.lan")).unwrap();
        assert_eq!(saved.name, "ts.lan");
    }

    #[test]
    fn rdp_settings_and_the_comment_are_kept_and_brought_into_range() {
        let store = store();
        let mut host = draft("app", "10.0.0.3");
        host.rdp.display = "fixed".into();
        host.rdp.width = 1280;
        host.rdp.height = 50;
        host.rdp.color_depth = 7;
        host.rdp.audio = "loud".into();
        host.rdp.admin = true;
        host.comment = "  the old ERP box  ".into();
        let saved = store.save_host(host).unwrap();
        assert_eq!(saved.rdp.display, "fixed");
        assert_eq!((saved.rdp.width, saved.rdp.height), (1280, 200));
        assert_eq!(saved.rdp.color_depth, 32);
        assert_eq!(saved.rdp.audio, "local");
        assert!(saved.rdp.admin);
        assert_eq!(saved.comment, "the old ERP box");
    }

    #[test]
    fn updating_keeps_the_id_and_changes_the_fields() {
        let store = store();
        let saved = store.save_host(draft("old", "10.0.0.1")).unwrap();

        let mut edit = draft("new", "10.0.0.2");
        edit.id = Some(saved.id);
        edit.port = 3390;
        edit.username = "lorin".into();
        edit.domain = "CORP".into();
        let updated = store.save_host(edit).unwrap();

        assert_eq!(updated.id, saved.id);
        assert_eq!(
            (
                updated.name.as_str(),
                updated.address.as_str(),
                updated.port
            ),
            ("new", "10.0.0.2", 3390)
        );
        assert_eq!(
            (updated.username.as_str(), updated.domain.as_str()),
            ("lorin", "CORP")
        );
        assert_eq!(
            store.list_hosts().unwrap().len(),
            1,
            "an update must not insert"
        );
        assert_eq!(live_logins(&store), 1, "the login is edited, not replaced");
    }

    #[test]
    fn a_host_without_a_username_has_no_login_of_its_own() {
        let store = unlocked();
        let mut host = draft("ws", "10.0.0.4");
        host.password = set("secret");
        let saved = store.save_host(host).unwrap();
        assert_eq!(live_logins(&store), 1);
        assert_eq!(live_secrets(&store), 1);

        let mut edit = draft("ws", "10.0.0.4");
        edit.id = Some(saved.id);
        edit.username = "  ".into();
        let edited = store.save_host(edit).unwrap();
        assert_eq!(edited.username, "");
        assert!(!edited.has_password);
        assert_eq!(live_logins(&store), 0, "the dropped login is a tombstone");
        assert_eq!(live_secrets(&store), 0, "and so is its password");
    }

    #[test]
    fn a_password_needs_a_username() {
        let store = unlocked();
        let mut host = draft("ws", "10.0.0.4");
        host.username = String::new();
        host.password = set("secret");
        assert!(matches!(
            store.save_host(host),
            Err(StoreError::Invalid {
                field: "username",
                problem: "required"
            })
        ));
    }

    #[test]
    fn updates_bump_the_revision_for_sync() {
        let store = store();
        let saved = store.save_host(draft("a", "10.0.0.1")).unwrap();
        let mut edit = draft("b", "10.0.0.1");
        edit.id = Some(saved.id);
        store.save_host(edit).unwrap();
        let rev: i64 = store
            .conn
            .lock()
            .query_row(
                "SELECT rev FROM hosts WHERE id = ?1",
                [saved.id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rev, 2);
    }

    #[test]
    fn validation_names_the_field_that_is_wrong() {
        let store = store();

        let spaced = draft("x", "10.0.0 .1");
        assert!(matches!(
            store.save_host(spaced),
            Err(StoreError::Invalid {
                field: "address",
                problem: "whitespace"
            })
        ));

        let mut port_zero = draft("x", "10.0.0.1");
        port_zero.port = 0;
        assert!(matches!(
            store.save_host(port_zero),
            Err(StoreError::Invalid {
                field: "port",
                problem: "out-of-range"
            })
        ));

        let mut spaced_domain = draft("x", "10.0.0.1");
        spaced_domain.domain = "CO RP".into();
        assert!(matches!(
            store.save_host(spaced_domain),
            Err(StoreError::Invalid {
                field: "domain",
                problem: "whitespace"
            })
        ));
    }

    #[test]
    fn a_username_with_a_space_is_a_windows_username() {
        let mut host = draft("x", "10.0.0.1");
        host.username = "Lorin Mini".into();
        let saved = store().save_host(host).unwrap();
        assert_eq!(saved.username, "Lorin Mini");
    }

    #[test]
    fn deleted_hosts_disappear_but_stay_as_tombstones_with_their_logins() {
        let store = unlocked();
        let mut host = draft("gone", "10.0.0.1");
        host.password = set("pw");
        host.gateway_username = "gw".into();
        host.gateway_password = set("gwpw");
        host.rdp.gateway = Some(uwurdp_proto::GatewaySettings {
            address: "gw.example.com".into(),
            ..Default::default()
        });
        let saved = store.save_host(host).unwrap();
        assert_eq!(saved.gateway_username, "gw");
        assert!(saved.has_gateway_password);
        assert_eq!(saved.rdp.gateway.as_ref().map(|g| g.port), Some(443));
        assert_eq!(live_logins(&store), 2);
        assert_eq!(live_secrets(&store), 2);

        store.delete_host(saved.id).unwrap();
        assert!(store.list_hosts().unwrap().is_empty());
        assert_eq!(
            count(&store, "SELECT count(*) FROM hosts WHERE deleted = 1"),
            1
        );
        assert_eq!(live_logins(&store), 0);
        assert_eq!(live_secrets(&store), 0);
        assert!(matches!(
            store.delete_host(saved.id),
            Err(StoreError::UnknownHost(_))
        ));
    }

    #[test]
    fn marking_a_connection_does_not_count_as_an_edit() {
        let store = store();
        let saved = store.save_host(draft("a", "10.0.0.1")).unwrap();
        store.mark_connected(saved.id).unwrap();
        let host = store.get_host(saved.id).unwrap().unwrap();
        assert!(host.last_connected_ms.is_some());
        assert_eq!(count(&store, "SELECT rev FROM hosts"), 1);
    }

    #[test]
    fn setting_a_password_needs_the_vault_and_writes_nothing_without_it() {
        let store = store();
        let mut host = draft("a", "10.0.0.1");
        host.password = set("pw");
        assert!(matches!(
            store.save_host(host),
            Err(StoreError::VaultLocked)
        ));
        assert!(store.list_hosts().unwrap().is_empty());
    }

    #[test]
    fn keeping_replacing_and_forgetting_a_password() {
        let store = unlocked();
        let mut host = draft("a", "10.0.0.1");
        host.password = set("one");
        let saved = store.save_host(host).unwrap();
        assert_eq!(&**store.reveal_host_password(saved.id).unwrap(), b"one");

        let mut keep = draft("a", "10.0.0.1");
        keep.id = Some(saved.id);
        store.save_host(keep).unwrap();
        assert_eq!(&**store.reveal_host_password(saved.id).unwrap(), b"one");

        let mut replace = draft("a", "10.0.0.1");
        replace.id = Some(saved.id);
        replace.password = set("two");
        store.save_host(replace).unwrap();
        assert_eq!(&**store.reveal_host_password(saved.id).unwrap(), b"two");
        assert_eq!(live_secrets(&store), 1, "the old one is gone");

        let forgotten = store.forget_host_password(saved.id).unwrap();
        assert!(!forgotten.has_password);
        assert_eq!(forgotten.username, "admin");
        assert_eq!(live_secrets(&store), 0);
    }

    #[test]
    fn the_connect_dialog_can_give_a_host_its_login() {
        let store = unlocked();
        let mut host = draft("a", "10.0.0.1");
        host.username = String::new();
        let saved = store.save_host(host).unwrap();
        let updated = store
            .set_host_login(
                saved.id,
                "lorin",
                "CORP",
                Some(SecretText::new("pw".to_string())),
            )
            .unwrap();
        assert_eq!(
            (updated.username.as_str(), updated.domain.as_str()),
            ("lorin", "CORP")
        );
        assert!(updated.has_password);
    }

    #[test]
    fn a_shared_login_is_copied_before_it_changes() {
        let store = unlocked();
        let mut one = draft("one", "10.0.0.1");
        one.password = set("pw");
        let one = store.save_host(one).unwrap();
        let two = store.save_host(draft("two", "10.0.0.2")).unwrap();
        // What an import or an older build may leave: two hosts, one login.
        {
            let conn = store.conn.lock();
            conn.execute(
                "UPDATE hosts SET identity_id = (SELECT identity_id FROM hosts WHERE id = ?1)
                  WHERE id = ?2",
                [one.id.to_string(), two.id.to_string()],
            )
            .unwrap();
        }
        let mut edit = draft("two", "10.0.0.2");
        edit.id = Some(two.id);
        edit.username = "other".into();
        store.save_host(edit).unwrap();

        assert_eq!(store.get_host(one.id).unwrap().unwrap().username, "admin");
        let two = store.get_host(two.id).unwrap().unwrap();
        assert_eq!(two.username, "other");
        assert!(two.has_password, "the copy keeps the password");
        assert_eq!(&**store.reveal_host_password(one.id).unwrap(), b"pw");
    }
}
