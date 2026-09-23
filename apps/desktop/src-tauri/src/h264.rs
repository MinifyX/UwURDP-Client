//! OpenH264, Cisco's H.264 decoder, on the user's say-so.
//!
//! H.264 is patented. Cisco pays the license for its own OpenH264 binaries,
//! on conditions: the binary is downloaded from Cisco to the user's device
//! separately (never shipped inside an app), the user can turn its use on
//! and off, and the app says "OpenH264 Video Codec provided by Cisco
//! Systems, Inc." where that switch is. So UwURDP ships no H.264 code; this
//! module fetches Cisco's file when the user turns H.264 on, checks it
//! against the SHA-256 of the release we know, keeps it in the app's local
//! data folder and deletes it when the user turns H.264 off.
//!
//! The engine loads it per session (and checks the hash again, see
//! `uwurdp_core`'s gfx module); a file that is missing or wrong just means no
//! H.264 — the graphics pipeline works without it.

use parking_lot::Mutex;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager as _};

/// The OpenH264 release the `openh264` crate we build against knows.
pub(crate) const VERSION: &str = "2.6.0";
const BASE_URL: &str = "https://ciscobinary.openh264.org";
/// The biggest compressed file Cisco publishes is well under a megabyte.
const MAX_DOWNLOAD: u64 = 8 * 1024 * 1024;
const MAX_UNPACKED: u64 = 32 * 1024 * 1024;

/// Cisco's file for this platform and its SHA-256 (from Cisco's release,
/// as the `openh264` crate lists them).
struct Binary {
    file: &'static str,
    sha256: &'static str,
}

fn binary() -> Option<Binary> {
    let (file, sha256) = if cfg!(all(windows, target_arch = "x86_64")) {
        (
            "openh264-2.6.0-win64.dll",
            "2076cb5675ec6c1a4c70e7a2a322552f547b6eeed649d6dfcd9e02a543b24691",
        )
    } else if cfg!(all(windows, target_arch = "aarch64")) {
        (
            "openh264-2.6.0-win-arm64.dll",
            "fb75103938f4f47d119b983e06334df41a803bc72fb5c46e3623f6fea5782732",
        )
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        (
            "libopenh264-2.6.0-mac-arm64.dylib",
            "052e98bfcf7a9167d22f3bbb3f5988ef79065591f36af8b52924b22b13624551",
        )
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        (
            "libopenh264-2.6.0-mac-x64.dylib",
            "e3dc8bc01fe69363f61fd3c02fd27798537a585eadd38cd808f303d1ee505a19",
        )
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        (
            "libopenh264-2.6.0-linux64.8.so",
            "2f0cde7c6a6abcf5cae76942894ea42897fa677bce4ed6c91a24dd1b041d5f04",
        )
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        (
            "libopenh264-2.6.0-linux-arm64.8.so",
            "12e7b33623667cdab0e575170c147b1b36eadb77d0d2aa7ceb5afd3e58902140",
        )
    } else {
        return None;
    };
    Some(Binary { file, sha256 })
}

/// What the settings show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub(crate) enum Status {
    /// Cisco has no binary for this platform.
    Unsupported,
    Off,
    Downloading,
    Ready {
        version: &'static str,
    },
    Failed {
        message: String,
    },
}

pub(crate) struct Codec {
    folder: Option<PathBuf>,
    state: Mutex<State>,
}

struct State {
    enabled: bool,
    status: Status,
    /// Bumped on every switch, so a download that finishes after the user
    /// turned H.264 off again does not turn it back on.
    generation: u64,
}

impl Codec {
    pub fn new(app: &AppHandle) -> Arc<Self> {
        let folder = app
            .path()
            .app_local_data_dir()
            .ok()
            .map(|dir| dir.join("openh264"));
        Arc::new(Self {
            folder,
            state: Mutex::new(State {
                enabled: false,
                status: if binary().is_some() {
                    Status::Off
                } else {
                    Status::Unsupported
                },
                generation: 0,
            }),
        })
    }

    fn path(&self) -> Option<PathBuf> {
        Some(self.folder.as_ref()?.join(binary()?.file))
    }

    pub fn status(&self) -> Status {
        self.state.lock().status.clone()
    }

    /// The library for a new session: only when H.264 is on and installed.
    pub fn library(&self) -> Option<PathBuf> {
        let state = self.state.lock();
        if !(state.enabled && matches!(state.status, Status::Ready { .. })) {
            return None;
        }
        drop(state);
        self.path().filter(|path| path.is_file())
    }

