//! Settings → Sync, when it syncs through UwULock: signing in, signing out,
//! the move from UwUSync, and the thread that listens to UwULock's realtime
//! channel.
//!
//! The work is `uwurdp_sync::lock`'s; this layer adds what the operating
//! system keeps (the refresh token and the token that skips two-step login,
//! sealed like the UwUSync pairing's secrets, see [`crate::device`]) and the
//! words the page shows. The master password goes into one command and is
//! wiped when it returns; the keys it opens are never kept, only the space's.

use crate::sync::{self, blocking, Sync, SyncFailure};
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use uwurdp_store::{Joining, LockEnrolment, LockState, Store, VaultStatus};
use uwurdp_sync::lock::{
    self, live, AcceptSpace, Lock, LockDevice, MoveReport, SignIn, SignInOutcome, SignedIn,
    TwoFactorAnswer, TwoFactorMethod,
};
use uwurdp_vault::KdfParams;
use zeroize::Zeroizing;

type SyncResult<T> = Result<T, SyncFailure>;

/// What the page shows about UwULock.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LockStatus {
    server_url: Option<String>,
    email: Option<String>,
    /// The session ended: the master password is needed to go on syncing.
    needs_sign_in: bool,
    /// The realtime channel is open.
    live: bool,
    live_refused: bool,
    signed_in_ms: Option<u64>,
    /// A move from UwUSync began and did not finish.
    move_started_ms: Option<u64>,
    /// The server has app sync switched off; syncing waits, the session stays.
    switched_off: bool,
    /// The move is done, and this device may still be removed from UwUSync.
    left_behind: bool,
}

impl LockStatus {
    pub(crate) fn of(state: LockState, sync: &Sync) -> Self {
        Self {
            needs_sign_in: state.active && !state.signed_in,
            server_url: state.server_url,
            email: state.email,
            live: state.active && sync.live.load(Ordering::SeqCst),
            live_refused: sync.live_refused.load(Ordering::SeqCst),
            signed_in_ms: state.signed_in_ms,
            move_started_ms: state.move_started_ms,
            switched_off: state.active && sync.switched_off.load(Ordering::SeqCst),
            left_behind: sync.left_behind.lock().is_some(),
        }
    }
}

/// The code of the second step, as the page sends it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TwoFactorInput {
    provider: u8,
    code: String,
    remember: bool,
}

/// The person agreed to take the space the account has now, after
/// [`LockOutcome::SpaceChanged`]: its id, or none to make a new one.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AcceptSpaceInput {
    id: Option<String>,
}

impl AcceptSpaceInput {
    fn parse(self) -> SyncResult<AcceptSpace> {
        match self.id {
            None => Ok(AcceptSpace::New),
            Some(id) => uuid::Uuid::parse_str(&id)
                .map(AcceptSpace::Listed)
                .map_err(|_| SyncFailure::error("that is no space id")),
        }
    }
}

/// How a sign-in (or the move, which begins with one) came out.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum LockOutcome {
    /// Signed in; `madeSpace` when this was the first UwURDP on the account.
    SignedIn {
        #[serde(rename = "madeSpace")]
        made_space: bool,
    },
    /// Moved: the copy was checked and this device syncs through UwULock.
    Moved {
        report: MoveReport,
        /// No other device of the UwUSync account is left.
        #[serde(rename = "lastDevice")]
        last_device: bool,
    },
    /// A code is needed; `message` says why when one was given and refused.
    TwoFactor {
        methods: Vec<TwoFactorMethod>,
        message: Option<String>,
    },
    /// The account's space is not the one this device used there (`now` is
    /// `None` when there is none): another device gave it a new key, it was
    /// deleted, or the server misbehaves. Nothing changed here; the page asks
    /// and signs in again with `acceptSpace`.
    SpaceChanged { was: String, now: Option<String> },
}

fn device(store: &Store) -> SyncResult<LockDevice> {
    Ok(LockDevice::this_system(store.lock_device_identifier()?))
}

/// The worker's pass flag, held: a pass that runs now is waited for, and
/// none starts until this is dropped.
struct Running<'a>(&'a Sync);

