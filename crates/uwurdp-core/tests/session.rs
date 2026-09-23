//! End-to-end: the real client against the in-process dev server.

// The dev-dependency ironrdp-server links aws-lc, whose objects export a few
// symbols; MSVC then reports creating an import library. Harmless, and it
// never happens in the app, which does not link aws-lc.
#![allow(linker_messages)]

#[path = "support/dev_server.rs"]
mod dev_server;

use dev_server::{DevServer, DevServerOptions};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use uwurdp_core::{
    AudioMode, FrameSink, InputEvent, MouseButton, RdpError, RdpTarget, SessionId, SessionManager,
    SessionSettings, SinkError,
};
use zeroize::Zeroizing;

const WAIT: Duration = Duration::from_secs(20);

fn server() -> DevServer {
    server_with(DevServerOptions::default())
}

fn server_with(options: DevServerOptions) -> DevServer {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    dev_server::start(options).expect("dev server")
}

fn graphics_server() -> DevServer {
    server_with(DevServerOptions {
        graphics: true,
        ..DevServerOptions::default()
    })
}

fn settings() -> SessionSettings {
    SessionSettings {
        width: 1024,
        height: 768,
        audio: AudioMode::Off,
        // Tests must not touch the developer's real clipboard.
        clipboard: false,
        client_name: "uwurdp-test".into(),
        ..SessionSettings::default()
    }
}

fn target(server: &DevServer, password: &str, trusted: Option<String>) -> RdpTarget {
    RdpTarget {
        address: server.addr.ip().to_string(),
        port: server.addr.port(),
        username: dev_server::USERNAME.into(),
        domain: None,
        password: Zeroizing::new(password.into()),
        trusted_fingerprint: trusted,
        settings: settings(),
        gateway: None,
    }
}

enum Event {
    Message(Vec<u8>),
    Finished,
}

struct ChannelSink(mpsc::UnboundedSender<Event>);

impl FrameSink for ChannelSink {
    fn send(&self, frame: &[u8]) -> Result<(), SinkError> {
        self.0
            .send(Event::Message(frame.to_vec()))
            .map_err(|_| SinkError::Closed)
    }

    fn finish(&self) {
        let _ = self.0.send(Event::Finished);
    }
}

/// What the web page would do: keep a framebuffer, apply messages, ack
/// every BITMAPS once drawn.
struct Page {
    manager: Arc<SessionManager>,
    id: SessionId,
    events: mpsc::UnboundedReceiver<Event>,
    width: u16,
    height: u16,
    pixels: Vec<u8>,
    sizes: Vec<(u16, u16)>,
    bitmaps: usize,
    first_kinds: Vec<u8>,
    closed: Option<serde_json::Value>,
    finished: bool,
}

impl Page {
    fn new(
        manager: Arc<SessionManager>,
        id: SessionId,
        events: mpsc::UnboundedReceiver<Event>,
    ) -> Self {
        Self {
            manager,
            id,
            events,
            width: 0,
            height: 0,
            pixels: Vec::new(),
            sizes: Vec::new(),
            bitmaps: 0,
            first_kinds: Vec::new(),
            closed: None,
            finished: false,
        }
    }
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

impl Page {
    fn apply(&mut self, message: &[u8]) {
        if self.first_kinds.len() < 2 {
            self.first_kinds.push(message[0]);
        }
        match message[0] {
            1 => {
                let count = u16_at(message, 1);
                let mut at = 3;
                for _ in 0..count {
                    let (x, y, w, h) = (
                        usize::from(u16_at(message, at)),
                        usize::from(u16_at(message, at + 2)),
                        usize::from(u16_at(message, at + 4)),
                        usize::from(u16_at(message, at + 6)),
                    );
                    at += 8;
                    for row in 0..h {
                        let src = &message[at + row * w * 4..at + (row + 1) * w * 4];
                        let dst = ((y + row) * usize::from(self.width) + x) * 4;
                        self.pixels[dst..dst + w * 4].copy_from_slice(src);
                    }
                    at += w * h * 4;
                }
                assert_eq!(at, message.len(), "BITMAPS length mismatch");
                self.bitmaps += 1;
                self.manager.ack(self.id).ok();
            }
            2 => {
                assert_eq!(message.len(), 5);
                self.width = u16_at(message, 1);
                self.height = u16_at(message, 3);
                self.sizes.push((self.width, self.height));
                self.pixels = vec![0; usize::from(self.width) * usize::from(self.height) * 4];
            }
            3 => {
                let (w, h) = (
                    usize::from(u16_at(message, 5)),
                    usize::from(u16_at(message, 7)),
                );
                assert_eq!(message.len(), 9 + w * h * 4);
            }
            4 | 5 => assert_eq!(message.len(), 1),
            6 => assert_eq!(message.len(), 5),
            7 => {
                assert!(self.closed.is_none(), "CLOSED sent twice");
                self.closed = Some(serde_json::from_slice(&message[1..]).expect("CLOSED json"));
            }
            other => panic!("unknown message kind {other}"),
        }
    }

