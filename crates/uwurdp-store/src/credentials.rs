//! Which login a host connects with.
//!
//! A host's own login wins; without one, its group's; without that, the
//! connect dialog asks. That is RDCMan's "inherit from parent", one level deep.
//! The connect path asks [`Store::host_login`] what applies and only reveals a
//! password when it is actually connecting, through
//! [`Store::reveal_login_password`], which fails while the vault is locked.

use crate::vault::Revealed;
use crate::{Result, Store, StoreError};
use rusqlite::OptionalExtension;
use serde::Serialize;
use uuid::Uuid;

/// A login as it applies to one connection. Carries nothing secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedLogin {
    pub username: String,
    pub domain: String,
    pub has_password: bool,
    /// Handed down by the host's group rather than the host's own.
    pub from_group: bool,
    #[serde(skip)]
    identity: Uuid,
    #[serde(skip)]
    secret: Option<Uuid>,
}

type LoginRow = (String, String, String, Option<String>);

fn resolved(row: LoginRow, from_group: bool) -> Result<ResolvedLogin> {
    let (identity, username, domain, secret) = row;
    let parse = |text: &str| {
        Uuid::parse_str(text).map_err(|_| StoreError::Invalid {
            field: "identity",
            problem: "damaged",
        })
    };
    Ok(ResolvedLogin {
        username,
        domain,
        has_password: secret.is_some(),
        from_group,
        identity: parse(&identity)?,
        secret: secret.as_deref().map(parse).transpose()?,
    })
}

