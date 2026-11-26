//! The packets of the RD Gateway's HTTP transport ([MS-TSGU] 2.2.10), all
//! little-endian behind an 8-byte header (type, reserved, total length).
//!
//! Only what a client needs: it sends the handshake, tunnel, tunnel auth,
//! channel and data packets, and reads the server's answers plus data, keep
//! alives, service messages and the channel close.
//!
//! [MS-TSGU]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tsgu/

pub(crate) const PKT_HANDSHAKE_REQUEST: u16 = 0x1;
pub(crate) const PKT_HANDSHAKE_RESPONSE: u16 = 0x2;
pub(crate) const PKT_TUNNEL_CREATE: u16 = 0x4;
pub(crate) const PKT_TUNNEL_RESPONSE: u16 = 0x5;
pub(crate) const PKT_TUNNEL_AUTH: u16 = 0x6;
pub(crate) const PKT_TUNNEL_AUTH_RESPONSE: u16 = 0x7;
pub(crate) const PKT_CHANNEL_CREATE: u16 = 0x8;
pub(crate) const PKT_CHANNEL_RESPONSE: u16 = 0x9;
pub(crate) const PKT_DATA: u16 = 0xA;
pub(crate) const PKT_SERVICE_MESSAGE: u16 = 0xB;
pub(crate) const PKT_REAUTH_MESSAGE: u16 = 0xC;
pub(crate) const PKT_KEEPALIVE: u16 = 0xD;
pub(crate) const PKT_CLOSE_CHANNEL: u16 = 0x10;
pub(crate) const PKT_CLOSE_CHANNEL_RESPONSE: u16 = 0x11;

pub(crate) const HEADER_LEN: usize = 8;

/// Quarantine (statement of health): what mstsc and FreeRDP offer. Without
/// NAP on the gateway it has no effect.
const HTTP_CAPABILITY_TYPE_QUAR_SOH: u32 = 0x1;
const HTTP_CAPABILITY_IDLE_TIMEOUT: u32 = 0x2;

const HTTP_TUNNEL_RESPONSE_FIELD_TUNNEL_ID: u16 = 0x1;
const HTTP_TUNNEL_RESPONSE_FIELD_CAPS: u16 = 0x2;
const HTTP_TUNNEL_AUTH_RESPONSE_FIELD_IDLE_TIMEOUT: u16 = 0x2;
const HTTP_TUNNEL_AUTH_RESPONSE_FIELD_REDIR_FLAGS: u16 = 0x1;
const HTTP_CHANNEL_RESPONSE_FIELD_CHANNELID: u16 = 0x1;

/// `protocol` of the channel create packet: RDP.
const CHANNEL_PROTOCOL_RDP: u16 = 3;

/// Payload room of one DATA packet: its length field is 16 bits.
pub(crate) const MAX_DATA: usize = 0xFFFF - HEADER_LEN - 2;

fn header(out: &mut Vec<u8>, packet_type: u16, body_len: usize) {
    let total = u32::try_from(HEADER_LEN + body_len).unwrap_or(u32::MAX);
    out.extend_from_slice(&packet_type.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&total.to_le_bytes());
}

/// UTF-16LE with the terminating NUL, as the gateway's strings are.
fn utf16z(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

pub(crate) fn handshake_request() -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + 6);
    header(&mut out, PKT_HANDSHAKE_REQUEST, 6);
    out.push(1); // verMajor
    out.push(0); // verMinor
    out.extend_from_slice(&0u16.to_le_bytes()); // clientVersion
    out.extend_from_slice(&0u16.to_le_bytes()); // extendedAuth: none (HTTP auth already done)
    out
}

pub(crate) fn tunnel_create() -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + 8);
    header(&mut out, PKT_TUNNEL_CREATE, 8);
    out.extend_from_slice(
        &(HTTP_CAPABILITY_TYPE_QUAR_SOH | HTTP_CAPABILITY_IDLE_TIMEOUT).to_le_bytes(),
    );
    out.extend_from_slice(&0u16.to_le_bytes()); // fieldsPresent: no PAA cookie
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out
}

