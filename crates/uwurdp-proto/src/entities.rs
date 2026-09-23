//! What UwURDP stores and syncs.
//!
//! The record kinds and the envelope are UwUSSH's, unchanged, because both
//! apps sync through the same server. What an RDP host needs beyond an SSH
//! host — how its desktop is shown, audio, clipboard, a gateway — lives in
//! [`RdpSettings`], one object inside the host's payload.
//!
//! One record is an [`Envelope`](crate::Envelope) — id, kind, clock, tombstone
//! — plus a sealed payload. The payloads live here, and they hold *only* the
//! fields that mean something on another device: no local connection times, no
//! key file paths, no detected system. The envelope carries everything else, so
//! the sync engine can move records around without knowing whether a blob is a
//! host, a key or a snippet.
//!
//! Every payload keeps the fields it did not recognise in [`Extra`]. A device
//! running an older build can therefore edit a host a newer one wrote without
//! dropping the fields it has never heard of.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Fields a payload did not recognise, kept verbatim so they survive an edit
/// on a device whose build is older than the one that wrote them.
pub type Extra = serde_json::Map<String, serde_json::Value>;

fn extra_is_empty(extra: &Extra) -> bool {
    extra.is_empty()
}

/// What kind of record a blob holds. Part of the associated data when
/// encrypting, so a malicious server cannot hand back a key where a snippet was
/// expected.
///
/// **Append only.** The discriminant is what goes into the associated data;
/// changing one would make every sealed record of that kind unreadable. They
/// are written out so that reordering the list can't do it by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum EntityKind {
    Host = 0,
    Group = 1,
    Identity = 2,
    Key = 3,
    Snippet = 4,
    PortForward = 5,
    KnownHost = 6,
    TerminalProfile = 7,
    /// A password, private key or passphrase, which records point at by id.
    /// Its payload is the secret itself, not JSON.
    Secret = 8,
    /// What one device holds: every record's id and version, so another
    /// device can tell whether the server handed it everything. See
    /// [`crate::manifest`].
    Manifest = 9,
}

impl EntityKind {
    /// Every kind, in discriminant order.
    pub const ALL: [Self; 10] = [
        Self::Host,
        Self::Group,
        Self::Identity,
        Self::Key,
        Self::Snippet,
        Self::PortForward,
        Self::KnownHost,
        Self::TerminalProfile,
        Self::Secret,
        Self::Manifest,
    ];

    /// The kind a discriminant stands for, or `None` for one a newer build
    /// added.
    pub fn from_discriminant(value: u8) -> Option<Self> {
        Self::ALL.get(usize::from(value)).copied()
    }

    /// Kinds a record can point at, before the kinds that point at them. A
    /// batch applied in this order needs the fewest placeholder rows.
    ///
    /// Manifests are not in here on purpose: this list doubles as the list of
    /// record tables, and a manifest sorts last anyway, after the records it
    /// talks about.
    pub const APPLY_ORDER: [Self; 7] = [
        Self::Secret,
        Self::Key,
        Self::Identity,
        Self::Group,
        Self::Host,
        Self::Snippet,
        Self::KnownHost,
    ];

    /// Where this kind sorts when a batch is applied. Unknown-to-us kinds go
    /// last; they are stored and passed on, not understood.
    pub fn apply_rank(self) -> usize {
        Self::APPLY_ORDER
            .iter()
            .position(|kind| *kind == self)
            .unwrap_or(Self::APPLY_ORDER.len())
    }
}

/// A host, as another device needs it. `workspace` is a string rather than an
/// enum on purpose: a build that meets a workspace it does not know shows the
/// host in the private one instead of refusing the whole record.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostPayload {
    pub name: String,
    pub address: String,
    pub port: u16,
    pub workspace: String,
    /// The order the user dragged the host into, within its group.
    pub position: i64,
    pub group_id: Option<Uuid>,
    /// Its own login. `None` logs in with the group's, like RDCMan's
    /// "inherit from parent".
    pub identity_id: Option<Uuid>,
    #[serde(default)]
    pub rdp: RdpSettings,
    /// The login for the host's gateway, when it has one of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_identity_id: Option<Uuid>,
    /// A note, like RDCMan's comment field.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub comment: String,
    #[serde(flatten, default, skip_serializing_if = "extra_is_empty")]
    pub extra: Extra,
}

