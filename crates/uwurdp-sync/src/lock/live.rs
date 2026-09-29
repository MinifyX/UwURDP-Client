//! UwULock's realtime channel: one WebSocket that says *that* something
//! changed in this app's space, never what — the next sync pass fetches it.
//! A lost message costs a moment and nothing else, so everything here is
//! about staying connected politely: the token renewed on the same
//! connection before it runs out, a dead connection noticed by the missing
//! heartbeat, and the waits the server asks for between reconnects.
//!
//! Blocking and on a thread of its own, like the rest of this crate. The
//! socket is read with a short timeout, so the thread can notice that it
//! should stop, send a new token, or pass on a change it held back for a
//! moment to let a burst of them settle.

use super::api::Lock;
use super::SPACE;
use crate::engine::TransportError;
use rand::Rng;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::CloseFrame;
use tungstenite::{Message, WebSocket};

const PROTOCOL: &str = "uwu.realtime.v1";
/// How long one read waits: how quickly the thread notices anything else.
const POLL: Duration = Duration::from_millis(100);
/// A burst of changes becomes one pass.
const DEBOUNCE: Duration = Duration::from_millis(250);
/// Until the server says otherwise.
const HEARTBEAT: Duration = Duration::from_secs(25);
/// The server wants `auth` within 10 seconds and answers `ready`.
const READY_WITHIN: Duration = Duration::from_secs(15);
/// A new token goes out this long before the old one runs out.
const RENEW_AHEAD: u64 = 60;
/// A connection that lived this long resets the backoff.
const STABLE: Duration = Duration::from_secs(60);

/// What the channel tells the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Connected and signed in. Whatever changed while this device was not
    /// listening is only found by a pass, so one is due now.
    Ready,
    /// Something in this app's space changed: a pass is due.
    Changed,
}

/// Why a connection, or the whole channel, ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// Asked to stop.
    Stopped,
    /// Connect again after this long.
    Retry(Duration),
    /// The session is over: only the master password brings it back.
    SignIn,
    /// The server refuses this app the channel (4403): not again until the
    /// next sign-in.
    Refused,
    /// The server ended the session, and why (`securityStamp`,
    /// `deviceRemoved`, `disabled`, `keysRotated`). Nothing decrypted stays.
    Logout(String),
}

/// What the server's close code asks for (§5.3 of the contract).
fn after_close(code: u16) -> Ended {
    let mut rng = rand::thread_rng();
    let mut between =
        |from: u64, to: u64| Duration::from_millis(rng.gen_range(from * 1000..=to * 1000));
    match code {
        1000 | 1001 => Ended::Retry(between(1, 5)),
        1012 => Ended::Retry(between(2, 10)),
        4400 => Ended::Retry(Duration::from_secs(60)),
        // Once the token is renewed (see `connection`).
        4401 => Ended::Retry(Duration::from_secs(1)),
        4403 => Ended::Refused,
        4408 => Ended::Retry(Duration::from_secs(1)),
        4429 => Ended::Retry(between(60, 75)),
        _ => Ended::Retry(Duration::ZERO),
    }
}

/// Exponential backoff from 1 to 60 seconds with ±30 % jitter, for failures
/// the server named no wait for.
#[derive(Debug, Default)]
pub struct Backoff {
    failures: u32,
}

impl Backoff {
    pub fn delay(&mut self) -> Duration {
        let base = 1u64 << self.failures.min(6);
        self.failures += 1;
        let base = Duration::from_secs(base.min(60)).as_millis() as f64;
        let jitter = rand::thread_rng().gen_range(0.7..=1.3);
        Duration::from_millis((base * jitter) as u64)
    }

    pub fn reset(&mut self) {
        self.failures = 0;
    }
}

/// Keep the channel open until `keep_going` says no, or the session ends.
/// Changes arrive in `on_event`, on this thread.
pub fn run(lock: &Lock, keep_going: &dyn Fn() -> bool, on_event: &mut dyn FnMut(Event)) -> Ended {
    let mut backoff = Backoff::default();
    loop {
        if !keep_going() {
            return Ended::Stopped;
        }
        let started = Instant::now();
        let ended = connection(lock, keep_going, on_event);
        if started.elapsed() >= STABLE {
            backoff.reset();
        }
        let wait = match ended {
            Ended::Retry(wait) if wait.is_zero() => backoff.delay(),
            Ended::Retry(wait) => wait,
            other => return other,
        };
        tracing::debug!(?wait, "realtime channel closed; connecting again");
        let until = Instant::now() + wait;
        while Instant::now() < until {
            if !keep_going() {
                return Ended::Stopped;
            }
            std::thread::sleep(POLL.min(until.saturating_duration_since(Instant::now())));
        }
    }
}

