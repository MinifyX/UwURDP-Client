//! The vault, and importing RDCMan's and mstsc's files.
//!
//! The vault comes first when a file brings passwords — RDCMan and mstsc keep
//! them sealed for the Windows account, and here they are opened and sealed
//! again, for the vault. A file without passwords needs no vault at all.
//! Reading a file ([`pick_import_files`], [`scan_rdcman_file`]) reports what
//! it holds, in counts only; [`run_import`] writes it.

use crate::backup::BackupFailure;
use crate::dialogs::Filter;
use crate::{err, AppState, CommandResult};
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::State;
use uuid::Uuid;
use uwurdp_import::{Decrypt, ImportBundle, ImportedAudio, ImportedDisplay, ImportedSettings};
use uwurdp_store::{
    GatewaySettings, GroupInput, HostInput, ImportSet, LoginInput, RdpSettings, Store, VaultStatus,
    Workspace,
};
use zeroize::Zeroizing;

// ── Vault ────────────────────────────────────────────────────────────────

#[tauri::command]
pub(crate) fn vault_status(state: State<'_, AppState>) -> CommandResult<VaultStatus> {
    state.store.vault_status().map_err(err)
}

/// The vault's status, and whether this device opens it without the master
/// password.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VaultState {
    status: VaultStatus,
    remembered: bool,
    /// The vault is synced and needs the account key besides the password,
    /// and this device has lost the copy it kept: the recovery kit's code has
    /// to be typed with the password.
    needs_recovery_code: bool,
    /// Needs an account key, but this device isn't paired: a sync connect
    /// that the server refused left the vault like this in 0.1.0-beta.8 and
    /// before, with the key thrown away. Nobody has it — only a key this
    /// device remembered opens the vault, and then a new master password
    /// sets it free (see [`repair_vault`]).
    stranded: bool,
}

#[tauri::command]
pub(crate) fn vault_state(state: State<'_, AppState>) -> CommandResult<VaultState> {
    let needs_account_key = state.store.vault_needs_account_key().map_err(err)?;
    Ok(VaultState {
        status: state.store.vault_status().map_err(err)?,
        remembered: state.store.vault_is_remembered().map_err(err)?,
        needs_recovery_code: needs_account_key
            && crate::sync::kept_account_key(&state.store).is_none(),
        stranded: is_stranded(&state.store).map_err(err)?,
    })
}

fn is_stranded(store: &Store) -> uwurdp_store::Result<bool> {
    Ok(store.vault_needs_account_key()? && !store.sync_state()?.paired())
}

/// Free a stranded vault: open it with the key this device remembered and
/// wrap it again under a new master password alone.
///
/// No weaker than before: whoever runs as this user opens a remembered vault
/// anyway (see `uwurdp_store::device`); this only lets them give it a
/// password again.
#[tauri::command]
pub(crate) async fn repair_vault(
    state: State<'_, AppState>,
    password: String,
    remember: bool,
) -> CommandResult<()> {
    let password = Zeroizing::new(password);
    if password.trim().is_empty() {
        return Err("the master password cannot be empty".into());
    }
    with_store(&state, move |store| {
        if !is_stranded(store).map_err(err)? {
            return Err("this vault needs no repair".into());
        }
        if store.vault_status().map_err(err)? != VaultStatus::Unlocked
            && !store
                .unlock_remembered_vault(crate::device::unprotect)
                .map_err(err)?
        {
            return Err("this device no longer keeps the vault's key".into());
        }
        store.rewrap_vault(password.as_bytes(), None).map_err(err)?;
        tracing::info!("a stranded vault has a master password of its own again");
        set_remembered(store, remember)
    })
    .await
}

/// Keep the vault key for this Windows user (`true`), or stop (`false`).
fn set_remembered(store: &Store, remember: bool) -> CommandResult<()> {
    if remember {
        store.remember_vault(crate::device::protect).map_err(err)
    } else {
        store.forget_remembered_vault().map_err(err)
    }
}

