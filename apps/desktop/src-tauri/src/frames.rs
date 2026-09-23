//! Desktop frames over a loopback WebSocket.
//!
//! Tauri's IPC is fine for commands, but not for a desktop's pixels: every
//! channel message over 1 KiB is an `eval` on the main thread followed by a
//! fetch through the webview's custom protocol, and every input event or ack
//! a synchronous command on that same main thread. A full-HD frame is 8 MB.
//! A WebSocket on 127.0.0.1 moves the same bytes without touching the main
//! thread, in both directions, in order.
//!
//! One socket per connect attempt. The page opens it before `connect_host`,
//! with the server's random token and the attempt's name; the socket is
//! registered, the server says `ready`, and only then does the page connect.
//! `connect_host` takes the socket and hands the engine a [`SocketSink`].
//!
//! Page → app: a one-byte binary message `[1]` acknowledges one `BITMAPS`
//! message; a text message is a JSON array of input events.
//! App → page: the frame messages of [`uwurdp_core::frame`], and a close
//! frame when the session is over.

use futures_util::{SinkExt as _, StreamExt as _};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use uwurdp_core::{FrameSink, InputEvent, SessionId, SessionManager, SinkError};

/// The acknowledgement byte.
const ACK: u8 = 1;
/// A page sends a handful of input events per frame; more is not a keyboard.
const MAX_INPUT_EVENTS: usize = 256;
/// Input arrives as small JSON; nothing the page sends is anywhere near this.
const MAX_INCOMING: usize = 64 * 1024;

/// Where the page finds the socket.
#[derive(Clone, serde::Serialize)]
pub(crate) struct Endpoint {
    pub port: u16,
    pub token: String,
}

/// What `connect_host` takes over from an opened socket.
pub(crate) struct Opened {
    pub sink: SocketSink,
    pub link: Arc<Link>,
}

/// The session a socket speaks for, once it has one.
#[derive(Default)]
pub(crate) struct Link {
    state: Mutex<LinkState>,
}

#[derive(Default)]
struct LinkState {
    session: Option<SessionId>,
    /// Frames the page drew before the session had its id.
    early_acks: u32,
}

impl Link {
    /// The session is up: acks that came early are passed on now.
    pub fn bind(&self, sessions: &SessionManager, session: SessionId) {
        let early = {
            let mut state = self.state.lock();
            state.session = Some(session);
            std::mem::take(&mut state.early_acks)
        };
        for _ in 0..early {
            let _ = sessions.ack(session);
        }
    }

    fn ack(&self, sessions: &SessionManager) {
        let session = {
            let mut state = self.state.lock();
            match state.session {
                Some(session) => session,
                None => {
                    state.early_acks = state.early_acks.saturating_add(1);
                    return;
                }
            }
        };
        let _ = sessions.ack(session);
    }

    fn session(&self) -> Option<SessionId> {
        self.state.lock().session
    }
}

/// The engine's end of a socket.
pub(crate) struct SocketSink {
    tx: mpsc::UnboundedSender<Message>,
}

impl FrameSink for SocketSink {
    fn send(&self, frame: &[u8]) -> Result<(), SinkError> {
        self.tx
            .send(Message::Binary(frame.to_vec().into()))
            .map_err(|_| SinkError::Closed)
    }

    fn send_owned(&self, frame: Vec<u8>) -> Result<(), SinkError> {
        self.tx
            .send(Message::Binary(frame.into()))
            .map_err(|_| SinkError::Closed)
    }

    fn finish(&self) {
        let _ = self.tx.send(Message::Close(None));
    }
}

pub(crate) struct FrameServer {
    endpoint: Endpoint,
    opened: Mutex<HashMap<String, Opened>>,
}

