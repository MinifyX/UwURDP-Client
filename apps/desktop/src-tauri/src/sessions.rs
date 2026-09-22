//! What the page does with an open desktop besides frames, acks and input
//! (those travel over [`crate::frames`]): size, the clipboard, closing, and
//! full screen for the window.

use crate::{err, frames::Endpoint, AppState, CommandResult};
use tauri::{Manager, State};
use uwurdp_core::SessionId;

/// Where the page opens a desktop's socket.
#[tauri::command]
pub(crate) fn frame_socket(state: State<'_, AppState>) -> Endpoint {
    state.frames.endpoint()
}

/// Deliberately not `async`: resizes reach the session in the order asked.
#[tauri::command]
pub(crate) fn resize_session(
    state: State<'_, AppState>,
    id: SessionId,
    width: u16,
    height: u16,
    scale: u32,
) -> CommandResult<()> {
    state
        .sessions
        .resize(
            id,
            width.clamp(200, 8192),
            height.clamp(200, 8192),
            scale.clamp(100, 500),
        )
        .map_err(err)
}

#[tauri::command]
pub(crate) fn clipboard_changed(state: State<'_, AppState>, id: SessionId) -> CommandResult<()> {
    state.sessions.clipboard_changed(id).map_err(err)
}

#[tauri::command]
pub(crate) async fn close_session(state: State<'_, AppState>, id: SessionId) -> CommandResult<()> {
    crate::hosts::forget_session(&state, id);
    // A session that already ended on its own is fine to close again.
    if state.sessions.is_open(id) {
        state.sessions.close(id).map_err(err)?;
    }
    Ok(())
}

/// Full screen for the whole window; the page hides its own chrome.
#[tauri::command]
pub(crate) fn set_fullscreen(app: tauri::AppHandle, on: bool) -> CommandResult<()> {
    let window = app
        .get_webview_window("main")
        .ok_or("the main window is gone")?;
    window.set_fullscreen(on).map_err(err)
}
