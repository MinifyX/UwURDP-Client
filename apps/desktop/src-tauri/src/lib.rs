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
//! - [`system`] — updates, links, a fresh start for a reloaded page

mod backup;
mod device;
mod dialogs;
mod hosts;
mod import;
mod sessions;
mod sync;
mod system;
mod updates;

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::Manager;
use uwurdp_core::{FrameSink, ObservedCertificate, SessionId, SessionManager, SinkError};
use uwurdp_store::Store;

pub(crate) struct AppState {
    pub sessions: Arc<SessionManager>,
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
}

/// Desktop frames on their way to the webview.
///
/// This is the whole Tauri-specific surface of the data path. Everything else —
/// decoding, coalescing, flow control — is transport-agnostic in `uwurdp-core`.
pub(crate) struct ChannelSink {
    pub channel: Channel<InvokeResponseBody>,
}

impl FrameSink for ChannelSink {
    fn send(&self, frame: &[u8]) -> Result<(), SinkError> {
        self.channel
            .send(InvokeResponseBody::Raw(frame.to_vec()))
            .map_err(|err| SinkError::Other(err.to_string()))
    }

    /// An empty frame is the end-of-stream marker; real frames are never empty.
    fn finish(&self) {
        let _ = self.channel.send(InvokeResponseBody::Raw(Vec::new()));
    }
}

pub(crate) type CommandResult<T> = Result<T, String>;

pub(crate) fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

pub fn run() {
    system::restrict_dll_search();

    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("UWURDP_LOG").unwrap_or_else(|_| "uwurdp=debug,warn".to_string()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
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
            // A vault this device keeps the key for opens right away, so the
            // master password is a once-per-device thing.
            if let Err(error) = store.unlock_remembered_vault(device::unprotect) {
                tracing::warn!(%error, "could not open the vault with this device's key");
            }

            app.manage(AppState {
                sessions: Arc::new(SessionManager::new()),
                store: Arc::new(store),
                presented: Mutex::new(HashMap::new()),
                session_hosts: Mutex::new(HashMap::new()),
                picked_export: backup::PickedExport::default(),
                pending_import: import::PendingImport::default(),
            });
            updates::start(app.handle());
            sync::start(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            sessions::rdp_input,
            sessions::resize_session,
            sessions::ack_frame,
            sessions::clipboard_changed,
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
            system::close_all_sessions,
            system::set_update_channel,
            system::update_status,
            system::check_for_updates,
            system::install_update,
            system::open_project_page,
        ])
        .run(tauri::generate_context!())
        .expect("failed to start UwURDP");
}
