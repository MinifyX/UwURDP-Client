//! Groups, workspaces and the order of the host list.
//!
//! A group is a record in `host_groups` and a host points at it by id, so an
//! empty group stays, groups keep the order they were dragged into, and
//! renaming one is a single record instead of one per host. Names stay the
//! handle the user interface uses — nobody drags an id around — so every call
//! here takes names and resolves them on the way in.
//!
//! A group can hand a login to its hosts — every host without a login of its
//! own connects with it, like RDCMan's "inherit from parent". See
//! [`Store::set_group_login`].
//!
//! Every move is one call — a host onto a group, between two hosts, into the
//! other workspace — and renumbers what it touched in one transaction. Only
//! rows whose place actually changed get a new revision, so a drag doesn't turn
//! into a sync of the whole list.

use crate::hosts::{
    ensure_group, group_id, group_name, read_host, HostRecord, PasswordChange, Workspace,
};
use crate::logins::{apply_login, read_login, release_login, LoginChange};
use crate::vault::truncate_wal;
use crate::{tick, vault_id, Result, Store, StoreError};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupRecord {
    pub workspace: Workspace,
    pub name: String,
    pub position: i64,
    /// The login the group hands to its hosts; empty for none.
    pub username: String,
    pub domain: String,
    pub has_password: bool,
}

fn required(name: &str) -> Result<String> {
    group_name(Some(name.to_string()))?.ok_or(StoreError::Invalid {
        field: "groupPath",
        problem: "required",
    })
}

fn unknown_group() -> StoreError {
    StoreError::Invalid {
        field: "groupPath",
        problem: "unknown",
    }
}

