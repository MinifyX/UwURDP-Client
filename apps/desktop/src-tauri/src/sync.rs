//! Settings → Sync: connecting a UwUSync server, adding and removing devices,
//! and the thread that keeps this device in step.
//!
//! Every step the person takes is one command here, and every command is a
//! thin wrapper around `uwurdp_sync::flow`, which puts the pieces in the right
//! order. What this layer adds is the operating system's seal (see
//! [`crate::device`]), the worker thread, and the words the page shows.
//!
//! The worker is a plain thread, because the transport is blocking and must
//! not share a thread with the terminals' runtime. It wakes every few seconds,
//! pushes when something is waiting, pulls once a minute, and at once when the
//! page asks. It never runs while the vault is locked: a pass needs the key to
//! seal and to open, and the lock is not the sync's to lift.

use crate::{err, AppState, CommandResult};
use parking_lot::{Condvar, Mutex};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;
use uwurdp_store::{Store, StoreError, VaultStatus, Withheld};
use uwurdp_sync::flow::{self, FlowError, PairingOffer};
use uwurdp_sync::{pairing, Server, SyncError, SyncReport, TransportError};
use zeroize::Zeroizing;

/// How often the worker looks whether something waits to be pushed.
const TICK: Duration = Duration::from_secs(5);
/// How often it pulls when nothing happens here.
const PULL_EVERY: Duration = Duration::from_secs(60);
/// After a failed pass, how long before the next try.
const BACKOFF: Duration = Duration::from_secs(30);

/// What the page is told when something went wrong. `kind` is what it acts
/// on; `message` is for showing.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum SyncFailure {
    /// The vault has to be opened first.
    VaultLocked,
    /// The master password (or the recovery code with it) is wrong.
    PasswordWrong,
    /// The code isn't one, or was misheard.
    BadCode,
    /// The server couldn't be reached.
    Unreachable {
        message: String,
    },
    /// The server said no, and why.
    Refused {
        message: String,
    },
    /// The other device did not answer in time, or the words didn't match.
    PairingFailed {
        message: String,
    },
    Error {
        message: String,
    },
}

impl SyncFailure {
    fn error(message: impl std::fmt::Display) -> Self {
        Self::Error {
            message: message.to_string(),
        }
    }
}

impl From<StoreError> for SyncFailure {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::VaultLocked => Self::VaultLocked,
            StoreError::Vault(_) => Self::PasswordWrong,
            other => Self::error(other),
        }
    }
}

impl From<TransportError> for SyncFailure {
    fn from(error: TransportError) -> Self {
        match error {
            TransportError::Unreachable(message) => Self::Unreachable { message },
            TransportError::Refused(message) => Self::Refused { message },
        }
    }
}

impl From<FlowError> for SyncFailure {
    fn from(error: FlowError) -> Self {
        match error {
            FlowError::Transport(error) => error.into(),
            FlowError::Store(error) => error.into(),
            FlowError::Vault(_) => Self::PasswordWrong,
            FlowError::BadSetupCode => Self::BadCode,
            FlowError::VaultLocked => Self::VaultLocked,
            FlowError::NotPaired => Self::error("this device is not paired with a server"),
            FlowError::NoAccountKey => Self::PairingFailed {
                message: "the other device did not send the account key".into(),
            },
        }
    }
}

impl From<SyncError> for SyncFailure {
    fn from(error: SyncError) -> Self {
        match error {
            SyncError::VaultLocked => Self::VaultLocked,
            SyncError::Transport(error) => error.into(),
            other => Self::error(other),
        }
    }
}

type SyncResult<T> = Result<T, SyncFailure>;

/// What the last pass did, for the status line.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LastPass {
    at_ms: u64,
    report: Option<SyncReport>,
    /// Why it failed, in the server's or the network's words.
    error: Option<String>,
}

/// The worker's side, shared with the commands.
#[derive(Default)]
pub(crate) struct Sync {
    /// A signed-in server, kept between passes. Dropped on any failure, so the
    /// next pass connects afresh.
    server: Mutex<Option<Arc<Server>>>,
    last: Mutex<Option<LastPass>>,
    running: AtomicBool,
    /// The last pass found the server keeping records back, and the window
    /// has been told.
    alarmed: AtomicBool,
    /// A pairing this device offered and is waiting on.
    offer: Mutex<Option<Arc<PairingOffer>>>,
    /// Someone is waiting on the offer. Two waiters on one code would both
    /// speak for side A of the handshake, and the other device would hear
    /// nonsense.
    waiting: AtomicBool,
    /// The page asked for a pass now.
    wake: Mutex<bool>,
    woken: Condvar,
}