impl<'a> Running<'a> {
    fn take(sync: &'a Sync) -> SyncResult<Self> {
        let until = std::time::Instant::now() + Duration::from_secs(120);
        while sync.running.swap(true, Ordering::SeqCst) {
            if std::time::Instant::now() > until {
                return Err(SyncFailure::error("a sync pass does not finish"));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(Self(sync))
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::SeqCst);
    }
}

/// Keep a new refresh token, sealed, as soon as the server hands it out.
fn keeper(store: &Arc<Store>) -> impl Fn(&str) + Send + std::marker::Sync + 'static {
    let store = Arc::clone(store);
    move |token: &str| match crate::device::protect_lock(token.as_bytes()) {
        Ok(sealed) => {
            if let Err(error) = store.keep_lock_refresh_token(&sealed) {
                tracing::warn!(%error, "could not keep the new UwULock refresh token");
            }
        }
        Err(error) => tracing::warn!(%error, "could not seal the new UwULock refresh token"),
    }
}

/// UwULock, signed in: the cached connection, or one made from what this
/// device kept.
pub(crate) fn connection(store: &Arc<Store>, sync: &Sync) -> SyncResult<Arc<Lock>> {
    if let Some(lock) = sync.lock.lock().clone() {
        return Ok(lock);
    }
    let state = store.lock_state()?;
    let (true, Some(url), Some(space)) = (state.active, state.server_url, state.space_id) else {
        return Err(SyncFailure::error(
            "this device does not sync through UwULock",
        ));
    };
    let refresh = store
        .lock_refresh_token(crate::device::unprotect_lock)?
        .ok_or(SyncFailure::SignIn)?;
    let lock = Arc::new(
        Lock::connect(&url, device(store)?)?
            .with_space(space)
            .with_refresh_token(refresh)
            .keeping(keeper(store)),
    );
    *sync.lock.lock() = Some(Arc::clone(&lock));
    Ok(lock)
}

/// The session is over: nothing kept that would try it again.
pub(crate) fn session_ended(store: &Store, sync: &Sync) {
    if let Err(error) = store.end_lock_session() {
        tracing::warn!(%error, "could not forget the UwULock session");
    }
    *sync.lock.lock() = None;
}

/// Sign in, the way both commands begin.
fn sign_in(
    store: &Store,
    server_url: &str,
    email: &str,
    password: &str,
    two_factor: Option<TwoFactorInput>,
    accept_space: Option<AcceptSpaceInput>,
) -> SyncResult<Result<SignedIn, LockOutcome>> {
    let server = lock::normalize_server(server_url).ok();
    // The token that skips the second step, on every sign-in without a code:
    // after the session ended, after a restart, with the vault locked. It
    // goes only to the server and account that issued it.
    let remembered = match &two_factor {
        None => lock::remember_token(store, server_url, email, crate::device::unprotect_lock)?,
        Some(_) => None,
    };
    let known = lock::known(store, server_url, email)?;
    let request = SignIn {
        server_url,
        email,
        password,
        two_factor: two_factor.map(|input| TwoFactorAnswer {
            provider: input.provider,
            code: input.code,
            remember: input.remember,
        }),
        remember_token: remembered.as_deref().map(String::as_str),
        known,
        accept_space: accept_space.map(AcceptSpaceInput::parse).transpose()?,
    };
    Ok(match lock::sign_in(&request, device(store)?)? {
        SignInOutcome::SignedIn(signed) => Ok(*signed),
        SignInOutcome::TwoFactor { methods, message } => {
            Err(LockOutcome::TwoFactor { methods, message })
        }
        SignInOutcome::SpaceChanged {
            was,
            now,
            remember_token,
        } => {
            // Kept for the next try, which would need a new code otherwise.
            if let (Some(token), Some(server)) = (remember_token, &server) {
                store.keep_lock_remember_token(server, email, &seal(&token)?)?;
            }
            Err(LockOutcome::SpaceChanged {
                was: was.to_string(),
                now: now.map(|id| id.to_string()),
            })
        }
    })
}

/// The refresh token and the "remember this device" token, sealed.
type SealedTokens = (Option<Vec<u8>>, Option<Vec<u8>>);

