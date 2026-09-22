//! A toy RDP server for manual tests and the app's end-to-end run, built on
//! `ironrdp-server`. Shared by `examples/dev_rdpd.rs` and the integration
//! tests (included with `#[path]`, so it can use dev-dependencies without a
//! cargo feature leaking into the library).
//!
//! It serves a synthetic desktop: a pink gradient, a square that follows the
//! mouse, a dot wherever the left button goes down, and a hue shift on every
//! key press. It speaks CredSSP (NLA) only and accepts exactly one account.
//! DisplayControl resizes are honored.
//!
//! `ironrdp-server` handles one connection at a time; a second client waits
//! until the first one leaves.

#![allow(dead_code)] // Each includer uses a different subset.

use ironrdp_server::tokio_rustls::rustls;
use ironrdp_server::tokio_rustls::TlsAcceptor;
use ironrdp_server::{
    BitmapUpdate, Credentials, DesktopSize, DisplayUpdate, KeyboardEvent, MouseEvent, PixelFormat,
    RdpServer, RdpServerDisplay, RdpServerDisplayUpdates, RdpServerInputHandler, ServerEvent,
};
use std::net::SocketAddr;
use std::num::{NonZeroU16, NonZeroUsize};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot, Notify};

pub const DEFAULT_ADDR: &str = "127.0.0.1:3390";
pub const USERNAME: &str = "uwu";
pub const PASSWORD: &str = "nyu";

/// Base colour of the desktop, #ff4d8d, as a hue in degrees.
const BASE_HUE: f32 = 339.0;
/// How far one key press turns the hue.
const HUE_STEP: f32 = 25.0;
const SQUARE: u16 = 40;
const DOT_RADIUS: i32 = 5;
const MAX_DOTS: usize = 1000;

pub struct DevServerOptions {
    pub addr: SocketAddr,
    pub width: u16,
    pub height: u16,
}

impl Default for DevServerOptions {
    fn default() -> Self {
        Self {
            addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            width: 1024,
            height: 768,
        }
    }
}

/// A running server. Dropping the handle asks it to stop.
pub struct DevServer {
    pub addr: SocketAddr,
    /// `SHA256:...` of the generated certificate, as the client computes it.
    pub fingerprint: String,
    events: mpsc::UnboundedSender<ServerEvent>,
    scene: Arc<Shared>,
}

impl DevServer {
    /// Current desktop size of the scene (changes on DisplayControl resize).
    pub fn size(&self) -> (u16, u16) {
        let scene = self.scene.scene.lock().expect("scene lock");
        (scene.width, scene.height)
    }

    /// How many key presses the server has seen.
    pub fn key_presses(&self) -> u32 {
        self.scene.scene.lock().expect("scene lock").key_presses
    }
}

impl Drop for DevServer {
    fn drop(&mut self) {
        // One Quit ends an active connection, the next one the accept loop.
        let _ = self.events.send(ServerEvent::Quit("stop".into()));
        let _ = self.events.send(ServerEvent::Quit("stop".into()));
    }
}

struct Scene {
    width: u16,
    height: u16,
    hue: f32,
    cursor: (u16, u16),
    dots: Vec<(u16, u16)>,
    pending_resize: Option<(u16, u16)>,
    dirty: bool,
    key_presses: u32,
}

struct Shared {
    scene: Mutex<Scene>,
    changed: Notify,
}

impl Shared {
    fn update(&self, f: impl FnOnce(&mut Scene)) {
        if let Ok(mut scene) = self.scene.lock() {
            f(&mut scene);
            scene.dirty = true;
        }
        self.changed.notify_one();
    }
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [u8; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    ]
}

