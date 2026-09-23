//! The host list, connecting to a host, and deciding about certificates.
//!
//! Connecting is a conversation rather than a call. Most errors from
//! [`connect_host`] are the next question for the user — trust this
//! certificate? which login? — and the webview asks it and calls again with
//! the answer. Secrets only ever travel inbound, for the single call that uses
//! them; a password from the vault is revealed here, in Rust, and never goes
//! to the page.

// A connection attempt fails at most once per click; a large error is not a
// cost worth boxing the engine's certificate details for.
#![allow(clippy::result_large_err)]

use crate::{err, AppState, CommandResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;
use uuid::Uuid;
use uwurdp_core::{
    AudioMode, GatewayTarget, ObservedCertificate, RdpError, RdpTarget, SessionId, SessionSettings,
};
use uwurdp_store::{
    GroupRecord, HostDraft, HostRecord, PasswordChange, ResolvedLogin, SecretText, StoreError,
    Workspace,
};
use zeroize::Zeroizing;

#[tauri::command]
pub(crate) fn list_hosts(state: State<'_, AppState>) -> CommandResult<Vec<HostRecord>> {
    state.store.list_hosts().map_err(err)
}

/// Validation failures carry the field, so the form can mark the right input.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum SaveFailure {
    Invalid {
        field: &'static str,
        problem: &'static str,
    },
    /// Storing a password needs the vault open (or created) first.
    VaultLocked,
    Error {
        message: String,
    },
}

impl From<StoreError> for SaveFailure {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Invalid { field, problem } => Self::Invalid { field, problem },
            StoreError::VaultLocked => Self::VaultLocked,
            other => Self::Error {
                message: other.to_string(),
            },
        }
    }
}

#[tauri::command]
pub(crate) fn save_host(
    state: State<'_, AppState>,
    draft: HostDraft,
) -> Result<HostRecord, SaveFailure> {
    Ok(state.store.save_host(draft)?)
}

#[tauri::command]
pub(crate) fn delete_host(state: State<'_, AppState>, id: Uuid) -> CommandResult<()> {
    state.store.delete_host(id).map_err(err)
}

/// Keep a login typed into the connect dialog as the host's own. `None` keeps
/// the password that is stored (or none).
#[tauri::command]
pub(crate) fn set_host_login(
    state: State<'_, AppState>,
    id: Uuid,
    username: String,
    domain: String,
    password: Option<String>,
) -> Result<HostRecord, SaveFailure> {
    let password = password.filter(|p| !p.is_empty()).map(SecretText::new);
    Ok(state
        .store
        .set_host_login(id, &username, &domain, password)?)
}

// ── Groups and order ────────────────────────────────────────────────────────

#[tauri::command]
pub(crate) fn list_groups(state: State<'_, AppState>) -> CommandResult<Vec<GroupRecord>> {
    state.store.list_groups().map_err(err)
}

#[tauri::command]
pub(crate) fn create_group(
    state: State<'_, AppState>,
    workspace: Workspace,
    name: String,
) -> Result<GroupRecord, SaveFailure> {
    Ok(state.store.create_group(workspace, &name)?)
}

#[tauri::command]
pub(crate) fn rename_group(
    state: State<'_, AppState>,
    workspace: Workspace,
    from: String,
    to: String,
) -> Result<(), SaveFailure> {
    Ok(state.store.rename_group(workspace, &from, &to)?)
}

#[tauri::command]
pub(crate) fn delete_group(
    state: State<'_, AppState>,
    workspace: Workspace,
    name: String,
) -> CommandResult<()> {
    state.store.delete_group(workspace, &name).map_err(err)
}

/// The login a group hands to its hosts; an empty username removes it.
#[tauri::command]
pub(crate) fn set_group_login(
    state: State<'_, AppState>,
    workspace: Workspace,
    name: String,
    username: String,
    domain: String,
    password: PasswordChange,
) -> Result<(), SaveFailure> {
    Ok(state
        .store
        .set_group_login(workspace, &name, &username, &domain, &password)?)
}

