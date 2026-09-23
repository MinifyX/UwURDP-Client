//! App-level commands: updates, links out of the app, and a fresh start for a
//! page that (re)loaded.

use crate::h264::{Codec, Status};
use crate::updates::{self, Channel, UpdateInfo};
use crate::{AppState, CommandResult};
use std::sync::Arc;
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;

/// A page just started. Whatever an earlier page left open can't be reached
/// from here any more, so it goes.
// Async on purpose: closing spawns a task on the runtime, and sync commands
// run on the main thread, outside it.
#[tauri::command]
pub(crate) async fn close_all_sessions(state: State<'_, AppState>) -> Result<usize, ()> {
    state.presented.lock().clear();
    state.session_hosts.lock().clear();
    Ok(state.sessions.close_all())
}

#[tauri::command]
pub(crate) fn set_update_channel(app: AppHandle, channel: Channel) {
    updates::set_channel(&app, channel);
}

/// Where OpenH264 stands (the page asks again while it downloads).
#[tauri::command]
pub(crate) fn h264_status(codec: State<'_, Arc<Codec>>) -> Status {
    codec.status()
}

/// The page hands over the H.264 setting on start and on every change.
#[tauri::command]
pub(crate) fn set_h264(codec: State<'_, Arc<Codec>>, enabled: bool) -> Status {
    codec.set_enabled(enabled)
}

/// A downloaded update waiting for a restart, if any.
#[tauri::command]
pub(crate) fn update_status(app: AppHandle) -> Option<UpdateInfo> {
    updates::ready(&app)
}

#[tauri::command]
pub(crate) async fn check_for_updates(app: AppHandle) -> CommandResult<Option<UpdateInfo>> {
    updates::check(&app).await
}

/// Async: a Linux package waits for the password prompt and the package
/// manager, which must not hold up the main thread.
#[tauri::command]
pub(crate) async fn install_update(app: AppHandle) -> CommandResult<()> {
    updates::install_now(&app).await
}

/// The project pages the app links to. The page names one; it never hands in
/// an address of its own.
#[tauri::command]
pub(crate) fn open_project_page(app: AppHandle, page: String) -> CommandResult<()> {
    let url = match page.as_str() {
        "source" => "https://github.com/MinifyX/UwURDP-Client",
        "releases" => "https://github.com/MinifyX/UwURDP-Client/releases",
        "issues" => "https://github.com/MinifyX/UwURDP-Client/issues",
        "license" => "https://www.gnu.org/licenses/gpl-3.0.html",
        _ => return Err(format!("unknown page: {page}")),
    };
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| format!("Couldn't open the browser: {e}"))
}

/// On Windows, DLLs loaded by name at runtime resolve from System32 only, never
/// the install folder or PATH. The runtime half of `/DEPENDENTLOADFLAG` in
/// `build.rs`, which only covers statically imported DLLs. Must run before
/// anything else loads a DLL.
pub(crate) fn restrict_dll_search() {
    #[cfg(windows)]
    // SAFETY: a process-wide flag, set once before any other thread exists.
    unsafe {
        use windows_sys::Win32::System::LibraryLoader::{
            SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_SYSTEM32,
        };
        SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32);
    }
}
