//! Getting from an address to an active RDP session.
//!
//! The order matters for security, so here it is spelled out:
//!
//! 1. TCP connect (with [`CONNECT_TIMEOUT`]).
//! 2. X.224 negotiation: we offer CredSSP only (`nla`) or TLS only.
//! 3. TLS handshake (rustls/ring), then the certificate pin check. An unknown
//!    or changed certificate ends the connection *here*: the only thing the
//!    server has seen so far is the user name in the X.224 routing cookie.
//! 4. CredSSP (NTLM) when NLA was negotiated.
//! 5. The rest of the connection sequence (capabilities, licensing, ...).
//!
//! We drive CredSSP ourselves instead of calling `ironrdp_tokio::connect_finalize`
//! for two reasons: knowing that an error happened *during authentication* is
//! what lets us report [`RdpError::AuthFailed`] reliably, and we only speak
//! NTLM — IronRDP's Kerberos path needs an HTTP client for KDC proxies that
//! would pull reqwest and a second TLS stack into the app. NTLM works against
//! every Windows host that allows it (the default, including domain members
//! reached by IP).

use crate::clipboard::TextClipboardBackend;
use crate::config::{AudioMode, RdpTarget, SessionSettings};
use crate::error::RdpError;
use crate::tls;
use ironrdp_connector::credssp::{CredsspProcessGenerator, CredsspSequence};
use ironrdp_connector::sspi::credssp::ClientState;
use ironrdp_connector::sspi::generator::GeneratorState;
use ironrdp_connector::{
    general_err, BitmapConfig, ClientConnector, ClientConnectorState, ConnectionResult,
    ConnectorError, ConnectorErrorKind, ConnectorResult, Credentials, DesktopSize, ServerName,
};
use ironrdp_core::WriteBuf;
use ironrdp_displaycontrol::client::DisplayControlClient;
use ironrdp_dvc::DrdynvcClient;
use ironrdp_pdu::gcc::KeyboardType;
use ironrdp_pdu::nego::FailureCode;
use ironrdp_pdu::rdp::capability_sets::{
    client_codecs_capabilities, BitmapCodecs, CodecProperty, MajorPlatformType,
};
use ironrdp_pdu::rdp::client_info::{PerformanceFlags, TimezoneInfo};
use ironrdp_tokio::{FramedRead, FramedWrite, TokioFramed};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tracing::{debug, info};
use zeroize::Zeroize as _;

/// DNS plus TCP connect.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Everything after TCP: negotiation, TLS, CredSSP, capability exchange.
/// Generous, because a Windows host that just woke up can take a while to
/// produce its licensing PDUs.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// The stream an active session runs on.
pub(crate) type Framed = TokioFramed<Stream>;
pub(crate) type Stream = TlsStream<TcpStream>;

pub(crate) struct Established {
    pub result: ConnectionResult,
    pub framed: Framed,
}

/// Optional channels built by the caller (they need the session's channels).
#[derive(Default)]
pub(crate) struct Channels {
    pub clipboard: Option<TextClipboardBackend>,
}

pub(crate) async fn establish(
    target: &RdpTarget,
    channels: Channels,
) -> Result<Established, RdpError> {
    if target.gateway.is_some() {
        // See the crate docs for why ironrdp-mstsgu 0.0.1 is not used.
        return Err(RdpError::Gateway {
            message: "RD Gateway is not supported yet".into(),
        });
    }

    let tcp = tokio::time::timeout(
        CONNECT_TIMEOUT,
        TcpStream::connect((target.address.as_str(), target.port)),
    )
    .await
    .map_err(|_| RdpError::Timeout)?
    .map_err(|e| RdpError::Unreachable {
        message: format!("{}:{}: {e}", target.address, target.port),
    })?;
    // Input latency matters more than packet count.
    let _ = tcp.set_nodelay(true);
    let client_addr = tcp.local_addr().map_err(|e| RdpError::Unreachable {
        message: e.to_string(),
    })?;

    let connector = build_connector(target, client_addr, channels);
    tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(tcp, connector, target))
        .await
        .map_err(|_| RdpError::Timeout)?
}

