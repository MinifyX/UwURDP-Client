//! The Tauri host.
//!
//! This layer is thin on purpose: it turns IPC calls into calls on the crates
//! and pipes frames back. The engine lives in `uwurdp-core`, the data in
//! `uwurdp-store`, and both are tested without a window.
//!
//! - [`sessions`] — input, size and frames of an open desktop
//! - [`hosts`] — the host list, groups, connecting, certificate decisions
//! - [`import`] — the vault and importing RDCMan's and mstsc's files
//! - [`backup`] — exporting to and importing from `.uwurdp` files
//! - [`sync`] — Settings → Sync and the thread that keeps devices in step
//! - [`lock`] — the same through UwULock: signing in, the move from UwUSync,
//!   the realtime channel
//! - [`system`] — what this build can do, updates, links, a fresh start for
//!   a reloaded page
//! - [`deep_link`] — `uwurdp://connect/<host-id>`, and one UwURDP at a time
//! - [`h264`] — Cisco's OpenH264, fetched when the user turns H.264 on
//! - [`sandbox_access`] — shared folders through the Mac App Store's sandbox
//!
//! Two builds come out of this crate (docs/app-store.md): the default one,
//! which every download from GitHub is (`self-update`, `h264-download`), and
//! the Mac App Store's (`--no-default-features --features mas`): sandboxed,
//! no updater compiled in, no OpenH264 download.

mod backup;
mod deep_link;
mod device;
mod dialogs;
mod frames;
mod h264;
mod hosts;
mod import;
mod lock;
mod sandbox_access;
mod sessions;
mod sync;
mod system;
#[cfg(feature = "self-update")]
mod updates;

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::Manager;
use tauri_plugin_deep_link::DeepLinkExt;
use uwurdp_core::{ObservedCertificate, SessionId, SessionManager};
use uwurdp_store::Store;

pub(crate) struct AppState {
    pub sessions: Arc<SessionManager>,
    /// Where desktop frames, acks and input travel; see [`frames`].
    pub frames: Arc<frames::FrameServer>,
    pub store: Arc<Store>,
    /// Certificates a server presented in the last connection attempt, per
    /// address and port. Trusting one is only possible for one in here, so a
    /// compromised webview cannot hand in a certificate of its own choosing.
    /// Each entry expires, so one the user declined doesn't stay trustable.
    pub presented: Mutex<HashMap<(String, u16), (ObservedCertificate, std::time::Instant)>>,
    /// Which host each open desktop belongs to.
    pub session_hosts: Mutex<HashMap<SessionId, uuid::Uuid>>,
    pub picked_export: backup::PickedExport,
    pub pending_import: import::PendingImport,
    /// The sandbox's access to each open desktop's shared folders (Mac App
    /// Store build), switched off when the desktop closes.
    pub drive_access: Mutex<HashMap<SessionId, Vec<sandbox_access::Access>>>,
}

pub(crate) type CommandResult<T> = Result<T, String>;

pub(crate) fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