#[tauri::command]
pub(crate) fn move_group(
    state: State<'_, AppState>,
    workspace: Workspace,
    name: String,
    to: Workspace,
    before: Option<String>,
) -> CommandResult<()> {
    state
        .store
        .move_group(workspace, &name, to, before.as_deref())
        .map_err(err)
}

#[tauri::command]
pub(crate) fn move_host(
    state: State<'_, AppState>,
    id: Uuid,
    to: Workspace,
    group: Option<String>,
    before: Option<Uuid>,
) -> CommandResult<HostRecord> {
    state
        .store
        .move_host(id, to, group.as_deref(), before)
        .map_err(err)
}

// ── Connecting ──────────────────────────────────────────────────────────────

/// Everything that can come back from a connection attempt. The engine's
/// errors keep their own `kind` tag; the rest are questions or `internal`.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum ConnectFailure {
    Rdp(RdpError),
    /// No login, or no password for it: the page asks, prefilled.
    LoginRequired {
        kind: &'static str,
        username: String,
        domain: String,
        #[serde(rename = "fromGroup")]
        from_group: bool,
    },
    GatewayLoginRequired {
        kind: &'static str,
        username: String,
        domain: String,
    },
    /// The login lives in the vault, and the vault is locked. The page asks
    /// for the master password and tries again.
    VaultLocked {
        kind: &'static str,
    },
    Internal {
        kind: &'static str,
        message: String,
    },
}

pub(crate) fn internal(message: impl std::fmt::Display) -> ConnectFailure {
    ConnectFailure::Internal {
        kind: "internal",
        message: message.to_string(),
    }
}

/// A locked vault is a question for the user; any other failure while revealing
/// a secret is internal.
fn from_vault<T>(result: Result<T, StoreError>) -> Result<T, ConnectFailure> {
    result.map_err(|error| match error {
        StoreError::VaultLocked => ConnectFailure::VaultLocked {
            kind: "vault-locked",
        },
        other => internal(other),
    })
}

/// How a connection attempt is named: the tab it belongs to. Short and plain,
/// since it only keys a map.
fn check_attempt(attempt: &str) -> Result<(), ConnectFailure> {
    let plain = !attempt.is_empty()
        && attempt.len() <= 64
        && attempt
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if plain {
        Ok(())
    } else {
        Err(internal("invalid connection attempt"))
    }
}

/// How long a certificate the server presented can be trusted from the
/// dialog. Long enough to compare thumbprints calmly, short enough that a
/// declined one doesn't stay trustable.
const PRESENTED_TTL: std::time::Duration = std::time::Duration::from_secs(10 * 60);

fn key_for(address: &str, port: u16) -> (String, u16) {
    (address.trim().to_ascii_lowercase(), port)
}

/// A login typed into the connect dialog, for this one attempt.
#[derive(Deserialize)]
pub(crate) struct TypedLogin {
    username: String,
    domain: String,
    password: String,
}

struct Login {
    username: String,
    domain: Option<String>,
    password: Zeroizing<String>,
}

impl From<TypedLogin> for Login {
    fn from(typed: TypedLogin) -> Self {
        let password = Zeroizing::new(typed.password);
        Self {
            username: typed.username.trim().to_string(),
            domain: Some(typed.domain.trim().to_string()).filter(|d| !d.is_empty()),
            password,
        }
    }
}

/// A stored login, with its password revealed — or the question to ask.
fn stored_login(
    state: &AppState,
    login: Option<ResolvedLogin>,
    need_password: bool,
) -> Result<Login, ConnectFailure> {
    let Some(login) = login else {
        return if need_password {
            Err(ConnectFailure::LoginRequired {
                kind: "login-required",
                username: String::new(),
                domain: String::new(),
                from_group: false,
            })
        } else {
            Ok(Login {
                username: String::new(),
                domain: None,
                password: Zeroizing::new(String::new()),
            })
        };
    };
    let password = if login.has_password {
        from_vault(state.store.reveal_login_password(&login))?
    } else {
        None
    };
    let password = match password {
        Some(bytes) => Zeroizing::new(
            String::from_utf8(bytes.to_vec())
                .map_err(|_| internal("a stored password is not text"))?,
        ),
        // A login without a password: ask for it, the rest prefilled. Without
        // NLA the server's own logon screen asks instead.
        None if need_password => {
            return Err(ConnectFailure::LoginRequired {
                kind: "login-required",
                username: login.username,
                domain: login.domain,
                from_group: login.from_group,
            })
        }
        None => Zeroizing::new(String::new()),
    };
    Ok(Login {
        domain: Some(login.domain).filter(|d| !d.is_empty()),
        username: login.username,
        password,
    })
}