fn build_connector(
    target: &RdpTarget,
    client_addr: std::net::SocketAddr,
    channels: Channels,
) -> ClientConnector {
    let (username, domain) = split_username(&target.username, target.domain.as_deref());
    let config = connector_config(&target.settings, username, domain, target.password.as_str());

    let drdynvc =
        DrdynvcClient::new().with_dynamic_channel(DisplayControlClient::new(|_| Ok(Vec::new())));
    let mut connector = ClientConnector::new(config, client_addr).with_static_channel(drdynvc);

    if let Some(backend) = channels.clipboard {
        connector.attach_static_channel(ironrdp_cliprdr::CliprdrClient::new(Box::new(backend)));
    }

    #[cfg(feature = "audio")]
    if target.settings.audio == AudioMode::Local {
        connector.attach_static_channel(ironrdp_rdpsnd::client::Rdpsnd::new(Box::new(
            ironrdp_rdpsnd_native::cpal::RdpsndBackend::new(),
        )));
    }

    connector
}

/// `DOMAIN\user` in the user field is split up when no separate domain was
/// given, since that is how people type it. `user@domain` (UPN) is passed on
/// as it is; NTLM understands it.
pub(crate) fn split_username(username: &str, domain: Option<&str>) -> (String, Option<String>) {
    let domain = domain.map(str::trim).filter(|d| !d.is_empty());
    if domain.is_none() {
        if let Some((d, u)) = username.split_once('\\') {
            if !d.is_empty() && !u.is_empty() {
                return (u.to_owned(), Some(d.to_owned()));
            }
        }
    }
    (username.to_owned(), domain.map(str::to_owned))
}

pub(crate) fn performance_flags(settings: &SessionSettings) -> PerformanceFlags {
    let mut flags = PerformanceFlags::empty();
    if !settings.wallpaper {
        flags |= PerformanceFlags::DISABLE_WALLPAPER;
    }
    if settings.animations {
        flags |= PerformanceFlags::ENABLE_DESKTOP_COMPOSITION;
    } else {
        flags |=
            PerformanceFlags::DISABLE_FULLWINDOWDRAG | PerformanceFlags::DISABLE_MENUANIMATIONS;
    }
    if settings.font_smoothing {
        flags |= PerformanceFlags::ENABLE_FONT_SMOOTHING;
    }
    flags
}

/// Clamps a requested desktop size to what servers accept.
pub(crate) fn clamp_desktop(width: u16, height: u16) -> (u16, u16) {
    (width.clamp(200, 8192), height.clamp(200, 8192))
}