/// Run the master password's key derivation — about a second on purpose — on
/// a worker thread, so the window keeps drawing meanwhile.
async fn with_store<T: Send + 'static>(
    state: &AppState,
    work: impl FnOnce(&Store) -> CommandResult<T> + Send + 'static,
) -> CommandResult<T> {
    let store = Arc::clone(&state.store);
    tauri::async_runtime::spawn_blocking(move || work(&store))
        .await
        .map_err(err)?
}

#[tauri::command]
pub(crate) async fn create_vault(
    state: State<'_, AppState>,
    password: String,
    remember: bool,
) -> CommandResult<()> {
    let password = Zeroizing::new(password);
    if password.trim().is_empty() {
        return Err("the master password cannot be empty".into());
    }
    with_store(&state, move |store| {
        store.create_vault(password.as_bytes()).map_err(err)?;
        set_remembered(store, remember)
    })
    .await
}

/// Unlock with the master password. `remember` changes whether this device
/// keeps the key; `None` leaves that as it is.
///
/// A synced vault also needs the account key. A paired device kept it, sealed
/// for this user; one that lost it gets it typed in from the recovery kit.
#[tauri::command]
pub(crate) async fn unlock_vault(
    state: State<'_, AppState>,
    password: String,
    remember: Option<bool>,
    recovery_code: Option<String>,
) -> CommandResult<()> {
    let password = Zeroizing::new(password);
    let recovery_code = recovery_code.map(Zeroizing::new);
    with_store(&state, move |store| {
        if store.vault_needs_account_key().map_err(err)? {
            let account_key = match recovery_code.as_deref().filter(|c| !c.trim().is_empty()) {
                Some(code) => Some(
                    uwurdp_vault::AccountKey::from_code(code)
                        .map_err(|_| "that recovery code has a typo in it".to_string())?,
                ),
                None => crate::sync::kept_account_key(store),
            };
            let account_key = account_key.ok_or_else(|| {
                "this vault needs the code from the recovery kit as well".to_string()
            })?;
            store
                .unlock_vault_with(password.as_bytes(), Some(&account_key))
                .map_err(err)?;
        } else {
            store.unlock_vault(password.as_bytes()).map_err(err)?;
        }
        match remember {
            Some(remember) => set_remembered(store, remember),
            None => Ok(()),
        }
    })
    .await
}

#[tauri::command]
pub(crate) fn set_vault_remembered(
    state: State<'_, AppState>,
    remember: bool,
) -> CommandResult<()> {
    set_remembered(&state.store, remember)
}

#[tauri::command]
pub(crate) fn lock_vault(state: State<'_, AppState>) {
    state.store.lock_vault();
}

// ── Import ─────────────────────────────────────────────────────────────────

/// What was read and not written yet: between the preview and "import".
#[derive(Default)]
pub(crate) struct PendingImport {
    bundle: Mutex<Option<ImportBundle>>,
    /// RDCMan's own files, by the token the page names them with. The page
    /// never hands in a path.
    rdcman: Mutex<HashMap<String, PathBuf>>,
}

/// Passwords RDCMan and mstsc sealed with DPAPI open only here, for this user.
fn decrypter() -> Box<dyn Decrypt> {
    #[cfg(windows)]
    {
        Box::new(uwurdp_import::Dpapi)
    }
    #[cfg(not(windows))]
    {
        struct Never;
        impl Decrypt for Never {
            fn decrypt(&self, _: &[u8]) -> Option<uwurdp_import::Secret> {
                None
            }
        }
        Box::new(Never)
    }
}

/// A file RDCMan had open, as the page shows it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RdcManFile {
    token: String,
    name: String,
    folder: String,
}

