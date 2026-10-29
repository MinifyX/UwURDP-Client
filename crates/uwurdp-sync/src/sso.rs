//! Signing in with UwUAuth, the way a desktop app does it (RFC 8252).
//!
//! The sync server is the OpenID Connect client; this device only carries the
//! browser there and back. It listens on a loopback address for exactly one
//! answer, checks that the answer carries the `state` it was told to expect,
//! and hands the code to the server together with the PKCE verifier only this
//! device knows. The server turns that into a short-lived ticket.
//!
//! What the ticket is good for is deliberately small: it stands in for an
//! invite or an enrolment token. The vault still opens with the master
//! password (and the account key) and nothing else — UwUAuth never sees
//! either, and signing in there never opens anything here.

use crate::engine::TransportError;
use crate::http::{self, Server};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};
use uwurdp_proto::api::{SsoFinish, SsoStart, SsoTicket};

/// The path the browser comes back to. UwUAuth takes any port on a loopback
/// address but holds the path and host to what was registered.
pub const CALLBACK_PATH: &str = "/uwusync/callback";
/// How long a person has to sign in before the device stops listening.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// The most a browser's request line and headers may weigh.
const MAX_REQUEST: usize = 16 * 1024;
/// Answers on the callback path with a state that is not this sign-in's
/// before the listener gives up: a page somewhere guessing at the port does
/// not get to keep the device waiting. Anything else (a favicon, a stray tab)
/// is answered and ignored.
const MAX_WRONG_STATE: usize = 8;
/// How long one connection to the listener may take to send its request: a
/// connection that dawdles is dropped, so it cannot hold the real answer up.
const REQUEST_DEADLINE: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum SsoError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("could not listen for the browser: {0}")]
    Listen(std::io::Error),
    #[error("signing in took too long")]
    TimedOut,
    #[error("signing in was cancelled")]
    Cancelled,
    #[error("UwUAuth refused: {0}")]
    Refused(String),
}

/// A sign-in under way: the address to open in the browser, and what this
/// device needs to check the answer.
pub struct SignIn {
    /// Open this in the system browser.
    pub url: String,
    state: String,
    verifier: String,
    listener: TcpListener,
}

impl std::fmt::Debug for SignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignIn")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

/// Listen on a fresh loopback port and ask the server where to send the
/// browser.
pub fn begin(server: &Server) -> Result<SignIn, SsoError> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(SsoError::Listen)?;
    let port = listener.local_addr().map_err(SsoError::Listen)?.port();
    let verifier = random_text();
    let started = server.sso_start(&SsoStart {
        redirect_uri: format!("http://127.0.0.1:{port}{CALLBACK_PATH}"),
        code_challenge: challenge(&verifier),
    })?;
    if !acceptable_sign_in_url(&started.url) {
        return Err(TransportError::Refused("a sign-in address that is not https".into()).into());
    }
    Ok(SignIn {
        url: started.url,
        state: started.state,
        verifier,
        listener,
    })
}

/// Where the browser may be sent: https anywhere, or plain http only to this
/// machine's own `127.0.0.1` (a UwUAuth on the same machine). The host is
/// compared as parsed, so `http://127.0.0.1.example.com` or
/// `http://127.0.0.1@example.com` are not mistaken for it.
fn acceptable_sign_in_url(text: &str) -> bool {
    let Ok(url) = url::Url::parse(text) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    match url.scheme() {
        "https" => url.host().is_some(),
        "http" => url.host() == Some(url::Host::Ipv4(std::net::Ipv4Addr::LOCALHOST)),
        _ => false,
    }
}

/// Wait for the browser to come back, then trade the answer for a ticket.
///
/// `cancelled` is asked a few times a second, so the interface can stop the
/// wait.
pub fn finish(
    server: &Server,
    sign_in: SignIn,
    deadline: Instant,
    cancelled: impl Fn() -> bool,
) -> Result<SsoTicket, SsoError> {
    let answer = wait_for_answer(&sign_in, deadline, cancelled)?;
    Ok(server.sso_token(&SsoFinish {
        state: sign_in.state,
        code: answer.code,
        code_verifier: sign_in.verifier,
        iss: answer.iss,
    })?)
}

struct Answer {
    code: String,
    iss: Option<String>,
}

