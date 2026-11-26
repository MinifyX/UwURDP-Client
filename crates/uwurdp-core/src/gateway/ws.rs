//! Just enough WebSocket (RFC 6455) for the gateway's WebSocket transport:
//! binary messages both ways, ping/pong and close. The opening handshake is
//! done by `http.rs`, since it has to carry NTLM.

use std::io;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

const OP_CONTINUATION: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
pub(crate) const OP_BINARY: u8 = 0x2;
pub(crate) const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
pub(crate) const OP_PONG: u8 = 0xA;

/// Biggest message we assemble. Gateway packets stay below 64 KiB.
const MAX_MESSAGE: usize = 1 << 20;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Message {
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong,
    Close,
}

/// One frame from the client: always final, always masked (RFC 6455 5.3).
pub(crate) fn encode_frame(opcode: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(0x80 | (opcode & 0x0F));
    match payload.len() {
        n @ 0..=125 => out.push(0x80 | u8::try_from(n).unwrap_or(125)),
        n @ 126..=0xFFFF => {
            out.push(0x80 | 126);
            out.extend_from_slice(&u16::try_from(n).unwrap_or(u16::MAX).to_be_bytes());
        }
        n => {
            out.push(0x80 | 127);
            out.extend_from_slice(&u64::try_from(n).unwrap_or(u64::MAX).to_be_bytes());
        }
    }
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    out
}

/// A fresh masking key. Masking only keeps proxies from mistaking our bytes
/// for HTTP; it is not a secret, so a UUID's random bits do.
pub(crate) fn mask() -> [u8; 4] {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    [bytes[0], bytes[1], bytes[2], bytes[3]]
}

pub(crate) async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    opcode: u8,
    payload: &[u8],
) -> io::Result<()> {
    writer
        .write_all(&encode_frame(opcode, payload, mask()))
        .await?;
    writer.flush().await
}

/// Reads frames until a whole message is there. Control frames in the
/// middle of a fragmented message come out on their own.
pub(crate) struct MessageReader {
    partial: Option<Vec<u8>>,
}

impl MessageReader {
    pub(crate) fn new() -> Self {
        Self { partial: None }
    }

    pub(crate) async fn read<R: AsyncRead + Unpin>(
        &mut self,
        reader: &mut R,
    ) -> io::Result<Message> {
        loop {
            let mut head = [0u8; 2];
            reader.read_exact(&mut head).await?;
            let fin = head[0] & 0x80 != 0;
            let opcode = head[0] & 0x0F;
            let masked = head[1] & 0x80 != 0;
            let len = match head[1] & 0x7F {
                126 => {
                    let mut b = [0u8; 2];
                    reader.read_exact(&mut b).await?;
                    u64::from(u16::from_be_bytes(b))
                }
                127 => {
                    let mut b = [0u8; 8];
                    reader.read_exact(&mut b).await?;
                    u64::from_be_bytes(b)
                }
                n => u64::from(n),
            };
            let len = usize::try_from(len)
                .ok()
                .filter(|&n| n <= MAX_MESSAGE)
                .ok_or_else(|| invalid("WebSocket frame too large"))?;
            let key = if masked {
                let mut k = [0u8; 4];
                reader.read_exact(&mut k).await?;
                Some(k)
            } else {
                None
            };
            let mut payload = vec![0u8; len];
            reader.read_exact(&mut payload).await?;
            if let Some(k) = key {
                for (i, b) in payload.iter_mut().enumerate() {
                    *b ^= k[i % 4];
                }
            }

            match opcode {
                OP_PING => return Ok(Message::Ping(payload)),
                OP_PONG => return Ok(Message::Pong),
                OP_CLOSE => return Ok(Message::Close),
                OP_BINARY | OP_TEXT => {
                    if self.partial.is_some() {
                        return Err(invalid("WebSocket message started inside another"));
                    }
                    if fin {
                        return Ok(Message::Binary(payload));
                    }
                    self.partial = Some(payload);
                }
                OP_CONTINUATION => {
                    let Some(mut partial) = self.partial.take() else {
                        return Err(invalid("WebSocket continuation without a message"));
                    };
                    if partial.len() + payload.len() > MAX_MESSAGE {
                        return Err(invalid("WebSocket message too large"));
                    }
                    partial.extend_from_slice(&payload);
                    if fin {
                        return Ok(Message::Binary(partial));
                    }
                    self.partial = Some(partial);
                }
                other => return Err(invalid(&format!("unknown WebSocket opcode {other}"))),
            }
        }
    }
}

fn invalid(text: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server frame (unmasked), for feeding the reader.
    fn server_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![if fin { 0x80 } else { 0 } | opcode];
        match payload.len() {
            n @ 0..=125 => out.push(u8::try_from(n).unwrap()),
            n => {
                out.push(126);
                out.extend_from_slice(&u16::try_from(n).unwrap().to_be_bytes());
            }
        }
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn client_frames_are_masked_with_the_right_length_encoding() {
        let frame = encode_frame(OP_BINARY, b"abc", [1, 2, 3, 4]);
        assert_eq!(
            frame,
            [0x82, 0x83, 1, 2, 3, 4, b'a' ^ 1, b'b' ^ 2, b'c' ^ 3]
        );

        let frame = encode_frame(OP_BINARY, &[0u8; 300], [0; 4]);
        assert_eq!(&frame[..4], &[0x82, 0x80 | 126, 0x01, 0x2C]);
        assert_eq!(frame.len(), 4 + 4 + 300);
    }

    #[tokio::test]
    async fn fragments_are_joined_and_control_frames_pass_between() {
        let bytes = [
            server_frame(false, OP_BINARY, b"hel"),
            server_frame(true, OP_PING, b"p"),
            server_frame(true, OP_CONTINUATION, &[b'l'; 200]),
            server_frame(true, OP_CLOSE, &[]),
        ]
        .concat();
        let mut input = &bytes[..];
        let mut reader = MessageReader::new();
        assert_eq!(
            reader.read(&mut input).await.unwrap(),
            Message::Ping(b"p".to_vec())
        );
        let Message::Binary(joined) = reader.read(&mut input).await.unwrap() else {
            panic!("expected binary");
        };
        assert_eq!(&joined[..3], b"hel");
        assert_eq!(joined.len(), 203);
        assert_eq!(reader.read(&mut input).await.unwrap(), Message::Close);
    }

    #[tokio::test]
    async fn a_stray_continuation_is_an_error() {
        let bytes = server_frame(true, OP_CONTINUATION, b"x");
        let mut input = &bytes[..];
        assert!(MessageReader::new().read(&mut input).await.is_err());
    }
}
