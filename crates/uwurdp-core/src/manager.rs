//! The app-facing handle on all sessions.
//!
//! Everything except `connect` is synchronous and only pushes onto an
//! unbounded channel: Tauri runs sync commands on the main thread, where
//! there is no tokio reactor to await anything on.

use crate::clipboard::{self, TextClipboardBackend};
use crate::config::RdpTarget;
use crate::connect::{self, Channels};
use crate::error::{RdpError, SessionError};
use crate::frame::{self, CloseReason};
use crate::gfx;
use crate::input::InputEvent;
use crate::session::{self, Command, SessionParts};
use crate::sink::FrameSink;
use crate::SessionId;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, warn};

/// What the page hears when a session task panicked.
const CRASHED: &str =
    "UwURDP ran into an internal error in this session. The details are in the log.";

/// The graphics pipeline for a session, with H.264 when the settings name
/// an OpenH264 library that loads.
#[cfg(feature = "h264")]
fn graphics_channel(target: &RdpTarget) -> (gfx::GfxChannel, gfx::Shared) {
    let decoder = target.settings.h264_library.as_deref().and_then(|path| {
        match gfx::h264::H264Decoder::load(path) {
            Ok(decoder) => Some(decoder),
            Err(error) => {
                warn!(%error, path = %path.display(), "OpenH264 could not be loaded; no H.264 for this session");
                None
            }
        }
    });
    gfx::channel(decoder)
}

#[cfg(not(feature = "h264"))]
fn graphics_channel(_target: &RdpTarget) -> (gfx::GfxChannel, gfx::Shared) {
    gfx::channel()
}

struct Entry {
    commands: mpsc::UnboundedSender<Command>,
    open: Arc<AtomicBool>,
}

#[derive(Default)]
struct Inner {
    sessions: Mutex<HashMap<SessionId, Entry>>,
    /// Connection attempts in flight, by the name the app gave them.
    attempts: Mutex<HashMap<String, watch::Sender<bool>>>,
}

/// All RDP sessions of the app.
#[derive(Default)]
pub struct SessionManager {
    inner: Arc<Inner>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Connects and returns once the session is active (or failed).
    ///
    /// `attempt` names the attempt so [`cancel`](Self::cancel) can abort it.
    /// The server certificate is checked right after the TLS upgrade and
    /// before CredSSP: unknown → [`RdpError::UnknownCertificate`], different
    /// from the trusted one → [`RdpError::CertificateChanged`]; either way the
    /// connection is dropped before any credential is sent.
    ///
    /// On success the session loop runs on the current tokio runtime and
    /// frame messages start flowing to `sink`: first `DESKTOP_SIZE`, then a
    /// full-screen `BITMAPS`.
    pub async fn connect(
        &self,
        attempt: &str,
        target: RdpTarget,
        sink: impl FrameSink,
    ) -> Result<SessionId, RdpError> {
        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        self.inner
            .attempts
            .lock()
            .insert(attempt.to_owned(), cancel_tx);
        // Removes the attempt however this future ends, dropped included.
        let _registration = AttemptGuard {
            inner: &self.inner,
            attempt,
        };

        let (commands_tx, commands_rx) = mpsc::unbounded_channel();
        let (mut channels, clipboard) = if target.settings.clipboard {
            let to_session = commands_tx.clone();
            let worker = clipboard::spawn_worker(Box::new(move |m| {
                let _ = to_session.send(Command::Clipboard(m));
            }));
            let to_session = commands_tx.clone();
            let backend = TextClipboardBackend::new(
                worker.clone(),
                Box::new(move |m| {
                    let _ = to_session.send(Command::Clipboard(m));
                }),
            );
            (
                Channels {
                    clipboard: Some(backend),
                    graphics: None,
                },
                Some(worker),
            )
        } else {
            (Channels::default(), None)
        };

        let graphics = target.settings.graphics_pipeline.then(|| {
            let (channel, shared) = graphics_channel(&target);
            channels.graphics = Some(channel);
            shared
        });

        let established = tokio::select! {
            result = connect::establish(&target, channels) => result?,
            _ = cancel_rx.wait_for(|cancelled| *cancelled) => return Err(RdpError::Cancelled),
        };
        drop(target);

        let id = SessionId::new_v4();
        let open = Arc::new(AtomicBool::new(true));
        self.inner.sessions.lock().insert(
            id,
            Entry {
                commands: commands_tx,
                open: open.clone(),
            },
        );

        let parts = SessionParts {
            established,
            commands: commands_rx,
            clipboard,
            graphics,
        };
        let inner: Weak<Inner> = Arc::downgrade(&self.inner);
        let sink = Arc::new(sink);
        tokio::spawn(async move {
            // Its own task, so a panic in there (a decoder tripping over
            // what a server sent, say) ends this session and no other.
            let running = sink.clone();
            let ended = tokio::spawn(async move { session::run(parts, &*running).await }).await;
            if let Err(error) = ended {
                error!(%id, %error, "the session task failed");
                sink.send(&frame::closed(CloseReason::Error, CRASHED)).ok();
                sink.finish();
            }
            open.store(false, Ordering::Relaxed);
            if let Some(inner) = inner.upgrade() {
                inner.sessions.lock().remove(&id);
            }
            debug!(%id, "session removed");
        });
        Ok(id)
    }