fn wait_for_answer(
    sign_in: &SignIn,
    deadline: Instant,
    cancelled: impl Fn() -> bool,
) -> Result<Answer, SsoError> {
    sign_in
        .listener
        .set_nonblocking(true)
        .map_err(SsoError::Listen)?;
    let mut wrong_state = 0;
    loop {
        if cancelled() {
            return Err(SsoError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(SsoError::TimedOut);
        }
        let stream = match sign_in.listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                // Nothing to wait on but the person: a short nap between looks.
                std::thread::sleep(Duration::from_millis(25));
                continue;
            }
            Err(error) => return Err(SsoError::Listen(error)),
        };
        match answer_from(stream, &sign_in.state) {
            Heard::Answer(answer) => return Ok(answer),
            Heard::Refused(error) => return Err(error),
            Heard::WrongState => {
                wrong_state += 1;
                if wrong_state >= MAX_WRONG_STATE {
                    return Err(SsoError::Refused(
                        "too many answers to another sign-in".into(),
                    ));
                }
            }
            Heard::Other => {}
        }
    }
}

/// What one connection to the listener brought.
enum Heard {
    /// The answer to this sign-in.
    Answer(Answer),
    /// UwUAuth's refusal of this sign-in.
    Refused(SsoError),
    /// The callback, but for a state that is not this one.
    WrongState,
    /// Anything else: a favicon, a stray tab, a connection that said nothing
    /// in time.
    Other,
}

/// Read one request, within [`REQUEST_DEADLINE`]. Anything that is not the
/// answer this sign-in waits for is told so and ignored.
fn answer_from(mut stream: TcpStream, state: &str) -> Heard {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_write_timeout(Some(REQUEST_DEADLINE));
    let Some(target) = read_target(&mut stream, Instant::now() + REQUEST_DEADLINE) else {
        return Heard::Other;
    };
    let path = target.split('?').next().unwrap_or_default();
    if path != CALLBACK_PATH {
        reply(&mut stream, "404 Not Found", NOT_HERE);
        return Heard::Other;
    }
    let query = target.split_once('?').map(|(_, query)| query).unwrap_or("");
    let mut code = None;
    let mut got_state = None;
    let mut iss = None;
    let mut error = None;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match &*key {
            "code" => code = Some(value.into_owned()),
            "state" => got_state = Some(value.into_owned()),
            "iss" => iss = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }
    // Only the answer to this sign-in counts: another page that knows the
    // port must not be able to slip its own code in.
    let matches = got_state
        .as_deref()
        .is_some_and(|got| constant_time_eq(got.as_bytes(), state.as_bytes()));
    if !matches {
        reply(&mut stream, "400 Bad Request", NOT_HERE);
        return Heard::WrongState;
    }
    if let Some(error) = error {
        reply(&mut stream, "200 OK", REFUSED);
        let error: String = error
            .chars()
            .filter(|c| c.is_ascii_graphic())
            .take(64)
            .collect();
        return Heard::Refused(SsoError::Refused(error));
    }
    let Some(code) = code.filter(|code| !code.is_empty() && code.len() <= 2048) else {
        reply(&mut stream, "400 Bad Request", NOT_HERE);
        return Heard::Other;
    };
    reply(&mut stream, "200 OK", DONE);
    Heard::Answer(Answer { code, iss })
}

/// The request target of a `GET`, read up to the end of the headers — all of
/// it before `deadline`, however slowly the bytes trickle in.
fn read_target(stream: &mut TcpStream, deadline: Instant) -> Option<String> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
        if buffer.len() >= MAX_REQUEST {
            return None;
        }
        let left = deadline.checked_duration_since(Instant::now())?;
        if left.is_zero() {
            return None;
        }
        stream.set_read_timeout(Some(left)).ok()?;
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let head = std::str::from_utf8(&buffer).ok()?;
    let line = head.lines().next()?;
    let mut parts = line.split(' ');
    let (method, target) = (parts.next()?, parts.next()?);
    (method == "GET").then(|| target.to_string())
}

fn reply(stream: &mut TcpStream, status: &str, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

const DONE: &str = "<!doctype html><meta charset=utf-8><title>UwURDP</title>\
<p style=\"font:16px system-ui;margin:3em\">Angemeldet. Du kannst dieses Fenster schließen und zu UwURDP zurückkehren.<br>\
Signed in. You can close this window and go back to UwURDP.</p>";
const REFUSED: &str = "<!doctype html><meta charset=utf-8><title>UwURDP</title>\
<p style=\"font:16px system-ui;margin:3em\">Die Anmeldung wurde abgelehnt. Mehr dazu in UwURDP.<br>\
Signing in was refused. UwURDP says more.</p>";
const NOT_HERE: &str = "<!doctype html><meta charset=utf-8><title>UwURDP</title>";

/// 32 random bytes, as text: the PKCE verifier (RFC 7636, 43 characters).
fn random_text() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    http::encode_base64(bytes)
}