pub(crate) fn connector_config(
    settings: &SessionSettings,
    username: String,
    domain: Option<String>,
    password: &str,
) -> ironrdp_connector::Config {
    let color_depth = match settings.color_depth {
        15 | 16 | 24 | 32 => settings.color_depth,
        _ => 32,
    };
    let (width, height) = clamp_desktop(settings.width, settings.height);
    // MS-RDPBCGR: the scale factor is ignored outside 100..=500.
    let scale_factor = if (100..=500).contains(&settings.scale_factor) {
        settings.scale_factor
    } else {
        0
    };
    let platform = if cfg!(windows) {
        MajorPlatformType::WINDOWS
    } else if cfg!(target_os = "macos") {
        MajorPlatformType::MACINTOSH
    } else {
        MajorPlatformType::UNIX
    };

    ironrdp_connector::Config {
        desktop_size: DesktopSize { width, height },
        desktop_scale_factor: scale_factor,
        // NLA on means NLA only: offering plain TLS as a fallback would let
        // anyone in the middle downgrade us and collect the password from
        // the logon packet.
        enable_tls: !settings.nla,
        enable_credssp: settings.nla,
        credentials: Credentials::UsernamePassword {
            username,
            password: password.to_owned(),
        },
        domain,
        client_build: 0,
        client_name: settings.client_name.clone(),
        keyboard_type: KeyboardType::IbmEnhanced,
        keyboard_subtype: 0,
        keyboard_functional_keys_count: 12,
        keyboard_layout: settings.keyboard_layout,
        ime_file_name: String::new(),
        bitmap: Some(BitmapConfig {
            lossy_compression: true,
            color_depth,
            codecs: bitmap_codecs(),
        }),
        dig_product_id: String::new(),
        client_dir: String::from("C:\\Windows\\System32\\mstscax.dll"),
        alternate_shell: String::new(),
        work_dir: String::new(),
        platform,
        hardware_id: None,
        request_data: None,
        // With TLS only, the logon packet carries the password; ask the
        // server to use it instead of showing its own logon screen.
        autologon: !settings.nla && !password.is_empty(),
        enable_audio_playback: settings.audio == AudioMode::Local && cfg!(feature = "audio"),
        performance_flags: performance_flags(settings),
        license_cache: None,
        timezone_info: TimezoneInfo::default(),
        // No bulk compression: IronRDP cannot carry the decompressor across
        // a deactivation-reactivation (every resize), and a fresh one would
        // desynchronize from the server's history.
        compression_type: None,
        enable_server_pointer: true,
        // Pointer bitmaps come out straight (not premultiplied) RGBA and
        // the page draws the cursor itself.
        pointer_software_rendering: false,
        multitransport_flags: None,
    }
}

/// RemoteFX only. The defaults of `client_codecs_capabilities` also list
/// QOI/QOIZ whenever *any* crate in the build turns on ironrdp-pdu's `qoi`
/// feature (ironrdp-server does), while decoding them needs the same
/// feature on ironrdp-session — advertising a codec we cannot decode would
/// leave the screen black. Windows servers only speak RemoteFX anyway.
pub(crate) fn bitmap_codecs() -> BitmapCodecs {
    let mut codecs = client_codecs_capabilities(&[]).unwrap_or_default();
    codecs
        .0
        .retain(|codec| matches!(codec.property, CodecProperty::RemoteFx(_)));
    codecs
}

/// Which step failed, for error reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Negotiation,
    Authentication,
    Finalization,
}

async fn handshake(
    tcp: TcpStream,
    mut connector: ClientConnector,
    target: &RdpTarget,
) -> Result<Established, RdpError> {
    let mut framed = TokioFramed::new(tcp);

    let should_upgrade = ironrdp_tokio::connect_begin(&mut framed, &mut connector)
        .await
        .map_err(|e| map_connector_error(&e, Phase::Negotiation, &target.settings))?;

    let (tcp, leftover) = framed.into_inner();
    let (tls_stream, der) =
        tls::upgrade(tcp, &target.address)
            .await
            .map_err(|e| RdpError::Protocol {
                message: format!("TLS handshake failed: {e}"),
            })?;

    let certificate = tls::describe_certificate(&der)?;
    // The pin check. Returning here drops the stream: no credential has
    // been sent, and none will be.
    tls::check_trust(&certificate.observed, target.trusted_fingerprint.as_deref())?;
    debug!(fingerprint = %certificate.observed.fingerprint, "server certificate trusted");

    let _upgraded = ironrdp_tokio::mark_as_upgraded(should_upgrade, &mut connector);
    let mut framed = TokioFramed::new_with_leftover(tls_stream, leftover);

    if connector.should_perform_credssp() {
        let server_name = ServerName::new(target.address.clone());
        credssp(
            &mut connector,
            &mut framed,
            server_name,
            certificate.public_key,
        )
        .await
        .map_err(|e| map_connector_error(&e, Phase::Authentication, &target.settings))?;
    }

    let mut buf = WriteBuf::new();
    let result = loop {
        if let Err(e) =
            ironrdp_tokio::single_sequence_step(&mut framed, &mut connector, &mut buf).await
        {
            forget_password(&mut connector);
            return Err(map_connector_error(
                &e,
                Phase::Finalization,
                &target.settings,
            ));
        }
        if matches!(connector.state, ClientConnectorState::Connected { .. }) {
            forget_password(&mut connector);
            if let ClientConnectorState::Connected { result } = std::mem::take(&mut connector.state)
            {
                break result;
            }
        }
    };
    info!(
        width = result.desktop_size.width,
        height = result.desktop_size.height,
        "RDP session active"
    );
    Ok(Established { result, framed })
}