impl Sync {
    fn poke(&self) {
        *self.wake.lock() = true;
        self.woken.notify_one();
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The server this device is paired with, signed in — the cached one, or a
/// fresh connection.
fn server(store: &Store, sync: &Sync) -> SyncResult<Arc<Server>> {
    if let Some(server) = sync.server.lock().clone() {
        return Ok(server);
    }
    let server = Arc::new(flow::reconnect(store, crate::device::unprotect_sync)?);
    *sync.server.lock() = Some(Arc::clone(&server));
    Ok(server)
}

/// One pass, with the outcome written down and told to the page.
fn pass(app: &AppHandle, store: &Store, sync: &Sync) -> SyncResult<SyncReport> {
    if sync.running.swap(true, Ordering::SeqCst) {
        return Err(SyncFailure::error("a pass is already running"));
    }
    let outcome = (|| {
        let server = server(store, sync)?;
        uwurdp_sync::sync_once(store, server.as_ref()).map_err(SyncFailure::from)
    })();
    sync.running.store(false, Ordering::SeqCst);

    let last = match &outcome {
        Ok(report) => LastPass {
            at_ms: now_ms(),
            report: Some(*report),
            error: None,
        },
        Err(failure) => {
            // A fresh connection next time: the token, the pin, the address
            // may all be what went wrong.
            *sync.server.lock() = None;
            LastPass {
                at_ms: now_ms(),
                report: None,
                error: Some(describe(failure)),
            }
        }
    };
    *sync.last.lock() = Some(last);
    if let Ok(report) = &outcome {
        if report.apply.applied > 0 {
            // Another device changed something: the host list loads again.
            let _ = app.emit("sync:changed", report.apply.applied);
        }
        // Not just a line on the settings page: the window hears of it once,
        // when it begins, whatever it is showing.
        let withheld = report.withheld.any();
        if withheld && !sync.alarmed.swap(true, Ordering::SeqCst) {
            let _ = app.emit("sync:withheld", report.withheld);
        } else if !withheld {
            sync.alarmed.store(false, Ordering::SeqCst);
        }
    }
    let _ = app.emit("sync:status", ());
    outcome
}

fn describe(failure: &SyncFailure) -> String {
    match failure {
        SyncFailure::VaultLocked => "the vault is locked".into(),
        SyncFailure::PasswordWrong => "wrong password".into(),
        SyncFailure::BadCode => "not a code".into(),
        SyncFailure::Unreachable { message }
        | SyncFailure::Refused { message }
        | SyncFailure::PairingFailed { message }
        | SyncFailure::Error { message } => message.clone(),
    }
}

/// Starts the worker. It lives as long as the app.
pub(crate) fn start(app: &AppHandle) {
    app.manage(Sync::default());
    let app = app.clone();
    std::thread::Builder::new()
        .name("uwurdp-sync".into())
        .spawn(move || worker(app))
        .expect("the sync thread starts");
}

fn worker(app: AppHandle) {
    let state = app.state::<AppState>();
    let sync = app.state::<Sync>();
    let store = Arc::clone(&state.store);
    // A first pass shortly after start, so a device that was off for a week
    // catches up without waiting a minute.
    let mut next_pull = Instant::now() + Duration::from_secs(3);
    let mut not_before = Instant::now();
    loop {
        let asked = {
            let mut wake = sync.wake.lock();
            if !*wake {
                sync.woken.wait_for(&mut wake, TICK);
            }
            std::mem::take(&mut *wake)
        };
        let paired = store.sync_state().map(|s| s.paired()).unwrap_or(false);
        let unlocked = matches!(store.vault_status(), Ok(VaultStatus::Unlocked));
        if !paired || !unlocked {
            continue;
        }
        let now = Instant::now();
        let waiting = store.pending_count().unwrap_or(0) > 0;
        let due = asked || now >= next_pull || (waiting && now >= not_before);
        if !due {
            continue;
        }
        match pass(&app, &store, &sync) {
            Ok(_) => {
                next_pull = Instant::now() + PULL_EVERY;
                not_before = Instant::now();
            }
            Err(failure) => {
                tracing::info!(error = %describe(&failure), "sync pass failed");
                next_pull = Instant::now() + BACKOFF;
                not_before = Instant::now() + BACKOFF;
            }
        }
    }
}

/// Run blocking sync work on a worker thread, so the window keeps drawing —
/// Argon2 and SPAKE2 take a moment, and the network takes longer.
async fn blocking<T: Send + 'static>(
    app: &AppHandle,
    work: impl FnOnce(&AppHandle, &Store, &Sync) -> SyncResult<T> + Send + 'static,
) -> SyncResult<T> {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let sync = app.state::<Sync>();
        let store = Arc::clone(&state.store);
        work(&app, &store, &sync)
    })
    .await
    .map_err(SyncFailure::error)?
}