impl Store {
    fn login_row(&self, sql: &str, host_id: Uuid) -> Result<Option<LoginRow>> {
        Ok(self
            .conn
            .lock()
            .query_row(sql, [host_id.to_string()], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .optional()?)
    }

    fn check_host(&self, host_id: Uuid) -> Result<()> {
        let live: bool = self.conn.lock().query_row(
            "SELECT EXISTS (SELECT 1 FROM hosts WHERE id = ?1 AND deleted = 0)",
            [host_id.to_string()],
            |row| row.get(0),
        )?;
        if live {
            Ok(())
        } else {
            Err(StoreError::UnknownHost(host_id))
        }
    }

    /// The login a host connects with: its own, else its group's, else none
    /// (the connect dialog asks).
    pub fn host_login(&self, host_id: Uuid) -> Result<Option<ResolvedLogin>> {
        self.check_host(host_id)?;
        let own = self.login_row(
            "SELECT i.id, i.username, i.domain, i.password_secret_id
               FROM hosts h JOIN identities i ON i.id = h.identity_id AND i.deleted = 0
              WHERE h.id = ?1 AND h.deleted = 0 AND i.username != ''",
            host_id,
        )?;
        if let Some(row) = own {
            return resolved(row, false).map(Some);
        }
        let group = self.login_row(
            "SELECT i.id, i.username, i.domain, i.password_secret_id
               FROM hosts h
               JOIN host_groups g ON g.id = h.group_id AND g.deleted = 0
               JOIN identities i ON i.id = g.identity_id AND i.deleted = 0
              WHERE h.id = ?1 AND h.deleted = 0 AND i.username != ''",
            host_id,
        )?;
        group.map(|row| resolved(row, true)).transpose()
    }

    /// A login by its id, as a group or an export names it.
    pub(crate) fn login_by_identity(&self, identity: &str) -> Result<Option<ResolvedLogin>> {
        let row: Option<LoginRow> = self
            .conn
            .lock()
            .query_row(
                "SELECT id, username, domain, password_secret_id FROM identities
                  WHERE id = ?1 AND deleted = 0 AND username != ''",
                [identity],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        row.map(|row| resolved(row, false)).transpose()
    }

    /// The login for a host's gateway, when it has one of its own.
    pub fn host_gateway_login(&self, host_id: Uuid) -> Result<Option<ResolvedLogin>> {
        self.check_host(host_id)?;
        let row = self.login_row(
            "SELECT i.id, i.username, i.domain, i.password_secret_id
               FROM hosts h JOIN identities i ON i.id = h.gateway_identity_id AND i.deleted = 0
              WHERE h.id = ?1 AND h.deleted = 0 AND i.username != ''",
            host_id,
        )?;
        row.map(|row| resolved(row, false)).transpose()
    }

    /// The password stored with a login, if it has one. Fails while the vault
    /// is locked.
    pub fn reveal_login_password(&self, login: &ResolvedLogin) -> Result<Option<Revealed>> {
        // The login may have changed since it was resolved; only a secret it
        // still points at is revealed.
        let current: Option<String> = self
            .conn
            .lock()
            .query_row(
                "SELECT password_secret_id FROM identities WHERE id = ?1 AND deleted = 0",
                [login.identity.to_string()],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        match (login.secret, current) {
            (Some(secret), Some(current)) if current == secret.to_string() => {
                self.reveal_secret(secret).map(Some)
            }
            _ => Ok(None),
        }
    }

    /// The stored password of a host's own login. Fails if the vault is
    /// locked or the host has none.
    pub fn reveal_host_password(&self, host_id: Uuid) -> Result<Revealed> {
        let login = self
            .host_login(host_id)?
            .filter(|login| !login.from_group)
            .ok_or(StoreError::UnknownHost(host_id))?;
        self.reveal_login_password(&login)?
            .ok_or(StoreError::UnknownHost(host_id))
    }
}

#[cfg(test)]
mod tests {
    use crate::hosts::tests::{draft, set, unlocked};
    use crate::hosts::Workspace;
    use crate::PasswordChange;

    #[test]
    fn a_host_uses_its_own_login_then_its_groups_then_none() {
        let store = unlocked();
        let mut own = draft("own", "10.0.0.1");
        own.group_path = Some("Servers".into());
        own.password = set("own-pw");
        let own = store.save_host(own).unwrap();

        let mut inherits = draft("inherits", "10.0.0.2");
        inherits.username = String::new();
        inherits.group_path = Some("Servers".into());
        let inherits = store.save_host(inherits).unwrap();

        let mut alone = draft("alone", "10.0.0.3");
        alone.username = String::new();
        let alone = store.save_host(alone).unwrap();

        assert!(store.host_login(inherits.id).unwrap().is_none());
        store
            .set_group_login(
                Workspace::Private,
                "Servers",
                "svc",
                "CORP",
                &set("group-pw"),
            )
            .unwrap();

        let login = store.host_login(own.id).unwrap().unwrap();
        assert_eq!(login.username, "admin");
        assert!(!login.from_group);
        let revealed = store.reveal_login_password(&login).unwrap().unwrap();
        assert_eq!(&**revealed, b"own-pw");

        let login = store.host_login(inherits.id).unwrap().unwrap();
        assert_eq!(
            (login.username.as_str(), login.domain.as_str()),
            ("svc", "CORP")
        );
        assert!(login.from_group);
        let revealed = store.reveal_login_password(&login).unwrap().unwrap();
        assert_eq!(&**revealed, b"group-pw");

        assert!(store.host_login(alone.id).unwrap().is_none());
    }

    #[test]
    fn a_password_changed_after_resolving_is_not_revealed_stale() {
        let store = unlocked();
        let mut host = draft("a", "10.0.0.1");
        host.password = set("one");
        let host = store.save_host(host).unwrap();
        let login = store.host_login(host.id).unwrap().unwrap();

        let mut edit = draft("a", "10.0.0.1");
        edit.id = Some(host.id);
        edit.password = PasswordChange::Forget;
        store.save_host(edit).unwrap();
        assert!(store.reveal_login_password(&login).unwrap().is_none());
    }

    #[test]
    fn a_gateway_login_is_its_own() {
        let store = unlocked();
        let mut host = draft("a", "10.0.0.1");
        host.gateway_username = "gw-user".into();
        host.gateway_password = set("gw-pw");
        let host = store.save_host(host).unwrap();
        let gateway = store.host_gateway_login(host.id).unwrap().unwrap();
        assert_eq!(gateway.username, "gw-user");
        let revealed = store.reveal_login_password(&gateway).unwrap().unwrap();
        assert_eq!(&**revealed, b"gw-pw");
    }
}