/// Renders the scene as BGRA.
fn render(scene: &Scene) -> Vec<u8> {
    let (w, h) = (usize::from(scene.width), usize::from(scene.height));
    let mut data = vec![0u8; w * h * 4];
    // The gradient: hue drifts left to right, brightness falls top to bottom.
    let columns: Vec<[u8; 3]> = (0..w)
        .map(|x| hsv_to_rgb(scene.hue + 30.0 * x as f32 / w as f32, 0.70, 1.0))
        .collect();
    for y in 0..h {
        let shade = 1.0 - 0.35 * y as f32 / h as f32;
        let row = &mut data[y * w * 4..(y + 1) * w * 4];
        for (x, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let [r, g, b] = columns[x];
            px[0] = (f32::from(b) * shade) as u8;
            px[1] = (f32::from(g) * shade) as u8;
            px[2] = (f32::from(r) * shade) as u8;
            px[3] = 0xFF;
        }
    }
    let mut fill = |x0: i32, y0: i32, x1: i32, y1: i32, bgr: [u8; 3], round: bool| {
        let (cx, cy) = ((x0 + x1) / 2, (y0 + y1) / 2);
        for y in y0.max(0)..y1.min(h as i32) {
            for x in x0.max(0)..x1.min(w as i32) {
                if round && (x - cx).pow(2) + (y - cy).pow(2) > DOT_RADIUS * DOT_RADIUS {
                    continue;
                }
                let i = (y as usize * w + x as usize) * 4;
                data[i..i + 3].copy_from_slice(&bgr);
            }
        }
    };
    for &(x, y) in &scene.dots {
        let (x, y) = (i32::from(x), i32::from(y));
        fill(
            x - DOT_RADIUS,
            y - DOT_RADIUS,
            x + DOT_RADIUS + 1,
            y + DOT_RADIUS + 1,
            [0x2a, 0x0a, 0x3b],
            true,
        );
    }
    let half = i32::from(SQUARE / 2);
    let (cx, cy) = (i32::from(scene.cursor.0), i32::from(scene.cursor.1));
    fill(
        cx - half,
        cy - half,
        cx + half,
        cy + half,
        [0xff, 0xff, 0xff],
        false,
    );
    data
}

fn full_frame(scene: &Scene) -> Option<DisplayUpdate> {
    let width = NonZeroU16::new(scene.width)?;
    let height = NonZeroU16::new(scene.height)?;
    let stride = NonZeroUsize::new(usize::from(scene.width) * 4)?;
    Some(DisplayUpdate::Bitmap(BitmapUpdate {
        x: 0,
        y: 0,
        width,
        height,
        format: PixelFormat::BgrA32,
        data: render(scene).into(),
        stride,
    }))
}

struct Display(Arc<Shared>);

#[async_trait::async_trait]
impl RdpServerDisplay for Display {
    async fn size(&mut self) -> DesktopSize {
        let scene = self.0.scene.lock().expect("scene lock");
        DesktopSize {
            width: scene.width,
            height: scene.height,
        }
    }

    async fn request_initial_size(&mut self, client: DesktopSize) -> DesktopSize {
        // Serve whatever size the client asked for.
        let width = client.width.clamp(200, 8192);
        let height = client.height.clamp(200, 8192);
        self.0.update(|s| {
            s.width = width;
            s.height = height;
            s.cursor = (width / 2, height / 2);
        });
        DesktopSize { width, height }
    }

    async fn updates(&mut self) -> anyhow::Result<Box<dyn RdpServerDisplayUpdates>> {
        // Called on connect and after each reactivation: start with a full frame.
        self.0.update(|_| {});
        Ok(Box::new(Updates(self.0.clone())))
    }

    fn request_layout(&mut self, layout: ironrdp_displaycontrol::pdu::DisplayControlMonitorLayout) {
        let Some(monitor) = layout.monitors().first() else {
            return;
        };
        let (w, h) = monitor.dimensions();
        let (w, h) = (
            u16::try_from(w.clamp(200, 8192)).unwrap_or(1024),
            u16::try_from(h.clamp(200, 8192)).unwrap_or(768),
        );
        self.0.update(|s| s.pending_resize = Some((w, h)));
    }
}

struct Updates(Arc<Shared>);

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for Updates {
    async fn next_update(&mut self) -> anyhow::Result<Option<DisplayUpdate>> {
        loop {
            {
                let mut scene = self.0.scene.lock().expect("scene lock");
                if let Some((width, height)) = scene.pending_resize.take() {
                    if (width, height) != (scene.width, scene.height) {
                        scene.width = width;
                        scene.height = height;
                        scene.cursor.0 = scene.cursor.0.min(width - 1);
                        scene.cursor.1 = scene.cursor.1.min(height - 1);
                        scene.dirty = true;
                        return Ok(Some(DisplayUpdate::Resize(DesktopSize { width, height })));
                    }
                }
                if scene.dirty {
                    scene.dirty = false;
                    return Ok(full_frame(&scene));
                }
            }
            // Cancel-safe: a notification that arrives while nobody waits
            // is kept as a permit for the next call.
            self.0.changed.notified().await;
        }
    }
}