// ── What the page shows ────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SyncStatus {
    paired: bool,
    server_url: Option<String>,
    /// The pinned certificate, `SHA256:…`, to compare with what the server prints.
    tls_fingerprint: Option<String>,
    device_id: Option<Uuid>,
    paired_ms: Option<u64>,
    last_sync_ms: Option<u64>,
    last: Option<LastPass>,
    /// Changes on this device the server hasn't taken yet.
    pending: usize,
    running: bool,
    vault: VaultStatus,
    /// A name for this device, to suggest when pairing.
    device_name: String,
    /// Waiting for another device to join right now.
    offering: bool,
    /// What the last complete check found the server keeping back — kept
    /// across restarts, so it shows before the first pass has run.
    withheld: Withheld,
}

#[tauri::command]
pub(crate) fn sync_status(
    state: State<'_, AppState>,
    sync: State<'_, Sync>,
) -> CommandResult<SyncStatus> {
    let store = &state.store;
    let s = store.sync_state().map_err(err)?;
    Ok(SyncStatus {
        paired: s.paired(),
        server_url: s.server_url,
        tls_fingerprint: s.tls_fingerprint,
        device_id: s.device_id,
        paired_ms: s.paired_ms,
        last_sync_ms: s.last_sync_ms,
        last: sync.last.lock().clone(),
        pending: store.pending_count().unwrap_or(0),
        running: sync.running.load(Ordering::SeqCst),
        vault: store.vault_status().map_err(err)?,
        device_name: device_name(),
        offering: sync.offer.lock().is_some(),
        withheld: store.withheld().unwrap_or_default(),
    })
}

/// This computer's name, the way the other devices will see it listed.
fn device_name() -> String {
    let from_env = ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .filter(|name| !name.trim().is_empty());
    let from_file = || {
        std::fs::read_to_string("/etc/hostname")
            .ok()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
    };
    // Apps started from the Dock or a menu get no HOSTNAME. macOS keeps the
    // name people gave their Mac apart from the network one.
    let from_command = || {
        let (program, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
            ("/usr/sbin/scutil", &["--get", "ComputerName"])
        } else {
            ("hostname", &[])
        };
        std::process::Command::new(program)
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|name| !name.is_empty())
    };
    from_env
        .or_else(from_file)
        .or_else(from_command)
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "Mac".into()
            } else {
                "UwURDP".into()
            }
        })
        .chars()
        .take(64)
        .collect()
}

fn clean_name(name: &str) -> SyncResult<String> {
    let name: String = name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    if name.is_empty() {
        return Err(SyncFailure::error("the device needs a name"));
    }
    Ok(name)
}

/// The master password is right: checked against the vault that is here,
/// before anything is wrapped again with it.
fn check_password(store: &Store, password: &[u8]) -> SyncResult<()> {
    match store.vault_status()? {
        VaultStatus::Absent => Ok(()),
        _ => store.unlock_vault(password).map_err(SyncFailure::from),
    }
}

/// Whether the vault should be remembered on this device after pairing: only
/// if it was before. A remembered vault opens for anything running as this
/// user, so that is a choice the person makes where they can see it — the
/// vault dialog's checkbox, the next time it asks — never one pairing makes
/// for them.
fn wants_remembered(store: &Store) -> bool {
    store.vault_is_remembered().unwrap_or(false)
}

/// Keep a remembered vault remembered: a vault key that changed (joining
/// another account's vault) needs sealing again.
fn keep_remembered(store: &Store, was_remembered: bool) {
    if was_remembered {
        if let Err(error) = store.remember_vault(crate::device::protect) {
            tracing::warn!(%error, "could not remember the vault again");
        }
    }
}

// ── Connecting ─────────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Connected {
    /// The account key as the recovery kit shows it: right after the account
    /// was made, and again on a paired device for its master password.
    recovery_code: String,
    server_url: String,
    tls_fingerprint: Option<String>,
}

