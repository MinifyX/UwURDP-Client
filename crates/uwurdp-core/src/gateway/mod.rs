//! RD Gateway ([MS-TSGU] HTTP transport over WebSocket): the RDP connection
//! runs through a tunnel the gateway opens to the target host.
//!
//! In order:
//!
//! 1. TCP and TLS to the gateway. Its certificate is checked like a browser
//!    would (system roots, host name): a gateway is a web server with a real
//!    certificate, and it is about to receive the user's password.
//! 2. NTLM on the HTTP request that upgrades to a WebSocket, bound to that
//!    certificate (see `http.rs`).
//! 3. Handshake, tunnel, tunnel authorization and channel to `host:port`.
//! 4. From then on the RDP bytes travel in DATA packets. A task pair copies
//!    them between the WebSocket and a local [`DuplexStream`], which is what
//!    the RDP connection (X.224, TLS to the host, CredSSP) runs on, exactly as
//!    it would on a TCP socket.
//!
//! UDP through the gateway (its DTLS side channel on port 3391) is not done:
//! over a gateway the session stays on TCP, like with most non-Windows
//! clients.
//!
//! [MS-TSGU]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tsgu/

mod http;
pub(crate) mod tsgu;
mod ws;

use crate::config::GatewayTarget;
use crate::connect::CONNECT_TIMEOUT;
use crate::error::RdpError;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, DuplexStream};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::ServerName;
use tracing::{debug, info, warn};
use tsgu::Packet;

/// Room in the local pipe between the RDP connection and the tunnel tasks.
const PIPE: usize = 256 * 1024;

/// A keep-alive goes out when we have sent nothing for this long, so the
/// gateway's idle timer and any proxy in between see the tunnel alive.
const KEEPALIVE: Duration = Duration::from_secs(30);

pub(crate) struct Tunnel {
    pub stream: DuplexStream,
    /// Our end of the TCP connection to the gateway, for the client info.
    pub local_addr: SocketAddr,
}

fn gateway_err(message: impl Into<String>) -> RdpError {
    RdpError::Gateway {
        message: message.into(),
    }
}

/// Opens the tunnel to `host:port` through `gateway`.
pub(crate) async fn connect(
    gateway: &GatewayTarget,
    host: &str,
    port: u16,
    client_name: &str,
) -> Result<Tunnel, RdpError> {
    let tcp = tokio::time::timeout(
        CONNECT_TIMEOUT,
        TcpStream::connect((gateway.address.as_str(), gateway.port)),
    )
    .await
    .map_err(|_| RdpError::Timeout)?
    .map_err(|e| RdpError::Unreachable {
        message: format!("gateway {}:{}: {e}", gateway.address, gateway.port),
    })?;
    let _ = tcp.set_nodelay(true);
    let local_addr = tcp.local_addr().map_err(|e| RdpError::Unreachable {
        message: e.to_string(),
    })?;

    let (mut tls, cert_der) = tls(tcp, &gateway.address).await?;
    debug!(gateway = %gateway.address, "gateway certificate verified");

    let leftover = http::upgrade(
        &mut tls,
        &gateway.address,
        &gateway.username,
        gateway.domain.as_deref(),
        gateway.password.as_str(),
        &cert_der,
    )
    .await
    // A refused gateway login stays a gateway error: `AuthFailed` would send
    // the user to the *host's* credentials.
    .map_err(|e| match e {
        http::UpgradeError::Auth(message) | http::UpgradeError::Other(message) => {
            gateway_err(message)
        }
    })?;
    debug!("gateway WebSocket open");

    let (read_half, mut write_half) = tokio::io::split(tls);
    let mut reader = Inbound {
        io: std::io::Cursor::new(leftover).chain(read_half),
        messages: ws::MessageReader::new(),
        packets: tsgu::Reader::default(),
    };

    open_channel(&mut reader, &mut write_half, host, port, client_name).await?;
    info!(%host, port, gateway = %gateway.address, "gateway tunnel open");

    let (ours, theirs) = tokio::io::duplex(PIPE);
    tokio::spawn(pump(reader, write_half, theirs));
    Ok(Tunnel {
        stream: ours,
        local_addr,
    })
}

