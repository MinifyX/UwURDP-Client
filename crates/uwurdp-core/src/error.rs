//! Errors the app sees.

use crate::SessionId;
use serde::Serialize;

/// The server certificate as observed during the TLS handshake, for the
/// "do you trust this server?" dialog.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ObservedCertificate {
    /// `"SHA256:<base64 without padding>"` over the leaf certificate's DER,
    /// formatted like `ssh-keygen -l` so it reads familiar next to UwUSSH.
    pub fingerprint: String,
    pub subject: String,
    pub issuer: String,
    /// RFC 3339, UTC.
    pub not_before: String,
    pub not_after: String,
    /// Standard base64 (with padding) of the certificate DER.
    pub der_base64: String,
}

/// Why `connect` failed. Serializes as `{"kind": "auth-failed", ...}`.
#[derive(Debug, Clone, Serialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RdpError {
    /// First contact: the certificate has never been trusted. Nothing but the
    /// X.224 negotiation and the TLS handshake happened; no credential left
    /// the machine.
    #[error("the server certificate is not trusted yet ({})", observed.fingerprint)]
    UnknownCertificate { observed: ObservedCertificate },
    /// The server presented a different certificate than the one trusted.
    /// Could be a renewed certificate — or someone in the middle.
    #[error("the server certificate changed (expected {expected}, got {})", observed.fingerprint)]
    CertificateChanged {
        expected: String,
        observed: ObservedCertificate,
    },
    /// Wrong user name or password, or the account may not log on remotely.
    #[error("authentication failed: {message}")]
    AuthFailed { message: String },
    /// DNS or TCP connect failed.
    #[error("the server is unreachable: {message}")]
    Unreachable { message: String },
    #[error("the connection timed out")]
    Timeout,
    /// Security protocol mismatch (e.g. the server requires NLA).
    #[error("negotiation failed: {message}")]
    Negotiation { message: String },
    #[error("gateway: {message}")]
    Gateway { message: String },
    #[error("cancelled")]
    Cancelled,
    #[error("protocol error: {message}")]
    Protocol { message: String },
}

/// Why a call on a running session failed.
#[derive(Debug, Clone, Serialize, thiserror::Error, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SessionError {
    #[error("no session with id {id}")]
    UnknownSession { id: SessionId },
    #[error("the session has ended")]
    Closed,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cert() -> ObservedCertificate {
        ObservedCertificate {
            fingerprint: "SHA256:abc".into(),
            subject: "CN=host".into(),
            issuer: "CN=host".into(),
            not_before: "2024-01-01T00:00:00Z".into(),
            not_after: "2025-01-01T00:00:00Z".into(),
            der_base64: "AAAA".into(),
        }
    }

    #[test]
    fn errors_serialize_with_a_kebab_case_kind_tag() {
        let value = serde_json::to_value(RdpError::UnknownCertificate { observed: cert() })
            .expect("serialize");
        assert_eq!(value["kind"], "unknown-certificate");
        assert_eq!(value["observed"]["derBase64"], "AAAA");
        assert_eq!(value["observed"]["notBefore"], "2024-01-01T00:00:00Z");

        let value = serde_json::to_value(RdpError::CertificateChanged {
            expected: "SHA256:old".into(),
            observed: cert(),
        })
        .expect("serialize");
        assert_eq!(value["kind"], "certificate-changed");
        assert_eq!(value["expected"], "SHA256:old");

        let value = serde_json::to_value(RdpError::AuthFailed {
            message: "nope".into(),
        })
        .expect("serialize");
        assert_eq!(value["kind"], "auth-failed");
        assert_eq!(value["message"], "nope");

        let value = serde_json::to_value(RdpError::Timeout).expect("serialize");
        assert_eq!(value, serde_json::json!({ "kind": "timeout" }));
    }
}
