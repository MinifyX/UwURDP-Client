//! The tunnel's packet exchange and data pump against a scripted gateway,
//! over an in-memory pipe (TLS and NTLM are covered on their own).

use super::*;

/// A server frame (unmasked) around one gateway packet.
fn frame(packet: &[u8]) -> Vec<u8> {
    let mut out = vec![0x82];
    match packet.len() {
        n @ 0..=125 => out.push(u8::try_from(n).unwrap()),
        n => {
            out.push(126);
            out.extend_from_slice(&u16::try_from(n).unwrap().to_be_bytes());
        }
    }
    out.extend_from_slice(packet);
    out
}

fn packet(packet_type: u16, body: &[u8]) -> Vec<u8> {
    let mut out = packet_type.to_le_bytes().to_vec();
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&u32::try_from(8 + body.len()).unwrap().to_le_bytes());
    out.extend_from_slice(body);
    out
}

/// The gateway side: reads the client's (masked) frames as packets.
struct FakeGateway {
    io: DuplexStream,
    messages: ws::MessageReader,
    packets: tsgu::Reader,
}

impl FakeGateway {
    async fn next(&mut self) -> Packet {
        loop {
            if let Some(p) = self.packets.next().unwrap() {
                return p;
            }
            match self.messages.read(&mut self.io).await.unwrap() {
                ws::Message::Binary(bytes) => self.packets.push(&bytes),
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    async fn reply(&mut self, packet_type: u16, body: &[u8]) {
        self.io
            .write_all(&frame(&packet(packet_type, body)))
            .await
            .unwrap();
    }

    /// Answers the four setup steps, with a keep-alive and a service
    /// message thrown in where the client must skip them.
    async fn accept(&mut self) -> String {
        assert_eq!(self.next().await.packet_type, tsgu::PKT_HANDSHAKE_REQUEST);
        self.reply(tsgu::PKT_KEEPALIVE, &[]).await;
        self.reply(
            tsgu::PKT_HANDSHAKE_RESPONSE,
            &[0, 0, 0, 0, 1, 0, 0, 0, 0, 0],
        )
        .await;

        assert_eq!(self.next().await.packet_type, tsgu::PKT_TUNNEL_CREATE);
        self.reply(
            tsgu::PKT_TUNNEL_RESPONSE,
            &[1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 5, 0, 0, 0],
        )
        .await;

        assert_eq!(self.next().await.packet_type, tsgu::PKT_TUNNEL_AUTH);
        self.reply(tsgu::PKT_SERVICE_MESSAGE, &[2, 0, b'x', 0])
            .await;
        self.reply(tsgu::PKT_TUNNEL_AUTH_RESPONSE, &[0, 0, 0, 0, 0, 0, 0, 0])
            .await;

        let create = self.next().await;
        assert_eq!(create.packet_type, tsgu::PKT_CHANNEL_CREATE);
        self.reply(
            tsgu::PKT_CHANNEL_RESPONSE,
            &[0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0],
        )
        .await;
        // The host name, from its counted UTF-16 string.
        let units: Vec<u16> = create.body[8..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .take_while(|&u| u != 0)
            .collect();
        String::from_utf16(&units).unwrap()
    }
}

fn pair() -> (
    Inbound<tokio::io::ReadHalf<DuplexStream>>,
    tokio::io::WriteHalf<DuplexStream>,
    FakeGateway,
) {
    let (client, server) = tokio::io::duplex(1 << 20);
    let (read, write) = tokio::io::split(client);
    let inbound = Inbound {
        io: read,
        messages: ws::MessageReader::new(),
        packets: tsgu::Reader::default(),
    };
    let gateway = FakeGateway {
        io: server,
        messages: ws::MessageReader::new(),
        packets: tsgu::Reader::default(),
    };
    (inbound, write, gateway)
}

#[tokio::test]
async fn the_channel_opens_and_data_flows_both_ways() {
    let (mut inbound, mut write, mut gateway) = pair();
    let server = tokio::spawn(async move {
        let host = gateway.accept().await;
        // Echo one DATA packet, then close the channel.
        let data = gateway.next().await;
        assert_eq!(data.packet_type, tsgu::PKT_DATA);
        let payload = tsgu::parse_data(&data.body).unwrap().to_vec();
        let mut echo = (u16::try_from(payload.len()).unwrap())
            .to_le_bytes()
            .to_vec();
        echo.extend_from_slice(&payload);
        gateway.reply(tsgu::PKT_DATA, &echo).await;
        gateway.reply(tsgu::PKT_CLOSE_CHANNEL, &[0, 0, 0, 0]).await;
        (host, payload)
    });

    open_channel(
        &mut inbound,
        &mut write,
        "lvinzenz1.jjkdom.local",
        3389,
        "UwURDP",
    )
    .await
    .expect("channel");
    let (ours, theirs) = tokio::io::duplex(PIPE);
    tokio::spawn(pump(inbound, write, theirs));
    let (mut read, mut write) = tokio::io::split(ours);
    write.write_all(b"\x03\x00\x00\x13hello").await.unwrap();

    let mut echoed = Vec::new();
    // The gateway's close ends the stream after the echo.
    read.read_to_end(&mut echoed).await.unwrap();
    assert_eq!(echoed, b"\x03\x00\x00\x13hello");

    let (host, payload) = server.await.unwrap();
    assert_eq!(host, "lvinzenz1.jjkdom.local");
    assert_eq!(payload, b"\x03\x00\x00\x13hello");
}

#[tokio::test]
async fn a_refused_channel_names_the_policy() {
    let (mut inbound, mut write, mut gateway) = pair();
    tokio::spawn(async move {
        assert_eq!(
            gateway.next().await.packet_type,
            tsgu::PKT_HANDSHAKE_REQUEST
        );
        gateway
            .reply(
                tsgu::PKT_HANDSHAKE_RESPONSE,
                &[0, 0, 0, 0, 1, 0, 0, 0, 0, 0],
            )
            .await;
        assert_eq!(gateway.next().await.packet_type, tsgu::PKT_TUNNEL_CREATE);
        gateway
            .reply(tsgu::PKT_TUNNEL_RESPONSE, &[1, 0, 0, 0, 0, 0, 0, 0, 0, 0])
            .await;
        assert_eq!(gateway.next().await.packet_type, tsgu::PKT_TUNNEL_AUTH);
        gateway
            .reply(tsgu::PKT_TUNNEL_AUTH_RESPONSE, &[0, 0, 0, 0, 0, 0, 0, 0])
            .await;
        assert_eq!(gateway.next().await.packet_type, tsgu::PKT_CHANNEL_CREATE);
        // E_PROXY_RAP_ACCESSDENIED
        gateway
            .reply(
                tsgu::PKT_CHANNEL_RESPONSE,
                &[0xDA, 0x59, 0x07, 0x80, 0, 0, 0, 0],
            )
            .await;
        // Keep the pipe open until the client is done reading.
        let _ = gateway.io.read(&mut [0u8; 1]).await;
    });

    let err = open_channel(&mut inbound, &mut write, "other", 3389, "UwURDP")
        .await
        .expect_err("refused");
    match err {
        RdpError::Gateway { message } => assert!(message.contains("RD RAP"), "{message}"),
        other => panic!("unexpected {other:?}"),
    }
}

/// TLS to a real gateway with our certificate check, no login:
/// `UWURDP_LIVE_GATEWAY=gw.example.com cargo test -p uwurdp-core live_gateway -- --ignored`
#[tokio::test]
#[ignore = "needs a real RD Gateway (UWURDP_LIVE_GATEWAY)"]
async fn live_gateway_certificate_is_trusted() {
    let host = std::env::var("UWURDP_LIVE_GATEWAY").expect("UWURDP_LIVE_GATEWAY");
    let tcp = TcpStream::connect((host.as_str(), 443)).await.expect("tcp");
    let (_tls, der) = tls(tcp, &host).await.expect("trusted certificate");
    assert!(http::channel_binding(&der).starts_with(b"tls-server-end-point:"));
}

#[tokio::test]
async fn closing_the_rdp_side_closes_the_channel() {
    let (mut inbound, mut write, mut gateway) = pair();
    let server = tokio::spawn(async move {
        gateway.accept().await;
        gateway.next().await.packet_type
    });
    open_channel(&mut inbound, &mut write, "h", 3389, "c")
        .await
        .unwrap();
    let (ours, theirs) = tokio::io::duplex(PIPE);
    tokio::spawn(pump(inbound, write, theirs));
    drop(ours);
    assert_eq!(server.await.unwrap(), tsgu::PKT_CLOSE_CHANNEL);
}