/// How a host's remote desktop is shown and what it may use here. Every field
/// has a default, so a host an older build wrote — or a field a newer one
/// added — never fails a record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RdpSettings {
    /// `fit` (the desktop follows the tab's size), `fixed` (`width` ×
    /// `height`) or `fullscreen`. A string, for the same reason as
    /// `workspace`: an unknown mode shows as `fit` instead of failing.
    pub display: String,
    pub width: u16,
    pub height: u16,
    /// Scale a desktop that does not fit into the tab instead of scrolling it.
    pub smart_sizing: bool,
    pub color_depth: u32,
    /// `local` (play it here), `remote` (leave it on the server) or `off`.
    pub audio: String,
    pub clipboard: bool,
    /// The console session, like `mstsc /admin`.
    pub admin: bool,
    /// Network Level Authentication (CredSSP). Off only for old servers.
    pub nla: bool,
    pub wallpaper: bool,
    /// Offer the graphics pipeline (RDPEGFX). Off only for a server that
    /// draws wrongly with it; the old bitmap path is much slower on current
    /// Windows.
    pub graphics_pipeline: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway: Option<GatewaySettings>,
    #[serde(flatten, skip_serializing_if = "extra_is_empty")]
    pub extra: Extra,
}

impl Default for RdpSettings {
    fn default() -> Self {
        Self {
            display: "fit".into(),
            width: 1920,
            height: 1080,
            smart_sizing: true,
            color_depth: 32,
            audio: "local".into(),
            clipboard: true,
            admin: false,
            nla: true,
            wallpaper: true,
            graphics_pipeline: true,
            gateway: None,
            extra: Extra::new(),
        }
    }
}

/// A Remote Desktop Gateway in front of the host.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GatewaySettings {
    pub address: String,
    pub port: u16,
    /// Log in to the gateway with the host's own login rather than
    /// `gateway_identity_id`.
    pub use_host_login: bool,
    /// Skip the gateway for addresses on the local network.
    pub bypass_local: bool,
    #[serde(flatten, skip_serializing_if = "extra_is_empty")]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupPayload {
    pub workspace: String,
    pub name: String,
    pub position: i64,
    /// The login every host in the group uses unless it has its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_id: Option<Uuid>,
    #[serde(flatten, default, skip_serializing_if = "extra_is_empty")]
    pub extra: Extra,
}

