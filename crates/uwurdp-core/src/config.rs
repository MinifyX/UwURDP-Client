//! What the app asks for: the target and the session settings.

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// Settings the page picks per connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSettings {
    /// Desktop size in device pixels.
    pub width: u16,
    pub height: u16,
    /// 100 = 100 %, e.g. 150 on a HiDPI screen.
    pub scale_factor: u32,
    /// 15, 16, 24 or 32. Anything else is treated as 32.
    pub color_depth: u32,
    pub audio: AudioMode,
    /// Text clipboard in both directions.
    pub clipboard: bool,
    /// Console / admin session. IronRDP 0.17 hard-codes the GCC cluster data
    /// block to "none", so there is no way to request the console session:
    /// this flag is accepted and ignored (see the crate docs).
    pub admin: bool,
    /// Network Level Authentication (CredSSP). `true` requires it; `false`
    /// connects with plain TLS and sends the credentials in the logon packet.
    pub nla: bool,
    pub wallpaper: bool,
    pub animations: bool,
    pub font_smoothing: bool,
    /// Windows keyboard layout id (e.g. 0x0407 for German); 0 = let the server decide.
    pub keyboard_layout: u32,
    /// Shown on the server; the app passes the hostname. Truncated to 15
    /// characters by the protocol.
    pub client_name: String,
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 800,
            scale_factor: 100,
            color_depth: 32,
            audio: AudioMode::Local,
            clipboard: true,
            admin: false,
            nla: true,
            wallpaper: true,
            animations: true,
            font_smoothing: true,
            keyboard_layout: 0,
            client_name: String::from("UwURDP"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AudioMode {
    /// Play here, through the default output device.
    Local,
    /// Leave audio on the server. IronRDP cannot send the "play on the
    /// remote computer" flag, so this currently behaves like [`AudioMode::Off`].
    Remote,
    /// No audio.
    Off,
}

/// An RD Gateway to tunnel through.
pub struct GatewayTarget {
    pub address: String,
    pub port: u16,
    pub username: String,
    pub domain: Option<String>,
    pub password: Zeroizing<String>,
}

impl std::fmt::Debug for GatewayTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayTarget")
            .field("address", &self.address)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("domain", &self.domain)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Where to connect and as whom.
pub struct RdpTarget {
    pub address: String,
    pub port: u16,
    pub username: String,
    pub domain: Option<String>,
    pub password: Zeroizing<String>,
    /// `"SHA256:<base64 without padding>"` of the server certificate's DER, as
    /// trusted before; `None` = never seen.
    pub trusted_fingerprint: Option<String>,
    pub settings: SessionSettings,
    pub gateway: Option<GatewayTarget>,
}

impl std::fmt::Debug for RdpTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RdpTarget")
            .field("address", &self.address)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("domain", &self.domain)
            .field("password", &"<redacted>")
            .field("trusted_fingerprint", &self.trusted_fingerprint)
            .field("settings", &self.settings)
            .field("gateway", &self.gateway)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_use_camel_case_and_audio_kebab_case() {
        let json = serde_json::json!({
            "width": 1920, "height": 1080, "scaleFactor": 150, "colorDepth": 32,
            "audio": "remote", "clipboard": true, "admin": false, "nla": true,
            "wallpaper": false, "animations": false, "fontSmoothing": true,
            "keyboardLayout": 1031, "clientName": "laptop"
        });
        let settings: SessionSettings = serde_json::from_value(json).expect("parse");
        assert_eq!(settings.scale_factor, 150);
        assert_eq!(settings.audio, AudioMode::Remote);
        assert_eq!(settings.keyboard_layout, 1031);
        let back = serde_json::to_value(&settings).expect("serialize");
        assert_eq!(back["fontSmoothing"], true);
        assert_eq!(back["audio"], "remote");
    }

    #[test]
    fn debug_output_never_shows_passwords() {
        let target = RdpTarget {
            address: "host".into(),
            port: 3389,
            username: "user".into(),
            domain: None,
            password: Zeroizing::new("hunter2".into()),
            trusted_fingerprint: None,
            settings: SessionSettings::default(),
            gateway: Some(GatewayTarget {
                address: "gw".into(),
                port: 443,
                username: "user".into(),
                domain: None,
                password: Zeroizing::new("gwsecret".into()),
            }),
        };
        let debug = format!("{target:?}");
        assert!(!debug.contains("hunter2"));
        assert!(!debug.contains("gwsecret"));
    }
}
