//! Migrations, tracked in SQLite's own `user_version`.
//!
//! Each version is one SQL batch applied in one transaction, so an interrupted
//! upgrade leaves the previous schema intact rather than half of the new one.

use crate::{Result, StoreError};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

pub const SCHEMA_VERSION: i64 = 7;

/// The sync header (`id`, `vault_id`, clock, `rev`, `deleted`) is on every
/// syncable table from the start; see the crate docs for why.
const V1: &str = r#"
CREATE TABLE meta (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    device_id       INTEGER NOT NULL,
    vault_id        TEXT    NOT NULL,
    clock_wall_ms   INTEGER NOT NULL DEFAULT 0,
    clock_counter   INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE identities (
    id              TEXT    PRIMARY KEY,
    vault_id        TEXT    NOT NULL,
    label           TEXT    NOT NULL,
    username        TEXT    NOT NULL,
    auth_type       TEXT    NOT NULL
                    CHECK (auth_type IN ('password', 'key', 'agent', 'keyboard-interactive', 'cert')),
    -- Local only, never synced: until the vault exists a key is a file on
    -- this machine, and its path means nothing on another one.
    key_path        TEXT,
    hlc_wall_ms     INTEGER NOT NULL,
    hlc_counter     INTEGER NOT NULL,
    hlc_device      INTEGER NOT NULL,
    rev             INTEGER NOT NULL DEFAULT 1,
    deleted         INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE hosts (
    id                  TEXT    PRIMARY KEY,
    vault_id            TEXT    NOT NULL,
    name                TEXT    NOT NULL,
    address             TEXT    NOT NULL,
    port                INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    identity_id         TEXT    REFERENCES identities (id),
    group_path          TEXT,
    hlc_wall_ms         INTEGER NOT NULL,
    hlc_counter         INTEGER NOT NULL,
    hlc_device          INTEGER NOT NULL,
    rev                 INTEGER NOT NULL DEFAULT 1,
    deleted             INTEGER NOT NULL DEFAULT 0,
    -- Local only: when *this* device last connected.
    last_connected_ms   INTEGER
);

CREATE INDEX hosts_live ON hosts (deleted, name);

CREATE TABLE known_hosts (
    id                  TEXT    PRIMARY KEY,
    vault_id            TEXT    NOT NULL,
    address             TEXT    NOT NULL,
    port                INTEGER NOT NULL,
    algorithm           TEXT    NOT NULL,
    fingerprint_sha256  TEXT    NOT NULL,
    public_key          TEXT    NOT NULL,
    first_seen_ms       INTEGER NOT NULL,
    hlc_wall_ms         INTEGER NOT NULL,
    hlc_counter         INTEGER NOT NULL,
    hlc_device          INTEGER NOT NULL,
    rev                 INTEGER NOT NULL DEFAULT 1,
    deleted             INTEGER NOT NULL DEFAULT 0,
    UNIQUE (address, port)
);
"#;

/// Secrets, keys and snippets, plus the vault that seals them. V2 is where
/// UwURDP first keeps anything secret — everything before it referenced keys
/// by path and asked for passwords every time.
const V2: &str = r#"
CREATE TABLE vault (
    id                  INTEGER PRIMARY KEY CHECK (id = 1),
    vault_id            TEXT    NOT NULL,
    kdf_memory_kib      INTEGER NOT NULL,
    kdf_time_cost       INTEGER NOT NULL,
    kdf_parallelism     INTEGER NOT NULL,
    salt                BLOB    NOT NULL,
    wrapped_nonce       BLOB    NOT NULL,
    wrapped_blob        BLOB    NOT NULL
);

-- One sealed value: a password, a private key or a passphrase. The plaintext
-- is only ever XChaCha20-Poly1305 ciphertext here; the vault key that opens it
-- never touches the database.
CREATE TABLE secrets (
    id                  TEXT    PRIMARY KEY,
    vault_id            TEXT    NOT NULL,
    nonce               BLOB    NOT NULL,
    blob                BLOB    NOT NULL,
    hlc_wall_ms         INTEGER NOT NULL,
    hlc_counter         INTEGER NOT NULL,
    hlc_device          INTEGER NOT NULL,
    rev                 INTEGER NOT NULL DEFAULT 1,
    deleted             INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE keys (
    id                      TEXT    PRIMARY KEY,
    vault_id                TEXT    NOT NULL,
    label                   TEXT    NOT NULL,
    key_type                TEXT    NOT NULL,
    public_key              TEXT    NOT NULL DEFAULT '',
    private_secret_id       TEXT    REFERENCES secrets (id),
    passphrase_secret_id    TEXT    REFERENCES secrets (id),
    hlc_wall_ms             INTEGER NOT NULL,
    hlc_counter             INTEGER NOT NULL,
    hlc_device              INTEGER NOT NULL,
    rev                     INTEGER NOT NULL DEFAULT 1,
    deleted                 INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE snippets (
    id                  TEXT    PRIMARY KEY,
    vault_id            TEXT    NOT NULL,
    label               TEXT    NOT NULL,
    body                TEXT    NOT NULL,
    group_path          TEXT,
    hlc_wall_ms         INTEGER NOT NULL,
    hlc_counter         INTEGER NOT NULL,
    hlc_device          INTEGER NOT NULL,
    rev                 INTEGER NOT NULL DEFAULT 1,
    deleted             INTEGER NOT NULL DEFAULT 0
);

-- An identity can now carry a stored password and point at a key in the vault,
-- next to the file-path key it already had.
ALTER TABLE identities ADD COLUMN password_secret_id TEXT REFERENCES secrets (id);
ALTER TABLE identities ADD COLUMN key_id TEXT REFERENCES keys (id);
"#;

/// Workspaces, groups of their own and a manual order; what a connection found
/// out about a server; and the vault key kept for this user on this device.
const V3: &str = r#"
-- Private or business, like UwUMail's workspaces. Every host is in one.
ALTER TABLE hosts ADD COLUMN workspace TEXT NOT NULL DEFAULT 'private'
    CHECK (workspace IN ('private', 'business'));
-- The order the user dragged hosts into within their group. Ties sort by name.
ALTER TABLE hosts ADD COLUMN position INTEGER NOT NULL DEFAULT 0;
-- Local only: the system the last connection found, like 'ubuntu'.
ALTER TABLE hosts ADD COLUMN os_id TEXT;

-- A group is a record of its own, so an empty one stays and groups keep the
-- order they were dragged into. Hosts still name their group by `group_path`.
CREATE TABLE host_groups (
    id                  TEXT    PRIMARY KEY,
    vault_id            TEXT    NOT NULL,
    workspace           TEXT    NOT NULL CHECK (workspace IN ('private', 'business')),
    name                TEXT    NOT NULL,
    position            INTEGER NOT NULL DEFAULT 0,
    hlc_wall_ms         INTEGER NOT NULL,
    hlc_counter         INTEGER NOT NULL,
    hlc_device          INTEGER NOT NULL,
    rev                 INTEGER NOT NULL DEFAULT 1,
    deleted             INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX host_groups_live ON host_groups (deleted, workspace, position);

-- Local only, never synced: the vault key sealed by the operating system for
-- this user, so the vault opens without the master password. The check value
-- was sealed with the real vault key and tells a stale entry from a good one.
CREATE TABLE device_unlock (
    id                  INTEGER PRIMARY KEY CHECK (id = 1),
    vault_id            TEXT    NOT NULL,
    protected           BLOB    NOT NULL,
    check_nonce         BLOB    NOT NULL,
    check_blob          BLOB    NOT NULL,
    created_ms          INTEGER NOT NULL
);
"#;

/// What sync needs from the local database, and the one change it forces on
/// the data model: a host points at its group by id.
///
/// Renaming a group used to rewrite every host in it, which turned one edit
/// into one record per host — and, on two devices at once, into one conflict
/// per host. With an id, a rename is one record.
const V4: &str = r#"
-- Sync bookkeeping, on every table whose rows travel:
--   `dirty`       changed here and not yet accepted by the server
--   `server_seq`  the version the server last confirmed, 0 for never
--   `sync_extra`  fields a newer build wrote that this one does not know,
--                 kept verbatim so editing a record here does not drop them
ALTER TABLE hosts        ADD COLUMN dirty      INTEGER NOT NULL DEFAULT 1;
ALTER TABLE hosts        ADD COLUMN server_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE hosts        ADD COLUMN sync_extra TEXT;
ALTER TABLE host_groups  ADD COLUMN dirty      INTEGER NOT NULL DEFAULT 1;
ALTER TABLE host_groups  ADD COLUMN server_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE host_groups  ADD COLUMN sync_extra TEXT;
ALTER TABLE identities   ADD COLUMN dirty      INTEGER NOT NULL DEFAULT 1;
ALTER TABLE identities   ADD COLUMN server_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE identities   ADD COLUMN sync_extra TEXT;
ALTER TABLE keys         ADD COLUMN dirty      INTEGER NOT NULL DEFAULT 1;
ALTER TABLE keys         ADD COLUMN server_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE keys         ADD COLUMN sync_extra TEXT;
ALTER TABLE snippets     ADD COLUMN dirty      INTEGER NOT NULL DEFAULT 1;
ALTER TABLE snippets     ADD COLUMN server_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE snippets     ADD COLUMN sync_extra TEXT;
ALTER TABLE known_hosts  ADD COLUMN dirty      INTEGER NOT NULL DEFAULT 1;
ALTER TABLE known_hosts  ADD COLUMN server_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE known_hosts  ADD COLUMN sync_extra TEXT;
-- A secret's payload is the secret itself, so there is nothing extra to keep.
ALTER TABLE secrets      ADD COLUMN dirty      INTEGER NOT NULL DEFAULT 1;
ALTER TABLE secrets      ADD COLUMN server_seq INTEGER NOT NULL DEFAULT 0;

-- A tombstone from before there was a server has nobody to tell.
UPDATE hosts       SET dirty = 0 WHERE deleted = 1;
UPDATE host_groups SET dirty = 0 WHERE deleted = 1;
UPDATE identities  SET dirty = 0 WHERE deleted = 1;
UPDATE keys        SET dirty = 0 WHERE deleted = 1;
UPDATE snippets    SET dirty = 0 WHERE deleted = 1;
UPDATE known_hosts SET dirty = 0 WHERE deleted = 1;
UPDATE secrets     SET dirty = 0 WHERE deleted = 1;

-- Finding what to push must not scan the whole list, so each table gets a
-- partial index over just the rows that are waiting.
CREATE INDEX hosts_pending       ON hosts (dirty)       WHERE dirty = 1;
CREATE INDEX host_groups_pending ON host_groups (dirty) WHERE dirty = 1;
CREATE INDEX identities_pending  ON identities (dirty)  WHERE dirty = 1;
CREATE INDEX keys_pending        ON keys (dirty)        WHERE dirty = 1;
CREATE INDEX snippets_pending    ON snippets (dirty)    WHERE dirty = 1;
CREATE INDEX known_hosts_pending ON known_hosts (dirty) WHERE dirty = 1;
CREATE INDEX secrets_pending     ON secrets (dirty)     WHERE dirty = 1;

-- Where this device stands with its server. Local, never synced.
CREATE TABLE sync_state (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    server_url      TEXT,
    account_id      TEXT,
    device_id       TEXT,
    -- The server sequence number this device has seen everything up to.
    cursor          INTEGER NOT NULL DEFAULT 0,
    last_sync_ms    INTEGER
);
INSERT INTO sync_state (id) VALUES (1);

ALTER TABLE hosts ADD COLUMN group_id TEXT REFERENCES host_groups (id);
"#;

/// Dropping `group_path` is separate: the column has to stay until every host
/// has an id, and `ALTER TABLE … DROP COLUMN` cannot run in the same batch as
/// the statements that read it.
const V4_DROP_GROUP_PATH: &str = r#"
ALTER TABLE hosts DROP COLUMN group_path;
CREATE INDEX hosts_by_group ON hosts (group_id) WHERE group_id IS NOT NULL;
"#;

/// What a synced vault and a paired device need to remember.
const V5: &str = r#"
-- Whether opening this vault needs the account key as well as the password.
-- Written down rather than guessed at, so a device that does not have the key
-- says so instead of claiming the password is wrong.
ALTER TABLE vault ADD COLUMN needs_account_key INTEGER NOT NULL DEFAULT 0;

-- What the pairing agreed on. The two secrets are sealed by the operating
-- system for this user, not by the vault: the account key is needed *to* open
-- the vault, so it cannot live inside it.
ALTER TABLE sync_state ADD COLUMN tls_fingerprint       TEXT;
ALTER TABLE sync_state ADD COLUMN protected_device_key   BLOB;
ALTER TABLE sync_state ADD COLUMN protected_account_key  BLOB;
ALTER TABLE sync_state ADD COLUMN paired_ms              INTEGER;
"#;

/// Manifests: what each device holds, so a device can tell a server that
/// keeps records back from one that has nothing more (see
/// `uwurdp_proto::manifest`).
const V6: &str = r#"
-- One row per device of the vault, this one's own included. `entries` is the
-- payload exactly as it is sealed. Synced like any record, so it carries the
-- same header; only this device's own row is ever dirty.
CREATE TABLE manifests (
    id                  TEXT    PRIMARY KEY,
    vault_id            TEXT    NOT NULL,
    entries             BLOB    NOT NULL,
    hlc_wall_ms         INTEGER NOT NULL,
    hlc_counter         INTEGER NOT NULL,
    hlc_device          INTEGER NOT NULL,
    rev                 INTEGER NOT NULL DEFAULT 1,
    deleted             INTEGER NOT NULL DEFAULT 0,
    dirty               INTEGER NOT NULL DEFAULT 0,
    server_seq          INTEGER NOT NULL DEFAULT 0
);

-- Record versions that arrived and lost their slot without becoming a
-- tombstone: a host key another device trusted for an address that already
-- had a newer one. Such a record is not here, and that is not the server's
-- doing.
CREATE TABLE superseded (
    id                  TEXT    PRIMARY KEY,
    kind                INTEGER NOT NULL,
    hlc_wall_ms         INTEGER NOT NULL,
    hlc_counter         INTEGER NOT NULL,
    hlc_device          INTEGER NOT NULL
);

-- What the last complete check found missing or out of date. Local, and
-- replaced as a whole by every check.
CREATE TABLE manifest_violations (
    kind                INTEGER NOT NULL,
    id                  TEXT    NOT NULL,
    problem             TEXT    NOT NULL,
    PRIMARY KEY (kind, id)
);

-- The manifest the device that added this one had published when it did, as
-- it said inside the pairing handshake: this device must see that one, or a
-- newer one, before it can believe it has everything.
ALTER TABLE sync_state ADD COLUMN floor_manifest_id  TEXT;
ALTER TABLE sync_state ADD COLUMN floor_wall_ms      INTEGER;
ALTER TABLE sync_state ADD COLUMN floor_counter      INTEGER;
ALTER TABLE sync_state ADD COLUMN floor_device       INTEGER;
"#;

/// What an RDP host needs beyond the SSH host this schema started as: how its
/// desktop is shown, a Windows domain on every login, a login a group hands to
/// its hosts (RDCMan's "inherit from parent"), and a gateway's own login.
const V7: &str = r#"
ALTER TABLE identities ADD COLUMN domain TEXT NOT NULL DEFAULT '';
-- `RdpSettings` as JSON; NULL means the defaults.
ALTER TABLE hosts ADD COLUMN rdp TEXT;
ALTER TABLE hosts ADD COLUMN comment TEXT NOT NULL DEFAULT '';
ALTER TABLE hosts ADD COLUMN gateway_identity_id TEXT REFERENCES identities (id);
ALTER TABLE host_groups ADD COLUMN identity_id TEXT REFERENCES identities (id);
"#;

pub fn migrate(conn: &mut Connection) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;

    if version > SCHEMA_VERSION {
        // Refusing is the only safe option: an older build writing into a newer
        // schema can corrupt data it does not understand.
        return Err(StoreError::SchemaTooNew {
            found: version,
            known: SCHEMA_VERSION,
        });
    }

    if version < 1 {
        let tx = conn.transaction()?;
        tx.execute_batch(V1)?;
        tx.execute(
            "INSERT INTO meta (id, device_id, vault_id) VALUES (1, ?1, ?2)",
            params![i64::from(rand::random::<u32>()), Uuid::now_v7().to_string()],
        )?;
        tx.pragma_update(None, "user_version", 1)?;
        tx.commit()?;
        tracing::info!("store created at schema 1");
    }

    if version < 2 {
        let tx = conn.transaction()?;
        tx.execute_batch(V2)?;
        tx.pragma_update(None, "user_version", 2)?;
        tx.commit()?;
        tracing::info!("store migrated to schema 2");
    }

    if version < 3 {
        let tx = conn.transaction()?;
        tx.execute_batch(V3)?;
        // Every group a host already names becomes a group record, in
        // alphabetical order — the order the list showed until now.
        let names = {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT group_path FROM hosts
                  WHERE deleted = 0 AND group_path IS NOT NULL
                  ORDER BY lower(group_path)",
            )?;
            let names = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            names
        };
        let (vault_id, device): (String, i64) = tx.query_row(
            "SELECT vault_id, device_id FROM meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        for (position, name) in names.iter().enumerate() {
            tx.execute(
                "INSERT INTO host_groups
                    (id, vault_id, workspace, name, position, hlc_wall_ms, hlc_counter, hlc_device)
                 VALUES (?1, ?2, 'private', ?3, ?4, 0, 0, ?5)",
                params![
                    Uuid::now_v7().to_string(),
                    vault_id,
                    name,
                    position as i64,
                    device
                ],
            )?;
        }
        // Repairs for what 0.1.0-beta.1 left behind. Deleting a host tombstoned
        // its login even when other hosts still used it: those come back.
        // And a deleted host's password stayed, as ciphertext: every secret
        // no live login or key points at goes now.
        tx.execute_batch(
            "UPDATE identities SET deleted = 0
              WHERE deleted = 1
                AND id IN (SELECT identity_id FROM hosts
                            WHERE deleted = 0 AND identity_id IS NOT NULL);
             UPDATE secrets SET deleted = 1, nonce = x'', blob = x'', rev = rev + 1
              WHERE deleted = 0
                AND id NOT IN (SELECT password_secret_id FROM identities
                                WHERE deleted = 0 AND password_secret_id IS NOT NULL)
                AND id NOT IN (SELECT private_secret_id FROM keys
                                WHERE deleted = 0 AND private_secret_id IS NOT NULL)
                AND id NOT IN (SELECT passphrase_secret_id FROM keys
                                WHERE deleted = 0 AND passphrase_secret_id IS NOT NULL);",
        )?;
        tx.pragma_update(None, "user_version", 3)?;
        tx.commit()?;
        tracing::info!("store migrated to schema 3");
    }

    if version < 4 {
        let tx = conn.transaction()?;
        tx.execute_batch(V4)?;
        let (vault_id, device): (String, i64) = tx.query_row(
            "SELECT vault_id, device_id FROM meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        // Every group a host names by text gets a record if it has none, and
        // then the hosts of that group point at it by id.
        let named = {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT workspace, group_path FROM hosts
                  WHERE deleted = 0 AND group_path IS NOT NULL
                  ORDER BY workspace, lower(group_path)",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        for (workspace, name) in named {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT id FROM host_groups
                      WHERE workspace = ?1 AND name = ?2 AND deleted = 0",
                    params![workspace, name],
                    |row| row.get(0),
                )
                .optional()?;
            let id = match existing {
                Some(id) => id,
                None => {
                    let id = Uuid::now_v7().to_string();
                    let position: i64 = tx.query_row(
                        "SELECT coalesce(max(position) + 1, 0) FROM host_groups
                          WHERE workspace = ?1 AND deleted = 0",
                        [&workspace],
                        |row| row.get(0),
                    )?;
                    tx.execute(
                        "INSERT INTO host_groups
                            (id, vault_id, workspace, name, position,
                             hlc_wall_ms, hlc_counter, hlc_device)
                         VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, ?6)",
                        params![id, vault_id, workspace, name, position, device],
                    )?;
                    id
                }
            };
            tx.execute(
                "UPDATE hosts SET group_id = ?3
                  WHERE workspace = ?1 AND group_path = ?2 AND deleted = 0",
                params![workspace, name, id],
            )?;
        }
        tx.execute_batch(V4_DROP_GROUP_PATH)?;
        tx.pragma_update(None, "user_version", 4)?;
        tx.commit()?;
        tracing::info!("store migrated to schema 4");
    }

    if version < 5 {
        let tx = conn.transaction()?;
        tx.execute_batch(V5)?;
        tx.pragma_update(None, "user_version", 5)?;
        tx.commit()?;
        tracing::info!("store migrated to schema 5");
    }

    if version < 6 {
        let tx = conn.transaction()?;
        tx.execute_batch(V6)?;
        tx.pragma_update(None, "user_version", 6)?;
        tx.commit()?;
        tracing::info!("store migrated to schema 6");
    }

    if version < 7 {
        let tx = conn.transaction()?;
        tx.execute_batch(V7)?;
        tx.pragma_update(None, "user_version", 7)?;
        tx.commit()?;
        tracing::info!("store migrated to schema 7");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrating_twice_is_harmless() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        migrate(&mut conn).unwrap();
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM meta", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn a_v1_database_upgrades_to_v2_without_losing_its_hosts() {
        let mut conn = Connection::open_in_memory().unwrap();
        // Bring it up as a V1 database would have been.
        conn.execute_batch(V1).unwrap();
        conn.execute(
            "INSERT INTO meta (id, device_id, vault_id) VALUES (1, 1, ?1)",
            [Uuid::now_v7().to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO hosts (id, vault_id, name, address, port,
                                hlc_wall_ms, hlc_counter, hlc_device)
             VALUES (?1, ?2, 'nas', 'nas.lan', 22, 0, 0, 1)",
            [Uuid::now_v7().to_string(), Uuid::now_v7().to_string()],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();

        migrate(&mut conn).unwrap();

        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let hosts: i64 = conn
            .query_row("SELECT count(*) FROM hosts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(hosts, 1, "the existing host survived the migration");
        // The V2 tables and columns exist now.
        conn.query_row("SELECT count(*) FROM secrets", [], |r| r.get::<_, i64>(0))
            .unwrap();
        conn.query_row("SELECT count(key_id) FROM identities", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
    }

    #[test]
    fn a_v2_database_upgrades_to_v3_with_its_groups_as_records() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(V1).unwrap();
        conn.execute_batch(V2).unwrap();
        conn.execute(
            "INSERT INTO meta (id, device_id, vault_id) VALUES (1, 1, ?1)",
            [Uuid::now_v7().to_string()],
        )
        .unwrap();
        for (name, group) in [("a", Some("Homelab")), ("b", Some("Work")), ("c", None)] {
            conn.execute(
                "INSERT INTO hosts (id, vault_id, name, address, port, group_path,
                                    hlc_wall_ms, hlc_counter, hlc_device)
                 VALUES (?1, 'v', ?2, '10.0.0.1', 22, ?3, 0, 0, 1)",
                params![Uuid::now_v7().to_string(), name, group],
            )
            .unwrap();
        }
        conn.pragma_update(None, "user_version", 2).unwrap();

        migrate(&mut conn).unwrap();

        let groups: Vec<(String, String, i64)> = conn
            .prepare("SELECT workspace, name, position FROM host_groups ORDER BY position")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            groups,
            vec![
                ("private".into(), "Homelab".into(), 0),
                ("private".into(), "Work".into(), 1)
            ]
        );
        let private: i64 = conn
            .query_row(
                "SELECT count(*) FROM hosts WHERE workspace = 'private'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(private, 3, "existing hosts start out private");
    }

    #[test]
    fn upgrading_repairs_what_the_first_beta_left_behind() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(V1).unwrap();
        conn.execute_batch(V2).unwrap();
        conn.execute(
            "INSERT INTO meta (id, device_id, vault_id) VALUES (1, 1, 'v')",
            [],
        )
        .unwrap();
        // Two secrets: one a live login uses, one a deleted host left behind.
        conn.execute_batch(
            "INSERT INTO secrets (id, vault_id, nonce, blob, hlc_wall_ms, hlc_counter, hlc_device)
                  VALUES ('kept', 'v', x'01', x'02', 0, 0, 1),
                         ('orphan', 'v', x'01', x'02', 0, 0, 1);
             INSERT INTO identities (id, vault_id, label, username, auth_type, password_secret_id,
                                     hlc_wall_ms, hlc_counter, hlc_device, deleted)
                  VALUES ('shared', 'v', '', 'uwu', 'password', 'kept', 0, 0, 1, 1),
                         ('gone', 'v', '', 'old', 'password', 'orphan', 0, 0, 1, 1);
             INSERT INTO hosts (id, vault_id, name, address, port, identity_id,
                                hlc_wall_ms, hlc_counter, hlc_device, deleted)
                  VALUES ('a', 'v', 'a', '10.0.0.1', 22, 'shared', 0, 0, 1, 1),
                         ('b', 'v', 'b', '10.0.0.2', 22, 'shared', 0, 0, 1, 0),
                         ('c', 'v', 'c', '10.0.0.3', 22, 'gone', 0, 0, 1, 1);",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();

        migrate(&mut conn).unwrap();

        let login: i64 = conn
            .query_row(
                "SELECT deleted FROM identities WHERE id = 'shared'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(login, 0, "b's login came back");
        let secrets: Vec<(String, i64, Vec<u8>)> = conn
            .prepare("SELECT id, deleted, blob FROM secrets ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            secrets,
            vec![
                ("kept".into(), 0, vec![2]),
                ("orphan".into(), 1, Vec::new())
            ]
        );
    }

    #[test]
    fn a_v3_database_upgrades_to_v4_with_its_hosts_pointing_at_group_records() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(V1).unwrap();
        conn.execute_batch(V2).unwrap();
        conn.execute_batch(V3).unwrap();
        conn.execute(
            "INSERT INTO meta (id, device_id, vault_id) VALUES (1, 1, 'v')",
            [],
        )
        .unwrap();
        // One group with a record, one a host names without one, one host
        // without a group, and a tombstone from before there was a server.
        conn.execute_batch(
            "INSERT INTO host_groups (id, vault_id, workspace, name, position,
                                      hlc_wall_ms, hlc_counter, hlc_device)
                  VALUES ('g-home', 'v', 'private', 'Homelab', 0, 1, 0, 1);
             INSERT INTO hosts (id, vault_id, name, address, port, group_path, workspace,
                                hlc_wall_ms, hlc_counter, hlc_device, deleted)
                  VALUES ('a', 'v', 'a', '10.0.0.1', 22, 'Homelab',  'private', 1, 0, 1, 0),
                         ('b', 'v', 'b', '10.0.0.2', 22, 'Clients',  'business', 1, 0, 1, 0),
                         ('c', 'v', 'c', '10.0.0.3', 22, NULL,       'private', 1, 0, 1, 0),
                         ('d', 'v', 'd', '10.0.0.4', 22, 'Homelab',  'private', 1, 0, 1, 1);",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 3).unwrap();

        migrate(&mut conn).unwrap();

        let group_of = |host: &str| -> Option<String> {
            conn.query_row(
                "SELECT g.name FROM hosts h
                   LEFT JOIN host_groups g ON g.id = h.group_id
                  WHERE h.id = ?1",
                [host],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(group_of("a").as_deref(), Some("Homelab"));
        assert_eq!(
            group_of("b").as_deref(),
            Some("Clients"),
            "a group only a host named got a record of its own"
        );
        assert_eq!(group_of("c"), None);

        // The host still in a group shares the record that already existed.
        let groups: i64 = conn
            .query_row(
                "SELECT count(*) FROM host_groups WHERE name = 'Homelab'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(groups, 1);

        let pending: Vec<(String, i64)> = conn
            .prepare("SELECT id, dirty FROM hosts ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            pending,
            vec![
                ("a".into(), 1),
                ("b".into(), 1),
                ("c".into(), 1),
                ("d".into(), 0)
            ],
            "everything waits to be pushed, except a tombstone nobody saw"
        );

        let cursor: i64 = conn
            .query_row("SELECT cursor FROM sync_state WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(cursor, 0);
    }

    #[test]
    fn a_v4_database_upgrades_to_v5_with_nothing_paired_and_no_account_key() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();

        let (needs_key, fingerprint, paired): (i64, Option<String>, Option<i64>) = conn
            .query_row(
                "SELECT
                    (SELECT count(*) FROM pragma_table_info('vault')
                      WHERE name = 'needs_account_key'),
                    (SELECT tls_fingerprint FROM sync_state WHERE id = 1),
                    (SELECT paired_ms FROM sync_state WHERE id = 1)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(needs_key, 1, "the vault header has the flag");
        assert_eq!(fingerprint, None, "and nothing is paired yet");
        assert_eq!(paired, None);
    }

    #[test]
    fn a_v5_database_upgrades_to_v6_with_no_manifests_and_no_floor() {
        let mut conn = Connection::open_in_memory().unwrap();
        for batch in [V1, V2, V3, V4, V4_DROP_GROUP_PATH, V5] {
            conn.execute_batch(batch).unwrap();
        }
        conn.execute(
            "INSERT INTO meta (id, device_id, vault_id) VALUES (1, 1, 'v')",
            [],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 5).unwrap();

        migrate(&mut conn).unwrap();

        let (manifests, violations, floor): (i64, i64, Option<String>) = conn
            .query_row(
                "SELECT (SELECT count(*) FROM manifests),
                        (SELECT count(*) FROM manifest_violations),
                        (SELECT floor_manifest_id FROM sync_state WHERE id = 1)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((manifests, violations, floor), (0, 0, None));
        conn.query_row("SELECT count(*) FROM superseded", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
    }

    #[test]
    fn a_newer_schema_is_refused_rather_than_trampled() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        assert!(matches!(
            migrate(&mut conn),
            Err(StoreError::SchemaTooNew { .. })
        ));
    }
}