impl FrameServer {
    /// Binds 127.0.0.1 on a free port and serves it on Tauri's runtime.
    pub fn start(sessions: Arc<SessionManager>) -> std::io::Result<Arc<Self>> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let server = Arc::new(Self {
            endpoint: Endpoint {
                port,
                token: format!(
                    "{}{}",
                    uuid::Uuid::new_v4().simple(),
                    uuid::Uuid::new_v4().simple()
                ),
            },
            opened: Mutex::new(HashMap::new()),
        });
        let serving = server.clone();
        tauri::async_runtime::spawn(async move {
            let listener = match TcpListener::from_std(listener) {
                Ok(listener) => listener,
                Err(error) => {
                    tracing::error!(%error, "frame socket unavailable");
                    return;
                }
            };
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let server = serving.clone();
                let sessions = sessions.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = server.serve(stream, sessions).await {
                        tracing::debug!(%error, "frame socket ended");
                    }
                });
            }
        });
        tracing::info!(port, "frame socket listening");
        Ok(server)
    }

    pub fn endpoint(&self) -> Endpoint {
        self.endpoint.clone()
    }

    /// The socket the page opened for `attempt`, if it did.
    pub fn take(&self, attempt: &str) -> Option<Opened> {
        self.opened.lock().remove(attempt)
    }

    // The handshake callback's error type is tungstenite's, not ours to shrink.
    #[allow(clippy::result_large_err)]
    async fn serve(
        &self,
        stream: TcpStream,
        sessions: Arc<SessionManager>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let _ = stream.set_nodelay(true);
        let mut attempt = None;
        let token = &self.endpoint.token;
        let check = |request: &Request, response: Response| {
            let query = request.uri().query().unwrap_or("");
            let mut given_token = None;
            let mut given_attempt = None;
            for pair in query.split('&') {
                match pair.split_once('=') {
                    Some(("t", value)) => given_token = Some(value),
                    Some(("a", value)) => given_attempt = Some(value),
                    _ => {}
                }
            }
            let valid_attempt = given_attempt.filter(|a| {
                !a.is_empty()
                    && a.len() <= 128
                    && a.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            });
            match (given_token, valid_attempt) {
                (Some(given), Some(a)) if same(given.as_bytes(), token.as_bytes()) => {
                    attempt = Some(a.to_owned());
                    Ok(response)
                }
                _ => {
                    let mut refused = ErrorResponse::new(None);
                    *refused.status_mut() = StatusCode::FORBIDDEN;
                    Err(refused)
                }
            }
        };
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_INCOMING))
            .max_frame_size(Some(MAX_INCOMING));
        let socket =
            tokio_tungstenite::accept_hdr_async_with_config(stream, check, Some(config)).await?;
        let attempt = attempt.ok_or("no attempt")?;

        let (mut write, mut read) = socket.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let link = Arc::new(Link::default());
        // A newer socket for the same attempt replaces an old one, whose
        // writer then ends with its sender.
        self.opened.lock().insert(
            attempt,
            Opened {
                sink: SocketSink { tx: tx.clone() },
                link: link.clone(),
            },
        );
        let _ = tx.send(Message::text("ready"));
        drop(tx);

        let writer = async move {
            while let Some(message) = rx.recv().await {
                let closing = matches!(message, Message::Close(_));
                if write.send(message).await.is_err() || closing {
                    break;
                }
            }
            let _ = write.close().await;
        };
        let reader = async move {
            while let Some(Ok(message)) = read.next().await {
                match message {
                    Message::Binary(bytes) if bytes.as_ref() == [ACK] => link.ack(&sessions),
                    Message::Text(text) => {
                        let Some(session) = link.session() else {
                            continue;
                        };
                        match serde_json::from_str::<Vec<InputEvent>>(text.as_str()) {
                            Ok(events) if events.len() <= MAX_INPUT_EVENTS => {
                                let _ = sessions.input(session, events);
                            }
                            Ok(_) => tracing::warn!("too many input events at once"),
                            Err(error) => tracing::warn!(%error, "unreadable input"),
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        };
        // The writer ends when the engine is done with the socket (its sink
        // and the waiting entry are dropped); the reader when the page is.
        tokio::select! {
            () = writer => {}
            () = reader => {}
        }
        Ok(())
    }
}

/// Compares without an early exit, so timing says nothing about the token.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_compare_whole() {
        assert!(same(b"abc", b"abc"));
        assert!(!same(b"abc", b"abd"));
        assert!(!same(b"abc", b"ab"));
        assert!(!same(b"", b"a"));
    }

    #[test]
    fn early_acks_wait_for_the_session() {
        let link = Link::default();
        let sessions = SessionManager::new();
        link.ack(&sessions);
        link.ack(&sessions);
        assert_eq!(link.state.lock().early_acks, 2);
        let id = SessionId::default();
        link.bind(&sessions, id);
        assert_eq!(link.state.lock().early_acks, 0);
        assert_eq!(link.session(), Some(id));
    }
}