/// Whether an address is on the local network, for "bypass the gateway for
/// local addresses": a private or link-local IP, or a name without a dot.
fn is_local(address: &str) -> bool {
    match address.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00,
        Err(_) => !address.contains('.') || address.to_ascii_lowercase().ends_with(".local"),
    }
}

/// Whether anything answers at `address:port` at all. Asked before the login
/// dialog, so a host that is off doesn't first ask for a password it then
/// can't use.
async fn reachable(address: &str, port: u16) -> Result<(), ConnectFailure> {
    let probe = tokio::net::TcpStream::connect((address, port));
    match tokio::time::timeout(std::time::Duration::from_secs(5), probe).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(ConnectFailure::Rdp(RdpError::Unreachable {
            message: error.to_string(),
        })),
        Err(_) => Err(ConnectFailure::Rdp(RdpError::Timeout)),
    }
}

/// The name this computer shows on the server, at most the 15 characters RDP
/// carries.
fn client_name() -> String {
    let name = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "UwURDP".into());
    name.chars().filter(|c| !c.is_control()).take(15).collect()
}

// A Tauri command takes its arguments flat, as the page sends them.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub(crate) async fn connect_host(
    state: State<'_, AppState>,
    codec: State<'_, Arc<crate::h264::Codec>>,
    id: Uuid,
    attempt: String,
    width: u16,
    height: u16,
    scale: u32,
    login: Option<TypedLogin>,
    gateway_login: Option<TypedLogin>,
) -> Result<SessionId, ConnectFailure> {
    check_attempt(&attempt)?;
    // The page opened the socket first; without it frames had nowhere to go.
    let socket = state
        .frames
        .take(&attempt)
        .ok_or_else(|| internal("the frame socket is not open"))?;
    let host = state
        .store
        .get_host(id)
        .map_err(internal)?
        .ok_or_else(|| internal("this host no longer exists"))?;
    let rdp = &host.rdp;

    let login = match login {
        Some(typed) => Login::from(typed),
        None => {
            let stored = stored_login(
                &state,
                state.store.host_login(id).map_err(internal)?,
                rdp.nla,
            );
            if let Err(ConnectFailure::LoginRequired { .. }) = &stored {
                if rdp.gateway.is_none() {
                    reachable(&host.address, host.port).await?;
                }
            }
            stored?
        }
    };

    let gateway = match &rdp.gateway {
        Some(gateway) if !(gateway.bypass_local && is_local(&host.address)) => {
            let gateway_login = if gateway.use_host_login {
                Login {
                    username: login.username.clone(),
                    domain: login.domain.clone(),
                    password: login.password.clone(),
                }
            } else {
                match gateway_login {
                    Some(typed) => Login::from(typed),
                    None => {
                        let stored = state.store.host_gateway_login(id).map_err(internal)?;
                        stored_login(&state, stored, true).map_err(|failure| match failure {
                            ConnectFailure::LoginRequired {
                                username, domain, ..
                            } => ConnectFailure::GatewayLoginRequired {
                                kind: "gateway-login-required",
                                username,
                                domain,
                            },
                            other => other,
                        })?
                    }
                }
            };
            Some(GatewayTarget {
                address: gateway.address.clone(),
                port: gateway.port,
                username: gateway_login.username,
                domain: gateway_login.domain,
                password: gateway_login.password,
            })
        }
        _ => None,
    };

    let trusted_fingerprint = state
        .store
        .known_host(&host.address, host.port)
        .map_err(internal)?
        .map(|known| known.fingerprint);

    let target = RdpTarget {
        address: host.address.clone(),
        port: host.port,
        username: login.username,
        domain: login.domain,
        password: login.password,
        trusted_fingerprint,
        settings: SessionSettings {
            width: width.clamp(200, 8192),
            height: height.clamp(200, 8192),
            scale_factor: scale.clamp(100, 500),
            color_depth: rdp.color_depth,
            audio: match rdp.audio.as_str() {
                "remote" => AudioMode::Remote,
                "off" => AudioMode::Off,
                _ => AudioMode::Local,
            },
            clipboard: rdp.clipboard,
            admin: rdp.admin,
            nla: rdp.nla,
            wallpaper: rdp.wallpaper,
            animations: false,
            font_smoothing: true,
            keyboard_layout: 0,
            client_name: client_name(),
            graphics_pipeline: rdp.graphics_pipeline,
            h264_library: rdp.graphics_pipeline.then(|| codec.library()).flatten(),
        },
        gateway,
    };

    match state.sessions.connect(&attempt, target, socket.sink).await {
        Ok(session) => {
            socket.link.bind(&state.sessions, session);
            if let Err(error) = state.store.mark_connected(host.id) {
                tracing::warn!(%error, "could not record the connection time");
            }
            state.session_hosts.lock().insert(session, host.id);
            Ok(session)
        }
        Err(error) => {
            if let RdpError::UnknownCertificate { observed }
            | RdpError::CertificateChanged { observed, .. } = &error
            {
                state.presented.lock().insert(
                    key_for(&host.address, host.port),
                    (observed.clone(), std::time::Instant::now()),
                );
            }
            Err(ConnectFailure::Rdp(error))
        }
    }
}