/// The files RDCMan itself lists as open (Windows only), so importing the
/// usual setup is one click.
#[tauri::command]
pub(crate) async fn rdcman_files(state: State<'_, AppState>) -> CommandResult<Vec<RdcManFile>> {
    let files = tauri::async_runtime::spawn_blocking(|| uwurdp_import::rdcman_settings().files)
        .await
        .map_err(err)?;
    let mut tokens = state.pending_import.rdcman.lock();
    tokens.clear();
    Ok(files
        .into_iter()
        .filter(|path| path.is_file())
        .map(|path| {
            let token = Uuid::now_v7().to_string();
            let file = RdcManFile {
                token: token.clone(),
                name: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                folder: path
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            };
            tokens.insert(token, path);
            file
        })
        .collect())
}

/// What an import would bring, in counts. Contains no host names, addresses or
/// secrets, so it is safe to hand to the webview for a preview.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportSummary {
    label: String,
    hosts: usize,
    groups: usize,
    logins: usize,
    passwords: usize,
    gateways: usize,
    /// Whether writing this needs the vault: it has passwords to seal.
    needs_vault: bool,
    /// One line per thing that could not be imported, with the reason.
    skipped: Vec<String>,
}

/// What an import actually changed.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImportReport {
    pub hosts_added: usize,
    pub hosts_skipped: usize,
    pub groups_added: usize,
    pub logins_added: usize,
    pub passwords_added: usize,
    pub skipped: Vec<String>,
}

impl ImportReport {
    pub(crate) fn of(outcome: uwurdp_store::ImportOutcome, skipped: Vec<String>) -> Self {
        Self {
            hosts_added: outcome.hosts_added,
            hosts_skipped: outcome.hosts_skipped,
            groups_added: outcome.groups_added,
            logins_added: outcome.logins_added,
            passwords_added: outcome.passwords_added,
            skipped,
        }
    }
}

/// The largest file taken as an RDCMan or mstsc file; real ones are kilobytes.
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