    fn pixel(&self, x: u16, y: u16) -> [u8; 4] {
        let i = (usize::from(y) * usize::from(self.width) + usize::from(x)) * 4;
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }

    /// Processes messages until `done` holds.
    async fn until(&mut self, what: &str, done: impl Fn(&Page) -> bool) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while !done(self) {
            let event = tokio::time::timeout_at(deadline, self.events.recv())
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "timed out waiting for {what} (sizes {:?}, {} bitmaps, pixel(5,5) {:?})",
                        self.sizes,
                        self.bitmaps,
                        (self.width > 5).then(|| self.pixel(5, 5))
                    )
                });
            match event {
                Some(Event::Message(m)) => self.apply(&m),
                Some(Event::Finished) | None => {
                    self.finished = true;
                    if !done(self) {
                        panic!("the stream ended while waiting for {what}");
                    }
                }
            }
        }
    }
}

async fn open(server: &DevServer) -> Page {
    open_with(server, settings()).await
}

async fn open_with(server: &DevServer, settings: SessionSettings) -> Page {
    let manager = Arc::new(SessionManager::new());
    let (tx, events) = mpsc::unbounded_channel();
    let mut target = target(
        server,
        dev_server::PASSWORD,
        Some(server.fingerprint.clone()),
    );
    target.settings = settings;
    let id = manager
        .connect("test", target, ChannelSink(tx))
        .await
        .expect("connect");
    assert!(manager.is_open(id));
    Page::new(manager, id, events)
}

