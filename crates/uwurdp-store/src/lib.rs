//! The local store: one SQLite file with everything UwURDP knows about your
//! hosts.
//!
//! Records already carry the sync header from `uwurdp-proto` — id, vault,
//! hybrid logical clock, revision, tombstone — even though sync only arrives in
//! M2. Adding those columns later would mean migrating every user's data;
//! carrying them from the first row costs nothing.
//!
//! **No secret lives here in the clear.** Passwords and keys are sealed with the
//! vault before they reach SQLite (see [`vault`]); a host without a stored
//! password asks on every connect, and key files stay where they are, referenced
//! by path.

pub mod backup;
pub mod credentials;
pub mod device;
pub mod groups;
pub mod hosts;
pub mod import;
pub mod known_hosts;
mod logins;
pub mod manifest;
mod schema;
pub mod secret;
pub mod sync;
pub mod vault;

pub use backup::{decode_export, encode_export, export_is_sealed, Backup, BackupSummary};
pub use credentials::ResolvedLogin;
pub use groups::GroupRecord;
pub use hosts::{normalize_rdp, HostDraft, HostRecord, PasswordChange, Workspace};
pub use import::{GroupInput, HostInput, ImportOutcome, ImportSet, KnownHostInput, LoginInput};
pub use known_hosts::KnownHostRecord;
pub use manifest::{manifest_id, ManifestFloor, Problem, Violation, Withheld};
pub use schema::SCHEMA_VERSION;
pub use secret::SecretText;
pub use sync::{ApplyReport, Enrolment, EnrolmentKeys, Pushed, SyncState};
pub use uwurdp_proto::{GatewaySettings, RdpSettings};
pub use vault::{Revealed, VaultStatus};

use parking_lot::Mutex;
use rusqlite::{params, Connection, Transaction};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;
use uwurdp_proto::Hlc;
use uwurdp_vault::UnlockedVault;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("could not create the data directory: {0}")]
    Io(#[from] std::io::Error),
    /// A field failed validation. `field` uses the UI's camelCase names so the
    /// frontend can put the message next to the right input.
    #[error("invalid {field}: {problem}")]
    Invalid {
        field: &'static str,
        problem: &'static str,
    },
    #[error("no host with id {0}")]
    UnknownHost(Uuid),
    #[error(
        "this database was written by a newer UwURDP (schema {found}, this build knows {known})"
    )]
    SchemaTooNew { found: i64, known: i64 },
    #[error("the vault is locked")]
    VaultLocked,
    #[error("the vault already exists")]
    VaultExists,
    #[error("no vault has been created yet")]
    NoVault,
    #[error(transparent)]
    Vault(#[from] uwurdp_vault::VaultError),
    #[error("no key with id {0}")]
    UnknownKey(Uuid),
    #[error("the key is still used by {hosts} host(s)")]
    KeyInUse { hosts: usize },
    #[error("the operating system could not protect the vault key: {0}")]
    Device(String),
    #[error("this export is protected by a password")]
    ExportPasswordRequired,
    #[error("the export password is wrong")]
    ExportPasswordWrong,
    #[error("export file: {0}")]
    Export(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

pub struct Store {
    // rusqlite's Connection is Send but not Sync. Every call is short, so one
    // lock is simpler than a pool and never the bottleneck of an SSH client.
    conn: Mutex<Connection>,
    device: u32,
    // The vault key, once unlocked, lives here and nowhere on disk. Dropping
    // the store — or calling `lock_vault` — wipes it.
    vault: Mutex<Option<UnlockedVault>>,
}

/// The database holds host names, addresses, snippets and — without sync — the
/// salt and wrapped key a guess at the master password would start from. The
/// default umask leaves all of that readable by every other user on the
/// machine; this doesn't. SQLite gives its `-wal` and `-shm` files the mode
/// of the database, so the file itself is enough. Windows profiles are
/// private to their user already.
fn private_to_this_user(path: &Path, made_folder: bool) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        // Only a folder made just now: one that was already there — `/tmp`,
        // for a database someone pointed at it — stays as it was.
        if let Some(parent) = path.parent().filter(|_| made_folder) {
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        for suffix in ["-wal", "-shm"] {
            let mut side = path.as_os_str().to_owned();
            side.push(suffix);
            if std::path::Path::new(&side).exists() {
                std::fs::set_permissions(&side, std::fs::Permissions::from_mode(0o600))?;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = (path, made_folder);
    Ok(())
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let mut made_folder = false;
        if let Some(parent) = path.parent() {
            made_folder = !parent.as_os_str().is_empty() && !parent.exists();
            std::fs::create_dir_all(parent)?;
        }
        private_to_this_user(path, made_folder)?;
        let conn = Connection::open(path)?;
        // WAL, like UwUMail: readers never block the writer, and a crash
        // mid-write cannot corrupt the file.
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Freed pages are overwritten with zeros: a forgotten password's
        // ciphertext must not linger in free space (see `truncate_wal`).
        conn.pragma_update(None, "secure_delete", "ON")?;
        conn.busy_timeout(Duration::from_secs(5))?;
        schema::migrate(&mut conn)?;
        let device = conn.query_row("SELECT device_id FROM meta WHERE id = 1", [], |row| {
            row.get::<_, i64>(0)
        })? as u32;
        Ok(Self {
            conn: Mutex::new(conn),
            device,
            vault: Mutex::new(None),
        })
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .lock()
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }
}

/// Advance the local hybrid logical clock inside a write transaction.
fn tick(tx: &Transaction, device: u32) -> Result<Hlc> {
    let (wall, counter): (i64, i64) = tx.query_row(
        "SELECT clock_wall_ms, clock_counter FROM meta WHERE id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let next = Hlc::new(wall as u64, counter as u32, device).tick(now_ms());
    tx.execute(
        "UPDATE meta SET clock_wall_ms = ?1, clock_counter = ?2 WHERE id = 1",
        params![next.wall_ms as i64, next.counter as i64],
    )?;
    Ok(next)
}

fn vault_id(tx: &Transaction) -> Result<String> {
    Ok(
        tx.query_row("SELECT vault_id FROM meta WHERE id = 1", [], |row| {
            row.get(0)
        })?,
    )
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_store_is_on_the_current_schema_and_empty() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert!(store.list_hosts().unwrap().is_empty());
    }

    #[test]
    fn data_survives_reopening_the_file() {
        let path = std::env::temp_dir().join(format!("uwurdp-store-test-{}.db", Uuid::now_v7()));
        {
            let store = Store::open(&path).unwrap();
            store
                .save_host(hosts::tests::draft("prox-1", "10.0.0.12"))
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let hosts = store.list_hosts().unwrap();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].address, "10.0.0.12");

        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_database_and_a_folder_made_for_it_are_this_users_alone() {
        use std::os::unix::fs::PermissionsExt;
        let folder = std::env::temp_dir().join(format!("uwurdp-store-perm-{}", Uuid::now_v7()));
        let path = folder.join("uwurdp.db");
        let store = Store::open(&path).unwrap();
        store
            .save_host(hosts::tests::draft("a", "10.0.0.1"))
            .unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&folder), 0o700);
        assert_eq!(mode(&path), 0o600);
        drop(store);
        let _ = std::fs::remove_dir_all(&folder);
    }
}