pub fn run() {
    system::restrict_dll_search();
    #[cfg(feature = "self-update")]
    updates::wait_for_previous();

    let mut builder = tauri::Builder::default();
    // One UwURDP at a time: starting it again, or opening a link, brings the
    // running one to the front, which then handles the link. It goes first,
    // so a second start ends before it does anything else. A trial copy with
    // its own host list (UWURDP_DB) runs beside the real one.
    //
    // Not in the Mac App Store build: the plugin's socket lives in /tmp,
    // which the sandbox closes, and macOS keeps an app to one copy anyway,
    // handing links to the running one through `on_open_url`.
    if !cfg!(feature = "mas") && std::env::var_os("UWURDP_DB").is_none() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // A link among the arguments reaches `deep_link::received` through
            // the deep-link plugin; a plain second start just shows the window.
            deep_link::show(app);
        }));
    }
    let builder = builder
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init());
    // The updater's own commands are in no capability, so the page cannot
    // reach them; `updates.rs` drives it from here. Not in the Mac App Store
    // build, which the store updates.
    #[cfg(feature = "self-update")]
    let builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    builder
        .setup(|app| {
            init_logging(app.path().app_log_dir().ok());
            tracing::info!(
                version = %app.package_info().version,
                store = system::BUILD_INFO.store,
                "UwURDP starting"
            );

            // macOS ends an app without asking the window; this asks the page
            // first (`onMacQuit`, answered through `finish_quit`), which asks
            // the user while desktops are open.
            #[cfg(target_os = "macos")]
            {
                use tauri::Emitter as _;
                let handle = app.handle().clone();
                if let Err(error) = uwu_macos::install_quit_guard(move || {
                    handle.emit(uwu_macos::QUIT_EVENT, ()).is_ok()
                }) {
                    tracing::warn!(%error, "quit guard");
                }
            }

            #[cfg(feature = "self-update")]
            if updates::apply_pending_on_start(app.handle()) {
                // The downloaded setup replaces this version and starts UwURDP again.
                std::process::exit(0);
            }

            // Same place as UwUMail keeps its database:
            // %APPDATA%\app.uwurdp.desktop\uwurdp.db on Windows.
            // UWURDP_DB points elsewhere, so trying things out never touches
            // the real host list.
            let path = match std::env::var_os("UWURDP_DB") {
                Some(path) => std::path::PathBuf::from(path),
                None => app.path().app_data_dir()?.join("uwurdp.db"),
            };
            let store = Store::open(&path)?;
            tracing::info!(path = %path.display(), "store open");
            if let Some(folder) = path.parent() {
                sandbox_access::configure(folder);
            }
            #[cfg(all(target_os = "macos", feature = "mas"))]
            sync::warm_device_name();
            // A vault this device keeps the key for opens right away, so the
            // master password is a once-per-device thing.
            if let Err(error) = store.unlock_remembered_vault(device::unprotect) {
                tracing::warn!(%error, "could not open the vault with this device's key");
            }

            let sessions = Arc::new(SessionManager::new());
            let frames = frames::FrameServer::start(sessions.clone())?;
            app.manage(AppState {
                sessions,
                frames,
                store: Arc::new(store),
                presented: Mutex::new(HashMap::new()),
                session_hosts: Mutex::new(HashMap::new()),
                picked_export: backup::PickedExport::default(),
                pending_import: import::PendingImport::default(),
                drive_access: Mutex::new(HashMap::new()),
            });
            app.manage(h264::Codec::new(app.handle()));
            #[cfg(feature = "self-update")]
            updates::start(app.handle());
            sync::start(app.handle());

            app.manage(deep_link::Pending::default());
            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                deep_link::received(&handle, event.urls().iter().map(|url| url.as_str()));
            });
            // A link that started UwURDP (Windows and Linux pass it as an
            // argument; on macOS it arrives through `on_open_url`).
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                deep_link::received(app.handle(), urls.iter().map(|url| url.as_str()));
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            finish_quit,
            system::build_info,
            sessions::frame_socket,
            sessions::resize_session,
            sessions::clipboard_changed,
            sessions::clipboard_download,
            sessions::clipboard_offer_files,
            sessions::close_session,
            sessions::set_fullscreen,
            hosts::list_hosts,
            hosts::save_host,
            hosts::delete_host,
            hosts::set_host_login,
            hosts::list_groups,
            hosts::create_group,
            hosts::rename_group,
            hosts::delete_group,
            hosts::set_group_login,
            hosts::set_group_drives,
            hosts::pick_shared_folder,
            hosts::move_group,
            hosts::move_host,
            hosts::connect_host,
            hosts::cancel_connect,
            hosts::trust_certificate,
            import::vault_status,
            import::vault_state,
            import::create_vault,
            import::unlock_vault,
            import::repair_vault,
            import::set_vault_remembered,
            import::lock_vault,
            import::rdcman_files,
            import::pick_import_files,
            import::scan_rdcman_file,
            import::run_import,
            backup::export_hosts,
            backup::pick_export_file,
            backup::read_export_file,
            backup::import_export_file,
            sync::sync_status,
            sync::sync_connect,
            sync::sync_join,
            sync::sync_offer,
            sync::sync_wait_for_device,
            sync::sync_cancel_offer,
            sync::sync_devices,
            sync::sync_revoke,
            sync::sync_now,
            sync::sync_disconnect,
            sync::sync_recovery_code,
            lock::lock_sign_in,
            lock::lock_send_email_code,
            lock::lock_move,
            lock::lock_leave_uwusync,
            lock::lock_forget_move,
            lock::lock_app_sync_off,
            lock::lock_sign_out,
            system::close_all_sessions,
            system::set_update_channel,
            system::update_status,
            system::check_for_updates,
            system::install_update,
            system::open_project_page,
            system::h264_status,
            system::set_h264,
            deep_link::take_deep_link,
            deep_link::sync_for_link,
        ])
        .build(tauri::generate_context!())
        .expect("failed to start UwURDP")
        .run(|app, event| {
            // macOS: closing the window with no desktop open only hides it
            // (the page's `hideWindowOnClose`); a click on the Dock icon
            // brings it back.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen {
                has_visible_windows: false,
                ..
            } = event
            {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (app, event);
        });
}

/// The page's answer to a quit from the Dock, ⌘Q or a logout (macOS): go
/// ahead, or stay because the user kept the open desktops. Does nothing
/// elsewhere.
#[tauri::command]
fn finish_quit(proceed: bool) {
    uwu_macos::reply_quit(proceed);
}

/// The log file stops growing here; what matters is usually near the start
/// or is a panic, and a runaway warning must not fill the disk.
const LOG_LIMIT: u64 = 20 * 1024 * 1024;

/// Logs to stdout and, so that a crash on someone's machine leaves a trace,
/// to `uwurdp.log` in the app's log folder (`%LOCALAPPDATA%\app.uwurdp.desktop\logs`
/// on Windows). The previous run's file is kept as `uwurdp.old.log`. Panics
/// are logged too, with where they happened, before they end the thread.
fn init_logging(dir: Option<std::path::PathBuf>) {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let filter = tracing_subscriber::EnvFilter::new(
        std::env::var("UWURDP_LOG").unwrap_or_else(|_| "uwurdp=debug,warn".to_string()),
    );
    let file = dir.and_then(|dir| {
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("uwurdp.log");
        let _ = std::fs::rename(&path, dir.join("uwurdp.old.log"));
        std::fs::File::create(path).ok()
    });
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(file.map(|file| {
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(LogFile::new(file))
        }))
        .try_init();

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        tracing::error!(
            thread = thread.name().unwrap_or("unnamed"),
            "panic: {info}\n{}",
            std::backtrace::Backtrace::force_capture()
        );
        default_hook(info);
    }));
}

/// A log file that stops taking lines at [`LOG_LIMIT`].
struct LogFile {
    file: Mutex<std::fs::File>,
    written: std::sync::atomic::AtomicU64,
}

impl LogFile {
    fn new(file: std::fs::File) -> Self {
        Self {
            file: Mutex::new(file),
            written: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

impl std::io::Write for &LogFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use std::sync::atomic::Ordering;
        let len = buf.len() as u64;
        if self.written.fetch_add(len, Ordering::Relaxed) + len > LOG_LIMIT {
            // Swallowed, not an error: logging must never fail the app.
            return Ok(buf.len());
        }
        self.file.lock().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.lock().flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogFile {
    type Writer = &'a LogFile;

    fn make_writer(&'a self) -> Self::Writer {
        self
    }
}
