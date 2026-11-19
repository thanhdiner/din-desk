//! Low-level Muse service streams over a single owned Noise connection.
use super::{
    protocol::{
        fragment, service_frame::Frame, ApplicationRequest, NoiseTransportFrame, Reassembler,
        Reset, ServiceFrame, ServiceRequest, ServiceResponse,
    },
    transport::NoiseSocket,
};
use prost::Message;
use std::error::Error;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Debug)]
pub struct ServiceError(pub i32);
impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Muse returned HTTP {}", self.0)
    }
}
impl Error for ServiceError {}

/// The single owner serializes Noise reads and writes to preserve nonce order.
pub struct MuseClient {
    socket: NoiseSocket,
    reassembler: Reassembler,
    next_stream: i64,
    next_chunk: i64,
}

impl MuseClient {
    /// Unary exchange, called only while no chat subscription is active.
    pub async fn request_json(&mut self, request: ApplicationRequest) -> Result<serde_json::Value> {
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            self.exchange_json(request),
        )
        .await?
    }

    async fn exchange_json(&mut self, request: ApplicationRequest) -> Result<serde_json::Value> {
        let id = self.start_request(request).await?;
        let mut started = false;
        let mut body = Vec::new();
        loop {
            let frame = self.receive().await?;
            if frame.stream_id != id {
                continue;
            }
            let (data, end) = match frame.frame {
                Some(Frame::Response(response)) if !started => {
                    if !(200..300).contains(&response.status) {
                        return Err(Box::new(ServiceError(response.status)));
                    }
                    started = true;
                    (response.body, response.end_body)
                }
                Some(Frame::BodyChunk(chunk)) if started => (chunk.data, chunk.end_body),
                _ => return Err("invalid Muse unary response".into()),
            };
            if body.len() + data.len() > 32 * 1024 * 1024 {
                return Err("Muse unary response too large".into());
            }
            body.extend(data);
            if end {
                let value: serde_json::Value = serde_json::from_slice(&body)?;
                if value.get("ok").and_then(|v| v.as_bool()) == Some(false) {
                    return Err("Muse rejected request".into());
                }
                return Ok(value.get("result").unwrap_or(&value).clone());
            }
        }
    }

    /// Check a cached connection before submitting a user message. Only this
    /// side-effect-free probe may be retried after reconnecting.
    pub async fn health_check(&mut self) -> Result<()> {
        let stream_id = self
            .start_request(ApplicationRequest {
                verb: "POST".into(),
                path: "/api/ping".into(),
                headers: Vec::new(),
                body: Vec::new(),
                end_body: true,
            })
            .await?;
        let mut started = false;
        loop {
            let frame = self.receive().await?;
            if frame.stream_id != stream_id {
                continue;
            }
            match frame.frame {
                Some(Frame::Response(response)) => {
                    if started || !(200..300).contains(&response.status) {
                        return Err("Muse health check was rejected".into());
                    }
                    started = true;
                    if response.end_body {
                        return Ok(());
                    }
                }
                Some(Frame::BodyChunk(chunk)) if started => {
                    if chunk.end_body {
                        return Ok(());
                    }
                }
                _ => return Err("invalid Muse health check response".into()),
            }
        }
    }

    pub fn new(socket: NoiseSocket) -> Self {
        Self {
            socket,
            reassembler: Reassembler::default(),
            next_stream: 1,
            next_chunk: 1,
        }
    }

    /// Starts a daemon service request; responses can interleave across stream IDs.
    pub async fn start_request(&mut self, request: ApplicationRequest) -> Result<i64> {
        tracing::debug!(path = %request.path, stream_id = self.next_stream, "sending Muse request");
        let stream_id = self.next_stream;
        self.next_stream = self
            .next_stream
            .checked_add(1)
            .ok_or("stream IDs exhausted")?;
        let frame = ServiceFrame {
            stream_id,
            frame: Some(Frame::Request(request)),
        };
        self.send_service_frame(frame).await?;
        Ok(stream_id)
    }

    pub async fn receive(&mut self) -> Result<ServiceFrame> {
        loop {
            let bytes = self.socket.receive().await?;
            let transport = NoiseTransportFrame::decode(bytes.as_slice())?;
            if let Some(bytes) = self.reassembler.push(transport)? {
                let response = ServiceResponse::decode(bytes.as_slice())?;
                let frame = ServiceFrame::decode(response.payload.as_slice())?;
                tracing::debug!(stream_id = frame.stream_id, "received Muse service frame");
                return Ok(frame);
            }
        }
    }

    pub async fn reset(&mut self, stream_id: i64) -> Result<()> {
        self.send_service_frame(ServiceFrame {
            stream_id,
            frame: Some(Frame::Reset(Reset {
                code: 0,
                reason: String::new(),
            })),
        })
        .await
    }

    async fn send_service_frame(&mut self, frame: ServiceFrame) -> Result<()> {
        let chunk_id = self.next_chunk;
        self.next_chunk = self
            .next_chunk
            .checked_add(1)
            .ok_or("chunk IDs exhausted")?;
        let request = ServiceRequest {
            service: 0,
            payload: frame.encode_to_vec(),
        };
        let payload = request.encode_to_vec();
        let chunks = fragment(chunk_id, &payload)?;
        tracing::debug!(
            bytes = payload.len(),
            chunks = chunks.len(),
            "sending Muse transport message"
        );
        for chunk in chunks {
            self.socket.send(&chunk.encode_to_vec()).await?;
        }
        Ok(())
    }
}