/// The tokens of a sign-in, sealed for keeping. A new "remember me" token
/// replaces the account's old one; without one, the store keeps the old one
/// — of this server and account, never another's.
fn sealed_tokens(signed: &SignedIn) -> SyncResult<SealedTokens> {
    let refresh = signed.refresh_token.as_ref().map(seal).transpose()?;
    let remember = signed.remember_token.as_ref().map(seal).transpose()?;
    Ok((refresh, remember))
}

fn seal(token: &Zeroizing<String>) -> SyncResult<Vec<u8>> {
    crate::device::protect_lock(token.as_bytes())
        .map_err(|error| SyncFailure::error(format!("the operating system: {error}")))
}

/// Start syncing with what a sign-in brought.
fn start_syncing(app: &AppHandle, store: &Arc<Store>, sync: &Sync, signed: SignedIn) {
    let lock = signed.lock.keeping(keeper(store));
    *sync.lock.lock() = Some(Arc::new(lock));
    sync.live_refused.store(false, Ordering::SeqCst);
    sync.poke();
    let _ = app.emit("sync:status", ());
}

/// Sign in to UwULock and sync through it from now on. For a device that
/// does not sync yet, and for one whose UwULock session ended.
///
/// The master password is the UwULock account's; from now on it also opens
/// the vault here. Hosts and secrets already here come along into the
/// account, which needs the vault open first when it holds any — unless this
/// device's vault is the account's space already, as after signing out or
/// when the password changed on the server.
#[tauri::command]
pub(crate) async fn lock_sign_in(
    app: AppHandle,
    server_url: String,
    email: String,
    password: String,
    two_factor: Option<TwoFactorInput>,
    accept_space: Option<AcceptSpaceInput>,
) -> SyncResult<LockOutcome> {
    let password = Zeroizing::new(password);
    blocking(&app, move |app, store, sync| {
        if store.sync_state()?.paired() {
            return Err(SyncFailure::error(
                "this device syncs through UwUSync: move it to UwULock instead",
            ));
        }
        let state = store.lock_state()?;
        if store.vault_status()? == VaultStatus::Locked && state.space_id.is_none() {
            return Err(SyncFailure::VaultLocked);
        }
        let signed = match sign_in(
            store,
            &server_url,
            &email,
            &password,
            two_factor,
            accept_space,
        )? {
            Ok(signed) => signed,
            Err(outcome) => return Ok(outcome),
        };
        let (refresh, remember) = sealed_tokens(&signed)?;
        let kdf = signed.kdf_to_keep();
        let remembered = sync::wants_remembered(store);
        store.join_space(
            uwurdp_store::Space {
                id: signed.space.id,
                key: signed.space.key.clone(),
            },
            password.as_bytes(),
            KdfParams::RECOMMENDED,
            Joining::SignIn,
            &LockEnrolment {
                server_url: &signed.server_url,
                email: &signed.email,
                protected_refresh_token: refresh,
                protected_remember_token: remember,
                kdf: &kdf,
            },
        )?;
        sync::keep_remembered(store, remembered);
        let made_space = signed.made_space;
        start_syncing(app, store, sync, signed);
        Ok(LockOutcome::SignedIn { made_space })
    })
    .await
}

/// Whether the UwULock Server at `server_url` has app sync switched off, for
/// the forms to say so before anyone types a password. A server that doesn't
/// say, or can't be reached, is not switched off as far as the page knows.
#[tauri::command]
pub(crate) async fn lock_app_sync_off(app: AppHandle, server_url: String) -> SyncResult<bool> {
    blocking(&app, move |_, store, _| {
        Ok(lock::app_sync_switched_off(&server_url, device(store)?).unwrap_or(false))
    })
    .await
}

/// Ask UwULock to email a two-step code.
#[tauri::command]
pub(crate) async fn lock_send_email_code(
    app: AppHandle,
    server_url: String,
    email: String,
    password: String,
) -> SyncResult<()> {
    let password = Zeroizing::new(password);
    blocking(&app, move |_, store, _| {
        let known = lock::known(store, &server_url, &email)?;
        lock::send_email_code(&server_url, &email, &password, &known, device(store)?)?;
        Ok(())
    })
    .await
}