    /// Aborts the connection attempt of that name, if one is in flight;
    /// its `connect` returns [`RdpError::Cancelled`].
    pub fn cancel(&self, attempt: &str) {
        if let Some(tx) = self.inner.attempts.lock().get(attempt) {
            let _ = tx.send(true);
        }
    }

    fn command(&self, id: SessionId, command: Command) -> Result<(), SessionError> {
        let sessions = self.inner.sessions.lock();
        let entry = sessions
            .get(&id)
            .ok_or(SessionError::UnknownSession { id })?;
        if !entry.open.load(Ordering::Relaxed) {
            return Err(SessionError::Closed);
        }
        entry
            .commands
            .send(command)
            .map_err(|_| SessionError::Closed)
    }

    pub fn input(&self, id: SessionId, events: Vec<InputEvent>) -> Result<(), SessionError> {
        if events.is_empty() {
            return Ok(());
        }
        self.command(id, Command::Input(events))
    }

    /// Asks the server for a new desktop size. Ignored (the page scales)
    /// when the server does not offer dynamic resize.
    pub fn resize(
        &self,
        id: SessionId,
        width: u16,
        height: u16,
        scale_factor: u32,
    ) -> Result<(), SessionError> {
        self.command(
            id,
            Command::Resize {
                width,
                height,
                scale_factor,
            },
        )
    }

    /// The page has drawn one `BITMAPS` message.
    pub fn ack(&self, id: SessionId) -> Result<(), SessionError> {
        self.command(id, Command::Ack)
    }

    /// The page regained focus: re-announce the local clipboard.
    pub fn clipboard_changed(&self, id: SessionId) -> Result<(), SessionError> {
        self.command(id, Command::ClipboardChanged)
    }

    /// Graceful shutdown; the sink gets `CLOSED` and `finish()` at the end.
    pub fn close(&self, id: SessionId) -> Result<(), SessionError> {
        self.command(id, Command::Close)
    }

    /// Closes every open session (gracefully, like [`close`](Self::close))
    /// and cancels every connection attempt in flight. Returns how many
    /// sessions were asked to close. Meant for app shutdown.
    pub fn close_all(&self) -> usize {
        for cancel in self.inner.attempts.lock().values() {
            let _ = cancel.send(true);
        }
        self.inner
            .sessions
            .lock()
            .values()
            .filter(|entry| entry.open.load(Ordering::Relaxed))
            .filter(|entry| entry.commands.send(Command::Close).is_ok())
            .count()
    }

    pub fn is_open(&self, id: SessionId) -> bool {
        self.inner
            .sessions
            .lock()
            .get(&id)
            .is_some_and(|e| e.open.load(Ordering::Relaxed))
    }
}

struct AttemptGuard<'a> {
    inner: &'a Inner,
    attempt: &'a str,
}

impl Drop for AttemptGuard<'_> {
    fn drop(&mut self) {
        self.inner.attempts.lock().remove(self.attempt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_sessions_are_reported_without_a_runtime() {
        // Deliberately no tokio runtime here: these calls must work from
        // Tauri's main thread.
        let manager = SessionManager::new();
        let id = SessionId::new_v4();
        assert_eq!(
            manager.input(id, vec![InputEvent::ReleaseAll]),
            Err(SessionError::UnknownSession { id })
        );
        assert_eq!(
            manager.resize(id, 800, 600, 100),
            Err(SessionError::UnknownSession { id })
        );
        assert_eq!(manager.ack(id), Err(SessionError::UnknownSession { id }));
        assert_eq!(
            manager.clipboard_changed(id),
            Err(SessionError::UnknownSession { id })
        );
        assert_eq!(manager.close(id), Err(SessionError::UnknownSession { id }));
        assert!(!manager.is_open(id));
        manager.cancel("nothing");
    }

    #[test]
    fn commands_reach_a_registered_session_without_a_runtime() {
        let manager = SessionManager::new();
        let id = SessionId::new_v4();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let open = Arc::new(AtomicBool::new(true));
        manager.inner.sessions.lock().insert(
            id,
            Entry {
                commands: tx,
                open: open.clone(),
            },
        );
        assert!(manager.is_open(id));
        manager.ack(id).expect("ack");
        manager
            .input(id, vec![InputEvent::Move { x: 1, y: 2 }])
            .expect("input");
        assert!(matches!(rx.try_recv(), Ok(Command::Ack)));
        assert!(matches!(rx.try_recv(), Ok(Command::Input(_))));

        open.store(false, Ordering::Relaxed);
        assert!(!manager.is_open(id));
        assert_eq!(manager.ack(id), Err(SessionError::Closed));
    }

    #[test]
    fn close_all_closes_open_sessions_and_cancels_attempts() {
        let manager = SessionManager::new();
        let mut receivers = Vec::new();
        for open in [true, true, false] {
            let (tx, rx) = mpsc::unbounded_channel();
            manager.inner.sessions.lock().insert(
                SessionId::new_v4(),
                Entry {
                    commands: tx,
                    open: Arc::new(AtomicBool::new(open)),
                },
            );
            receivers.push(rx);
        }
        let (cancel_tx, cancel_rx) = watch::channel(false);
        manager
            .inner
            .attempts
            .lock()
            .insert("pending".into(), cancel_tx);

        assert_eq!(manager.close_all(), 2);
        assert!(*cancel_rx.borrow());
        let closes = receivers
            .iter_mut()
            .map(|rx| rx.try_recv())
            .filter(|received| matches!(received, Ok(Command::Close)))
            .count();
        assert_eq!(closes, 2);
    }
}
