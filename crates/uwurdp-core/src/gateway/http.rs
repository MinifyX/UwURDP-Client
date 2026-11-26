//! The gateway's HTTP side: NTLM authentication on the request that upgrades
//! the connection to a WebSocket ([MS-TSGU] 3.3.5.1, WebSocket transport).
//!
//! NTLM authenticates the *connection*, so both legs (negotiate, then
//! authenticate + upgrade) go over the same TLS stream. The authenticate
//! message carries a channel binding to the gateway's certificate
//! (RFC 5929 `tls-server-end-point`): gateways with Extended Protection
//! (`EnforceChannelBinding`) refuse NTLM without it, and with it a man in the
//! middle cannot relay our login to the real gateway.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use sha2::Digest as _;
use sspi::{
    AuthIdentity, BufferType, ClientRequestFlags, CredentialUse, DataRepresentation, Ntlm,
    SecurityBuffer, SecurityStatus, Sspi as _, SspiImpl as _, Username,
};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use x509_cert::der::Decode as _;

/// The longest response head we read.
const MAX_HEAD: usize = 32 * 1024;
/// The longest 401 body we skip.
const MAX_BODY: usize = 256 * 1024;

pub(crate) enum UpgradeError {
    /// The gateway refused the user name or password.
    Auth(String),
    Other(String),
}

impl From<io::Error> for UpgradeError {
    fn from(e: io::Error) -> Self {
        Self::Other(e.to_string())
    }
}

/// `tls-server-end-point` channel binding data (RFC 5929 4.1): the
/// certificate hashed with its own signature's hash, SHA-256 for MD5/SHA-1.
pub(crate) fn channel_binding(cert_der: &[u8]) -> Vec<u8> {
    // sha384WithRSAEncryption, ecdsa-with-SHA384 / sha512WithRSAEncryption,
    // ecdsa-with-SHA512. RSASSA-PSS names its hash in parameters; Windows
    // gateways don't use it, and SHA-256 is what we'd guess anyway.
    const SHA384: [&str; 2] = ["1.2.840.113549.1.1.12", "1.2.840.10045.4.3.3"];
    const SHA512: [&str; 2] = ["1.2.840.113549.1.1.13", "1.2.840.10045.4.3.4"];
    let oid = x509_cert::Certificate::from_der(cert_der)
        .map(|c| c.signature_algorithm.oid.to_string())
        .unwrap_or_default();
    let hash = if SHA384.contains(&oid.as_str()) {
        sha2::Sha384::digest(cert_der).to_vec()
    } else if SHA512.contains(&oid.as_str()) {
        sha2::Sha512::digest(cert_der).to_vec()
    } else {
        sha2::Sha256::digest(cert_der).to_vec()
    };
    let mut data = b"tls-server-end-point:".to_vec();
    data.extend_from_slice(&hash);
    data
}

/// The response head: status code, headers (names lowercased) and whatever
/// was read past it.
pub(crate) struct Head {
    pub status: u16,
    pub status_line: String,
    pub headers: Vec<(String, String)>,
    pub rest: Vec<u8>,
}

impl Head {
    fn header<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers
            .iter()
            .filter(move |(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

pub(crate) fn parse_head(bytes: &[u8]) -> Result<Option<Head>, String> {
    let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Ok(None);
    };
    let text = std::str::from_utf8(&bytes[..end]).map_err(|_| "invalid HTTP response")?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or_default().to_owned();
    let status = status_line
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("invalid HTTP status line: {status_line}"))?;
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    Ok(Some(Head {
        status,
        status_line,
        headers,
        rest: bytes[end + 4..].to_vec(),
    }))
}

async fn read_head<R: AsyncRead + Unpin>(reader: &mut R, mut buf: Vec<u8>) -> io::Result<Head> {
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(head) = parse_head(&buf).map_err(io::Error::other)? {
            return Ok(head);
        }
        if buf.len() > MAX_HEAD {
            return Err(io::Error::other("the gateway's HTTP response is too long"));
        }
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the gateway closed the connection",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Skips a response body by its Content-Length and returns what was read
/// past it.
async fn skip_body<R: AsyncRead + Unpin>(reader: &mut R, head: &Head) -> io::Result<Vec<u8>> {
    let len: usize = head
        .header("content-length")
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if len > MAX_BODY {
        return Err(io::Error::other("the gateway's HTTP response is too long"));
    }
    let mut rest = head.rest.clone();
    while rest.len() < len {
        let mut chunk = vec![0u8; len - rest.len()];
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the gateway closed the connection",
            ));
        }
        rest.extend_from_slice(&chunk[..n]);
    }
    Ok(rest.split_off(len))
}