/// One connection, from the handshake to its end.
fn connection(
    lock: &Lock,
    keep_going: &dyn Fn() -> bool,
    on_event: &mut dyn FnMut(Event),
) -> Ended {
    let (token, _) = match lock.access_token() {
        Ok(token) => token,
        Err(TransportError::SignIn(_)) => return Ended::SignIn,
        Err(error) => {
            tracing::debug!(%error, "no token for the realtime channel");
            return Ended::Retry(Duration::ZERO);
        }
    };
    let mut socket = match open(lock) {
        Ok(socket) => socket,
        Err(error) => {
            tracing::debug!(%error, "the realtime channel did not open");
            return Ended::Retry(Duration::ZERO);
        }
    };
    if send(&mut socket, &auth(&token)).is_err() {
        return Ended::Retry(Duration::ZERO);
    }
    drop(token);

    let opened = Instant::now();
    let mut ready = false;
    let mut heartbeat = HEARTBEAT;
    let mut expires = 0u64;
    let mut last_frame = Instant::now();
    let mut due: Option<Instant> = None;
    loop {
        if !keep_going() {
            let _ = socket.close(Some(CloseFrame {
                code: CloseCode::Normal,
                reason: "".into(),
            }));
            let _ = socket.flush();
            return Ended::Stopped;
        }
        match socket.read() {
            Ok(message) => {
                last_frame = Instant::now();
                match message {
                    Message::Text(text) => {
                        let Ok(message) = serde_json::from_str::<Value>(text.as_str()) else {
                            continue;
                        };
                        match message["type"].as_str() {
                            Some("ready") => {
                                if !ready {
                                    on_event(Event::Ready);
                                }
                                ready = true;
                                expires = message["expires"].as_u64().unwrap_or(0);
                                heartbeat = message["heartbeat"]
                                    .as_u64()
                                    .filter(|&s| (1..=3600).contains(&s))
                                    .map(Duration::from_secs)
                                    .unwrap_or(HEARTBEAT);
                            }
                            Some("changed") if concerns_us(&message) => {
                                due.get_or_insert_with(|| Instant::now() + DEBOUNCE);
                            }
                            Some("logout") => {
                                let reason = message["reason"].as_str().unwrap_or("").to_string();
                                return Ended::Logout(reason);
                            }
                            // `info`, `pong`, and whatever a newer server says.
                            _ => {}
                        }
                    }
                    Message::Close(frame) => {
                        let code = frame.map(|f| u16::from(f.code)).unwrap_or(1000);
                        if code == 4401 {
                            // The token is no good: a new one first.
                            match lock.renew() {
                                Ok(()) => {}
                                Err(TransportError::SignIn(_)) => return Ended::SignIn,
                                Err(_) => return Ended::Retry(Duration::ZERO),
                            }
                        }
                        return after_close(code);
                    }
                    _ => {}
                }
            }
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                return Ended::Retry(Duration::ZERO)
            }
            Err(error) => {
                tracing::debug!(%error, "the realtime channel broke");
                return Ended::Retry(Duration::ZERO);
            }
        }
        // Pongs to the server's pings go out with the next write.
        match socket.flush() {
            Ok(()) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return Ended::Retry(Duration::ZERO),
        }

        let now = Instant::now();
        if due.is_some_and(|due| now >= due) {
            due = None;
            on_event(Event::Changed);
        }
        if !ready && now.duration_since(opened) > READY_WITHIN {
            return Ended::Retry(Duration::ZERO);
        }
        if now.duration_since(last_frame) > heartbeat * 3 {
            tracing::debug!("the realtime channel went quiet");
            return Ended::Retry(Duration::ZERO);
        }
        if ready && expires > 0 && unix_now() + RENEW_AHEAD >= expires {
            match lock.access_token() {
                Ok((token, next)) if next > expires => {
                    if send(&mut socket, &auth(&token)).is_err() {
                        return Ended::Retry(Duration::ZERO);
                    }
                    // Until `ready` confirms it, don't ask again.
                    expires = next;
                }
                // The token here was not renewed yet; the server closes
                // with 4401 when it runs out, and the next connection
                // refreshes.
                Ok(_) => expires = 0,
                Err(TransportError::SignIn(_)) => return Ended::SignIn,
                Err(_) => expires = 0,
            }
        }
    }
}

/// Whether a `changed` message is about this app's space.
fn concerns_us(message: &Value) -> bool {
    let named = |key: &str, what: &str| {
        message[key]
            .as_array()
            .is_some_and(|list| list.iter().any(|v| v.as_str() == Some(what)))
    };
    named("spaces", SPACE) || (message["spaces"].is_null() && named("areas", "suite"))
}

fn auth(token: &str) -> String {
    serde_json::json!({ "type": "auth", "token": token, "cursor": null }).to_string()
}

