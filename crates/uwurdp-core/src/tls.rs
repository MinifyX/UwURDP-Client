//! The TLS upgrade and the trust-on-first-use certificate check.
//!
//! RDP servers almost always present a self-signed certificate, so the
//! WebPKI chain check that browsers do would reject nearly every real server.
//! Instead we do what SSH does: remember the certificate's fingerprint the
//! first time the user accepts it, and refuse to talk to anyone else later.
//!
//! What we skip is only the *chain and name* validation. The handshake
//! signatures are still verified against the presented certificate — without
//! that, a man in the middle could replay the real server's (public)
//! certificate and pass the pin check while holding none of its keys.
//!
//! The check happens right after the handshake and before CredSSP, so an
//! unknown or changed certificate ends the connection before any credential
//! has been sent.

use crate::error::{ObservedCertificate, RdpError};
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use base64::Engine as _;
use sha2::{Digest, Sha256};
use std::io;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt as _};
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls;
use tokio_rustls::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use tokio_rustls::rustls::crypto::{
    verify_tls12_signature, verify_tls13_signature, CryptoProvider, WebPkiSupportedAlgorithms,
};
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_rustls::rustls::{DigitallySignedStruct, SignatureScheme};
use x509_cert::der::Decode as _;

/// The server certificate, as far as the rest of the connection needs it.
pub(crate) struct ServerCertificate {
    pub observed: ObservedCertificate,
    /// The SubjectPublicKey bits; CredSSP binds its exchange to them.
    pub public_key: Vec<u8>,
}

/// `SHA256:` + unpadded standard base64 of SHA-256 over `der`, the same shape
/// as `ssh-keygen -l`.
pub fn certificate_fingerprint(der: &[u8]) -> String {
    format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(der)))
}

/// Compares an observed certificate with what the user trusted before.
pub(crate) fn check_trust(
    observed: &ObservedCertificate,
    trusted: Option<&str>,
) -> Result<(), RdpError> {
    match trusted.map(str::trim) {
        None | Some("") => Err(RdpError::UnknownCertificate {
            observed: observed.clone(),
        }),
        Some(expected) if fingerprints_match(expected, &observed.fingerprint) => Ok(()),
        Some(expected) => Err(RdpError::CertificateChanged {
            expected: expected.to_owned(),
            observed: observed.clone(),
        }),
    }
}

/// Exact comparison, tolerant only of base64 padding someone may have kept
/// when copying a fingerprint from elsewhere.
fn fingerprints_match(expected: &str, observed: &str) -> bool {
    expected.trim_end_matches('=') == observed
}

pub(crate) fn describe_certificate(der: &[u8]) -> Result<ServerCertificate, RdpError> {
    let cert = x509_cert::Certificate::from_der(der).map_err(|e| RdpError::Protocol {
        message: format!("the server certificate cannot be parsed: {e}"),
    })?;
    let tbs = &cert.tbs_certificate;
    let public_key = tbs
        .subject_public_key_info
        .subject_public_key
        .as_bytes()
        .ok_or_else(|| RdpError::Protocol {
            message: "the server certificate's public key is malformed".into(),
        })?
        .to_vec();
    Ok(ServerCertificate {
        observed: ObservedCertificate {
            fingerprint: certificate_fingerprint(der),
            subject: tbs.subject.to_string(),
            issuer: tbs.issuer.to_string(),
            not_before: tbs.validity.not_before.to_date_time().to_string(),
            not_after: tbs.validity.not_after.to_date_time().to_string(),
            der_base64: STANDARD.encode(der),
        },
        public_key,
    })
}

/// The crypto provider for everything TLS in this crate. Named explicitly
/// everywhere, never taken from the process default: the test build links
/// aws-lc as well (through ironrdp-server), and rustls refuses to guess.
pub(crate) fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Runs the TLS handshake over an already negotiated RDP stream and returns
/// the stream plus the DER of the server's leaf certificate.
pub(crate) async fn upgrade<S>(stream: S, host: &str) -> io::Result<(TlsStream<S>, Vec<u8>)>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let provider = provider();
    let verifier = Arc::new(PinnedLater {
        algorithms: provider.signature_verification_algorithms,
    });
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    // CredSSP does not support TLS session resumption (MS-CSSP 3.1.5).
    config.resumption = rustls::client::Resumption::disabled();

    // SNI is meaningless for RDP; a host that is not a valid DNS name (or an
    // IP) just gets a placeholder instead of failing the connection.
    let server_name = ServerName::try_from(host.to_owned())
        .or_else(|_| ServerName::try_from("uwurdp.invalid"))
        .map_err(io::Error::other)?;

    let mut tls = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(server_name, stream)
        .await?;
    tls.flush().await?;

    let der = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|chain| chain.first())
        .map(|cert| cert.as_ref().to_vec())
        .ok_or_else(|| io::Error::other("the server sent no certificate"))?;
    Ok((tls, der))
}

/// Accepts any certificate *identity* (the pin check follows the handshake)
/// but still verifies every handshake signature against it.
#[derive(Debug)]
struct PinnedLater {
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinnedLater {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed(fingerprint: &str) -> ObservedCertificate {
        ObservedCertificate {
            fingerprint: fingerprint.into(),
            subject: String::new(),
            issuer: String::new(),
            not_before: String::new(),
            not_after: String::new(),
            der_base64: String::new(),
        }
    }

    #[test]
    fn fingerprint_is_sha256_base64_without_padding() {
        // SHA-256("") = e3b0c442...b855; its base64 is the well-known
        // "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=".
        assert_eq!(
            certificate_fingerprint(b""),
            "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU"
        );
        let fp = certificate_fingerprint(b"any certificate bytes");
        assert!(fp.starts_with("SHA256:"));
        assert!(!fp.ends_with('='));
        assert_eq!(fp.len(), "SHA256:".len() + 43);
    }

    #[test]
    fn never_seen_is_unknown() {
        let cert = observed("SHA256:aaa");
        assert!(matches!(
            check_trust(&cert, None),
            Err(RdpError::UnknownCertificate { .. })
        ));
        assert!(matches!(
            check_trust(&cert, Some("  ")),
            Err(RdpError::UnknownCertificate { .. })
        ));
    }

    #[test]
    fn same_fingerprint_is_trusted() {
        let cert = observed("SHA256:aaa");
        assert!(check_trust(&cert, Some("SHA256:aaa")).is_ok());
        assert!(check_trust(&cert, Some(" SHA256:aaa=")).is_ok());
    }

    #[test]
    fn different_fingerprint_is_a_change() {
        let cert = observed("SHA256:bbb");
        match check_trust(&cert, Some("SHA256:aaa")) {
            Err(RdpError::CertificateChanged { expected, observed }) => {
                assert_eq!(expected, "SHA256:aaa");
                assert_eq!(observed.fingerprint, "SHA256:bbb");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_generated_certificate_is_described() {
        let key = rcgen::KeyPair::generate().expect("key");
        let cert = rcgen::CertificateParams::new(vec!["uwu.test".into()])
            .expect("params")
            .self_signed(&key)
            .expect("cert");
        let der = cert.der().to_vec();
        let described = describe_certificate(&der).expect("describe");
        assert_eq!(
            described.observed.fingerprint,
            certificate_fingerprint(&der)
        );
        assert_eq!(
            STANDARD
                .decode(&described.observed.der_base64)
                .expect("b64"),
            der
        );
        assert!(!described.public_key.is_empty());
        assert!(described.observed.not_before.ends_with('Z'));
    }
}