/// The S256 challenge for a verifier.
pub(crate) fn challenge(verifier: &str) -> String {
    http::encode_base64(Sha256::digest(verifier.as_bytes()))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listening(state: &str) -> (SignIn, u16) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        (
            SignIn {
                url: String::new(),
                state: state.into(),
                verifier: "v".into(),
                listener,
            },
            port,
        )
    }

    fn get(port: u16, target: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(stream, "GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).unwrap();
        answer
    }

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    #[test]
    fn the_challenge_is_the_base64url_sha256_of_the_verifier() {
        // The example of RFC 7636, appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(random_text().len(), 43);
        assert_ne!(random_text(), random_text());
    }

    #[test]
    fn only_the_answer_with_the_right_state_is_taken() {
        let (sign_in, port) = listening("good-state");
        let browser = std::thread::spawn(move || {
            let stray = get(port, "/favicon.ico");
            let forged = get(port, "/uwusync/callback?code=evil&state=other");
            let real = get(port, "/uwusync/callback?code=abc%2F1&state=good-state&iss=https%3A%2F%2Fauth.example.com");
            (stray, forged, real)
        });
        let answer = wait_for_answer(&sign_in, far(), || false).unwrap();
        let (stray, forged, real) = browser.join().unwrap();

        assert!(stray.starts_with("HTTP/1.1 404"));
        assert!(forged.starts_with("HTTP/1.1 400"));
        assert!(real.starts_with("HTTP/1.1 200") && real.contains("Signed in"));
        assert_eq!(answer.code, "abc/1");
        assert_eq!(answer.iss.as_deref(), Some("https://auth.example.com"));
    }

    #[test]
    fn only_wrong_states_on_the_callback_count_towards_giving_up() {
        let (sign_in, port) = listening("good-state");
        let browser = std::thread::spawn(move || {
            // Far more strays than wrong states are allowed: they do not count.
            for _ in 0..(MAX_WRONG_STATE + 4) {
                get(port, "/favicon.ico");
            }
            for _ in 0..(MAX_WRONG_STATE - 1) {
                get(port, "/uwusync/callback?code=x&state=other");
            }
            get(port, "/uwusync/callback?code=real&state=good-state")
        });
        let answer = wait_for_answer(&sign_in, far(), || false).unwrap();
        assert!(browser.join().unwrap().starts_with("HTTP/1.1 200"));
        assert_eq!(answer.code, "real");

        let (sign_in, port) = listening("good-state");
        let browser = std::thread::spawn(move || {
            for _ in 0..MAX_WRONG_STATE {
                get(port, "/uwusync/callback?code=x&state=other");
            }
        });
        let result = wait_for_answer(&sign_in, far(), || false);
        browser.join().unwrap();
        assert!(matches!(result, Err(SsoError::Refused(_))));
    }

    #[test]
    fn a_connection_that_says_nothing_does_not_hold_up_the_answer() {
        let (sign_in, port) = listening("s");
        let browser = std::thread::spawn(move || {
            // Connected first, and silent.
            let _silent = TcpStream::connect(("127.0.0.1", port)).unwrap();
            let started = Instant::now();
            let real = get(port, "/uwusync/callback?code=c&state=s");
            (real, started.elapsed())
        });
        let answer = wait_for_answer(&sign_in, far(), || false).unwrap();
        let (real, waited) = browser.join().unwrap();
        assert!(real.starts_with("HTTP/1.1 200"));
        assert_eq!(answer.code, "c");
        assert!(
            waited < REQUEST_DEADLINE + Duration::from_secs(2),
            "{waited:?}"
        );
    }

    #[test]
    fn the_browser_goes_only_to_https_or_this_machine() {
        for good in [
            "https://auth.example.com/authorize?x=1",
            "http://127.0.0.1:8080/authorize",
        ] {
            assert!(acceptable_sign_in_url(good), "{good}");
        }
        for bad in [
            "http://auth.example.com/authorize",
            "http://127.0.0.1.example.com/authorize",
            "http://127.0.0.1@auth.example.com/authorize",
            "https://user@auth.example.com/authorize",
            "http://localhost/authorize",
            "javascript:alert(1)",
            "not a url",
        ] {
            assert!(!acceptable_sign_in_url(bad), "{bad}");
        }
    }

    #[test]
    fn a_refusal_from_uwuauth_ends_the_wait() {
        let (sign_in, port) = listening("s");
        let browser =
            std::thread::spawn(move || get(port, "/uwusync/callback?error=access_denied&state=s"));
        let result = wait_for_answer(&sign_in, far(), || false);
        browser.join().unwrap();
        assert!(matches!(result, Err(SsoError::Refused(reason)) if reason == "access_denied"));
    }

    #[test]
    fn the_wait_ends_when_cancelled_or_out_of_time() {
        let (sign_in, _) = listening("s");
        assert!(matches!(
            wait_for_answer(&sign_in, far(), || true),
            Err(SsoError::Cancelled)
        ));
        assert!(matches!(
            wait_for_answer(&sign_in, Instant::now(), || false),
            Err(SsoError::TimedOut)
        ));
    }
}