    /// Turns H.264 on (downloading Cisco's binary unless it is already
    /// here and intact) or off (deleting it).
    pub fn set_enabled(self: &Arc<Self>, enabled: bool) -> Status {
        let Some((binary, path)) = binary().zip(self.path()) else {
            return Status::Unsupported;
        };
        let generation = {
            let mut state = self.state.lock();
            if state.enabled == enabled && !matches!(state.status, Status::Failed { .. }) {
                return state.status.clone();
            }
            state.enabled = enabled;
            state.generation += 1;
            if !enabled {
                state.status = Status::Off;
                drop(state);
                // Sessions that still have it loaded keep it; on Windows the
                // file then stays until the next time H.264 is turned off.
                if let Err(error) = std::fs::remove_file(&path) {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        tracing::info!(%error, "OpenH264 could not be deleted yet");
                    }
                }
                return Status::Off;
            }
            if intact(&path, binary.sha256) {
                state.status = Status::Ready { version: VERSION };
                return state.status.clone();
            }
            state.status = Status::Downloading;
            state.generation
        };

        let codec = self.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let result = download(&binary, &path);
            let mut state = codec.state.lock();
            if state.generation != generation {
                return;
            }
            state.status = match result {
                Ok(()) => {
                    tracing::info!(file = binary.file, "OpenH264 installed");
                    Status::Ready { version: VERSION }
                }
                Err(message) => {
                    tracing::warn!(%message, "OpenH264 download failed");
                    Status::Failed { message }
                }
            };
        });
        Status::Downloading
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn intact(path: &Path, sha256: &str) -> bool {
    std::fs::read(path).is_ok_and(|bytes| sha256_hex(&bytes) == sha256)
}

/// Fetches `<file>.bz2` from Cisco, unpacks and checks it, and puts it in
/// place in one rename.
fn download(binary: &Binary, path: &Path) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .use_preconfigured_tls(uwurdp_sync::pin::webpki_config())
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(15))
        .user_agent(concat!("UwURDP/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let url = format!("{BASE_URL}/{}.bz2", binary.file);
    let response = client
        .get(&url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Cisco's server could not be reached: {e}"))?;
    let mut packed = Vec::new();
    response
        .take(MAX_DOWNLOAD + 1)
        .read_to_end(&mut packed)
        .map_err(|e| format!("The download broke off: {e}"))?;
    if packed.len() as u64 > MAX_DOWNLOAD {
        return Err("The download is far bigger than OpenH264 is.".into());
    }
    let bytes = unpack(&packed)?;
    if sha256_hex(&bytes) != binary.sha256 {
        return Err("The downloaded file is not the OpenH264 release UwURDP knows.".into());
    }
    write_atomically(path, &bytes).map_err(|e| format!("OpenH264 could not be saved: {e}"))
}

fn unpack(packed: &[u8]) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    bzip2::read::BzDecoder::new(packed)
        .take(MAX_UNPACKED + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("The download is damaged: {e}"))?;
    if bytes.len() as u64 > MAX_UNPACKED {
        return Err("The download unpacks to far more than OpenH264 is.".into());
    }
    Ok(bytes)
}

fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let folder = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no folder"))?;
    std::fs::create_dir_all(folder)?;
    let partial = path.with_extension("partial");
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_platform_has_a_pinned_binary() {
        // Every platform UwURDP is built for has one.
        let binary = binary().expect("a binary for this platform");
        assert!(binary.file.contains(VERSION));
        assert_eq!(binary.sha256.len(), 64);
    }

    #[test]
    fn status_serializes_for_the_page() {
        let json = serde_json::to_value(Status::Ready { version: VERSION }).expect("json");
        assert_eq!(json["state"], "ready");
        assert_eq!(json["version"], VERSION);
        let json = serde_json::to_value(Status::Failed {
            message: "nope".into(),
        })
        .expect("json");
        assert_eq!(json["state"], "failed");
        assert_eq!(json["message"], "nope");
    }

    #[test]
    fn unpacking_checks_and_bounds() {
        use std::io::Write as _;
        let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        encoder.write_all(b"hello").expect("write");
        let packed = encoder.finish().expect("finish");
        assert_eq!(unpack(&packed).expect("unpack"), b"hello");
        assert!(unpack(b"not bzip2").is_err());
    }

    #[test]
    fn only_the_known_file_counts_as_intact() {
        let dir = std::env::temp_dir().join(format!("uwurdp-h264-{}", uuid::Uuid::new_v4()));
        let path = dir.join("lib");
        write_atomically(&path, b"abc").expect("write");
        assert!(intact(&path, &sha256_hex(b"abc")));
        assert!(!intact(&path, &sha256_hex(b"abd")));
        assert!(!dir.join("partial").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