pub(crate) fn tunnel_auth(client_name: &str) -> Vec<u8> {
    let name = utf16z(client_name);
    let name_len = u16::try_from(name.len()).unwrap_or(u16::MAX);
    let mut out = Vec::with_capacity(HEADER_LEN + 4 + name.len());
    header(&mut out, PKT_TUNNEL_AUTH, 4 + name.len());
    out.extend_from_slice(&0u16.to_le_bytes()); // fieldsPresent: no statement of health
    out.extend_from_slice(&name_len.to_le_bytes());
    out.extend_from_slice(&name);
    out
}

pub(crate) fn channel_create(host: &str, port: u16) -> Vec<u8> {
    let host = utf16z(host);
    let host_len = u16::try_from(host.len()).unwrap_or(u16::MAX);
    let body = 6 + 2 + host.len();
    let mut out = Vec::with_capacity(HEADER_LEN + body);
    header(&mut out, PKT_CHANNEL_CREATE, body);
    out.push(1); // numResources
    out.push(0); // numAltResources
    out.extend_from_slice(&port.to_le_bytes());
    out.extend_from_slice(&CHANNEL_PROTOCOL_RDP.to_le_bytes());
    out.extend_from_slice(&host_len.to_le_bytes());
    out.extend_from_slice(&host);
    out
}

/// One DATA packet; `payload` must be at most [`MAX_DATA`] bytes.
pub(crate) fn data(payload: &[u8]) -> Vec<u8> {
    debug_assert!(payload.len() <= MAX_DATA);
    let len = u16::try_from(payload.len()).unwrap_or(u16::MAX);
    let mut out = Vec::with_capacity(HEADER_LEN + 2 + payload.len());
    header(&mut out, PKT_DATA, 2 + payload.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

pub(crate) fn close_channel() -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + 4);
    header(&mut out, PKT_CLOSE_CHANNEL, 4);
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

pub(crate) fn keepalive() -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN);
    header(&mut out, PKT_KEEPALIVE, 0);
    out
}

/// A packet cut out of the byte stream: its type and its body.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Packet {
    pub packet_type: u16,
    pub body: Vec<u8>,
}

/// The longest packet we accept from the gateway. DATA packets stay below
/// 64 KiB; nothing else the server sends comes close.
const MAX_PACKET: usize = 0x2_0000;

/// Reassembles packets from however the transport happens to cut the bytes
/// (one per WebSocket message in practice, but nothing promises that).
#[derive(Default)]
pub(crate) struct Reader {
    buf: Vec<u8>,
    /// Where the next packet starts in `buf`; consumed bytes are dropped
    /// once per `push`, not once per packet.
    pos: usize,
}

impl Reader {
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        self.buf.drain(..self.pos);
        self.pos = 0;
        self.buf.extend_from_slice(bytes);
    }

    /// The next complete packet, if one is buffered.
    pub(crate) fn next(&mut self) -> Result<Option<Packet>, String> {
        let rest = &self.buf[self.pos..];
        if rest.len() < HEADER_LEN {
            return Ok(None);
        }
        let packet_type = u16::from_le_bytes([rest[0], rest[1]]);
        let len = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]);
        let len = usize::try_from(len).unwrap_or(usize::MAX);
        if !(HEADER_LEN..=MAX_PACKET).contains(&len) {
            return Err(format!("invalid gateway packet length {len}"));
        }
        if rest.len() < len {
            return Ok(None);
        }
        let body = rest[HEADER_LEN..len].to_vec();
        self.pos += len;
        Ok(Some(Packet { packet_type, body }))
    }
}

/// Little cursor over a packet body.
struct Body<'a>(&'a [u8]);

impl<'a> Body<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.0.len() < n {
            return Err("truncated gateway packet".into());
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HandshakeResponse {
    pub error_code: u32,
    pub version: (u8, u8),
}

