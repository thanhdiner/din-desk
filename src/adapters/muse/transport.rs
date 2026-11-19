//! Noise XX over an outbound WebSocket. A connection has exactly one owner.
use std::{error::Error, time::Duration};

use futures_util::{SinkExt, StreamExt};
#[cfg(test)]
use prost::Message as _;
use snow::{params::NoiseParams, Builder, TransportState};
use tokio_tungstenite::{client_async_tls, tungstenite::Message, MaybeTlsStream, WebSocketStream};

type Socket = WebSocketStream<MaybeTlsStream<super::proxy::Stream>>;
type TransportResult<T> = Result<T, Box<dyn Error + Send + Sync>>;
const MAX_MESSAGE: usize = 65_535;
const AEAD_TAG: usize = 16;
pub(super) const MAX_PLAINTEXT: usize = MAX_MESSAGE - AEAD_TAG;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
const GATEWAY_ATTEMPTS: usize = 3;

/// The handshake response may contain credentials in its URL, headers or body.
/// Retain only the HTTP status at the application boundary.
#[derive(Debug)]
pub struct GatewayError(pub u16);

impl std::fmt::Display for GatewayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Muse gateway returned HTTP {} before chat submission", self.0)
    }
}

impl Error for GatewayError {}

#[cfg(test)]
#[derive(prost::Message)]
struct ServerHandshakePayload {
    #[prost(int32, tag = "2")]
    recovery_state: i32,
    #[prost(bytes = "vec", tag = "3")]
    nonce: Vec<u8>,
}

fn validate_handshake_payload(payload: &[u8]) -> TransportResult<()> {
    if payload.is_empty() {
        return Ok(());
    }
    // Standard browser gateways may use field 2 as a byte nonce. Recovery
    // gateways use a varint at the same tag, so inspect the wire type first.
    let mut bytes = payload;
    while !bytes.is_empty() {
        let (tag, wire) = prost::encoding::decode_key(&mut bytes)?;
        if tag == 2 && wire == prost::encoding::WireType::Varint {
            if prost::encoding::decode_varint(&mut bytes)? != 0 {
                return Err("Muse confidential VM requires recovery credentials; unsupported handshake mode".into());
            }
        } else {
            prost::encoding::skip_field(
                wire,
                tag,
                &mut bytes,
                prost::encoding::DecodeContext::default(),
            )?;
        }
    }
    Ok(())
}

pub struct NoiseSocket {
    socket: Socket,
    noise: TransportState,
}

impl NoiseSocket {
    /// The caller must not log `url`: Muse puts the gateway token in its query.
    pub async fn connect(url: &str) -> TransportResult<Self> {
        Self::connect_via(url, &super::proxy::AccountProxy::default()).await
    }

    pub async fn connect_via(
        url: &str,
        proxy: &super::proxy::AccountProxy,
    ) -> TransportResult<Self> {
        tokio::time::timeout(HANDSHAKE_TIMEOUT, Self::handshake(url, proxy)).await?
    }

    async fn handshake(url: &str, proxy: &super::proxy::AccountProxy) -> TransportResult<Self> {
        let mut socket = Self::open_websocket(url, proxy).await?;
        let params: NoiseParams = "Noise_XX_25519_AESGCM_SHA256".parse()?;
        let builder = Builder::new(params);
        let static_key = builder.generate_keypair()?;
        let mut handshake = builder
            .local_private_key(&static_key.private)?
            .build_initiator()?;
        let mut scratch = vec![0; MAX_MESSAGE];
        let size = handshake.write_message(&[], &mut scratch)?;
        socket
            .send(Message::Binary(scratch[..size].to_vec().into()))
            .await?;
        let response = read_binary(&mut socket).await?;
        let payload_size = handshake.read_message(&response, &mut scratch)?;
        validate_handshake_payload(&scratch[..payload_size])?;
        let size = handshake.write_message(&[], &mut scratch)?;
        socket
            .send(Message::Binary(scratch[..size].to_vec().into()))
            .await?;
        Ok(Self {
            socket,
            noise: handshake.into_transport_mode()?,
        })
    }