/// The connector keeps a plain `String` copy of the password in its config;
/// wipe it as soon as nothing needs it any more.
fn forget_password(connector: &mut ClientConnector) {
    if let Credentials::UsernamePassword { password, .. } = &mut connector.config.credentials {
        password.zeroize();
    }
}

/// CredSSP, NTLM only. Mirrors `ironrdp_async::connect_finalize`'s CredSSP
/// step, minus the network client for Kerberos.
async fn credssp<S>(
    connector: &mut ClientConnector,
    framed: &mut ironrdp_tokio::Framed<S>,
    server_name: ServerName,
    server_public_key: Vec<u8>,
) -> ConnectorResult<()>
where
    S: FramedRead + FramedWrite,
{
    let selected_protocol = match connector.state {
        ClientConnectorState::Credssp {
            selected_protocol, ..
        } => selected_protocol,
        _ => return Err(general_err!("invalid connector state for CredSSP")),
    };

    let (mut sequence, mut ts_request) = CredsspSequence::init(
        connector.config.credentials.clone(),
        connector.config.domain.as_deref(),
        selected_protocol,
        server_name,
        server_public_key,
        None,
    )?;

    let mut buf = WriteBuf::new();
    loop {
        let client_state = {
            let mut generator = sequence.process_ts_request(ts_request);
            resolve_without_network(&mut generator)?
        };

        buf.clear();
        let written = sequence.handle_process_result(client_state, &mut buf)?;
        if let Some(len) = written.size() {
            framed
                .write_all(&buf[..len])
                .await
                .map_err(|e| ironrdp_connector::custom_err!("write CredSSP message", e))?;
        }

        let Some(hint) = sequence.next_pdu_hint() else {
            break;
        };
        let pdu = framed
            .read_by_hint(hint)
            .await
            .map_err(|e| ironrdp_connector::custom_err!("read CredSSP message", e))?;
        match sequence.decode_server_message(&pdu)? {
            Some(next) => ts_request = next,
            None => break,
        }
    }

    connector.mark_credssp_as_done();
    Ok(())
}

fn resolve_without_network(
    generator: &mut CredsspProcessGenerator<'_>,
) -> ConnectorResult<ClientState> {
    match generator.start() {
        GeneratorState::Completed(state) => {
            state.map_err(|e| ConnectorError::new("CredSSP", ConnectorErrorKind::Credssp(e)))
        }
        // Only Kerberos talks to the network, and we never configure it.
        GeneratorState::Suspended(_) => Err(general_err!(
            "the server asked for Kerberos, which is not supported"
        )),
    }
}

/// Plain-English text for an NTSTATUS a CredSSP server reports.
fn nstatus_message(code: u32) -> Option<&'static str> {
    Some(match code {
        0xC000_006D => "Wrong user name or password.",
        0xC000_006E => "This account is restricted and may not log on from here.",
        0xC000_006F => "This account may not log on at this time of day.",
        0xC000_0070 => "This account may not log on from this computer.",
        0xC000_0071 | 0xC000_0224 => "The password has expired and must be changed.",
        0xC000_0072 => "This account is disabled.",
        0xC000_0193 => "This account has expired.",
        0xC000_0234 => "This account is locked out.",
        0xC000_015B => "This account is not allowed to log on remotely.",
        0xC000_0064 => "No such user.",
        _ => return None,
    })
}