/// The one-click move from UwUSync to UwULock.
///
/// Signs in, sends everything this device has to UwUSync first, copies
/// what UwUSync holds to the account's space, reads the copy back and holds
/// it against what was read. Only then does this device switch: the vault
/// becomes the space, wrapped under the UwULock master password, and the
/// UwUSync pairing is forgotten — its data stays on UwUSync, untouched.
/// Any failure before that leaves this device on UwUSync as it was, and
/// running the move again carries on where it stopped.
#[tauri::command]
pub(crate) async fn lock_move(
    app: AppHandle,
    server_url: String,
    email: String,
    password: String,
    two_factor: Option<TwoFactorInput>,
    accept_space: Option<AcceptSpaceInput>,
) -> SyncResult<LockOutcome> {
    let password = Zeroizing::new(password);
    blocking(&app, move |app, store, sync| {
        let before = store.sync_state()?;
        let Some(me) = before.device_id.filter(|_| before.paired()) else {
            return Err(SyncFailure::error(
                "this device does not sync through UwUSync",
            ));
        };
        if store.vault_status()? != VaultStatus::Unlocked {
            return Err(SyncFailure::VaultLocked);
        }
        let signed = match sign_in(
            store,
            &server_url,
            &email,
            &password,
            two_factor,
            accept_space,
        )? {
            Ok(signed) => signed,
            Err(outcome) => return Ok(outcome),
        };
        store.note_move_started(&signed.server_url, &signed.email)?;
        let _ = app.emit("sync:status", ());

        // No pass of the worker in between: from here to the switch, this
        // device's records hold still.
        let running = Running::take(sync)?;
        let moved = (|| -> SyncResult<(MoveReport, Arc<uwurdp_sync::Server>)> {
            let uwusync = sync::server(store, sync)?;
            // What waits here goes to UwUSync first, so the copy has it too.
            uwurdp_sync::sync_once(store, uwusync.as_ref())?;
            let report = lock::copy_to_lock(store, uwusync.as_ref(), &signed.lock, &signed.space)?;
            let (refresh, remember) = sealed_tokens(&signed)?;
            let kdf = signed.kdf_to_keep();
            let remembered = sync::wants_remembered(store);
            store.join_space(
                uwurdp_store::Space {
                    id: signed.space.id,
                    key: signed.space.key.clone(),
                },
                password.as_bytes(),
                KdfParams::RECOMMENDED,
                Joining::Moved,
                &LockEnrolment {
                    server_url: &signed.server_url,
                    email: &signed.email,
                    protected_refresh_token: refresh,
                    protected_remember_token: remember,
                    kdf: &kdf,
                },
            )?;
            sync::keep_remembered(store, remembered);
            Ok((report, uwusync))
        })();
        drop(running);
        let (report, uwusync) = moved?;

        // Whether anyone is left on UwUSync, so the page can say the account
        // there may go. A server that does not answer now leaves it unsaid.
        let last_device = uwusync
            .devices()
            .map(|devices| {
                !devices
                    .iter()
                    .any(|device| device.id != me && device.revoked_ms.is_none())
            })
            .unwrap_or(false);
        *sync.server.lock() = None;
        *sync.left_behind.lock() = Some((uwusync, me));
        *sync.last.lock() = None;
        start_syncing(app, store, sync, signed);
        Ok(LockOutcome::Moved {
            report,
            last_device,
        })
    })
    .await
}

/// After the move: remove this device from UwUSync, or leave it listed
/// there. Either way the UwUSync session is let go.
#[tauri::command]
pub(crate) async fn lock_leave_uwusync(app: AppHandle, revoke: bool) -> SyncResult<()> {
    blocking(&app, move |app, _, sync| {
        let left = sync.left_behind.lock().take();
        if let (true, Some((server, me))) = (revoke, left) {
            server.revoke(me, None)?;
        }
        let _ = app.emit("sync:status", ());
        Ok(())
    })
    .await
}