    async fn open_websocket(url: &str, proxy: &super::proxy::AccountProxy) -> TransportResult<Socket> {
        let target = reqwest::Url::parse(url).map_err(|_| "Invalid Muse gateway address")?;
        let host = target.host_str().ok_or("Missing Muse gateway host")?;
        let port = target
            .port_or_known_default()
            .ok_or("Missing Muse gateway port")?;
        for attempt in 1..=GATEWAY_ATTEMPTS {
            let stream = proxy.connect(host, port).await?;
            match client_async_tls(url, stream).await {
                Ok((socket, _)) => return Ok(socket),
                Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                    let status = response.status().as_u16();
                    if matches!(status, 502 | 503 | 504) && attempt < GATEWAY_ATTEMPTS {
                        // No Noise frame, conversation request or user message was sent.
                        // Keep the account/proxy fixed and share the outer 20-second deadline.
                        tracing::warn!(status, attempt, "Muse gateway unavailable; retrying WebSocket before chat submission");
                        tokio::time::sleep(Duration::from_millis(250 * attempt as u64)).await;
                    } else {
                        return Err(Box::new(GatewayError(status)));
                    }
                }
                Err(_) => return Err("Muse WebSocket connection failed".into()),
            }
        }
        unreachable!("gateway attempts are bounded")
    }

    pub async fn send(&mut self, plaintext: &[u8]) -> TransportResult<()> {
        if plaintext.len() > MAX_PLAINTEXT {
            return Err("Noise plaintext is too large".into());
        }
        let mut ciphertext = vec![0; plaintext.len() + AEAD_TAG];
        let size = self.noise.write_message(plaintext, &mut ciphertext)?;
        ciphertext.truncate(size);
        self.socket.send(Message::Binary(ciphertext.into())).await?;
        Ok(())
    }

    pub async fn receive(&mut self) -> TransportResult<Vec<u8>> {
        let ciphertext = read_binary(&mut self.socket).await?;
        let mut plaintext = vec![0; ciphertext.len()];
        let size = self.noise.read_message(&ciphertext, &mut plaintext)?;
        plaintext.truncate(size);
        Ok(plaintext)
    }
}