fn request(host: &str, connection_id: &str, ws_key: &str, authorization: &str) -> String {
    // The method is the gateway's own; WebSocket-capable gateways (Windows
    // Server 2016+) upgrade it like a GET, the way mstsc and FreeRDP do.
    format!(
        "RDG_OUT_DATA /remoteDesktopGateway/ HTTP/1.1\r\n\
         Host: {host}\r\n\
         Accept: */*\r\n\
         Cache-Control: no-cache\r\n\
         Pragma: no-cache\r\n\
         User-Agent: MS-RDGateway/1.0\r\n\
         Connection: Upgrade\r\n\
         Upgrade: websocket\r\n\
         Sec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: {ws_key}\r\n\
         RDG-Connection-Id: {{{connection_id}}}\r\n\
         Authorization: {authorization}\r\n\
         Content-Length: 0\r\n\
         \r\n"
    )
}

/// The NTLM challenge in the gateway's 401 and the scheme it came under.
/// Bare scheme offers (no token) are skipped: the challenge is the answer
/// to our negotiate message.
fn challenge(head: &Head) -> Option<(&'static str, Vec<u8>)> {
    head.header("www-authenticate").find_map(|value| {
        let (scheme, token) = value.split_once(' ')?;
        let scheme = match scheme.to_ascii_lowercase().as_str() {
            "ntlm" => "NTLM",
            // IIS takes a raw NTLM token under Negotiate as well.
            "negotiate" => "Negotiate",
            _ => return None,
        };
        let token = STANDARD.decode(token.trim()).ok()?;
        (!token.is_empty()).then_some((scheme, token))
    })
}

fn sspi_err(e: sspi::Error) -> UpgradeError {
    UpgradeError::Other(format!("NTLM: {e}"))
}

/// Authenticates with NTLM and upgrades to a WebSocket. Returns the bytes
/// read past the 101 response (the start of the WebSocket stream).
pub(crate) async fn upgrade<S>(
    stream: &mut S,
    host: &str,
    username: &str,
    domain: Option<&str>,
    password: &str,
    cert_der: &[u8],
) -> Result<Vec<u8>, UpgradeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (account, domain) = crate::connect::split_username(username, domain);
    let username = Username::new(&account, domain.as_deref())
        .map_err(|e| UpgradeError::Auth(format!("invalid gateway user name: {e}")))?;
    let identity = AuthIdentity {
        username,
        password: password.to_owned().into(),
    };

    let mut ntlm = Ntlm::new();
    ntlm.set_channel_bindings(&channel_binding(cert_der));
    let mut credentials = ntlm
        .acquire_credentials_handle()
        .with_credential_use(CredentialUse::Outbound)
        .with_auth_data(&identity)
        .execute(&mut ntlm)
        .map_err(sspi_err)?
        .credentials_handle;

    let target = format!("HTTP/{host}");
    let flags = ClientRequestFlags::ALLOCATE_MEMORY | ClientRequestFlags::INTEGRITY;
    let connection_id = uuid::Uuid::new_v4().to_string().to_uppercase();
    let ws_key = STANDARD.encode(uuid::Uuid::new_v4().as_bytes());

    // Leg 1: negotiate.
    let mut output = vec![SecurityBuffer::new(Vec::new(), BufferType::Token)];
    let mut builder = ntlm
        .initialize_security_context()
        .with_credentials_handle(&mut credentials)
        .with_context_requirements(flags)
        .with_target_data_representation(DataRepresentation::Native)
        .with_target_name(&target)
        .with_output(&mut output);
    ntlm.initialize_security_context_impl(&mut builder)
        .map_err(sspi_err)?
        .resolve_to_result()
        .map_err(sspi_err)?;
    let negotiate = STANDARD.encode(&output[0].buffer);

    stream
        .write_all(request(host, &connection_id, &ws_key, &format!("NTLM {negotiate}")).as_bytes())
        .await?;
    stream.flush().await?;
    let head = read_head(stream, Vec::new()).await?;
    if head.status != 401 {
        return Err(unexpected(&head));
    }
    let Some((scheme, challenge_token)) = challenge(&head) else {
        return Err(UpgradeError::Other(
            "the gateway does not offer NTLM authentication".into(),
        ));
    };
    let leftover = skip_body(stream, &head).await?;

    // Leg 2: authenticate (with the channel binding) and upgrade.
    let mut input = vec![SecurityBuffer::new(challenge_token, BufferType::Token)];
    let mut output = vec![SecurityBuffer::new(Vec::new(), BufferType::Token)];
    let mut builder = ntlm
        .initialize_security_context()
        .with_credentials_handle(&mut credentials)
        .with_context_requirements(flags)
        .with_target_data_representation(DataRepresentation::Native)
        .with_target_name(&target)
        .with_input(&mut input)
        .with_output(&mut output);
    let result = ntlm
        .initialize_security_context_impl(&mut builder)
        .map_err(sspi_err)?
        .resolve_to_result()
        .map_err(sspi_err)?;
    if matches!(
        result.status,
        SecurityStatus::CompleteAndContinue | SecurityStatus::CompleteNeeded
    ) {
        ntlm.complete_auth_token(&mut output).map_err(sspi_err)?;
    }
    let authenticate = STANDARD.encode(&output[0].buffer);

    stream
        .write_all(
            request(
                host,
                &connection_id,
                &ws_key,
                &format!("{scheme} {authenticate}"),
            )
            .as_bytes(),
        )
        .await?;
    stream.flush().await?;
    let head = read_head(stream, leftover).await?;
    match head.status {
        101 => Ok(head.rest),
        401 => Err(UpgradeError::Auth(
            "The gateway rejected the user name or password.".into(),
        )),
        _ => Err(unexpected(&head)),
    }
}

