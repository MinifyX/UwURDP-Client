//! Where finished frame messages go.
//!
//! The engine knows nothing about Tauri: the desktop app implements
//! [`FrameSink`] over an IPC channel, the tests implement it over a `Vec`.

/// Why a message could not be delivered.
#[derive(Debug, thiserror::Error)]
pub enum SinkError {
    /// The receiving side is gone for good (window closed, page reloaded).
    /// The session treats this like the user closing it.
    #[error("closed")]
    Closed,
    #[error("{0}")]
    Other(String),
}

/// Receives the binary messages described in [`crate::frame`], one message
/// per call.
///
/// `send` is called from the session task, so it must not block for long:
/// hand the bytes to a channel and return.
pub trait FrameSink: Send + Sync + 'static {
    fn send(&self, frame: &[u8]) -> Result<(), SinkError>;

    /// Like [`send`](Self::send), for a message the caller is done with. A
    /// sink that keeps the bytes takes them as they are: a full-HD `BITMAPS`
    /// message is 8 MB, and copying it once more per frame adds up.
    fn send_owned(&self, frame: Vec<u8>) -> Result<(), SinkError> {
        self.send(&frame)
    }

    /// End of stream: every message there will ever be has been sent (the
    /// app forwards this to the page as an empty message).
    fn finish(&self);
}
