//! Muse gateway protobuf envelopes and bounded transport-frame reassembly.
//!
//! Field numbers are derived from muse-cli's published Noise descriptors:
//! https://github.com/nikships/muse-cli/blob/main/docs/PROTOCOL.md
use std::{collections::HashMap, error::Error, fmt};

use prost::{Message, Oneof};

#[derive(Clone, PartialEq, Message)]
pub struct NoiseTransportFrame {
    #[prost(int64, tag = "1")]
    pub chunk_id: i64,
    #[prost(uint32, tag = "2")]
    pub chunk_index: u32,
    #[prost(uint32, tag = "3")]
    pub total_chunks: u32,
    #[prost(bytes = "vec", tag = "4")]
    pub payload: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ServiceRequest {
    #[prost(int32, tag = "1")]
    pub service: i32,
    #[prost(bytes = "vec", tag = "2")]
    pub payload: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ServiceResponse {
    #[prost(bytes = "vec", tag = "1")]
    pub payload: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ServiceFrame {
    #[prost(int64, tag = "1")]
    pub stream_id: i64,
    #[prost(oneof = "service_frame::Frame", tags = "2, 3, 4, 5")]
    pub frame: Option<service_frame::Frame>,
}

pub mod service_frame {
    use super::*;
    #[derive(Clone, PartialEq, Oneof)]
    pub enum Frame {
        #[prost(message, tag = "2")]
        Request(ApplicationRequest),
        #[prost(message, tag = "3")]
        Response(ApplicationResponse),
        #[prost(message, tag = "4")]
        BodyChunk(BodyChunk),
        #[prost(message, tag = "5")]
        Reset(Reset),
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct Header {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(string, tag = "2")]
    pub value: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct ApplicationRequest {
    #[prost(string, tag = "1")]
    pub verb: String,
    #[prost(string, tag = "2")]
    pub path: String,
    #[prost(message, repeated, tag = "3")]
    pub headers: Vec<Header>,
    #[prost(bytes = "vec", tag = "4")]
    pub body: Vec<u8>,
    #[prost(bool, tag = "5")]
    pub end_body: bool,
}

#[derive(Clone, PartialEq, Message)]
pub struct ApplicationResponse {
    #[prost(int32, tag = "1")]
    pub status: i32,
    #[prost(message, repeated, tag = "2")]
    pub headers: Vec<Header>,
    #[prost(bytes = "vec", tag = "3")]
    pub body: Vec<u8>,
    #[prost(bool, tag = "4")]
    pub end_body: bool,
}

#[derive(Clone, PartialEq, Message)]
pub struct BodyChunk {
    #[prost(bytes = "vec", tag = "1")]
    pub data: Vec<u8>,
    #[prost(bool, tag = "2")]
    pub end_body: bool,
}

#[derive(Clone, PartialEq, Message)]
pub struct Reset {
    #[prost(int32, tag = "1")]
    pub code: i32,
    #[prost(string, tag = "2")]
    pub reason: String,
}

const MAX_CHUNKS: u32 = 2048;
const MAX_PENDING: usize = 32;
pub(super) const MAX_BUFFERED_BYTES: usize = 64 * 1024 * 1024;
// Leave room for the protobuf transport envelope inside each Noise plaintext
// (65,519 bytes). Chunk binary service bytes, not UTF-8/JSON or WebSocket data.
const MAX_CHUNK_PAYLOAD: usize = 60 * 1024;

#[derive(Debug)]
pub struct FramingError(&'static str);
impl fmt::Display for FramingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl Error for FramingError {}

/// Fragment one encoded ServiceRequest. All chunks share an ID; only the full
/// concatenated payload is a protobuf service message. Bounds are checked before
/// yielding the first chunk so an oversized request is never partially sent.
pub fn fragment(
    chunk_id: i64,
    payload: &[u8],
) -> Result<impl ExactSizeIterator<Item = NoiseTransportFrame> + '_, FramingError> {
    if payload.len() > MAX_BUFFERED_BYTES {
        return Err(FramingError("outbound transport message exceeds 64 MiB"));
    }
    let total_chunks = payload.len().div_ceil(MAX_CHUNK_PAYLOAD).max(1) as u32;
    Ok((0..total_chunks).map(move |chunk_index| {
        let start = chunk_index as usize * MAX_CHUNK_PAYLOAD;
        let end = (start + MAX_CHUNK_PAYLOAD).min(payload.len());
        NoiseTransportFrame {
            chunk_id,
            chunk_index,
            total_chunks,
            payload: payload[start..end].to_vec(),
        }
    }))
}

struct Pending {
    parts: Vec<Option<Vec<u8>>>,
    received: usize,
    bytes: usize,
}

/// Collects interleaved, potentially out-of-order chunks for one connection.
#[derive(Default)]
pub struct Reassembler {
    pending: HashMap<i64, Pending>,
    buffered_bytes: usize,
}

impl Reassembler {
    pub fn push(
        &mut self,
        mut frame: NoiseTransportFrame,
    ) -> Result<Option<Vec<u8>>, FramingError> {
        // Some peers omit the default-valued count for a single chunk.
        if frame.total_chunks == 0 {
            frame.total_chunks = 1;
        }
        if frame.total_chunks > MAX_CHUNKS || frame.chunk_index >= frame.total_chunks {
            return Err(FramingError("invalid transport chunk coordinates"));
        }
        if frame.payload.len() > MAX_BUFFERED_BYTES.saturating_sub(self.buffered_bytes) {
            return Err(FramingError("transport buffer limit exceeded"));
        }
        if !self.pending.contains_key(&frame.chunk_id) && self.pending.len() >= MAX_PENDING {
            return Err(FramingError("too many incomplete transport messages"));
        }
        let pending = self
            .pending
            .entry(frame.chunk_id)
            .or_insert_with(|| Pending {
                parts: vec![None; frame.total_chunks as usize],
                received: 0,
                bytes: 0,
            });
        if pending.parts.len() != frame.total_chunks as usize {
            return Err(FramingError("inconsistent transport chunk count"));
        }
        let slot = &mut pending.parts[frame.chunk_index as usize];
        if slot.is_some() {
            return Err(FramingError("duplicate transport chunk"));
        }
        pending.bytes += frame.payload.len();
        self.buffered_bytes += frame.payload.len();
        pending.received += 1;
        *slot = Some(frame.payload);
        if pending.received != pending.parts.len() {
            return Ok(None);
        }
        let pending = self.pending.remove(&frame.chunk_id).expect("entry exists");
        self.buffered_bytes -= pending.bytes;
        let mut payload = Vec::with_capacity(pending.bytes);
        for part in pending.parts {
            payload.extend(part.expect("all chunks received"));
        }
        Ok(Some(payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(id: i64, index: u32, total: u32, payload: &[u8]) -> NoiseTransportFrame {
        NoiseTransportFrame {
            chunk_id: id,
            chunk_index: index,
            total_chunks: total,
            payload: payload.to_vec(),
        }
    }
    #[test]
    fn reassembles_interleaved_out_of_order_messages() {
        let mut r = Reassembler::default();
        assert!(r.push(frame(1, 1, 2, b"b")).unwrap().is_none());
        assert!(r.push(frame(2, 0, 2, b"x")).unwrap().is_none());
        assert_eq!(r.push(frame(1, 0, 2, b"a")).unwrap().unwrap(), b"ab");
        assert_eq!(r.push(frame(2, 1, 2, b"y")).unwrap().unwrap(), b"xy");
    }
    #[test]
    fn rejects_invalid_or_duplicate_chunks() {
        let mut r = Reassembler::default();
        assert!(r.push(frame(1, 2, 2, b"x")).is_err());
        assert!(r.push(frame(1, 0, 2, b"x")).unwrap().is_none());
        assert!(r.push(frame(1, 0, 2, b"x")).is_err());
        assert!(r.push(frame(1, 1, 3, b"y")).is_err());
    }

    #[test]
    fn outgoing_chunks_fit_noise_and_reassemble_at_boundaries_without_corruption() {
        for length in [
            0,
            1,
            MAX_CHUNK_PAYLOAD - 1,
            MAX_CHUNK_PAYLOAD,
            MAX_CHUNK_PAYLOAD + 1,
            512 * 1024,
            MAX_BUFFERED_BYTES,
        ] {
            let payload: Vec<u8> = (0..length).map(|index| (index % 251) as u8).collect();
            let chunks: Vec<_> = fragment(i64::MAX, &payload).unwrap().collect();
            assert_eq!(chunks.len(), length.div_ceil(MAX_CHUNK_PAYLOAD).max(1));
            let mut assembler = Reassembler::default();
            let mut completed = None;
            // The wire supports reordering as well as sequential transmission.
            for chunk in chunks.into_iter().rev() {
                assert!(chunk.encoded_len() <= super::super::transport::MAX_PLAINTEXT);
                assert_eq!(chunk.chunk_id, i64::MAX);
                if let Some(bytes) = assembler.push(chunk).unwrap() {
                    completed = Some(bytes);
                }
            }
            assert_eq!(completed.unwrap(), payload);
        }
        assert!(fragment(1, &vec![0; MAX_BUFFERED_BYTES + 1]).is_err());
    }
}