impl Store {
    /// Every group, in order, per workspace. A placeholder for a group whose
    /// record has not arrived from the server yet carries no name and is left
    /// out until it does.
    pub fn list_groups(&self) -> Result<Vec<GroupRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT workspace, name, position, identity_id FROM host_groups
              WHERE deleted = 0 AND name != ''
              ORDER BY workspace, position, lower(name)",
        )?;
        let rows = stmt
            .query_map([], |row| {
                let workspace: String = row.get(0)?;
                Ok((
                    Workspace::parse(&workspace),
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        rows.into_iter()
            .map(|(workspace, name, position, identity)| {
                let login = read_login(&conn, identity.as_deref())?;
                Ok(GroupRecord {
                    workspace,
                    name,
                    position,
                    username: login.username,
                    domain: login.domain,
                    has_password: login.has_password,
                })
            })
            .collect()
    }

    pub fn create_group(&self, workspace: Workspace, name: &str) -> Result<GroupRecord> {
        let name = required(name)?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        if group_id(&tx, workspace, &name)?.is_some() {
            return Err(StoreError::Invalid {
                field: "groupPath",
                problem: "exists",
            });
        }
        let vault = vault_id(&tx)?;
        let id = ensure_group(&tx, self.device, &vault, workspace, &name)?;
        let position = tx.query_row(
            "SELECT position FROM host_groups WHERE id = ?1",
            [&id],
            |row| row.get(0),
        )?;
        tx.commit()?;
        Ok(GroupRecord {
            workspace,
            name,
            position,
            username: String::new(),
            domain: String::new(),
            has_password: false,
        })
    }

    /// Set the login a group hands to its hosts, or drop it with an empty
    /// username. A `Set` password needs the vault unlocked.
    pub fn set_group_login(
        &self,
        workspace: Workspace,
        name: &str,
        username: &str,
        domain: &str,
        password: &PasswordChange,
    ) -> Result<()> {
        let mut conn = self.conn.lock();
        let vault_guard = self.vault.lock();
        if matches!(password, PasswordChange::Set { .. }) && vault_guard.is_none() {
            return Err(StoreError::VaultLocked);
        }
        let tx = conn.transaction()?;
        let Some(id) = group_id(&tx, workspace, name)? else {
            return Err(unknown_group());
        };
        let current: Option<String> = tx.query_row(
            "SELECT identity_id FROM host_groups WHERE id = ?1",
            [&id],
            |row| row.get(0),
        )?;
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
                password,
            },
            "username",
            clock,
        )?;
        drop(vault_guard);
        tx.execute(
            "UPDATE host_groups
                SET identity_id = ?2, rev = rev + 1,
                    dirty = 1, hlc_wall_ms = ?3, hlc_counter = ?4, hlc_device = ?5
              WHERE id = ?1",
            params![
                id,
                login.identity,
                clock.wall_ms as i64,
                clock.counter,
                clock.device
            ],
        )?;
        if let Some(released) = login.release {
            release_login(&tx, self.device, &released, clock)?;
        }
        tx.commit()?;
        if !matches!(password, PasswordChange::Keep) {
            truncate_wal(&conn);
        }
        Ok(())
    }

    /// Rename a group. Its hosts are not touched at all — they point at the
    /// record, not at the name. Renaming onto a group that already exists
    /// merges the two: the hosts join it at the end.
    pub fn rename_group(&self, workspace: Workspace, from: &str, to: &str) -> Result<()> {
        let to = required(to)?;
        if from == to {
            return Ok(());
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let Some(from_id) = group_id(&tx, workspace, from)? else {
            return Err(unknown_group());
        };
        match group_id(&tx, workspace, &to)? {
            Some(to_id) => {
                move_members(&tx, self.device, &from_id, workspace, Some(&to_id))?;
                tombstone_group(&tx, self.device, &from_id)?;
            }
            None => {
                let clock = tick(&tx, self.device)?;
                tx.execute(
                    "UPDATE host_groups
                        SET name = ?2, rev = rev + 1,
                            dirty = 1, hlc_wall_ms = ?3, hlc_counter = ?4, hlc_device = ?5
                      WHERE id = ?1",
                    params![
                        from_id,
                        to,
                        clock.wall_ms as i64,
                        clock.counter,
                        clock.device
                    ],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Delete a group. Its hosts stay, without a group, at the end of the list.
    pub fn delete_group(&self, workspace: Workspace, name: &str) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let Some(id) = group_id(&tx, workspace, name)? else {
            return Err(unknown_group());
        };
        move_members(&tx, self.device, &id, workspace, None)?;
        tombstone_group(&tx, self.device, &id)?;
        tx.commit()?;
        Ok(())
    }

    /// Put a group before another one (or at the end), in the same or the
    /// other workspace. Its hosts move with it; onto a group of the same name
    /// in the other workspace, they merge.
    pub fn move_group(
        &self,
        workspace: Workspace,
        name: &str,
        to: Workspace,
        before: Option<&str>,
    ) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let Some(mut id) = group_id(&tx, workspace, name)? else {
            return Err(unknown_group());
        };

        if to != workspace {
            match group_id(&tx, to, name)? {
                Some(target) => {
                    move_members(&tx, self.device, &id, to, Some(&target))?;
                    tombstone_group(&tx, self.device, &id)?;
                    id = target;
                }
                None => {
                    let clock = tick(&tx, self.device)?;
                    tx.execute(
                        "UPDATE host_groups
                            SET workspace = ?2, rev = rev + 1,
                                dirty = 1, hlc_wall_ms = ?3, hlc_counter = ?4, hlc_device = ?5
                          WHERE id = ?1",
                        params![
                            id,
                            to.as_str(),
                            clock.wall_ms as i64,
                            clock.counter,
                            clock.device
                        ],
                    )?;
                    tx.execute(
                        "UPDATE hosts
                            SET workspace = ?2, rev = rev + 1,
                                dirty = 1, hlc_wall_ms = ?3, hlc_counter = ?4, hlc_device = ?5
                          WHERE group_id = ?1 AND deleted = 0",
                        params![
                            id,
                            to.as_str(),
                            clock.wall_ms as i64,
                            clock.counter,
                            clock.device
                        ],
                    )?;
                }
            }
        }

        let before_id = match before {
            Some(name) => group_id(&tx, to, name)?,
            None => None,
        };
        let mut order: Vec<(String, i64)> = {
            let mut stmt = tx.prepare(
                "SELECT id, position FROM host_groups
                  WHERE workspace = ?1 AND deleted = 0
                  ORDER BY position, lower(name)",
            )?;
            let rows = stmt
                .query_map([to.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let Some(from) = order.iter().position(|(other, _)| *other == id) else {
            tx.commit()?;
            return Ok(());
        };
        let moved = order.remove(from);
        let at = before_id
            .and_then(|before| order.iter().position(|(other, _)| *other == before))
            .unwrap_or(order.len());
        order.insert(at, moved);

        let clock = tick(&tx, self.device)?;
        for (index, (group, position)) in order.iter().enumerate() {
            if *position == index as i64 {
                continue;
            }
            tx.execute(
                "UPDATE host_groups
                    SET position = ?2, rev = rev + 1,
                        dirty = 1, hlc_wall_ms = ?3, hlc_counter = ?4, hlc_device = ?5
                  WHERE id = ?1",
                params![
                    group,
                    index as i64,
                    clock.wall_ms as i64,
                    clock.counter,
                    clock.device
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Move a host into a workspace and group, before another host of that
    /// group or at its end.
    pub fn move_host(
        &self,
        id: Uuid,
        to: Workspace,
        group: Option<&str>,
        before: Option<Uuid>,
    ) -> Result<HostRecord> {
        let group = group_name(group.map(str::to_string))?;
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let current: Option<(String, Option<String>, i64)> = tx
            .query_row(
                "SELECT workspace, group_id, position FROM hosts WHERE id = ?1 AND deleted = 0",
                [id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (workspace, old_group, old_position) = current.ok_or(StoreError::UnknownHost(id))?;
        let vault = vault_id(&tx)?;
        let group = match &group {
            Some(name) => Some(ensure_group(&tx, self.device, &vault, to, name)?),
            None => None,
        };

        let mut order: Vec<(String, i64)> = {
            let mut stmt = tx.prepare(
                "SELECT id, position FROM hosts
                  WHERE deleted = 0 AND workspace = ?1 AND group_id IS ?2 AND id != ?3
                  ORDER BY position, lower(name)",
            )?;
            let rows = stmt
                .query_map(params![to.as_str(), group, id.to_string()], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let at = before
            .map(|b| b.to_string())
            .and_then(|b| order.iter().position(|(other, _)| *other == b))
            .unwrap_or(order.len());
        // The moved host counts as changed if anything about its place is new.
        let moved_changed =
            Workspace::parse(&workspace) != to || old_group != group || old_position != at as i64;
        order.insert(
            at,
            (
                id.to_string(),
                if moved_changed { -1 } else { old_position },
            ),
        );

        let clock = tick(&tx, self.device)?;
        for (index, (host, position)) in order.iter().enumerate() {
            if *position == index as i64 {
                continue;
            }
            tx.execute(
                "UPDATE hosts
                    SET workspace = ?2, group_id = ?3, position = ?4, rev = rev + 1,
                        dirty = 1, hlc_wall_ms = ?5, hlc_counter = ?6, hlc_device = ?7
                  WHERE id = ?1",
                params![
                    host,
                    to.as_str(),
                    group,
                    index as i64,
                    clock.wall_ms as i64,
                    clock.counter,
                    clock.device
                ],
            )?;
        }
        let record = read_host(&tx, id)?.ok_or(StoreError::UnknownHost(id))?;
        tx.commit()?;
        Ok(record)
    }
}

/// Every host of one group joins another one — or none — at its end.
fn move_members(
    tx: &Transaction,
    device: u32,
    from: &str,
    to_workspace: Workspace,
    to: Option<&str>,
) -> Result<()> {
    let hosts = {
        let mut stmt = tx.prepare(
            "SELECT id FROM hosts
              WHERE deleted = 0 AND group_id = ?1
              ORDER BY position, lower(name)",
        )?;
        let ids = stmt
            .query_map([from], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        ids
    };
    if hosts.is_empty() {
        return Ok(());
    }
    let first = crate::hosts::next_position(tx, to_workspace, to)?;
    let clock = tick(tx, device)?;
    for (position, id) in (first..).zip(hosts) {
        tx.execute(
            "UPDATE hosts
                SET workspace = ?2, group_id = ?3, position = ?4, rev = rev + 1,
                    dirty = 1, hlc_wall_ms = ?5, hlc_counter = ?6, hlc_device = ?7
              WHERE id = ?1",
            params![
                id,
                to_workspace.as_str(),
                to,
                position,
                clock.wall_ms as i64,
                clock.counter,
                clock.device
            ],
        )?;
    }
    Ok(())
}

/// A deleted group takes its login along, unless something else still uses
/// it.
fn tombstone_group(tx: &Transaction, device: u32, id: &str) -> Result<()> {
    let clock = tick(tx, device)?;
    let login: Option<String> = tx
        .query_row(
            "SELECT identity_id FROM host_groups WHERE id = ?1 AND deleted = 0",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    tx.execute(
        "UPDATE host_groups
            SET deleted = 1, rev = rev + 1,
                dirty = 1, hlc_wall_ms = ?2, hlc_counter = ?3, hlc_device = ?4
          WHERE id = ?1 AND deleted = 0",
        params![id, clock.wall_ms as i64, clock.counter, clock.device],
    )?;
    if let Some(login) = login {
        release_login(tx, device, &login, clock)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosts::tests::draft;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn add(store: &Store, name: &str, workspace: Workspace, group: Option<&str>) -> HostRecord {
        let mut host = draft(name, &format!("{name}.lan"));
        host.workspace = Some(workspace);
        host.group_path = group.map(str::to_string);
        store.save_host(host).unwrap()
    }

    /// Host names of one group, in list order.
    fn names(store: &Store, workspace: Workspace, group: Option<&str>) -> Vec<String> {
        store
            .list_hosts()
            .unwrap()
            .into_iter()
            .filter(|h| h.workspace == workspace && h.group_path.as_deref() == group)
            .map(|h| h.name)
            .collect()
    }

    fn group_names(store: &Store, workspace: Workspace) -> Vec<String> {
        store
            .list_groups()
            .unwrap()
            .into_iter()
            .filter(|g| g.workspace == workspace)
            .map(|g| g.name)
            .collect()
    }

    const P: Workspace = Workspace::Private;
    const B: Workspace = Workspace::Business;

    #[test]
    fn an_empty_group_stays() {
        let store = store();
        store.create_group(P, "Homelab").unwrap();
        assert_eq!(group_names(&store, P), vec!["Homelab"]);
        assert!(matches!(
            store.create_group(P, "Homelab"),
            Err(StoreError::Invalid {
                problem: "exists",
                ..
            })
        ));
        // The same name in the other workspace is another group.
        store.create_group(B, "Homelab").unwrap();
        assert!(matches!(
            store.create_group(P, "  "),
            Err(StoreError::Invalid {
                problem: "required",
                ..
            })
        ));
    }

    #[test]
    fn a_host_dropped_between_two_hosts_lands_there() {
        let store = store();
        let a = add(&store, "a", P, Some("g"));
        let b = add(&store, "b", P, Some("g"));
        let c = add(&store, "c", P, Some("g"));
        store.move_host(c.id, P, Some("g"), Some(b.id)).unwrap();
        assert_eq!(names(&store, P, Some("g")), vec!["a", "c", "b"]);
        store.move_host(a.id, P, Some("g"), None).unwrap();
        assert_eq!(names(&store, P, Some("g")), vec!["c", "b", "a"]);
    }

    #[test]
    fn a_host_dropped_on_the_other_workspace_moves_there() {
        let store = store();
        let a = add(&store, "a", P, Some("home"));
        add(&store, "b", B, Some("work"));
        let moved = store.move_host(a.id, B, Some("work"), None).unwrap();
        assert_eq!(moved.workspace, B);
        assert_eq!(names(&store, B, Some("work")), vec!["b", "a"]);
        assert!(names(&store, P, Some("home")).is_empty());
        assert_eq!(
            group_names(&store, P),
            vec!["home"],
            "the empty group stays behind"
        );

        let ungrouped = store.move_host(a.id, P, None, None).unwrap();
        assert_eq!((ungrouped.workspace, ungrouped.group_path), (P, None));
    }

    #[test]
    fn moving_within_a_group_bumps_only_what_moved() {
        let store = store();
        let a = add(&store, "a", P, None);
        let b = add(&store, "b", P, None);
        add(&store, "c", P, None);
        // b to the end: a keeps its place, b and c swap.
        store.move_host(b.id, P, None, None).unwrap();
        let rev = |id: Uuid| -> i64 {
            store
                .conn
                .lock()
                .query_row(
                    "SELECT rev FROM hosts WHERE id = ?1",
                    [id.to_string()],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(rev(a.id), 1, "an untouched host keeps its revision");
        assert_eq!(rev(b.id), 2);
    }

    #[test]
    fn renaming_a_group_renames_it_for_its_hosts() {
        let store = store();
        let a = add(&store, "a", P, Some("old"));
        store.rename_group(P, "old", "new").unwrap();
        assert_eq!(group_names(&store, P), vec!["new"]);
        assert_eq!(names(&store, P, Some("new")), vec!["a"]);

        // And it did not rewrite the hosts, which is what the id is for.
        let rev: i64 = store
            .conn
            .lock()
            .query_row(
                "SELECT rev FROM hosts WHERE id = ?1",
                [a.id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rev, 1, "a rename is one record, not one per host");
    }

    #[test]
    fn renaming_onto_an_existing_group_merges_them() {
        let store = store();
        add(&store, "a", P, Some("x"));
        add(&store, "b", P, Some("y"));
        store.rename_group(P, "x", "y").unwrap();
        assert_eq!(group_names(&store, P), vec!["y"]);
        assert_eq!(names(&store, P, Some("y")), vec!["b", "a"]);
    }

    #[test]
    fn deleting_a_group_keeps_its_hosts() {
        let store = store();
        add(&store, "loose", P, None);
        add(&store, "a", P, Some("g"));
        store.delete_group(P, "g").unwrap();
        assert!(group_names(&store, P).is_empty());
        assert_eq!(names(&store, P, None), vec!["loose", "a"]);
    }

    #[test]
    fn groups_can_be_reordered_and_moved_to_the_other_workspace() {
        let store = store();
        for name in ["one", "two", "three"] {
            store.create_group(P, name).unwrap();
        }
        store.move_group(P, "three", P, Some("one")).unwrap();
        assert_eq!(group_names(&store, P), vec!["three", "one", "two"]);

        add(&store, "a", P, Some("two"));
        store.create_group(B, "clients").unwrap();
        store.move_group(P, "two", B, None).unwrap();
        assert_eq!(group_names(&store, P), vec!["three", "one"]);
        assert_eq!(group_names(&store, B), vec!["clients", "two"]);
        assert_eq!(names(&store, B, Some("two")), vec!["a"]);
    }

    #[test]
    fn a_group_that_is_not_there_is_not_silently_ignored() {
        let store = store();
        for outcome in [
            store.rename_group(P, "nope", "other"),
            store.delete_group(P, "nope"),
            store.move_group(P, "nope", B, None),
        ] {
            assert!(matches!(
                outcome,
                Err(StoreError::Invalid {
                    problem: "unknown",
                    ..
                })
            ));
        }
    }
}