async fn connect_err(target: RdpTarget) -> RdpError {
    let manager = SessionManager::new();
    let (tx, _events) = mpsc::unbounded_channel();
    manager
        .connect("test", target, ChannelSink(tx))
        .await
        .expect_err("connect should fail")
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_certificate_is_reported_before_login() {
    let server = server();
    match connect_err(target(&server, dev_server::PASSWORD, None)).await {
        RdpError::UnknownCertificate { observed } => {
            assert_eq!(observed.fingerprint, server.fingerprint);
            assert!(observed.subject.contains("rcgen"), "{}", observed.subject);
            assert!(!observed.der_base64.is_empty());
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_different_certificate_is_a_change() {
    let server = server();
    let pinned = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned();
    match connect_err(target(&server, dev_server::PASSWORD, Some(pinned.clone()))).await {
        RdpError::CertificateChanged { expected, observed } => {
            assert_eq!(expected, pinned);
            assert_eq!(observed.fingerprint, server.fingerprint);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_password_is_auth_failed() {
    let server = server();
    let err = connect_err(target(&server, "wrong", Some(server.fingerprint.clone()))).await;
    assert!(matches!(err, RdpError::AuthFailed { .. }), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_only_against_an_nla_server_is_a_negotiation_error() {
    let server = server();
    let mut t = target(
        &server,
        dev_server::PASSWORD,
        Some(server.fingerprint.clone()),
    );
    t.settings.nla = false;
    match connect_err(t).await {
        RdpError::Negotiation { message } => assert!(message.contains("NLA"), "{message}"),
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_port_is_unreachable() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr").port()
    };
    let mut t = target(&server(), "x", None);
    t.address = "127.0.0.1".into();
    t.port = port;
    let err = connect_err(t).await;
    assert!(matches!(err, RdpError::Unreachable { .. }), "{err:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hanging_attempt_can_be_cancelled() {
    // Accepts TCP, then says nothing: the negotiation would wait forever.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _held = listener.accept().await;
        tokio::time::sleep(Duration::from_secs(60)).await;
    });

    let manager = Arc::new(SessionManager::new());
    let (tx, _events) = mpsc::unbounded_channel();
    let mut t = target(&server(), "x", None);
    t.address = addr.ip().to_string();
    t.port = addr.port();
    let connecting = {
        let manager = manager.clone();
        tokio::spawn(async move { manager.connect("hang", t, ChannelSink(tx)).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    manager.cancel("hang");
    let result = tokio::time::timeout(Duration::from_secs(5), connecting)
        .await
        .expect("cancel took effect")
        .expect("task");
    assert!(matches!(result, Err(RdpError::Cancelled)), "{result:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_streams_reacts_to_input_resizes_and_closes() {
    let server = server();
    let page = open(&server).await;
    full_session(&server, page).await;
    assert_eq!(server.graphics_frames(), (0, 0));
}

/// The same session through the graphics pipeline, as Windows serves it:
/// the client announces it, the server opens it, and every picture after
/// that — resize included — comes through it.
#[tokio::test(flavor = "multi_thread")]
async fn the_graphics_pipeline_carries_the_whole_session() {
    let server = graphics_server();
    let page = open(&server).await;
    full_session(&server, page).await;
    let (uncompressed, h264) = server.graphics_frames();
    // `full_session` saw every change arrive. The server folds changes that
    // come while frames wait for acknowledgement into one, so only the first
    // picture and the resized one are certain to be frames of their own.
    assert!(
        uncompressed >= 2,
        "only {uncompressed} frames through the pipeline"
    );
    assert_eq!(h264, 0, "no H.264 without OpenH264");
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_pipeline_a_graphics_server_uses_bitmaps() {
    let server = graphics_server();
    let mut page = open_with(
        &server,
        SessionSettings {
            graphics_pipeline: false,
            ..settings()
        },
    )
    .await;
    page.until("the pink desktop", |p| {
        p.bitmaps > 0 && {
            let [r, g, b, a] = p.pixel(5, 5);
            a == 255 && r > 200 && g < 150 && b > 60
        }
    })
    .await;
    assert_eq!(server.graphics_frames(), (0, 0));
}

async fn full_session(server: &DevServer, mut page: Page) {
    // DESKTOP_SIZE first, then a full-screen BITMAPS, then the real desktop.
    // (The graphics pipeline announces the same size once more, possibly
    // before the first pixels.)
    page.until("the first frame", |p| p.bitmaps > 0).await;
    assert_eq!(page.first_kinds[0], 2, "{:?}", page.first_kinds);
    assert!(
        matches!(page.first_kinds[1], 1 | 2),
        "{:?}",
        page.first_kinds
    );
    assert!(
        page.sizes.iter().all(|&size| size == (1024, 768)),
        "{:?}",
        page.sizes
    );
    page.until("the pink desktop", |p| {
        let [r, g, b, a] = p.pixel(5, 5);
        a == 255 && r > 200 && g < 150 && b > 60
    })
    .await;

    // Moving the mouse moves the white square.
    page.manager
        .input(page.id, vec![InputEvent::Move { x: 300, y: 200 }])
        .expect("input");
    page.until("the square under the cursor", |p| {
        p.pixel(300, 200).iter().all(|&c| c > 230)
    })
    .await;

    // A click leaves a dark dot after the square has moved on.
    page.manager
        .input(
            page.id,
            vec![
                InputEvent::Button {
                    button: MouseButton::Left,
                    down: true,
                    x: 600,
                    y: 400,
                },
                InputEvent::Button {
                    button: MouseButton::Left,
                    down: false,
                    x: 600,
                    y: 400,
                },
                InputEvent::Move { x: 100, y: 600 },
            ],
        )
        .expect("input");
    page.until("the dot", |p| {
        let [r, g, b, _] = p.pixel(600, 400);
        r < 80 && g < 40 && b < 80
    })
    .await;

    // A key press shifts the hue of the background.
    let before = page.pixel(1000, 20);
    page.manager
        .input(
            page.id,
            vec![
                InputEvent::Key {
                    code: 0x1E,
                    extended: false,
                    down: true,
                },
                InputEvent::Key {
                    code: 0x1E,
                    extended: false,
                    down: false,
                },
            ],
        )
        .expect("input");
    page.until("the hue shift", |p| {
        let now = p.pixel(1000, 20);
        (0..3)
            .map(|i| now[i].abs_diff(before[i]) as u32)
            .sum::<u32>()
            > 60
    })
    .await;
    assert!(server.key_presses() >= 1);

    // Resize through DisplayControl: a new DESKTOP_SIZE, then pixels again.
    // The channel opens a moment after the session starts, so retry.
    let deadline = tokio::time::Instant::now() + WAIT;
    while page.sizes.last() != Some(&(800, 600)) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "resize never happened"
        );
        page.manager.resize(page.id, 800, 600, 100).expect("resize");
        let bitmaps = page.bitmaps;
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            page.until("a resize", |p| {
                p.sizes.last() == Some(&(800, 600)) || p.bitmaps > bitmaps + 50
            }),
        )
        .await;
    }
    let after_resize = page.bitmaps;
    page.until("pixels after the resize", |p| {
        p.bitmaps > after_resize && p.pixel(5, 5)[3] == 255 && p.pixel(5, 5)[0] > 100
    })
    .await;
    assert_eq!(server.size(), (800, 600));
    assert_eq!((page.width, page.height), (800, 600));

    // Close: CLOSED with a reason, then finish.
    page.manager.close(page.id).expect("close");
    page.until("the end of the stream", |p| p.finished).await;
    let closed = page.closed.clone().expect("CLOSED before finish");
    assert_eq!(closed["reason"], "disconnect", "{closed}");
    assert!(closed["message"].as_str().is_some_and(|m| !m.is_empty()));
    // The session is removed a moment after its stream finished.
    let deadline = tokio::time::Instant::now() + WAIT;
    while page.manager.is_open(page.id) {
        assert!(tokio::time::Instant::now() < deadline, "still open");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(page.manager.ack(page.id).is_err());
}

/// Panics the first time it is handed pixels, like a decoder bug would.
struct PanickingSink {
    inner: ChannelSink,
    tripped: AtomicBool,
}

impl FrameSink for PanickingSink {
    fn send(&self, frame: &[u8]) -> Result<(), SinkError> {
        if frame[0] == 1 && !self.tripped.swap(true, Ordering::SeqCst) {
            panic!("a bug in one session");
        }
        self.inner.send(frame)
    }

    fn finish(&self) {
        self.inner.finish();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_panicking_session_takes_no_other_down() {
    let healthy_server = server();
    let crashing_server = server();
    let mut healthy = open(&healthy_server).await;
    healthy.until("the first frame", |p| p.bitmaps > 0).await;

    let (tx, events) = mpsc::unbounded_channel();
    let id = healthy
        .manager
        .connect(
            "crashing",
            target(
                &crashing_server,
                dev_server::PASSWORD,
                Some(crashing_server.fingerprint.clone()),
            ),
            PanickingSink {
                inner: ChannelSink(tx),
                tripped: AtomicBool::new(false),
            },
        )
        .await
        .expect("connect");
    let mut crashing = Page::new(healthy.manager.clone(), id, events);

    // The page hears why, and the stream ends.
    crashing
        .until("the end of the crashed session", |p| p.finished)
        .await;
    let closed = crashing.closed.clone().expect("CLOSED before finish");
    assert_eq!(closed["reason"], "error", "{closed}");
    assert!(closed["message"]
        .as_str()
        .is_some_and(|m| m.contains("internal error")));
    let deadline = tokio::time::Instant::now() + WAIT;
    while healthy.manager.is_open(id) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the crashed session stays open"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The other session never noticed.
    assert!(healthy.manager.is_open(healthy.id));
    healthy
        .manager
        .input(healthy.id, vec![InputEvent::Move { x: 300, y: 200 }])
        .expect("input");
    healthy
        .until("the square under the cursor", |p| {
            p.pixel(300, 200).iter().all(|&c| c > 230)
        })
        .await;
}