fn send(socket: &mut WebSocket<Stream>, text: &str) -> Result<(), tungstenite::Error> {
    socket.send(Message::text(text))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The socket under the WebSocket: TLS with this crate's own rustls, or
/// plain TCP to this machine, as the server's address says.
enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Stream {
    fn set_read_timeout(&mut self, timeout: Duration) -> std::io::Result<()> {
        match self {
            Stream::Plain(s) => s.set_read_timeout(Some(timeout)),
            Stream::Tls(s) => s.sock.set_read_timeout(Some(timeout)),
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

/// Connect and shake hands, with the subprotocol the server must agree to.
fn open(lock: &Lock) -> Result<WebSocket<Stream>, String> {
    let url = reqwest::Url::parse(&lock.url("/uwu/v1/realtime")).map_err(|e| e.to_string())?;
    let tls = match url.scheme() {
        "https" => true,
        "http" => false,
        other => return Err(format!("{other}:// has no realtime channel")),
    };
    let host = url.host_str().ok_or("no host")?.to_string();
    let port = url.port_or_known_default().ok_or("no port")?;
    let mut ws_url = url.clone();
    ws_url
        .set_scheme(if tls { "wss" } else { "ws" })
        .map_err(|()| "no WebSocket address")?;

    let host_for_dns = host.trim_start_matches('[').trim_end_matches(']');
    let addresses = (host_for_dns, port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?;
    let mut tcp = None;
    let mut last = String::from("no address");
    for address in addresses {
        match TcpStream::connect_timeout(&address, Duration::from_secs(10)) {
            Ok(stream) => {
                tcp = Some(stream);
                break;
            }
            Err(error) => last = error.to_string(),
        }
    }
    let tcp = tcp.ok_or(last)?;
    tcp.set_nodelay(true).map_err(|e| e.to_string())?;
    tcp.set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    // The handshake reads without a short timeout; the loop after it with one.
    tcp.set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;

    let stream = if tls {
        let name = rustls::pki_types::ServerName::try_from(host_for_dns.to_string())
            .map_err(|e| e.to_string())?;
        let connection = rustls::ClientConnection::new(Arc::new(crate::pin::roots_config()), name)
            .map_err(|e| e.to_string())?;
        Stream::Tls(Box::new(rustls::StreamOwned::new(connection, tcp)))
    } else {
        Stream::Plain(tcp)
    };

    let mut request = ws_url
        .as_str()
        .into_client_request()
        .map_err(|e| e.to_string())?;
    let headers = request.headers_mut();
    headers.insert(
        "Sec-WebSocket-Protocol",
        PROTOCOL.parse().map_err(|_| "subprotocol")?,
    );
    headers.insert(
        "User-Agent",
        concat!("UwURDP/", env!("CARGO_PKG_VERSION"))
            .parse()
            .map_err(|_| "user agent")?,
    );
    let (mut socket, _) = tungstenite::client(request, stream).map_err(|e| e.to_string())?;
    // On the socket itself: a timeout set through a clone of it does not
    // reach every system's socket.
    socket
        .get_mut()
        .set_read_timeout(POLL)
        .map_err(|e| e.to_string())?;
    Ok(socket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_waits_are_the_ones_the_server_asks_for() {
        for _ in 0..20 {
            let Ended::Retry(wait) = after_close(1000) else {
                panic!("1000 reconnects")
            };
            assert!((1..=5).contains(&wait.as_secs()));
            let Ended::Retry(wait) = after_close(4429) else {
                panic!("4429 reconnects")
            };
            assert!(wait >= Duration::from_secs(60));
        }
        assert_eq!(after_close(4403), Ended::Refused);
        assert_eq!(after_close(4400), Ended::Retry(Duration::from_secs(60)));
        assert_eq!(after_close(4000), Ended::Retry(Duration::ZERO));
    }

    #[test]
    fn backoff_grows_to_a_minute_with_jitter_and_starts_again() {
        let mut backoff = Backoff::default();
        let first = backoff.delay();
        assert!(first >= Duration::from_millis(700) && first <= Duration::from_millis(1300));
        for _ in 0..10 {
            backoff.delay();
        }
        let late = backoff.delay();
        assert!(late >= Duration::from_secs(42) && late <= Duration::from_secs(78));
        backoff.reset();
        assert!(backoff.delay() <= Duration::from_millis(1300));
    }

    #[test]
    fn only_changes_in_this_apps_space_count() {
        let changed = |v: Value| concerns_us(&v);
        assert!(changed(
            serde_json::json!({ "type": "changed", "areas": ["suite"], "spaces": [SPACE] })
        ));
        assert!(!changed(
            serde_json::json!({ "type": "changed", "areas": ["suite"], "spaces": ["mail"] })
        ));
        assert!(!changed(
            serde_json::json!({ "type": "changed", "areas": ["vault"] })
        ));
        assert!(changed(
            serde_json::json!({ "type": "changed", "areas": ["suite"] })
        ));
    }
}