async fn read_binary(socket: &mut Socket) -> TransportResult<Vec<u8>> {
    while let Some(message) = socket.next().await {
        match message? {
            Message::Binary(data) if data.len() <= MAX_MESSAGE => return Ok(data.to_vec()),
            Message::Binary(_) => return Err("Noise message is too large".into()),
            Message::Close(_) => return Err("Muse WebSocket closed".into()),
            Message::Text(_) => return Err("expected binary Noise message".into()),
            _ => {}
        }
    }
    Err("Muse WebSocket ended".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn retries_only_rejected_upgrades_before_sending_any_application_data() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for attempt in 0..3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                if attempt < 2 {
                    let mut request = Vec::new();
                    use tokio::io::AsyncReadExt;
                    while !request.ends_with(b"\r\n\r\n") {
                        request.push(stream.read_u8().await.unwrap());
                    }
                    let request = String::from_utf8(request).unwrap();
                    assert!(request.starts_with("GET /v1/noise?auth_token=fixture-private HTTP/1.1"));
                    stream.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                    stream.shutdown().await.unwrap();
                } else {
                    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                    let Message::Binary(payload) = socket.next().await.unwrap().unwrap() else { panic!("binary payload expected") };
                    assert_eq!(payload.as_ref(), b"application data once");
                }
            }
        });
        let mut socket = NoiseSocket::open_websocket(
            &format!("ws://{address}/v1/noise?auth_token=fixture-private"),
            &super::super::proxy::AccountProxy::default(),
        ).await.unwrap();
        socket.send(Message::Binary(b"application data once".to_vec().into())).await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn bounds_gateway_retries_and_preserves_only_nonsecret_status() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for status in [401, 403, 429, 502, 503, 504] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let expected = if matches!(status, 502 | 503 | 504) { 3 } else { 1 };
            let server = tokio::spawn(async move {
                for _ in 0..expected {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        request.push(stream.read_u8().await.unwrap());
                    }
                    let response = format!("HTTP/1.1 {status} Rejected\r\nContent-Length: 15\r\nSet-Cookie: fixture-private\r\nConnection: close\r\n\r\nfixture-private");
                    stream.write_all(response.as_bytes()).await.unwrap();
                    stream.shutdown().await.unwrap();
                }
                assert!(tokio::time::timeout(Duration::from_millis(100), listener.accept()).await.is_err());
            });
            let error = NoiseSocket::open_websocket(
                &format!("ws://{address}/?auth_token=fixture-private"),
                &super::super::proxy::AccountProxy::default(),
            ).await.err().expect("gateway must reject the upgrade");
            assert_eq!(error.downcast_ref::<GatewayError>().unwrap().0, status);
            assert!(!format!("{error:?}").contains("fixture-private"));
            let public = super::super::session::client_error(error.as_ref());
            assert!(public.contains(&status.to_string()));
            assert!(!public.contains("fixture-private"));
            server.await.unwrap();
        }
    }

    #[test]
    fn distinguishes_standard_metadata_from_recovery_mode() {
        let mut browser_standard = vec![0x08, 0x00, 0x12, 0x20];
        browser_standard.extend([1; 32]);
        assert!(validate_handshake_payload(&browser_standard).is_ok());
        let standard = ServerHandshakePayload {
            recovery_state: 0,
            nonce: vec![1; 32],
        }
        .encode_to_vec();
        assert!(validate_handshake_payload(&standard).is_ok());
        for recovery_state in [1, 2, 3] {
            let metadata = ServerHandshakePayload {
                recovery_state,
                nonce: vec![1; 32],
            }
            .encode_to_vec();
            assert!(validate_handshake_payload(&metadata).is_err());
        }
    }

    #[tokio::test]
    async fn performs_xx_handshake_and_encrypted_round_trip() {
        for proxied in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_hdr_async(
                    stream,
                    |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                     response| {
                        assert!(!request.headers().contains_key("proxy-authorization"));
                        Ok(response)
                    },
                )
                .await
                .unwrap();
                let params: NoiseParams = "Noise_XX_25519_AESGCM_SHA256".parse().unwrap();
                let builder = Builder::new(params);
                let key = builder.generate_keypair().unwrap();
                let mut noise = builder
                    .local_private_key(&key.private)
                    .unwrap()
                    .build_responder()
                    .unwrap();
                let mut buffer = vec![0; MAX_MESSAGE];
                let Message::Binary(first) = ws.next().await.unwrap().unwrap() else {
                    panic!("expected handshake")
                };
                noise.read_message(&first, &mut buffer).unwrap();
                let metadata = ServerHandshakePayload {
                    recovery_state: 0,
                    nonce: vec![1; 32],
                }
                .encode_to_vec();
                let len = noise.write_message(&metadata, &mut buffer).unwrap();
                ws.send(Message::Binary(buffer[..len].to_vec().into()))
                    .await
                    .unwrap();
                let Message::Binary(third) = ws.next().await.unwrap().unwrap() else {
                    panic!("expected handshake")
                };
                noise.read_message(&third, &mut buffer).unwrap();
                let mut noise = noise.into_transport_mode().unwrap();
                let Message::Binary(data) = ws.next().await.unwrap().unwrap() else {
                    panic!("expected data")
                };
                let len = noise.read_message(&data, &mut buffer).unwrap();
                assert_eq!(&buffer[..len], b"hello");
                let len = noise.write_message(b"world", &mut buffer).unwrap();
                ws.send(Message::Binary(buffer[..len].to_vec().into()))
                    .await
                    .unwrap();
            });
            let mut relay = None;
            let mut socket = if proxied {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let proxy_address = proxy_listener.local_addr().unwrap();
                relay = Some(tokio::spawn(async move {
                    let (mut inbound, _) = proxy_listener.accept().await.unwrap();
                    let mut headers = Vec::new();
                    while !headers.ends_with(b"\r\n\r\n") {
                        headers.push(inbound.read_u8().await.unwrap());
                    }
                    let headers = String::from_utf8(headers).unwrap();
                    assert!(headers.starts_with("CONNECT no-dns-gateway.invalid:80 HTTP/1.1"));
                    assert!(headers.contains("Proxy-Authorization: Basic dXNlcjpwQHNz"));
                    assert!(!headers.contains("auth_token"));
                    let mut upstream = tokio::net::TcpStream::connect(address).await.unwrap();
                    inbound
                        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                        .await
                        .unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut upstream).await;
                }));
                let proxy = super::super::proxy::AccountProxy::parse(&format!(
                    "http://user:p%40ss@{proxy_address}"
                ))
                .unwrap();
                NoiseSocket::connect_via(
                    "ws://no-dns-gateway.invalid/?auth_token=fake-gateway-token",
                    &proxy,
                )
                .await
                .unwrap()
            } else {
                NoiseSocket::connect(&format!("ws://{address}"))
                    .await
                    .unwrap()
            };
            socket.send(b"hello").await.unwrap();
            assert_eq!(socket.receive().await.unwrap(), b"world");
            server.await.unwrap();
            if let Some(relay) = relay {
                relay.abort();
            }
        }
    }
}
