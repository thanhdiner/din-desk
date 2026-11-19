//! Validate and demultiplex service response streams.
use super::protocol::{service_frame::Frame, Header, ServiceFrame};
use std::collections::HashMap;

const MAX_STREAMS: usize = 64;

#[derive(Debug, PartialEq)]
pub enum StreamEvent {
    Started {
        stream_id: i64,
        status: i32,
        headers: Vec<(String, String)>,
    },
    Data {
        stream_id: i64,
        data: Vec<u8>,
    },
    End {
        stream_id: i64,
    },
    Reset {
        stream_id: i64,
        code: i32,
    },
}

#[derive(Default)]
pub struct Streams {
    active: HashMap<i64, bool>,
}

impl Streams {
    /// Register a request before processing its response.
    pub fn register(&mut self, stream_id: i64) -> Result<(), &'static str> {
        if self.active.len() >= MAX_STREAMS {
            return Err("too many Muse streams");
        }
        if self.active.contains_key(&stream_id) {
            return Err("duplicate Muse stream ID");
        }
        self.active.insert(stream_id, false);
        Ok(())
    }

    pub fn consume(&mut self, frame: ServiceFrame) -> Result<Vec<StreamEvent>, &'static str> {
        let id = frame.stream_id;
        let started = self.active.get_mut(&id).ok_or("unknown Muse stream ID")?;
        let mut events = Vec::new();
        match frame.frame.ok_or("empty Muse service frame")? {
            Frame::Response(response) => {
                if *started {
                    return Err("duplicate Muse response");
                }
                *started = true;
                events.push(StreamEvent::Started {
                    stream_id: id,
                    status: response.status,
                    headers: response
                        .headers
                        .into_iter()
                        .map(|Header { key, value }| (key, value))
                        .collect(),
                });
                if !response.body.is_empty() {
                    events.push(StreamEvent::Data {
                        stream_id: id,
                        data: response.body,
                    });
                }
                if response.end_body {
                    self.active.remove(&id);
                    events.push(StreamEvent::End { stream_id: id });
                }
            }
            Frame::BodyChunk(chunk) => {
                if !*started {
                    return Err("Muse body arrived before response headers");
                }
                if !chunk.data.is_empty() {
                    events.push(StreamEvent::Data {
                        stream_id: id,
                        data: chunk.data,
                    });
                }
                if chunk.end_body {
                    self.active.remove(&id);
                    events.push(StreamEvent::End { stream_id: id });
                }
            }
            Frame::Reset(reset) => {
                self.active.remove(&id);
                events.push(StreamEvent::Reset {
                    stream_id: id,
                    code: reset.code,
                });
            }
            Frame::Request(_) => return Err("unexpected Muse request frame"),
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::muse::protocol::{ApplicationResponse, BodyChunk};

    #[test]
    fn handles_interleaved_streams_and_lifecycle() {
        let mut streams = Streams::default();
        streams.register(1).unwrap();
        streams.register(2).unwrap();
        let response = |id| ServiceFrame {
            stream_id: id,
            frame: Some(Frame::Response(ApplicationResponse {
                status: 200,
                headers: vec![],
                body: vec![],
                end_body: false,
            })),
        };
        streams.consume(response(1)).unwrap();
        streams.consume(response(2)).unwrap();
        let chunk = ServiceFrame {
            stream_id: 2,
            frame: Some(Frame::BodyChunk(BodyChunk {
                data: b"test".to_vec(),
                end_body: true,
            })),
        };
        assert_eq!(
            streams.consume(chunk).unwrap(),
            vec![
                StreamEvent::Data {
                    stream_id: 2,
                    data: b"test".to_vec()
                },
                StreamEvent::End { stream_id: 2 }
            ]
        );
        assert!(streams.consume(response(2)).is_err());
        assert!(streams.consume(response(1)).is_err());
    }
}
