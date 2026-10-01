//! Connects to a real server without the app, keeps the session open for a
//! while and logs what the graphics pipeline did: which codecs the server
//! used and how many updates failed (the "graphics pipeline done" line).
//!
//! ```text
//! UWURDP_PROBE_ADDR=192.0.2.10 UWURDP_PROBE_USER=alice UWURDP_PROBE_PASSWORD=… \
//!   UWURDP_PROBE_FINGERPRINT=SHA256:… UWURDP_PROBE_SECS=30 \
//!   cargo run --example probe
//! ```
//!
//! Without a fingerprint the first run fails with the certificate the server
//! showed; check it and pass it in. Frames are acknowledged; with
//! `UWURDP_PROBE_DUMP=picture.ppm` the last picture is saved, to compare
//! with a screenshot taken on the server.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use parking_lot::Mutex;
use uwurdp_core::frame::{KIND_BITMAPS, KIND_DESKTOP_SIZE};
use uwurdp_core::{FrameSink, RdpTarget, SessionId, SessionManager, SessionSettings, SinkError};
use zeroize::Zeroizing;

struct Acker {
    manager: Arc<SessionManager>,
    id: Arc<OnceLock<SessionId>>,
    picture: Arc<Mutex<Picture>>,
}

/// The desktop as the page would show it, RGBA.
#[derive(Default)]
struct Picture {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

impl Picture {
    fn apply(&mut self, frame: &[u8]) {
        let u16_at = |i: usize| usize::from(u16::from_le_bytes([frame[i], frame[i + 1]]));
        match frame.first() {
            Some(&KIND_DESKTOP_SIZE) => {
                self.width = u16_at(1);
                self.height = u16_at(3);
                self.rgba = vec![0; self.width * self.height * 4];
            }
            Some(&KIND_BITMAPS) => {
                let mut at = 3;
                for _ in 0..u16_at(1) {
                    let (x, y, w, h) = (u16_at(at), u16_at(at + 2), u16_at(at + 4), u16_at(at + 6));
                    at += 8;
                    for row in 0..h {
                        let to = ((y + row) * self.width + x) * 4;
                        if let Some(dest) = self.rgba.get_mut(to..to + w * 4) {
                            dest.copy_from_slice(&frame[at..at + w * 4]);
                        }
                        at += w * 4;
                    }
                }
            }
            _ => {}
        }
    }

    fn save_ppm(&self, path: &str) {
        let mut out = format!("P6\n{} {}\n255\n", self.width, self.height).into_bytes();
        for px in self.rgba.as_chunks::<4>().0 {
            out.extend_from_slice(&px[..3]);
        }
        std::fs::write(path, out).expect("write the picture");
    }
}

impl FrameSink for Acker {
    fn send(&self, frame: &[u8]) -> Result<(), SinkError> {
        self.picture.lock().apply(frame);
        if let Some(id) = self.id.get() {
            self.manager.ack(*id).ok();
        }
        Ok(())
    }

    fn finish(&self) {}
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "uwurdp_core=info".into()),
        )
        .init();

    let address = env("UWURDP_PROBE_ADDR").expect("UWURDP_PROBE_ADDR");
    let user = env("UWURDP_PROBE_USER").expect("UWURDP_PROBE_USER");
    let (domain, username) = match user.split_once('\\') {
        Some((domain, name)) => (Some(domain.to_owned()), name.to_owned()),
        None => (None, user),
    };
    let secs = env("UWURDP_PROBE_SECS").map_or(30, |s| s.parse().expect("seconds"));

    let target = RdpTarget {
        address,
        port: env("UWURDP_PROBE_PORT").map_or(3389, |p| p.parse().expect("port")),
        username,
        domain,
        password: Zeroizing::new(env("UWURDP_PROBE_PASSWORD").unwrap_or_default()),
        trusted_fingerprint: env("UWURDP_PROBE_FINGERPRINT"),
        settings: SessionSettings {
            width: 1920,
            height: 1080,
            client_name: "uwurdp-probe".into(),
            clipboard: false,
            ..SessionSettings::default()
        },
        gateway: None,
    };

    let manager = Arc::new(SessionManager::new());
    let id = Arc::new(OnceLock::new());
    let picture = Arc::new(Mutex::new(Picture::default()));
    let sink = Acker {
        manager: manager.clone(),
        id: id.clone(),
        picture: picture.clone(),
    };
    let session = match manager.connect("probe", target, sink).await {
        Ok(session) => session,
        Err(error) => {
            eprintln!("connect failed: {error:?}");
            std::process::exit(1);
        }
    };
    id.set(session).ok();
    // The first frames went out before the id was known.
    manager.ack(session).ok();

    tokio::time::sleep(Duration::from_secs(secs)).await;
    manager.close(session).ok();
    while manager.is_open(session) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if let Some(path) = env("UWURDP_PROBE_DUMP") {
        picture.lock().save_ppm(&path);
    }
}