struct Input(Arc<Shared>);

impl RdpServerInputHandler for Input {
    fn keyboard(&mut self, event: KeyboardEvent) {
        if let KeyboardEvent::Pressed { .. } | KeyboardEvent::UnicodePressed(_) = event {
            self.0.update(|s| {
                s.hue = (s.hue + HUE_STEP) % 360.0;
                s.key_presses += 1;
            });
        }
    }

    fn mouse(&mut self, event: MouseEvent) {
        match event {
            MouseEvent::Move { x, y } => self.0.update(|s| s.cursor = (x, y)),
            MouseEvent::LeftPressed => self.0.update(|s| {
                if s.dots.len() < MAX_DOTS {
                    let cursor = s.cursor;
                    s.dots.push(cursor);
                }
            }),
            _ => {}
        }
    }
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Starts the server on its own thread (ironrdp-server's connection loop is
/// not `Send`, so it gets a current-thread runtime of its own) and returns
/// once it listens.
pub fn start(options: DevServerOptions) -> Result<DevServer, BoxError> {
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_owned(), "uwurdp-dev".to_owned()])?;
    let cert_der = certified.cert.der().to_vec();
    let key_der = certified.signing_key.serialize_der();
    let fingerprint = uwurdp_core::certificate_fingerprint(&cert_der);

    let public_key = {
        use x509_cert::der::Decode as _;
        let cert = x509_cert::Certificate::from_der(&cert_der)?;
        cert.tbs_certificate
            .subject_public_key_info
            .subject_public_key
            .as_bytes()
            .ok_or("malformed public key")?
            .to_vec()
    };

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(
            vec![cert_der.into()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(key_der.into()),
        )?;
    let acceptor = TlsAcceptor::from(Arc::new(tls));

    let shared = Arc::new(Shared {
        scene: Mutex::new(Scene {
            width: options.width,
            height: options.height,
            hue: BASE_HUE,
            cursor: (options.width / 2, options.height / 2),
            dots: Vec::new(),
            pending_resize: None,
            dirty: true,
            key_presses: 0,
        }),
        changed: Notify::new(),
    });

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let thread_shared = shared.clone();
    std::thread::Builder::new()
        .name("dev-rdpd".into())
        .spawn(move || {
            // Built here: RdpServer is not Send.
            let mut server = RdpServer::builder()
                .with_addr(options.addr)
                .with_hybrid(acceptor, public_key)
                .with_input_handler(Input(thread_shared.clone()))
                .with_display_handler(Display(thread_shared))
                .with_honor_client_desktop_size(true)
                .build();
            server.set_credentials(Some(Credentials {
                username: USERNAME.to_owned(),
                password: PASSWORD.to_owned(),
                domain: None,
            }));
            let events = server.event_sender().clone();

            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    let _ = ready_tx.send(Err(e.to_string()));
                    return;
                }
            };
            runtime.block_on(async move {
                let (addr_tx, addr_rx) = oneshot::channel();
                let _ = events.send(ServerEvent::GetLocalAddr(addr_tx));
                let run = server.run();
                tokio::pin!(run);
                tokio::select! {
                    addr = addr_rx => {
                        let ready = addr
                            .ok()
                            .flatten()
                            .map(|addr| (addr, events.clone()))
                            .ok_or_else(|| "no local address".to_owned());
                        let _ = ready_tx.send(ready);
                    }
                    result = &mut run => {
                        let _ = ready_tx.send(Err(format!("server stopped: {result:?}")));
                        return;
                    }
                }
                if let Err(e) = run.await {
                    eprintln!("dev_rdpd: {e:#}");
                }
            });
        })?;

    let (addr, events) = ready_rx.recv()??;
    Ok(DevServer {
        addr,
        fingerprint,
        events,
        scene: shared,
    })
}