/// TLS to the gateway with full certificate validation. Returns the stream
/// and the leaf certificate (for the channel binding).
async fn tls(
    tcp: TcpStream,
    host: &str,
) -> Result<(tokio_rustls::client::TlsStream<TcpStream>, Vec<u8>), RdpError> {
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    let (added, _) = roots.add_parsable_certificates(native.certs);
    if added == 0 {
        return Err(gateway_err(
            "no trusted root certificates found on this computer to check the gateway with",
        ));
    }
    let config = rustls::ClientConfig::builder_with_provider(crate::tls::provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| gateway_err(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_name = ServerName::try_from(host.to_owned())
        .map_err(|_| gateway_err(format!("{host} is not a valid gateway host name")))?;

    let tls = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(server_name, tcp)
        .await
        .map_err(|e| {
            // rustls reports certificate problems as InvalidData with its
            // own error inside.
            if e.get_ref()
                .and_then(|inner| inner.downcast_ref::<rustls::Error>())
                .is_some_and(|r| matches!(r, rustls::Error::InvalidCertificate(_)))
            {
                gateway_err(format!("the gateway's certificate is not trusted: {e}"))
            } else {
                gateway_err(format!("TLS to the gateway failed: {e}"))
            }
        })?;
    let der = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|chain| chain.first())
        .map(|c| c.as_ref().to_vec())
        .ok_or_else(|| gateway_err("the gateway sent no certificate"))?;
    Ok((tls, der))
}

/// The read side of the WebSocket, with what is buffered at each layer.
struct Inbound<R> {
    io: R,
    messages: ws::MessageReader,
    packets: tsgu::Reader,
}

enum Event {
    Packet(Packet),
    Ping(Vec<u8>),
    Closed,
}

impl<R: AsyncRead + Unpin> Inbound<R> {
    async fn next(&mut self) -> io::Result<Event> {
        loop {
            if let Some(packet) = self.packets.next().map_err(io::Error::other)? {
                return Ok(Event::Packet(packet));
            }
            match self.messages.read(&mut self.io).await? {
                ws::Message::Binary(bytes) => self.packets.push(&bytes),
                ws::Message::Ping(payload) => return Ok(Event::Ping(payload)),
                ws::Message::Pong => {}
                ws::Message::Close => return Ok(Event::Closed),
            }
        }
    }
}

async fn send<W: AsyncWrite + Unpin>(writer: &mut W, packet: &[u8]) -> io::Result<()> {
    ws::write_message(writer, ws::OP_BINARY, packet).await
}

/// Waits for the answer of type `expected`, passing over keep-alives and
/// service messages.
async fn expect<R, W>(
    reader: &mut Inbound<R>,
    writer: &mut W,
    expected: u16,
) -> Result<Vec<u8>, RdpError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let lost = |e: io::Error| gateway_err(format!("the gateway connection was lost: {e}"));
    loop {
        match reader.next().await.map_err(lost)? {
            Event::Packet(p) if p.packet_type == expected => return Ok(p.body),
            Event::Packet(p) if p.packet_type == tsgu::PKT_SERVICE_MESSAGE => {
                info!(message = %tsgu::parse_service_message(&p.body), "gateway message");
            }
            Event::Packet(p) if p.packet_type == tsgu::PKT_KEEPALIVE => {}
            Event::Packet(p) if p.packet_type == tsgu::PKT_CLOSE_CHANNEL => {
                return Err(gateway_err(tsgu::describe_error(tsgu::parse_close(
                    &p.body,
                ))));
            }
            Event::Packet(p) => debug!(packet_type = p.packet_type, "unexpected gateway packet"),
            Event::Ping(payload) => ws::write_message(writer, ws::OP_PONG, &payload)
                .await
                .map_err(lost)?,
            Event::Closed => return Err(gateway_err("the gateway closed the connection")),
        }
    }
}

async fn open_channel<R, W>(
    reader: &mut Inbound<R>,
    writer: &mut W,
    host: &str,
    port: u16,
    client_name: &str,
) -> Result<(), RdpError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let lost = |e: io::Error| gateway_err(format!("the gateway connection was lost: {e}"));
    let refused = |code: u32| gateway_err(tsgu::describe_error(code));

    send(writer, &tsgu::handshake_request())
        .await
        .map_err(lost)?;
    let body = expect(reader, writer, tsgu::PKT_HANDSHAKE_RESPONSE).await?;
    let handshake = tsgu::parse_handshake_response(&body).map_err(gateway_err)?;
    if handshake.error_code != 0 {
        return Err(refused(handshake.error_code));
    }
    debug!(version = ?handshake.version, "gateway handshake");

    send(writer, &tsgu::tunnel_create()).await.map_err(lost)?;
    let body = expect(reader, writer, tsgu::PKT_TUNNEL_RESPONSE).await?;
    let tunnel = tsgu::parse_tunnel_response(&body).map_err(gateway_err)?;
    if tunnel.status_code != 0 {
        return Err(refused(tunnel.status_code));
    }
    debug!(tunnel_id = ?tunnel.tunnel_id, caps = ?tunnel.caps, "gateway tunnel");

    send(writer, &tsgu::tunnel_auth(client_name))
        .await
        .map_err(lost)?;
    let body = expect(reader, writer, tsgu::PKT_TUNNEL_AUTH_RESPONSE).await?;
    let auth = tsgu::parse_tunnel_auth_response(&body).map_err(gateway_err)?;
    if auth.error_code != 0 {
        return Err(refused(auth.error_code));
    }
    debug!(idle_timeout = ?auth.idle_timeout, "gateway tunnel authorized");

    send(writer, &tsgu::channel_create(host, port))
        .await
        .map_err(lost)?;
    let body = expect(reader, writer, tsgu::PKT_CHANNEL_RESPONSE).await?;
    let channel = tsgu::parse_channel_response(&body).map_err(gateway_err)?;
    if channel.error_code != 0 {
        return Err(refused(channel.error_code));
    }
    debug!(channel_id = ?channel.channel_id, "gateway channel");
    Ok(())
}