/// Trust the certificate the server at `address:port` just presented.
///
/// Replacing one that was already trusted additionally needs `replace`, the
/// user's explicit "accept the new certificate" from the warning — never a
/// side effect of trusting a first one.
#[tauri::command]
pub(crate) fn trust_certificate(
    state: State<'_, AppState>,
    address: String,
    port: u16,
    fingerprint: String,
    replace: bool,
) -> CommandResult<()> {
    let slot = key_for(&address, port);
    let presented: ObservedCertificate = state
        .presented
        .lock()
        .get(&slot)
        .filter(|(_, seen)| seen.elapsed() < PRESENTED_TTL)
        .map(|(observed, _)| observed.clone())
        .filter(|observed| observed.fingerprint == fingerprint)
        .ok_or("this certificate was not presented by the server in the last connection attempt")?;

    let replacing = state
        .store
        .known_host(&address, port)
        .map_err(err)?
        .is_some_and(|known| known.fingerprint != presented.fingerprint);
    if replacing && !replace {
        return Err("replacing a trusted certificate needs the user's confirmation".into());
    }

    state
        .store
        .trust_host_key(
            &address,
            port,
            "x509",
            &presented.fingerprint,
            &presented.der_base64,
        )
        .map_err(err)?;
    state.presented.lock().remove(&slot);
    Ok(())
}

/// The user closed a dialog instead of answering it, or closed the tab: stop
/// the attempt that was running.
// Async on purpose: cancelling may touch the runtime, and sync commands run on
// the main thread, outside it.
#[tauri::command]
pub(crate) async fn cancel_connect(state: State<'_, AppState>, attempt: String) -> Result<(), ()> {
    state.sessions.cancel(&attempt);
    Ok(())
}

/// Forget what belonged to a session that closed.
pub(crate) fn forget_session(state: &AppState, id: SessionId) {
    state.session_hosts.lock().remove(&id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_addresses_are_told_from_the_internet() {
        assert!(is_local("10.0.0.5"));
        assert!(is_local("192.168.1.20"));
        assert!(is_local("dc01"));
        assert!(is_local("nas.local"));
        assert!(!is_local("8.8.8.8"));
        assert!(!is_local("rdp.example.com"));
    }

    #[test]
    fn an_attempt_name_is_plain() {
        assert!(check_attempt("tab-abc-1").is_ok());
        assert!(check_attempt("").is_err());
        assert!(check_attempt("tab/../x").is_err());
    }
}