/// Turns IronRDP's error chain into a message without the source locations
/// it embeds (`[context @ file.rs:12] kind`).
fn describe(error: &ConnectorError) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut push = |text: String| {
        let text = strip_location(&text);
        if !text.is_empty() && !parts.iter().any(|p| p == &text) {
            parts.push(text);
        }
    };
    push(error.to_string());
    let mut source = std::error::Error::source(error);
    while let Some(e) = source {
        push(e.to_string());
        source = e.source();
    }
    parts.join(": ")
}

fn strip_location(text: &str) -> String {
    if let Some(rest) = text.strip_prefix('[') {
        if let Some((inner, kind)) = rest.split_once("] ") {
            let context = inner.split(" @ ").next().unwrap_or("").trim();
            let kind = kind.trim();
            return match kind {
                "custom error" | "general error" | "" => context.to_owned(),
                _ if context.is_empty() => kind.to_owned(),
                _ => format!("{context}: {kind}"),
            };
        }
    }
    text.trim().to_owned()
}

fn negotiation_message(code: FailureCode, settings: &SessionSettings) -> String {
    match code {
        FailureCode::HYBRID_REQUIRED_BY_SERVER => {
            "The server requires Network Level Authentication (NLA). Turn NLA on for this connection."
                .into()
        }
        FailureCode::SSL_REQUIRED_BY_SERVER if settings.nla => {
            "The server does not support Network Level Authentication (NLA). Turn NLA off for this connection to use TLS instead."
                .into()
        }
        FailureCode::SSL_REQUIRED_BY_SERVER => "The server requires TLS.".into(),
        FailureCode::SSL_NOT_ALLOWED_BY_SERVER => {
            "The server only offers legacy RDP security without TLS, which UwURDP does not support."
                .into()
        }
        FailureCode::SSL_CERT_NOT_ON_SERVER => {
            "The server has no certificate for TLS configured.".into()
        }
        FailureCode::SSL_WITH_USER_AUTH_REQUIRED_BY_SERVER => {
            "The server requires a client certificate, which UwURDP does not support.".into()
        }
        other => format!(
            "The server refused the security settings (code 0x{:08x}).",
            u32::from(other)
        ),
    }
}

fn is_disconnect(error: &ConnectorError) -> bool {
    let mut source = std::error::Error::source(error);
    while let Some(e) = source {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            return matches!(
                io.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
            );
        }
        source = e.source();
    }
    false
}

