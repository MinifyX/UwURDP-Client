//! Trusting one server, the way this app already trusts one SSH host.
//!
//! A homelab server has no certificate authority behind it, so there is nothing
//! for the usual chain of trust to check. What there is instead is a
//! fingerprint the setup code carried and the pairing passed on — and that is
//! the same shape of trust an SSH client has lived with for thirty years: the
//! first contact is the decision, everything after it is a comparison.
//!
//! **What is compared is the public key, not the certificate.** The server
//! makes itself a fresh certificate at every start from the same key, so a
//! device that pinned it once never has to be told again — and a certificate
//! that expires never locks anyone out of their own hosts.
//!
//! This verifier therefore looks at exactly one thing and says so: no chain, no
//! name, no expiry. Those checks exist to answer "is this really
//! example.com?", which a fingerprint answers better. What it does not replace
//! is TLS itself: the handshake is still verified against the key in the
//! certificate, so nobody in between can read or change anything.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring, verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error as TlsError, SignatureScheme};
use std::sync::Arc;

/// `SHA256:…` over a certificate's public key, as `ssh-keygen -l` prints a host
/// key's fingerprint. The server prints the same string.
pub fn fingerprint(spki_der: &[u8]) -> String {
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(spki_der);
    format!("SHA256:{}", STANDARD_NO_PAD.encode(digest))
}

/// The fingerprint of the key inside a certificate.
pub fn fingerprint_of_certificate(der: &[u8]) -> Result<String, TlsError> {
    use x509_cert::der::{Decode, Encode};
    let certificate = x509_cert::Certificate::from_der(der).map_err(|_| {
        TlsError::General("the server sent something that is not a certificate".into())
    })?;
    let spki = certificate
        .tbs_certificate()
        .subject_public_key_info()
        .to_der()
        .map_err(|_| TlsError::General("that certificate has no usable public key".into()))?;
    Ok(fingerprint(&spki))
}

/// Verifies that the server is the one whose fingerprint we were given, and
/// nothing else about it.
#[derive(Debug)]
pub struct PinnedServer {
    expected: String,
    provider: Arc<CryptoProvider>,
}

impl PinnedServer {
    pub fn new(fingerprint: &str) -> Self {
        Self {
            expected: fingerprint.trim().to_string(),
            provider: Arc::new(ring::default_provider()),
        }
    }
}

impl ServerCertVerifier for PinnedServer {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let found = fingerprint_of_certificate(end_entity)?;
        if found == self.expected {
            return Ok(ServerCertVerified::assertion());
        }
        // Worth being loud about: at this address, this is what a server in
        // the middle looks like.
        tracing::warn!(
            expected = %self.expected,
            found = %found,
            "the server's key is not the one this device pinned"
        );
        Err(TlsError::General(format!(
            "this is not the server this device paired with: it has {found}, not {}",
            self.expected
        )))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// A TLS setup that trusts exactly the server with this fingerprint.
pub fn pinned_config(fingerprint: &str) -> rustls::ClientConfig {
    rustls::ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("ring supports the versions rustls asks for")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedServer::new(fingerprint)))
        .with_no_client_auth()
}

/// A TLS setup for a server with a real certificate: the usual checks, with
/// the roots the operating system trusts.
pub fn webpki_config() -> rustls::ClientConfig {
    let roots: rustls::RootCertStore = webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect();
    rustls::ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("ring supports the versions rustls asks for")
        .with_root_certificates(roots)
        .with_no_client_auth()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A certificate made the way the server makes its own, so what is tested
    /// is the real shape of the thing and not a hand-written DER blob.
    fn certificate() -> (Vec<u8>, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        let cert = params.self_signed(&key).unwrap();
        use rcgen::PublicKeyData;
        (
            cert.der().to_vec(),
            fingerprint(&key.subject_public_key_info()),
        )
    }

    #[test]
    fn the_fingerprint_of_a_certificate_is_the_fingerprint_of_its_key() {
        let (der, expected) = certificate();
        assert_eq!(fingerprint_of_certificate(&der).unwrap(), expected);
        assert!(expected.starts_with("SHA256:"));
    }

    #[test]
    fn a_certificate_the_server_did_not_send_is_not_a_certificate() {
        assert!(fingerprint_of_certificate(b"not a certificate at all").is_err());
        assert!(fingerprint_of_certificate(&[]).is_err());
    }

    #[test]
    fn the_verifier_accepts_the_pinned_key_and_nothing_else() {
        let (der, fingerprint) = certificate();
        let (other_der, other_fingerprint) = certificate();
        assert_ne!(fingerprint, other_fingerprint);

        let verifier = PinnedServer::new(&fingerprint);
        let name = ServerName::try_from("nas.lan").unwrap();
        let now = UnixTime::now();

        assert!(verifier
            .verify_server_cert(&CertificateDer::from(der.clone()), &[], &name, &[], now)
            .is_ok());
        // The name is not what is being checked — the key is.
        assert!(verifier
            .verify_server_cert(
                &CertificateDer::from(der),
                &[],
                &ServerName::try_from("something.else").unwrap(),
                &[],
                now
            )
            .is_ok());
        // Another server at the same address gets nowhere.
        let error = verifier
            .verify_server_cert(&CertificateDer::from(other_der), &[], &name, &[], now)
            .expect_err("a different key must be refused");
        assert!(format!("{error}").contains("not the server"), "{error}");
    }

    #[test]
    fn a_fingerprint_is_compared_as_it_was_written_down() {
        let (der, fingerprint) = certificate();
        let verifier = PinnedServer::new(&format!("  {fingerprint}\n"));
        assert!(verifier
            .verify_server_cert(
                &CertificateDer::from(der),
                &[],
                &ServerName::try_from("nas.lan").unwrap(),
                &[],
                UnixTime::now()
            )
            .is_ok());
    }
}