/// First device: a setup code from `uwusync-server invite`, the master
/// password, a name. Makes the account and hands back the recovery kit.
#[tauri::command]
pub(crate) async fn sync_connect(
    app: AppHandle,
    code: String,
    password: String,
    name: String,
) -> SyncResult<Connected> {
    let password = Zeroizing::new(password);
    let setup = flow::parse_setup(&code).ok_or(SyncFailure::BadCode)?;
    let name = clean_name(&name)?;
    blocking(&app, move |app, store, sync| {
        if store.sync_state()?.paired() {
            return Err(SyncFailure::error("this device is already paired"));
        }
        if password.trim().is_empty() {
            return Err(SyncFailure::error("the master password cannot be empty"));
        }
        check_password(store, password.as_bytes())?;
        let remembered = wants_remembered(store);
        let (server, paired) = flow::create_account(
            store,
            &setup,
            password.as_bytes(),
            &name,
            crate::device::protect_sync,
        )?;
        keep_remembered(store, remembered);
        let recovery_code = paired
            .recovery
            .as_ref()
            .map(|key| key.to_code())
            .unwrap_or_default();
        *sync.server.lock() = Some(Arc::new(server));
        sync.poke();
        let _ = app.emit("sync:status", ());
        Ok(Connected {
            recovery_code,
            server_url: setup.server_url,
            tls_fingerprint: setup.tls_fingerprint,
        })
    })
    .await
}

/// A code another device showed, as pasted (`uwu2_…`) or as read out
/// (`K7M4Q-tiger-radio-kiwi`, with the server's address and fingerprint).
fn target_of(
    code: &str,
    server_url: Option<&str>,
    fingerprint: Option<&str>,
) -> Option<pairing::Target> {
    if let Some(target) = pairing::parse_offer(code) {
        return Some(target);
    }
    let (id, words) = pairing::parse_spoken(code)?;
    let server_url = server_url?.trim();
    if server_url.is_empty() {
        return None;
    }
    Some(pairing::Target {
        server_url: server_url.to_string(),
        tls_fingerprint: fingerprint
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(str::to_string),
        id,
        words,
    })
}

/// Another device: the code it showed, the account's master password, a name.
#[tauri::command]
pub(crate) async fn sync_join(
    app: AppHandle,
    code: String,
    server_url: Option<String>,
    fingerprint: Option<String>,
    password: String,
    name: String,
) -> SyncResult<()> {
    let password = Zeroizing::new(password);
    let target = target_of(&code, server_url.as_deref(), fingerprint.as_deref())
        .ok_or(SyncFailure::BadCode)?;
    let name = clean_name(&name)?;
    blocking(&app, move |app, store, sync| {
        if store.sync_state()?.paired() {
            return Err(SyncFailure::error("this device is already paired"));
        }
        // Hosts and secrets already here come along into the account's vault,
        // which needs them opened first — with this device's own password.
        if store.vault_status()? == VaultStatus::Locked {
            return Err(SyncFailure::VaultLocked);
        }
        let remembered = wants_remembered(store);
        let (server, _) = flow::join(
            store,
            &target,
            password.as_bytes(),
            &name,
            crate::device::protect_sync,
            Instant::now() + pairing::DEADLINE,
        )?;
        keep_remembered(store, remembered);
        *sync.server.lock() = Some(Arc::new(server));
        sync.poke();
        let _ = app.emit("sync:status", ());
        Ok(())
    })
    .await
}

// ── Adding a device from here ──────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfferShown {
    /// What to read out.
    spoken: String,
    /// What to paste, when the two can copy and paste.
    pasteable: String,
    server_url: String,
    tls_fingerprint: Option<String>,
    /// When the code stops working.
    expires_ms: u64,
}

