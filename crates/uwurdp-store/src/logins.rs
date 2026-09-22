//! Logins: a username, a Windows domain and maybe a password in the vault.
//!
//! A login is an `identities` row, and three things point at one: a host (its
//! own login), a group (the login its hosts use unless they have their own —
//! RDCMan's "inherit from parent") and a host's gateway. Every owner gets a
//! login of its own; only an import or an older build may leave one shared,
//! and then the first edit copies it, so changing one host never changes
//! another's login.
//!
//! A login without a username is no login: clearing the username in a form
//! drops it, and the owner inherits (a host) or asks (a gateway) again.

use crate::hosts::PasswordChange;
use crate::vault::{forget_secret, seal_secret};
use crate::{Result, StoreError};
use rusqlite::{params, OptionalExtension, Transaction};
use uuid::Uuid;
use uwurdp_proto::Hlc;
use uwurdp_vault::UnlockedVault;

/// What a form says about one login.
pub(crate) struct LoginChange<'a> {
    pub username: &'a str,
    pub domain: &'a str,
    pub password: &'a PasswordChange,
}

/// The login an owner points at after a change, and the one it pointed at
/// before if it has to be let go. The caller points the owner at the first,
/// then hands the second to [`release_login`] — in that order, so a login is
/// only ever tombstoned once nothing points at it.
pub(crate) struct Applied {
    pub identity: Option<String>,
    pub release: Option<String>,
}

fn invalid(field: &'static str, problem: &'static str) -> StoreError {
    StoreError::Invalid { field, problem }
}

/// A username as typed. Windows allows spaces in account names, so only
/// control characters and absurd lengths are refused.
pub(crate) fn clean_username(field: &'static str, value: &str) -> Result<String> {
    let value = value.trim();
    if value.chars().any(char::is_control) {
        return Err(invalid(field, "control"));
    }
    if value.chars().count() > 256 {
        return Err(invalid(field, "too-long"));
    }
    Ok(value.to_string())
}

/// A domain as typed: `CORP`, `corp.example.com`, or empty for a local account.
pub(crate) fn clean_domain(field: &'static str, value: &str) -> Result<String> {
    let value = value.trim();
    if value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(invalid(field, "whitespace"));
    }
    if value.chars().count() > 255 {
        return Err(invalid(field, "too-long"));
    }
    Ok(value.to_string())
}

/// How many live rows point at a login.
pub(crate) fn references(tx: &Transaction, identity: &str) -> Result<i64> {
    Ok(tx.query_row(
        "SELECT (SELECT count(*) FROM hosts WHERE identity_id = ?1 AND deleted = 0)
              + (SELECT count(*) FROM hosts WHERE gateway_identity_id = ?1 AND deleted = 0)
              + (SELECT count(*) FROM host_groups WHERE identity_id = ?1 AND deleted = 0)",
        [identity],
        |row| row.get(0),
    )?)
}

fn label_of(username: &str, domain: &str) -> String {
    if domain.is_empty() {
        username.to_string()
    } else {
        format!("{domain}\\{username}")
    }
}