pub(crate) fn parse_handshake_response(body: &[u8]) -> Result<HandshakeResponse, String> {
    let mut b = Body(body);
    let error_code = b.u32()?;
    let major = b.u8()?;
    let minor = b.u8()?;
    Ok(HandshakeResponse {
        error_code,
        version: (major, minor),
    })
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TunnelResponse {
    pub status_code: u32,
    pub tunnel_id: Option<u32>,
    pub caps: Option<u32>,
}

pub(crate) fn parse_tunnel_response(body: &[u8]) -> Result<TunnelResponse, String> {
    let mut b = Body(body);
    let _server_version = b.u16()?;
    let status_code = b.u32()?;
    let fields = b.u16()?;
    let _reserved = b.u16()?;
    let mut response = TunnelResponse {
        status_code,
        tunnel_id: None,
        caps: None,
    };
    if status_code != 0 {
        return Ok(response);
    }
    // The fields come in this order; the statement-of-health request and
    // the consent message that may follow are not needed here.
    if fields & HTTP_TUNNEL_RESPONSE_FIELD_TUNNEL_ID != 0 {
        response.tunnel_id = Some(b.u32()?);
    }
    if fields & HTTP_TUNNEL_RESPONSE_FIELD_CAPS != 0 {
        response.caps = Some(b.u32()?);
    }
    Ok(response)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TunnelAuthResponse {
    pub error_code: u32,
    /// Seconds; 0 = none.
    pub idle_timeout: Option<u32>,
}

pub(crate) fn parse_tunnel_auth_response(body: &[u8]) -> Result<TunnelAuthResponse, String> {
    let mut b = Body(body);
    let error_code = b.u32()?;
    let fields = b.u16()?;
    let _reserved = b.u16()?;
    let mut response = TunnelAuthResponse {
        error_code,
        idle_timeout: None,
    };
    if error_code != 0 {
        return Ok(response);
    }
    if fields & HTTP_TUNNEL_AUTH_RESPONSE_FIELD_REDIR_FLAGS != 0 {
        let _redirection_flags = b.u32()?;
    }
    if fields & HTTP_TUNNEL_AUTH_RESPONSE_FIELD_IDLE_TIMEOUT != 0 {
        response.idle_timeout = Some(b.u32()?);
    }
    Ok(response)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ChannelResponse {
    pub error_code: u32,
    pub channel_id: Option<u32>,
}

pub(crate) fn parse_channel_response(body: &[u8]) -> Result<ChannelResponse, String> {
    let mut b = Body(body);
    let error_code = b.u32()?;
    let fields = b.u16()?;
    let _reserved = b.u16()?;
    let channel_id = if error_code == 0 && fields & HTTP_CHANNEL_RESPONSE_FIELD_CHANNELID != 0 {
        Some(b.u32()?)
    } else {
        None
    };
    Ok(ChannelResponse {
        error_code,
        channel_id,
    })
}

/// The payload of a DATA packet.
pub(crate) fn parse_data(body: &[u8]) -> Result<&[u8], String> {
    let mut b = Body(body);
    let len = usize::from(b.u16()?);
    b.take(len)
}

/// The status of a CLOSE_CHANNEL packet.
pub(crate) fn parse_close(body: &[u8]) -> u32 {
    Body(body).u32().unwrap_or(0)
}

/// The text of a SERVICE_MESSAGE (an admin's broadcast).
pub(crate) fn parse_service_message(body: &[u8]) -> String {
    let mut b = Body(body);
    let Ok(len) = b.u16() else {
        return String::new();
    };
    let Ok(bytes) = b.take(usize::from(len)) else {
        return String::new();
    };
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Plain English for the gateway's HRESULTs ([MS-TSGU] 2.2.6), where we
/// know what they mean for the user.
pub(crate) fn describe_error(code: u32) -> String {
    let text = match code {
        // E_PROXY_INTERNALERROR
        0x8007_59D8 => "the gateway reported an internal error",
        // E_PROXY_RAP_ACCESSDENIED
        0x8007_59DA => "the gateway's resource policy (RD RAP) does not allow this computer",
        // E_PROXY_NAP_ACCESSDENIED
        0x8007_59DB => "the gateway's connection policy (RD CAP) does not allow this connection",
        // E_PROXY_TS_CONNECTFAILED
        0x8007_59DD => "the gateway could not reach the computer",
        _ => return format!("the gateway refused the connection (0x{code:08X})"),
    };
    format!("{text} (0x{code:08X})")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(bytes: &[u8]) -> Packet {
        let mut reader = Reader::default();
        reader.push(bytes);
        reader.next().expect("valid").expect("complete")
    }

    #[test]
    fn handshake_request_has_the_fixed_layout() {
        assert_eq!(
            handshake_request(),
            [1, 0, 0, 0, 14, 0, 0, 0, 1, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn tunnel_create_offers_soh_and_idle_timeout() {
        let p = packet(&tunnel_create());
        assert_eq!(p.packet_type, PKT_TUNNEL_CREATE);
        assert_eq!(p.body, [3, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn strings_are_utf16_with_nul_and_counted_in_bytes() {
        let p = packet(&tunnel_auth("PC"));
        assert_eq!(p.packet_type, PKT_TUNNEL_AUTH);
        assert_eq!(p.body, [0, 0, 6, 0, b'P', 0, b'C', 0, 0, 0]);

        let p = packet(&channel_create("h", 3390));
        assert_eq!(p.packet_type, PKT_CHANNEL_CREATE);
        assert_eq!(p.body, [1, 0, 0x3E, 0x0D, 3, 0, 4, 0, b'h', 0, 0, 0]);
    }

    #[test]
    fn data_round_trips_and_the_reader_reassembles() {
        let bytes = [data(b"hello"), keepalive(), data(b"!")].concat();
        let mut reader = Reader::default();
        // Fed in uneven pieces.
        for chunk in bytes.chunks(3) {
            reader.push(chunk);
        }
        let first = reader.next().unwrap().unwrap();
        assert_eq!(first.packet_type, PKT_DATA);
        assert_eq!(parse_data(&first.body).unwrap(), b"hello");
        assert_eq!(reader.next().unwrap().unwrap().packet_type, PKT_KEEPALIVE);
        assert_eq!(
            parse_data(&reader.next().unwrap().unwrap().body).unwrap(),
            b"!"
        );
        assert_eq!(reader.next().unwrap(), None);
    }

    #[test]
    fn partial_packets_wait_and_bad_lengths_fail() {
        let mut reader = Reader::default();
        reader.push(&data(b"abc")[..9]);
        assert_eq!(reader.next().unwrap(), None);

        let mut reader = Reader::default();
        reader.push(&[0xA, 0, 0, 0, 4, 0, 0, 0]);
        assert!(reader.next().is_err());
    }

    #[test]
    fn responses_parse() {
        // Tunnel response: version 1, OK, tunnel id + caps present.
        let body = [1, 0, 0, 0, 0, 0, 3, 0, 0, 0, 7, 0, 0, 0, 0x3F, 0, 0, 0];
        assert_eq!(
            parse_tunnel_response(&body).unwrap(),
            TunnelResponse {
                status_code: 0,
                tunnel_id: Some(7),
                caps: Some(0x3F)
            }
        );
        // Failure: fields are not read.
        let body = [1, 0, 0xDA, 0x59, 0x07, 0x80, 0, 0, 0, 0];
        assert_eq!(
            parse_tunnel_response(&body).unwrap().status_code,
            0x8007_59DA
        );

        // Tunnel auth response with redirection flags and idle timeout.
        let body = [0, 0, 0, 0, 3, 0, 0, 0, 0xFF, 0, 0, 0, 30, 0, 0, 0];
        assert_eq!(
            parse_tunnel_auth_response(&body).unwrap(),
            TunnelAuthResponse {
                error_code: 0,
                idle_timeout: Some(30)
            }
        );

        let body = [0, 0, 0, 0, 1, 0, 0, 0, 9, 0, 0, 0];
        assert_eq!(
            parse_channel_response(&body).unwrap(),
            ChannelResponse {
                error_code: 0,
                channel_id: Some(9)
            }
        );

        assert!(parse_handshake_response(&[0, 0]).is_err());
    }

    #[test]
    fn service_messages_and_errors_read_as_text() {
        let text = utf16z("Wartung um 18 Uhr");
        let mut body = u16::try_from(text.len()).unwrap().to_le_bytes().to_vec();
        body.extend_from_slice(&text);
        assert_eq!(parse_service_message(&body), "Wartung um 18 Uhr");
        assert!(describe_error(0x8007_59DA).contains("RD RAP"));
        assert!(describe_error(0x1234).contains("0x00001234"));
    }
}
