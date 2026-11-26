//! UDP for the session ([MS-RDPBCGR] 2.2.15 multitransport, reliable
//! RDP-UDP2 + TLS + [MS-RDPEMT]), on direct connections.
//!
//! The client offers reliable UDP with Soft-Sync in its GCC data. When the
//! server answers with an Initiate Multitransport Request, we open the UDP
//! transport to the same host and port and report the outcome; the server
//! then moves the dynamic channels (graphics pipeline, display control,
//! input, audio) onto UDP with a Soft-Sync exchange. Static channels,
//! fast-path input and the control PDUs stay on TCP.
//!
//! The UDP side's TLS must present the very certificate the user trusted for
//! the TCP connection, nothing else. `ironrdp-tls` checks the chain against
//! the system roots first and asks our pin only when that fails, so the UDP
//! TLS is given a name no CA issues for ([`PIN_ONLY_NAME`]): the chain check
//! always fails and the pin decides, as on TCP.
//!
//! Anything going wrong here is not fatal: the server is told `E_ABORT` and
//! the session stays on TCP.
//!
//! [MS-RDPBCGR]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/
//! [MS-RDPEMT]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpemt/

use crate::tls::certificate_fingerprint;
use ironrdp_pdu::gcc::MultiTransportFlags;
use ironrdp_pdu::rdp::multitransport::{MultitransportRequestPdu, RequestedProtocol};
use ironrdp_rdpeudp_tokio::{MultitransportBootstrap, UdpTlsConfig, UdpTransport};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info};

/// How long the UDP handshake may take before we settle for TCP. The TCP
/// connection sequence waits meanwhile, so keep it short.
pub(crate) const UDP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The TLS server name for the UDP side. `.invalid` is reserved (RFC 2606),
/// so no publicly or enterprise-trusted certificate can match it and only
/// the pinned certificate gets through. Windows does not look at SNI here.
pub(crate) const PIN_ONLY_NAME: &str = "uwurdp.invalid";

/// What the client announces when it wants UDP.
pub(crate) fn offered_flags() -> MultiTransportFlags {
    MultiTransportFlags::TRANSPORT_TYPE_UDP_FECR | MultiTransportFlags::SOFT_SYNC_TCP_TO_UDP
}

/// Where and how to open UDP, known once the TCP side is trusted.
#[derive(Clone)]
pub(crate) struct Plan {
    /// The address the TCP connection went to; RDP's UDP listens on the
    /// same port.
    pub server: SocketAddr,
    /// The host name, for logs (the UDP TLS itself uses [`PIN_ONLY_NAME`]).
    pub server_name: String,
    /// Fingerprint of the certificate trusted on TCP.
    pub fingerprint: String,
}

/// Whether to answer this request with a UDP attempt at all: only reliable
/// UDP (lossy UDP needs DTLS, which IronRDP does not have) and only with
/// Soft-Sync, the way Windows moves channels onto it.
pub(crate) fn wanted(request: &MultitransportRequestPdu, soft_sync: bool) -> bool {
    soft_sync && request.requested_protocol == RequestedProtocol::UdpFecR
}

/// Opens the UDP transport the server asked for.
pub(crate) async fn open(
    request: &MultitransportRequestPdu,
    plan: &Plan,
) -> Result<UdpTransport, String> {
    // ironrdp-tls builds its rustls config from the process-wide provider;
    // name ours so it never has to guess (tests link aws-lc too).
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();

    let pinned = plan.fingerprint.clone();
    let tls = UdpTlsConfig {
        certificate_validation: ironrdp_tls::CertificateValidation::Strict,
        certificate_validation_callback: Some(Arc::new(move |der: &[u8], _: &str, error: &str| {
            let same = certificate_fingerprint(der) == pinned;
            if !same {
                debug!(error, "the UDP side presented a different certificate");
            }
            same
        })),
        certificate_validation_endpoint: plan.server_name.clone(),
    };

    let mut bootstrap = MultitransportBootstrap::new(request.clone());
    let attempt = bootstrap.connect(
        plan.server,
        PIN_ONLY_NAME.to_owned(),
        ironrdp_rdpeudp::ConnectionConfig::default(),
        tls,
    );
    match tokio::time::timeout(UDP_CONNECT_TIMEOUT, attempt).await {
        Err(_) => Err(format!(
            "no UDP answer from {} within {} s",
            plan.server,
            UDP_CONNECT_TIMEOUT.as_secs()
        )),
        Ok(Err(e)) => Err(e.to_string()),
        Ok(Ok(())) => {
            info!(server = %plan.server, "UDP transport established");
            bootstrap
                .take_transport()
                .ok_or_else(|| "the UDP transport vanished".to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp_pdu::rdp::headers::{BasicSecurityHeader, BasicSecurityHeaderFlags};

    fn request(protocol: RequestedProtocol) -> MultitransportRequestPdu {
        MultitransportRequestPdu {
            security_header: BasicSecurityHeader {
                flags: BasicSecurityHeaderFlags::TRANSPORT_REQ,
            },
            request_id: 1,
            requested_protocol: protocol,
            security_cookie: [0; 16],
        }
    }

    #[test]
    fn only_reliable_udp_with_soft_sync_is_tried() {
        assert!(wanted(&request(RequestedProtocol::UdpFecR), true));
        assert!(!wanted(&request(RequestedProtocol::UdpFecR), false));
        assert!(!wanted(&request(RequestedProtocol::UdpFecL), true));
    }

    #[test]
    fn the_offer_is_reliable_udp_with_soft_sync() {
        let flags = offered_flags();
        assert!(flags.contains(MultiTransportFlags::TRANSPORT_TYPE_UDP_FECR));
        assert!(flags.contains(MultiTransportFlags::SOFT_SYNC_TCP_TO_UDP));
        assert!(!flags.contains(MultiTransportFlags::TRANSPORT_TYPE_UDP_FECL));
    }
}