/// Apply what a form says to the login `current` (the owner's login today, if
/// it has one). `username_field` names the input a validation error belongs
/// to. A `Set` password needs `vault`; the caller checks that up front, before
/// anything is written.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_login(
    tx: &Transaction,
    device: u32,
    vault: Option<&UnlockedVault>,
    vault_id: &str,
    current: Option<&str>,
    change: LoginChange<'_>,
    username_field: &'static str,
    clock: Hlc,
) -> Result<Applied> {
    let username = clean_username(username_field, change.username)?;
    let domain = clean_domain(
        if username_field == "username" {
            "domain"
        } else {
            "gatewayDomain"
        },
        change.domain,
    )?;

    if let PasswordChange::Set { value } = change.password {
        if value.expose().is_empty() {
            return Err(invalid("password", "required"));
        }
        if username.is_empty() {
            return Err(invalid(username_field, "required"));
        }
    }

    if username.is_empty() {
        return Ok(Applied {
            identity: None,
            release: current.map(str::to_string),
        });
    }

    let old_secret: Option<String> = match current {
        Some(identity) => tx
            .query_row(
                "SELECT password_secret_id FROM identities WHERE id = ?1",
                [identity],
                |row| row.get(0),
            )
            .optional()?
            .flatten(),
        None => None,
    };
    let secret = match change.password {
        PasswordChange::Keep => old_secret.clone(),
        PasswordChange::Set { value } => {
            let vault = vault.ok_or(StoreError::VaultLocked)?;
            Some(seal_secret(tx, device, vault, value.expose().as_bytes())?)
        }
        PasswordChange::Forget => None,
    };
    let label = label_of(&username, &domain);

    let shared = match current {
        Some(identity) => references(tx, identity)? > 1,
        None => false,
    };
    let applied = match current {
        Some(identity) if !shared => {
            tx.execute(
                "UPDATE identities
                    SET label = ?2, username = ?3, domain = ?4, auth_type = 'password',
                        key_path = NULL, key_id = NULL, password_secret_id = ?5,
                        dirty = 1, hlc_wall_ms = ?6, hlc_counter = ?7, hlc_device = ?8,
                        rev = rev + 1
                  WHERE id = ?1",
                params![
                    identity,
                    label,
                    username,
                    domain,
                    secret,
                    clock.wall_ms as i64,
                    clock.counter,
                    clock.device,
                ],
            )?;
            Applied {
                identity: Some(identity.to_string()),
                release: None,
            }
        }
        // A shared login stays as it is for the others; this owner gets a
        // copy with the change.
        _ => Applied {
            identity: Some(insert_login(
                tx,
                vault_id,
                &label,
                &username,
                &domain,
                secret.as_deref(),
                clock,
            )?),
            release: None,
        },
    };

    // A replaced password goes once nothing points at it any more.
    if !matches!(change.password, PasswordChange::Keep) {
        if let Some(old) = old_secret.filter(|old| Some(old) != secret.as_ref()) {
            release_secret(tx, device, &old)?;
        }
    }
    Ok(applied)
}

pub(crate) fn insert_login(
    tx: &Transaction,
    vault_id: &str,
    label: &str,
    username: &str,
    domain: &str,
    secret: Option<&str>,
    clock: Hlc,
) -> Result<String> {
    let id = Uuid::now_v7().to_string();
    tx.execute(
        "INSERT INTO identities
            (id, vault_id, label, username, domain, auth_type, password_secret_id,
             hlc_wall_ms, hlc_counter, hlc_device)
         VALUES (?1, ?2, ?3, ?4, ?5, 'password', ?6, ?7, ?8, ?9)",
        params![
            id,
            vault_id,
            label,
            username,
            domain,
            secret,
            clock.wall_ms as i64,
            clock.counter,
            clock.device,
        ],
    )?;
    Ok(id)
}

/// Tombstone a login nothing points at any more, and its password with it.
/// Still in use: left alone.
pub(crate) fn release_login(
    tx: &Transaction,
    device: u32,
    identity: &str,
    clock: Hlc,
) -> Result<()> {
    if references(tx, identity)? > 0 {
        return Ok(());
    }
    let secret: Option<String> = tx
        .query_row(
            "SELECT password_secret_id FROM identities WHERE id = ?1 AND deleted = 0",
            [identity],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let changed = tx.execute(
        "UPDATE identities
            SET deleted = 1, rev = rev + 1,
                dirty = 1, hlc_wall_ms = ?2, hlc_counter = ?3, hlc_device = ?4
          WHERE id = ?1 AND deleted = 0",
        params![identity, clock.wall_ms as i64, clock.counter, clock.device],
    )?;
    if changed > 0 {
        if let Some(secret) = secret {
            release_secret(tx, device, &secret)?;
        }
    }
    Ok(())
}

/// Forget a password once no login uses it any more. A copied login shares
/// its original's password until one of them changes it.
pub(crate) fn release_secret(tx: &Transaction, device: u32, secret: &str) -> Result<()> {
    let used: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM identities
                         WHERE password_secret_id = ?1 AND deleted = 0)",
        [secret],
        |row| row.get(0),
    )?;
    if used {
        Ok(())
    } else {
        forget_secret(tx, device, secret)
    }
}

/// A login as the interface shows it: nothing secret, only whether a
/// password is stored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LoginView {
    pub username: String,
    pub domain: String,
    pub has_password: bool,
}

pub(crate) fn read_login(tx: &rusqlite::Connection, identity: Option<&str>) -> Result<LoginView> {
    let Some(identity) = identity else {
        return Ok(LoginView::default());
    };
    Ok(tx
        .query_row(
            "SELECT username, domain, password_secret_id IS NOT NULL
               FROM identities WHERE id = ?1 AND deleted = 0",
            [identity],
            |row| {
                Ok(LoginView {
                    username: row.get(0)?,
                    domain: row.get(1)?,
                    has_password: row.get(2)?,
                })
            },
        )
        .optional()?
        .unwrap_or_default())
}
