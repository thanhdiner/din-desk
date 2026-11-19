//! Decode Muse's newline-delimited JSON subscription without splitting UTF-8.
use serde_json::Value;

// User echo rows can repeat the permitted 4 MiB input several times. Bound
// decoding separately from the small correlation queues that discard the echo.
const MAX_RECORD_BYTES: usize = 32 * 1024 * 1024;

#[derive(Default)]
pub struct EventDecoder {
    buffer: Vec<u8>,
}

impl EventDecoder {
    pub fn push(&mut self, data: &[u8]) -> Result<Vec<Value>, &'static str> {
        let mut records = Vec::new();
        // Process one line at a time so a large batch of small records is allowed.
        for part in data.split_inclusive(|byte| *byte == b'\n') {
            let ends_line = part.last() == Some(&b'\n');
            let line_part = if ends_line {
                &part[..part.len() - 1]
            } else {
                part
            };
            if line_part.len() > MAX_RECORD_BYTES.saturating_sub(self.buffer.len()) {
                return Err("Muse event exceeds size limit");
            }
            self.buffer.extend_from_slice(line_part);
            if ends_line {
                if self.buffer.last() == Some(&b'\r') {
                    self.buffer.pop();
                }
                if !self.buffer.is_empty() {
                    records.push(
                        serde_json::from_slice(&self.buffer)
                            .map_err(|_| "invalid Muse event JSON")?,
                    );
                }
                self.buffer.clear();
            }
        }
        Ok(records)
    }

    pub fn finish(&self) -> Result<(), &'static str> {
        if self.buffer.is_empty() {
            Ok(())
        } else {
            Err("incomplete Muse event")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_split_multibyte_and_multiple_records() {
        let bytes = "{\"text\":\"xin chào\"}\n{\"ok\":true}\r\n".as_bytes();
        let cut = bytes.iter().position(|byte| *byte == 0xc3).unwrap() + 1;
        let mut decoder = EventDecoder::default();
        assert!(decoder.push(&bytes[..cut]).unwrap().is_empty());
        let records = decoder.push(&bytes[cut..]).unwrap();
        assert_eq!(records[0]["text"], "xin chào");
        assert_eq!(records[1]["ok"], true);
        assert!(decoder.finish().is_ok());
    }

    #[test]
    fn rejects_unfinished_and_oversized_records() {
        let mut decoder = EventDecoder::default();
        decoder.push(b"{").unwrap();
        assert!(decoder.finish().is_err());
        assert!(decoder.push(&vec![b'x'; MAX_RECORD_BYTES]).is_err());
    }

    #[test]
    fn accepts_large_user_echo_spanning_many_service_chunks() {
        let text = "x".repeat(2 * 1024 * 1024);
        let mut bytes = serde_json::json!({"event":"message.user","payload":{"message_id":"own","display_text":text,"payload":{"text":text},"chat_context":{"text":text}}}).to_string().into_bytes();
        bytes.push(b'\n');
        let mut decoder = EventDecoder::default();
        let mut result = Vec::new();
        for part in bytes.chunks(60 * 1024) {
            result.extend(decoder.push(part).unwrap());
        }
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["payload"]["message_id"], "own");
        assert_eq!(
            result[0]["payload"]["display_text"].as_str().unwrap().len(),
            text.len()
        );
        assert!(decoder.finish().is_ok());
    }
}