fn map_connector_error(
    error: &ConnectorError,
    phase: Phase,
    settings: &SessionSettings,
) -> RdpError {
    debug!(error = %describe(error), ?phase, "connection failed");
    match error.kind() {
        ConnectorErrorKind::Negotiation(failure) => RdpError::Negotiation {
            message: negotiation_message(failure.code(), settings),
        },
        ConnectorErrorKind::AccessDenied => RdpError::AuthFailed {
            message: "The server denied access. The account may not be allowed to log on remotely."
                .into(),
        },
        ConnectorErrorKind::Credssp(e) => {
            let message = e
                .nstatus
                .and_then(|s| nstatus_message(s.0))
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    if e.error_type == ironrdp_connector::sspi::ErrorKind::InvalidToken
                        || e.error_type == ironrdp_connector::sspi::ErrorKind::LogonDenied
                    {
                        "Wrong user name or password.".to_owned()
                    } else {
                        format!("Authentication failed: {}", e.description)
                    }
                });
            RdpError::AuthFailed { message }
        }
        _ if phase == Phase::Authentication && is_disconnect(error) => RdpError::AuthFailed {
            message: "The server closed the connection during authentication. Check the user name and password."
                .into(),
        },
        // Some servers (IronRDP's among them) hang up instead of sending a
        // negotiation failure code when they dislike the offered protocols.
        _ if phase == Phase::Negotiation && is_disconnect(error) => RdpError::Negotiation {
            message: if settings.nla {
                "The server closed the connection during negotiation. It may not support Network Level Authentication (NLA)."
                    .into()
            } else {
                "The server closed the connection during negotiation. It may require Network Level Authentication (NLA)."
                    .into()
            },
        },
        _ => RdpError::Protocol {
            message: describe(error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_backslash_user_is_split() {
        assert_eq!(
            split_username("CORP\\alice", None),
            ("alice".into(), Some("CORP".into()))
        );
        // An explicit domain wins; the user field is left alone.
        assert_eq!(
            split_username("CORP\\alice", Some("OTHER")),
            ("CORP\\alice".into(), Some("OTHER".into()))
        );
        assert_eq!(
            split_username("alice@corp.example", Some(" ")),
            ("alice@corp.example".into(), None)
        );
        assert_eq!(split_username("\\alice", None), ("\\alice".into(), None));
    }

    #[test]
    fn performance_flags_follow_settings() {
        let mut s = SessionSettings {
            wallpaper: false,
            animations: false,
            font_smoothing: false,
            ..SessionSettings::default()
        };
        let f = performance_flags(&s);
        assert!(f.contains(PerformanceFlags::DISABLE_WALLPAPER));
        assert!(f.contains(PerformanceFlags::DISABLE_MENUANIMATIONS));
        assert!(!f.contains(PerformanceFlags::ENABLE_FONT_SMOOTHING));

        s.wallpaper = true;
        s.animations = true;
        s.font_smoothing = true;
        let f = performance_flags(&s);
        assert!(!f.contains(PerformanceFlags::DISABLE_WALLPAPER));
        assert!(!f.contains(PerformanceFlags::DISABLE_FULLWINDOWDRAG));
        assert!(f.contains(PerformanceFlags::ENABLE_FONT_SMOOTHING));
    }

    #[test]
    fn config_maps_nla_and_depth_and_size() {
        let s = SessionSettings {
            nla: true,
            color_depth: 17,
            width: 50,
            height: 10_000,
            scale_factor: 90,
            audio: AudioMode::Off,
            ..SessionSettings::default()
        };
        let c = connector_config(&s, "u".into(), None, "p");
        assert!(c.enable_credssp && !c.enable_tls);
        assert!(!c.autologon);
        assert_eq!(c.bitmap.as_ref().map(|b| b.color_depth), Some(32));
        assert_eq!((c.desktop_size.width, c.desktop_size.height), (200, 8192));
        assert_eq!(c.desktop_scale_factor, 0);
        assert!(!c.enable_audio_playback);

        let s = SessionSettings {
            nla: false,
            color_depth: 16,
            scale_factor: 150,
            ..SessionSettings::default()
        };
        let c = connector_config(&s, "u".into(), None, "p");
        assert!(!c.enable_credssp && c.enable_tls);
        assert!(c.autologon);
        assert_eq!(c.bitmap.as_ref().map(|b| b.color_depth), Some(16));
        assert_eq!(c.desktop_scale_factor, 150);
    }

    #[test]
    fn only_remotefx_is_advertised() {
        let codecs = bitmap_codecs();
        assert!(!codecs.0.is_empty());
        assert!(codecs
            .0
            .iter()
            .all(|c| matches!(c.property, CodecProperty::RemoteFx(_))));
    }

    #[test]
    fn location_is_stripped_from_messages() {
        assert_eq!(
            strip_location("[TCP connect @ src\\rdp.rs:12] custom error"),
            "TCP connect"
        );
        assert_eq!(
            strip_location("[CredSSP @ x.rs:1] access denied"),
            "CredSSP: access denied"
        );
        assert_eq!(strip_location("plain"), "plain");
    }

    #[test]
    fn hybrid_required_says_turn_nla_on() {
        let msg = negotiation_message(
            FailureCode::HYBRID_REQUIRED_BY_SERVER,
            &SessionSettings {
                nla: false,
                ..SessionSettings::default()
            },
        );
        assert!(msg.contains("Turn NLA on"));
    }

    #[test]
    fn logon_failure_status_reads_as_wrong_password() {
        assert_eq!(
            nstatus_message(0xC000_006D),
            Some("Wrong user name or password.")
        );
        assert_eq!(nstatus_message(0x1234), None);
    }
}