/// The master password first: the other end of the code receives the account
/// key, and a window left open should not hand that to whoever sits down at it.
#[tauri::command]
pub(crate) async fn sync_offer(app: AppHandle, password: String) -> SyncResult<OfferShown> {
    let password = Zeroizing::new(password);
    blocking(&app, move |_, store, sync| {
        let header = store
            .vault_header()?
            .ok_or_else(|| SyncFailure::error("there is no vault"))?;
        let account_key = flow::account_key(store, crate::device::unprotect_sync)?;
        header
            .unlock_with(password.as_bytes(), account_key.as_ref())
            .map_err(|_| SyncFailure::PasswordWrong)?;
        let server = server(store, sync)?;
        let offer = flow::offer_pairing(store, &server)?;
        let state = store.sync_state()?;
        let shown = OfferShown {
            spoken: offer.offer.spoken.clone(),
            pasteable: offer.offer.pasteable.clone(),
            server_url: state.server_url.unwrap_or_default(),
            tls_fingerprint: state.tls_fingerprint,
            expires_ms: now_ms() + pairing::DEADLINE.as_millis() as u64,
        };
        // A new code replaces an old one; the old session closes.
        if let Some(old) = sync.offer.lock().replace(Arc::new(offer)) {
            let _ = server.close_pairing(&old.offer.id);
        }
        Ok(shown)
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeviceJoined {
    name: String,
}

/// Wait for the device the code was read out to. Returns when it joined, the
/// code ran out, or the words didn't match.
#[tauri::command]
pub(crate) async fn sync_wait_for_device(app: AppHandle) -> SyncResult<DeviceJoined> {
    blocking(&app, |app, store, sync| {
        let offer = sync
            .offer
            .lock()
            .clone()
            .ok_or_else(|| SyncFailure::error("no code is being shown"))?;
        if sync.waiting.swap(true, Ordering::SeqCst) {
            return Err(SyncFailure::error("this code is already being waited on"));
        }
        struct Waiting<'a>(&'a AtomicBool);
        impl Drop for Waiting<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _waiting = Waiting(&sync.waiting);
        let server = server(store, sync)?;
        let joined = flow::wait_for_device(
            store,
            &server,
            &offer,
            crate::device::unprotect_sync,
            Instant::now() + pairing::DEADLINE,
        );
        // This offer is done, whatever came of it — unless a newer one took
        // its place meanwhile.
        {
            let mut current = sync.offer.lock();
            if current.as_ref().is_some_and(|c| Arc::ptr_eq(c, &offer)) {
                *current = None;
            }
        }
        let joined = joined?;
        let _ = app.emit("sync:status", ());
        Ok(DeviceJoined {
            name: joined.device_name,
        })
    })
    .await
}

#[tauri::command]
pub(crate) async fn sync_cancel_offer(app: AppHandle) -> SyncResult<()> {
    blocking(&app, |_, store, sync| {
        if let Some(offer) = sync.offer.lock().take() {
            if let Ok(server) = server(store, sync) {
                let _ = server.close_pairing(&offer.offer.id);
            }
        }
        Ok(())
    })
    .await
}

// ── Devices ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeviceRow {
    id: Uuid,
    name: String,
    created_ms: u64,
    last_seen_ms: Option<u64>,
    revoked_ms: Option<u64>,
    /// This device.
    current: bool,
}

#[tauri::command]
pub(crate) async fn sync_devices(app: AppHandle) -> SyncResult<Vec<DeviceRow>> {
    blocking(&app, |_, store, sync| {
        let me = store.sync_state()?.device_id;
        let server = server(store, sync)?;
        let devices = server
            .devices()
            .inspect_err(|_| *sync.server.lock() = None)?;
        Ok(devices
            .into_iter()
            .map(|device| DeviceRow {
                current: Some(device.id) == me,
                id: device.id,
                name: device.name,
                created_ms: device.created_ms,
                last_seen_ms: device.last_seen_ms,
                revoked_ms: device.revoked_ms,
            })
            .collect())
    })
    .await
}

/// Shut another device out. Needs the master password: the login key comes
/// from it, so a stolen device can't lock its owner out.
#[tauri::command]
pub(crate) async fn sync_revoke(
    app: AppHandle,
    device_id: Uuid,
    password: String,
) -> SyncResult<()> {
    let password = Zeroizing::new(password);
    blocking(&app, move |_, store, sync| {
        let state = store.sync_state()?;
        if state.device_id == Some(device_id) {
            return Err(SyncFailure::error(
                "this device leaves with “Disconnect”, not by revoking itself",
            ));
        }
        let header = store
            .vault_header()?
            .ok_or_else(|| SyncFailure::error("there is no vault"))?;
        let account_key = flow::account_key(store, crate::device::unprotect_sync)?;
        let login_key = header
            .login_key(password.as_bytes(), account_key.as_ref())
            .map_err(|_| SyncFailure::PasswordWrong)?;
        let server = server(store, sync)?;
        server.revoke(device_id, Some(login_key.as_slice()))?;
        Ok(())
    })
    .await
}

/// A pass now, instead of at the next tick.
#[tauri::command]
pub(crate) fn sync_now(sync: State<'_, Sync>) {
    sync.poke();
}

/// Stop syncing on this device. The hosts stay; the vault is wrapped again
/// without the account key, so the master password alone opens it once more.
/// The server is told this device is gone, as far as it can be reached.
#[tauri::command]
pub(crate) async fn sync_disconnect(app: AppHandle, password: String) -> SyncResult<()> {
    let password = Zeroizing::new(password);
    blocking(&app, move |app, store, sync| {
        let state = store.sync_state()?;
        if !state.paired() {
            return Ok(());
        }
        // The password first, against the vault as it is: wrong means nothing
        // changes.
        let header = store
            .vault_header()?
            .ok_or_else(|| SyncFailure::error("there is no vault"))?;
        let account_key = flow::account_key(store, crate::device::unprotect_sync)?;
        let unlocked = header
            .unlock_with(password.as_bytes(), account_key.as_ref())
            .map_err(|_| SyncFailure::PasswordWrong)?;
        drop(unlocked);
        if store.vault_status()? != VaultStatus::Unlocked {
            store.unlock_vault_with(password.as_bytes(), account_key.as_ref())?;
        }
        if let (Some(me), Ok(server)) = (state.device_id, server(store, sync)) {
            if let Err(error) = server.revoke(me, None) {
                tracing::info!(%error, "the server could not be told this device left");
            }
        }
        store.rewrap_vault(password.as_bytes(), None)?;
        store.forget_enrolment()?;
        *sync.server.lock() = None;
        *sync.last.lock() = None;
        if let Some(offer) = sync.offer.lock().take() {
            drop(offer);
        }
        let _ = app.emit("sync:status", ());
        Ok(())
    })
    .await
}

/// The recovery kit again, for a device that kept the account key: the kit
/// shown when the account was made is easily lost, and every paired device
/// holds the key anyway. The master password first, so an unattended but
/// unlocked window does not hand it out.
#[tauri::command]
pub(crate) async fn sync_recovery_code(app: AppHandle, password: String) -> SyncResult<Connected> {
    let password = Zeroizing::new(password);
    blocking(&app, move |_, store, _| {
        let state = store.sync_state()?;
        if !state.paired() {
            return Err(SyncFailure::error("this device is not paired"));
        }
        let header = store
            .vault_header()?
            .ok_or_else(|| SyncFailure::error("there is no vault"))?;
        let account_key = flow::account_key(store, crate::device::unprotect_sync)?
            .ok_or_else(|| SyncFailure::error("this device kept no account key"))?;
        header
            .unlock_with(password.as_bytes(), Some(&account_key))
            .map_err(|_| SyncFailure::PasswordWrong)?;
        Ok(Connected {
            recovery_code: account_key.to_code(),
            server_url: state.server_url.unwrap_or_default(),
            tls_fingerprint: state.tls_fingerprint,
        })
    })
    .await
}

/// The account key this device kept, for unlocking a vault that needs one.
/// `None` when it isn't paired, or what it kept no longer opens.
pub(crate) fn kept_account_key(store: &Store) -> Option<uwurdp_vault::AccountKey> {
    flow::account_key(store, crate::device::unprotect_sync)
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spoken_code_needs_the_server_it_belongs_to() {
        let words = pairing::words();
        let spoken = format!("K7M4Q-{words}");
        assert!(target_of(&spoken, None, None).is_none());
        assert!(target_of(&spoken, Some("  "), None).is_none());
        let target =
            target_of(&spoken, Some("https://nas.lan:8443"), Some(" SHA256:abc ")).unwrap();
        assert_eq!(target.id, "K7M4Q");
        assert_eq!(target.words, words);
        assert_eq!(target.tls_fingerprint.as_deref(), Some("SHA256:abc"));
        assert!(target_of("K7M4Q-not-our-words", Some("https://nas.lan"), None).is_none());
    }

    #[test]
    fn a_pasted_code_brings_its_own_server() {
        let offer = pairing::offer("K7M4Q", &pairing::words(), "https://nas.lan:8443", None);
        let target = target_of(&offer.pasteable, None, None).unwrap();
        assert_eq!(target.server_url, "https://nas.lan:8443");
    }

    #[test]
    fn a_device_name_is_one_line_and_not_empty() {
        assert_eq!(clean_name("  Laptop\n\t ").unwrap(), "Laptop");
        assert!(clean_name(" \n ").is_err());
        assert_eq!(clean_name(&"x".repeat(200)).unwrap().len(), 64);
    }
}