/// Give up on a move that did not finish. Nothing to undo: this device
/// never stopped syncing through UwUSync.
#[tauri::command]
pub(crate) async fn lock_forget_move(app: AppHandle) -> SyncResult<()> {
    blocking(&app, move |app, store, _| {
        store.forget_move()?;
        let _ = app.emit("sync:status", ());
        Ok(())
    })
    .await
}

/// Stop syncing through UwULock on this device. Hosts and vault stay, and
/// the UwULock master password keeps opening it.
#[tauri::command]
pub(crate) async fn lock_sign_out(app: AppHandle, password: String) -> SyncResult<()> {
    let password = Zeroizing::new(password);
    blocking(&app, move |app, store, sync| {
        if !store.lock_state()?.active {
            return Ok(());
        }
        // The password first, against the vault as it is: wrong means
        // nothing changes.
        let header = store
            .vault_header()?
            .ok_or_else(|| SyncFailure::error("there is no vault"))?;
        header
            .unlock(password.as_bytes())
            .map_err(|_| SyncFailure::PasswordWrong)?;
        store.leave_lock()?;
        *sync.lock.lock() = None;
        *sync.last.lock() = None;
        sync.live_refused.store(false, Ordering::SeqCst);
        let _ = app.emit("sync:status", ());
        Ok(())
    })
    .await
}

// ── The realtime channel ──────────────────────────────────────────────────

/// How often the listener looks whether it should connect.
const IDLE: Duration = Duration::from_secs(2);

/// Starts the thread that keeps UwULock's realtime channel open while this
/// device syncs through UwULock and the vault is open. It lives as long as
/// the app.
pub(crate) fn start_live(app: &AppHandle) {
    let app = app.clone();
    std::thread::Builder::new()
        .name("uwurdp-live".into())
        .spawn(move || listen(app))
        .expect("the realtime thread starts");
}

fn listen(app: AppHandle) {
    let state = app.state::<crate::AppState>();
    let sync = app.state::<Sync>();
    let store = Arc::clone(&state.store);
    loop {
        let wanted = store
            .lock_state()
            .map(|s| s.active && s.signed_in)
            .unwrap_or(false)
            && matches!(store.vault_status(), Ok(VaultStatus::Unlocked))
            && !sync.live_refused.load(Ordering::SeqCst)
            && !sync.switched_off.load(Ordering::SeqCst);
        if !wanted {
            std::thread::sleep(IDLE);
            continue;
        }
        let lock = match connection(&store, &sync) {
            Ok(lock) => lock,
            Err(SyncFailure::SignIn) => {
                session_ended(&store, &sync);
                let _ = app.emit("sync:status", ());
                continue;
            }
            Err(_) => {
                std::thread::sleep(IDLE);
                continue;
            }
        };
        // Until the vault is locked, or this connection is replaced by a
        // sign-in or dropped by a sign-out.
        let keep_going = || {
            matches!(store.vault_status(), Ok(VaultStatus::Unlocked))
                && sync
                    .lock
                    .lock()
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &lock))
        };
        let ended = live::run(&lock, &keep_going, &mut |event| {
            if event == live::Event::Ready && !sync.live.swap(true, Ordering::SeqCst) {
                let _ = app.emit("sync:status", ());
            }
            sync.poke();
        });
        if sync.live.swap(false, Ordering::SeqCst) {
            let _ = app.emit("sync:status", ());
        }
        match ended {
            live::Ended::Stopped | live::Ended::Retry(_) => {}
            live::Ended::SignIn => {
                session_ended(&store, &sync);
                let _ = app.emit("sync:status", ());
            }
            live::Ended::Refused => {
                tracing::info!("UwULock refuses the realtime channel; polling instead");
                sync.live_refused.store(true, Ordering::SeqCst);
            }
            live::Ended::Logout(reason) => {
                // The server ended this device's session (signed out
                // everywhere, removed, disabled, keys rotated): nothing
                // decrypted stays open.
                tracing::info!(%reason, "UwULock ended this device's session");
                session_ended(&store, &sync);
                store.lock_vault();
                let _ = app.emit("sync:logout", reason);
                let _ = app.emit("sync:status", ());
            }
        }
    }
}
