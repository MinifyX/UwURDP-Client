//! What the page does with an open desktop: input, size, acknowledgements,
//! the clipboard, closing, and full screen for the window.

use crate::{err, AppState, CommandResult};
use tauri::{Manager, State};
use uwurdp_core::{InputEvent, SessionId};

/// Deliberately not `async`. Async commands run concurrently on the runtime,
/// so a key-up could overtake its key-down. Synchronous commands run on the
/// main thread in the order they were invoked, and all this does is put the
/// events on the session's queue, so it never blocks.
#[tauri::command]
pub(crate) fn rdp_input(
    state: State<'_, AppState>,
    id: SessionId,
    events: Vec<InputEvent>,
) -> CommandResult<()> {
    // A page sends a handful per frame; anything like this is not a keyboard.
    if events.len() > 256 {
        return Err("too many input events at once".into());
    }
    state.sessions.input(id, events).map_err(err)
}

/// Synchronous for the same reason as [`rdp_input`].
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

/// The page has drawn one frame message. This is what lets the engine pause
/// before the webview drowns.
#[tauri::command]
pub(crate) fn ack_frame(state: State<'_, AppState>, id: SessionId) -> CommandResult<()> {
    state.sessions.ack(id).map_err(err)
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
