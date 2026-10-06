//! App-level commands: what this build can do, updates, links out of the
//! app, and a fresh start for a page that (re)loaded.

use crate::h264::{Codec, Status};
#[cfg(feature = "self-update")]
use crate::updates::{self, Channel, UpdateInfo};
use crate::{AppState, CommandResult};
use serde::Serialize;
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
    let closed = state.sessions.close_all();
    state.drive_access.lock().clear();
    Ok(closed)
}

/// What this build has, for the page to hide what it doesn't. The GitHub
/// builds have everything their platform allows; the Mac App Store build
/// (feature `mas`) is sandboxed and updated by the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BuildInfo {
    /// The Mac App Store build: sandboxed, no setup app, no self-update.
    pub store: bool,
    /// UwURDP finds and installs its own updates (the `self-update` feature).
    /// Off: no update channel, no "check now", no "restart to update".
    pub updates: bool,
    /// The H.264 switch can fetch Cisco's OpenH264 (the `h264-download`
    /// feature). Off: the setting has nothing to offer. A platform Cisco has
    /// no binary for still says so through `h264_status` ("unsupported").
    pub h264: bool,
    /// The "all drives" entry of drive redirection means something: every
    /// fixed drive, which only Windows has, and never in the sandbox.
    pub all_drives: bool,
    /// RDCMan's own list of open files can be read for a one-click import
    /// (Windows only). Off: importing goes through the open panel alone.
    pub rdcman_scan: bool,
}

pub(crate) const BUILD_INFO: BuildInfo = BuildInfo {
    store: cfg!(feature = "mas"),
    updates: cfg!(feature = "self-update"),
    h264: crate::h264::AVAILABLE,
    all_drives: cfg!(windows) && !cfg!(feature = "mas"),
    rdcman_scan: cfg!(windows) && !cfg!(feature = "mas"),
};

#[tauri::command]
pub(crate) fn build_info() -> BuildInfo {
    BUILD_INFO
}

/// The answer of every update command in a build that does not update itself.
#[cfg(not(feature = "self-update"))]
const NO_UPDATES: &str = "This build of UwURDP does not update itself.";

#[cfg(feature = "self-update")]
#[tauri::command]
pub(crate) fn set_update_channel(app: AppHandle, channel: Channel) {
    updates::set_channel(&app, channel);
}

/// Accepted and ignored, so a page that hands over its setting on start
/// needs no special case.
#[cfg(not(feature = "self-update"))]
#[tauri::command]
pub(crate) fn set_update_channel(channel: serde_json::Value) {
    let _ = channel;
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
#[cfg(feature = "self-update")]
#[tauri::command]
pub(crate) fn update_status(app: AppHandle) -> Option<UpdateInfo> {
    updates::ready(&app)
}

#[cfg(not(feature = "self-update"))]
#[tauri::command]
pub(crate) fn update_status() -> Option<()> {
    None
}

#[cfg(feature = "self-update")]
#[tauri::command]
pub(crate) async fn check_for_updates(app: AppHandle) -> CommandResult<Option<UpdateInfo>> {
    updates::check(&app).await
}

#[cfg(not(feature = "self-update"))]
#[tauri::command]
pub(crate) async fn check_for_updates() -> CommandResult<Option<()>> {
    Err(NO_UPDATES.into())
}

/// Async: a Linux package waits for the password prompt and the package
/// manager, which must not hold up the main thread.
#[cfg(feature = "self-update")]
#[tauri::command]
pub(crate) async fn install_update(app: AppHandle) -> CommandResult<()> {
    updates::install_now(&app).await
}

#[cfg(not(feature = "self-update"))]
#[tauri::command]
pub(crate) async fn install_update() -> CommandResult<()> {
    Err(NO_UPDATES.into())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_info_reads_as_the_page_expects() {
        let json = serde_json::to_value(BUILD_INFO).expect("json");
        assert_eq!(json["store"], cfg!(feature = "mas"));
        assert_eq!(json["updates"], cfg!(feature = "self-update"));
        assert!(json["h264"].is_boolean());
        assert!(json["allDrives"].is_boolean());
        assert!(json["rdcmanScan"].is_boolean());
        // The store build never updates itself and never downloads code.
        if json["store"] == true {
            for no in ["updates", "h264", "allDrives", "rdcmanScan"] {
                assert_eq!(json[no], false, "{no} in the store build");
            }
        }
    }
}