fn read_file(path: &Path) -> Result<Vec<u8>, String> {
    let size = std::fs::metadata(path).map_err(err)?.len();
    if size > MAX_FILE_BYTES {
        return Err(format!("{} is too large", path.display()));
    }
    std::fs::read(path).map_err(err)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Read one `.rdg` file.
fn read_rdg(path: &Path) -> Result<ImportBundle, String> {
    let bytes = read_file(path)?;
    let text = String::from_utf8_lossy(&bytes);
    // A byte order mark in front of the XML declaration is common in RDCMan's files.
    let text = text.trim_start_matches('\u{feff}');
    let profiles = uwurdp_import::rdcman_settings().profiles;
    uwurdp_import::parse_rdg(text, &profiles, decrypter().as_ref()).map_err(err)
}

/// Read several `.rdp` files into one bundle: one host each.
fn read_rdp_files(paths: &[PathBuf]) -> ImportBundle {
    let decrypt = decrypter();
    let mut merged = ImportBundle::default();
    for path in paths {
        let stem = path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match read_file(path).and_then(|bytes| {
            uwurdp_import::parse_rdp_file(&bytes, &stem, decrypt.as_ref()).map_err(err)
        }) {
            Ok(bundle) => merge(&mut merged, bundle),
            Err(error) => merged.skipped.push((file_name(path), error)),
        }
    }
    merged
}

/// Append `other` to `into`, moving its credential indices along.
fn merge(into: &mut ImportBundle, other: ImportBundle) {
    let offset = into.credentials.len();
    let shift = |index: Option<usize>| index.map(|i| i + offset);
    into.credentials.extend(other.credentials);
    for mut group in other.groups {
        group.credential = shift(group.credential);
        into.groups.push(group);
    }
    for mut host in other.hosts {
        host.credential = shift(host.credential);
        if let Some(gateway) = &mut host.settings.gateway {
            gateway.credential = shift(gateway.credential);
        }
        into.hosts.push(host);
    }
    into.skipped.extend(other.skipped);
}

fn summarize(bundle: &ImportBundle, label: String) -> ImportSummary {
    let mut skipped: Vec<String> = bundle
        .skipped
        .iter()
        .map(|(what, why)| format!("{what}: {why}"))
        .collect();
    skipped.sort();
    skipped.dedup();
    ImportSummary {
        label,
        hosts: bundle.hosts.len(),
        groups: bundle.groups.len(),
        logins: bundle
            .credentials
            .iter()
            .filter(|c| c.username.as_deref().is_some_and(|u| !u.trim().is_empty()))
            .count(),
        passwords: bundle
            .credentials
            .iter()
            .filter(|c| c.password.as_ref().is_some_and(|p| !p.is_empty()))
            .count(),
        gateways: bundle
            .hosts
            .iter()
            .filter(|h| h.settings.gateway.is_some())
            .count(),
        needs_vault: bundle
            .credentials
            .iter()
            .any(|c| c.password.as_ref().is_some_and(|p| !p.is_empty())),
        skipped,
    }
}

fn keep(state: &AppState, bundle: ImportBundle, label: String) -> ImportSummary {
    let summary = summarize(&bundle, label);
    *state.pending_import.bundle.lock() = Some(bundle);
    summary
}

/// Ask for `.rdg` or `.rdp` files and read them. `None` when the dialog was
/// cancelled.
#[tauri::command]
pub(crate) async fn pick_import_files(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    kind: String,
) -> CommandResult<Option<ImportSummary>> {
    match kind.as_str() {
        "rdg" => {
            let Some(path) = crate::dialogs::open(
                &app,
                "RDCMan-Datei öffnen",
                Filter {
                    name: "Remote Desktop Connection Manager",
                    extensions: &["rdg"],
                },
            )
            .await
            else {
                return Ok(None);
            };
            let label = file_name(&path);
            let bundle = tauri::async_runtime::spawn_blocking(move || read_rdg(&path))
                .await
                .map_err(err)??;
            Ok(Some(keep(&state, bundle, label)))
        }
        "rdp" => {
            let paths = crate::dialogs::open_many(
                &app,
                "RDP-Dateien öffnen",
                Filter {
                    name: "Remotedesktopverbindung",
                    extensions: &["rdp"],
                },
            )
            .await;
            if paths.is_empty() {
                return Ok(None);
            }
            let label = match paths.as_slice() {
                [one] => file_name(one),
                many => format!("{} .rdp", many.len()),
            };
            let bundle = tauri::async_runtime::spawn_blocking(move || read_rdp_files(&paths))
                .await
                .map_err(err)?;
            Ok(Some(keep(&state, bundle, label)))
        }
        other => Err(format!("unknown import kind: {other}")),
    }
}

/// Read one of the files RDCMan had open.
#[tauri::command]
pub(crate) async fn scan_rdcman_file(
    state: State<'_, AppState>,
    token: String,
) -> CommandResult<ImportSummary> {
    let path = state
        .pending_import
        .rdcman
        .lock()
        .get(&token)
        .cloned()
        .ok_or("that file is no longer on the list")?;
    let label = file_name(&path);
    let bundle = tauri::async_runtime::spawn_blocking(move || read_rdg(&path))
        .await
        .map_err(err)??;
    Ok(keep(&state, bundle, label))
}

/// Write what the last pick or scan read, into `workspace`.
#[tauri::command]
pub(crate) async fn run_import(
    state: State<'_, AppState>,
    workspace: Workspace,
) -> Result<ImportReport, BackupFailure> {
    let bundle = state
        .pending_import
        .bundle
        .lock()
        .take()
        .ok_or_else(|| BackupFailure::Error {
            message: "pick the file again".into(),
        })?;
    let skipped = summarize(&bundle, String::new()).skipped;
    let (set, kept) = to_import_set(bundle, workspace);
    let store = Arc::clone(&state.store);
    let result = tauri::async_runtime::spawn_blocking(move || store.import(set))
        .await
        .map_err(|e| BackupFailure::Error {
            message: e.to_string(),
        })?;
    match result {
        Ok(outcome) => Ok(ImportReport::of(outcome, skipped)),
        Err(error) => {
            // A locked vault is a question, not an end: the same set is read
            // again once the vault is open.
            *state.pending_import.bundle.lock() = Some(kept);
            Err(error.into())
        }
    }
}

/// A group name the store takes: flattened RDCMan paths can get long.
fn group_name(path: &str) -> String {
    let clean: String = path.chars().filter(|c| !c.is_control()).collect();
    let trimmed = clean.trim();
    if trimmed.chars().count() <= 80 {
        trimmed.to_string()
    } else {
        // Keep the end: the innermost group tells hosts apart.
        let tail: String = trimmed
            .chars()
            .rev()
            .take(79)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("…{tail}")
    }
}

fn rdp_settings(settings: &ImportedSettings) -> RdpSettings {
    let mut rdp = RdpSettings::default();
    match settings.display {
        Some(ImportedDisplay::FitWindow) | None => rdp.display = "fit".into(),
        Some(ImportedDisplay::FullScreen) => rdp.display = "fullscreen".into(),
        Some(ImportedDisplay::Fixed { width, height }) => {
            rdp.display = "fixed".into();
            rdp.width = width;
            rdp.height = height;
        }
    }
    if let Some(depth) = settings.color_depth {
        rdp.color_depth = depth;
    }
    if let Some(admin) = settings.admin {
        rdp.admin = admin;
    }
    if let Some(audio) = settings.audio {
        rdp.audio = match audio {
            ImportedAudio::Local => "local",
            ImportedAudio::Remote => "remote",
            ImportedAudio::Off => "off",
        }
        .into();
    }
    if let Some(clipboard) = settings.clipboard {
        rdp.clipboard = clipboard;
    }
    rdp.gateway = settings.gateway.as_ref().map(|gateway| GatewaySettings {
        address: gateway.address.clone(),
        port: gateway.port.unwrap_or(443),
        use_host_login: gateway.use_host_credentials || gateway.credential.is_none(),
        bypass_local: gateway.bypass_local,
        extra: Default::default(),
    });
    rdp
}

/// The importer's neutral shape, as the store takes it. Returns the set and
/// the bundle again (minus nothing), so a set refused for a locked vault can
/// be retried without reading the file again.
fn to_import_set(bundle: ImportBundle, workspace: Workspace) -> (ImportSet, ImportBundle) {
    let logins = bundle
        .credentials
        .iter()
        .map(|credential| LoginInput {
            username: credential.username.clone().unwrap_or_default(),
            domain: credential.domain.clone().unwrap_or_default(),
            password: credential.password.clone(),
        })
        .collect();
    let groups = bundle
        .groups
        .iter()
        .map(|group| GroupInput {
            workspace,
            name: group_name(&group.path),
            login: group.credential,
        })
        .collect();
    let hosts = bundle
        .hosts
        .iter()
        .map(|host| {
            let mut comment = host.comment.clone().unwrap_or_default();
            // Settings UwURDP doesn't have yet stay readable, in the comment.
            let extras: Vec<String> = host
                .extras
                .iter()
                .filter(|(_, value)| !value.trim().is_empty())
                .map(|(key, value)| format!("{key}: {value}"))
                .collect();
            if !extras.is_empty() {
                if !comment.is_empty() {
                    comment.push_str("\n\n");
                }
                comment.push_str(&extras.join("\n"));
            }
            HostInput {
                name: host.name.clone(),
                address: host.address.clone(),
                port: host.port,
                group_path: host.group_path.as_deref().map(group_name),
                login: host.credential,
                gateway_login: host
                    .settings
                    .gateway
                    .as_ref()
                    .filter(|g| !g.use_host_credentials)
                    .and_then(|g| g.credential),
                workspace,
                position: None,
                rdp: rdp_settings(&host.settings),
                comment,
            }
        })
        .collect();
    (
        ImportSet {
            groups,
            hosts,
            logins,
            known_hosts: Vec::new(),
        },
        bundle,
    )
}