fn unexpected(head: &Head) -> UpgradeError {
    match head.status {
        // What gateways without the WebSocket transport (before Windows
        // Server 2016) or behind a proxy that strips Upgrade answer.
        400 | 404 | 405 | 501 => UpgradeError::Other(format!(
            "the gateway does not support the WebSocket transport ({})",
            head.status_line
        )),
        403 => UpgradeError::Auth("The gateway denied access for this user.".into()),
        _ => UpgradeError::Other(format!("unexpected gateway response: {}", head.status_line)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heads_parse_with_lowercased_names_and_leftover() {
        let raw = b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Negotiate\r\nWWW-Authenticate: NTLM TlRMTVNTUAAC\r\nContent-Length: 3\r\n\r\nabcXY";
        let head = parse_head(raw).unwrap().unwrap();
        assert_eq!(head.status, 401);
        assert_eq!(head.rest, b"abcXY");
        // The bare Negotiate offer is skipped for the header with a token.
        let (scheme, token) = challenge(&head).unwrap();
        assert_eq!(scheme, "NTLM");
        assert_eq!(token, b"NTLMSSP\0\x02");
        assert!(parse_head(b"HTTP/1.1 200 OK\r\n").unwrap().is_none());
    }

    #[test]
    fn no_challenge_without_a_token() {
        let raw = b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Negotiate\r\nWWW-Authenticate: Basic realm=\"x\"\r\n\r\n";
        let head = parse_head(raw).unwrap().unwrap();
        assert!(challenge(&head).is_none());
    }

    #[tokio::test]
    async fn bodies_are_skipped_and_the_rest_kept() {
        let head = parse_head(b"HTTP/1.1 401 x\r\nContent-Length: 5\r\n\r\nab")
            .unwrap()
            .unwrap();
        let mut more = &b"cdeNEXT"[..];
        assert_eq!(skip_body(&mut more, &head).await.unwrap(), b"");
        assert_eq!(more, b"NEXT");
    }

    #[test]
    fn channel_binding_uses_sha256_for_sha256_certificates() {
        let key = rcgen::KeyPair::generate().expect("key");
        let cert = rcgen::CertificateParams::new(vec!["gw.test".into()])
            .expect("params")
            .self_signed(&key)
            .expect("cert");
        let der = cert.der().to_vec();
        let binding = channel_binding(&der);
        assert!(binding.starts_with(b"tls-server-end-point:"));
        // rcgen signs with ECDSA P-256 / SHA-256 by default.
        assert_eq!(binding.len(), "tls-server-end-point:".len() + 32);
        assert_eq!(&binding[21..], sha2::Sha256::digest(&der).as_slice());
    }

    #[test]
    fn requests_ask_for_the_upgrade_with_authorization() {
        let r = request("gw.example", "ABC", "a2V5", "NTLM eA==");
        assert!(r.starts_with("RDG_OUT_DATA /remoteDesktopGateway/ HTTP/1.1\r\n"));
        assert!(r.contains("Upgrade: websocket\r\n"));
        assert!(r.contains("RDG-Connection-Id: {ABC}\r\n"));
        assert!(r.contains("Authorization: NTLM eA==\r\n"));
        assert!(r.ends_with("\r\n\r\n"));
    }
}