/// Username plus how to authenticate. Kept separate from [`HostPayload`] on
/// purpose: one login serves forty hosts — a group's does exactly that — and
/// changing its password is one edit rather than forty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityPayload {
    pub label: String,
    pub username: String,
    /// The Windows domain, empty for a local account.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub domain: String,
    /// `password`, `key`, `agent`, `keyboard-interactive` or `cert`.
    pub auth_type: String,
    pub key_id: Option<Uuid>,
    /// Points at a [`EntityKind::Secret`] record. Never the password itself.
    pub password_secret_id: Option<Uuid>,
    #[serde(flatten, default, skip_serializing_if = "extra_is_empty")]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyPayload {
    pub label: String,
    /// `ssh-ed25519`, `ssh-rsa`, … or what an importer called it.
    pub key_type: String,
    /// The public half, in the clear: a host form shows which key it uses
    /// without unlocking anything.
    pub public_key: String,
    pub private_secret_id: Option<Uuid>,
    pub passphrase_secret_id: Option<Uuid>,
    #[serde(flatten, default, skip_serializing_if = "extra_is_empty")]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnippetPayload {
    pub label: String,
    pub body: String,
    pub group_path: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "extra_is_empty")]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownHostPayload {
    pub address: String,
    pub port: u16,
    pub algorithm: String,
    /// `SHA256:…`, as `ssh-keygen -lf` prints it.
    pub fingerprint_sha256: String,
    pub public_key: String,
    pub first_seen_ms: u64,
    #[serde(flatten, default, skip_serializing_if = "extra_is_empty")]
    pub extra: Extra,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_field_an_older_build_does_not_know_survives_a_round_trip() {
        // What a newer build wrote: a host with tags.
        let written = r#"{"name":"prox-1","address":"10.0.0.12","port":22,
            "workspace":"private","position":0,"group_id":null,"identity_id":null,
            "tags":["homelab"]}"#;

        let host: HostPayload = serde_json::from_str(written).expect("parse");
        assert_eq!(host.name, "prox-1");
        assert!(host.extra.contains_key("tags"));

        // The older build edits the name and writes it back.
        let edited = HostPayload {
            name: "prox-one".into(),
            ..host
        };
        let json = serde_json::to_value(&edited).expect("serialise");
        assert_eq!(json["name"], "prox-one");
        assert_eq!(json["tags"][0], "homelab", "the tags must still be there");
    }

    #[test]
    fn nothing_extra_shows_up_when_there_is_nothing_extra() {
        let json = serde_json::to_string(&GroupPayload {
            workspace: "private".into(),
            name: "Homelab".into(),
            position: 0,
            identity_id: None,
            extra: Extra::new(),
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"workspace":"private","name":"Homelab","position":0}"#
        );
    }

    #[test]
    fn a_host_without_rdp_settings_gets_the_defaults() {
        let written = r#"{"name":"dc-1","address":"10.0.0.5","port":3389,
            "workspace":"business","position":0,"group_id":null,"identity_id":null}"#;
        let host: HostPayload = serde_json::from_str(written).expect("parse");
        assert_eq!(host.rdp, RdpSettings::default());
        assert!(
            host.extra.is_empty(),
            "no rdp object is not an unknown field"
        );
    }

    #[test]
    fn an_rdp_setting_a_newer_build_added_survives_an_edit() {
        let written = r#"{"display":"fixed","width":1280,"height":720,"multimon":true}"#;
        let rdp: RdpSettings = serde_json::from_str(written).expect("parse");
        assert_eq!(rdp.display, "fixed");
        assert_eq!(rdp.width, 1280);
        assert!(rdp.clipboard, "missing fields take their defaults");
        let json = serde_json::to_value(&rdp).expect("serialise");
        assert_eq!(json["multimon"], true);
    }

    #[test]
    fn records_are_applied_before_the_records_that_point_at_them() {
        assert!(EntityKind::Secret.apply_rank() < EntityKind::Key.apply_rank());
        assert!(EntityKind::Key.apply_rank() < EntityKind::Identity.apply_rank());
        assert!(EntityKind::Identity.apply_rank() < EntityKind::Host.apply_rank());
        assert!(EntityKind::Group.apply_rank() < EntityKind::Host.apply_rank());
        assert_eq!(
            EntityKind::PortForward.apply_rank(),
            EntityKind::APPLY_ORDER.len(),
            "a kind with no table yet sorts last"
        );
    }

    #[test]
    fn the_discriminants_never_move() {
        // They go into the associated data of every sealed record, so a
        // reordering here would make existing vaults unreadable.
        assert_eq!(EntityKind::Host as u8, 0);
        assert_eq!(EntityKind::Group as u8, 1);
        assert_eq!(EntityKind::Identity as u8, 2);
        assert_eq!(EntityKind::Key as u8, 3);
        assert_eq!(EntityKind::Snippet as u8, 4);
        assert_eq!(EntityKind::PortForward as u8, 5);
        assert_eq!(EntityKind::KnownHost as u8, 6);
        assert_eq!(EntityKind::TerminalProfile as u8, 7);
        assert_eq!(EntityKind::Secret as u8, 8);
        assert_eq!(EntityKind::Manifest as u8, 9);
        for (index, kind) in EntityKind::ALL.iter().enumerate() {
            assert_eq!(*kind as usize, index, "ALL is in discriminant order");
            assert_eq!(EntityKind::from_discriminant(index as u8), Some(*kind));
        }
        assert_eq!(EntityKind::from_discriminant(10), None);
    }

    #[test]
    fn a_manifest_is_applied_after_everything_it_lists() {
        for kind in EntityKind::APPLY_ORDER {
            assert!(kind.apply_rank() < EntityKind::Manifest.apply_rank());
        }
        assert_eq!(
            serde_json::to_string(&EntityKind::Manifest).unwrap(),
            r#""manifest""#,
            "the name the server stores it under"
        );
    }
}