/// Copies RDP bytes between the WebSocket and the local pipe until either
/// side ends; then both are dropped, which the RDP connection sees as the
/// end of its stream.
async fn pump<R, W>(reader: Inbound<R>, writer: W, pipe: DuplexStream)
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (pipe_read, pipe_write) = tokio::io::split(pipe);
    // One pong in flight is enough: answering only the latest ping is
    // allowed (RFC 6455 5.5.3), and a gateway that floods pings while not
    // reading cannot pile them up here.
    let (pong_tx, pong_rx) = mpsc::channel(1);
    let mut inbound = tokio::spawn(inbound(reader, pipe_write, pong_tx));
    let mut outbound = tokio::spawn(outbound(writer, pipe_read, pong_rx));
    tokio::select! {
        _ = &mut inbound => outbound.abort(),
        _ = &mut outbound => inbound.abort(),
    }
    debug!("gateway tunnel closed");
}

async fn inbound<R, P>(mut reader: Inbound<R>, mut pipe: P, pong: mpsc::Sender<Vec<u8>>)
where
    R: AsyncRead + Unpin,
    P: AsyncWrite + Unpin,
{
    loop {
        match reader.next().await {
            Ok(Event::Packet(p)) => match p.packet_type {
                tsgu::PKT_DATA => match tsgu::parse_data(&p.body) {
                    Ok(payload) => {
                        if pipe.write_all(payload).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "malformed gateway data packet");
                        break;
                    }
                },
                tsgu::PKT_CLOSE_CHANNEL => {
                    info!(status = %format!("0x{:08X}", tsgu::parse_close(&p.body)), "the gateway closed the channel");
                    break;
                }
                tsgu::PKT_SERVICE_MESSAGE => {
                    info!(message = %tsgu::parse_service_message(&p.body), "gateway message");
                }
                tsgu::PKT_REAUTH_MESSAGE => {
                    // Periodic reauthentication is a gateway policy option;
                    // without an answer the gateway ends the tunnel when it
                    // runs out.
                    warn!("the gateway asks to re-authenticate, which UwURDP does not do yet");
                }
                tsgu::PKT_KEEPALIVE | tsgu::PKT_CLOSE_CHANNEL_RESPONSE => {}
                other => debug!(packet_type = other, "ignoring a gateway packet"),
            },
            Ok(Event::Ping(payload)) => {
                if let Err(mpsc::error::TrySendError::Closed(_)) = pong.try_send(payload) {
                    break;
                }
            }
            Ok(Event::Closed) => break,
            Err(e) => {
                debug!(error = %e, "gateway connection lost");
                break;
            }
        }
    }
    let _ = pipe.shutdown().await;
}

async fn outbound<W, P>(mut writer: W, mut pipe: P, mut pong: mpsc::Receiver<Vec<u8>>)
where
    W: AsyncWrite + Unpin,
    P: AsyncRead + Unpin,
{
    let mut buf = vec![0u8; tsgu::MAX_DATA];
    let mut idle = tokio::time::interval(KEEPALIVE);
    idle.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    idle.reset();
    loop {
        let result = tokio::select! {
            payload = pong.recv() => match payload {
                Some(payload) => ws::write_message(&mut writer, ws::OP_PONG, &payload).await,
                None => break,
            },
            read = pipe.read(&mut buf) => match read {
                Ok(0) | Err(_) => {
                    // The RDP connection is done: close the channel politely.
                    let _ = send(&mut writer, &tsgu::close_channel()).await;
                    let _ = ws::write_message(&mut writer, ws::OP_CLOSE, &[]).await;
                    break;
                }
                Ok(n) => {
                    idle.reset();
                    send(&mut writer, &tsgu::data(&buf[..n])).await
                }
            },
            _ = idle.tick() => send(&mut writer, &tsgu::keepalive()).await,
        };
        if let Err(e) = result {
            debug!(error = %e, "gateway connection lost");
            break;
        }
    }
}

#[cfg(test)]
mod tests;
