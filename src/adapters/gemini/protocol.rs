//! Gemini Web wire format. Keep protocol details outside the application core.
use serde_json::{json, Value};

pub const MAX_RESPONSE: usize = 16 * 1024 * 1024;

pub fn bootstrap_field(html: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":");
    for (index, _) in html.match_indices(&needle) {
        let suffix = &html[index + needle.len()..];
        if let Some(Ok(Value::String(value))) =
            serde_json::Deserializer::from_str(suffix.trim_start())
                .into_iter::<Value>()
                .next()
        {
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

pub fn generate_body(prompt: &str, metadata: &Value, refs: &[Value], request_id: &str) -> Value {
    let mut fields = vec![Value::Null; 69];
    fields[0] = json!([
        prompt,
        0,
        null,
        if refs.is_empty() {
            Value::Null
        } else {
            json!(refs)
        },
        null,
        null,
        0
    ]);
    fields[1] = json!(["en"]);
    fields[2] = metadata.clone();
    for index in [6, 41] {
        fields[index] = json!([1]);
    }
    for index in [7, 10, 27] {
        fields[index] = json!(1);
    }
    for index in [11, 18, 53] {
        fields[index] = json!(0);
    }
    fields[17] = json!([[0]]);
    fields[30] = json!([4]);
    fields[59] = json!(request_id);
    fields[61] = json!([]);
    fields[68] = json!(2);
    json!([null, serde_json::to_string(&fields).expect("JSON fields")])
}

pub fn fresh_metadata() -> Value {
    json!(["", "", "", null, null, null, null, null, null, ""])
}

/// Parse complete JSON values rather than trusting length units: UTF-8/UTF-16
/// length prefixes differ for Unicode. The total received bytes remain bounded.
#[derive(Default)]
pub struct Frames {
    buffer: Vec<u8>,
    received: usize,
}
impl Frames {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, String> {
        self.received += bytes.len();
        if self.received > MAX_RESPONSE {
            return Err("Gemini response exceeds 16 MiB".into());
        }
        self.buffer.extend_from_slice(bytes);
        let mut values = Vec::new();
        loop {
            let blank = self
                .buffer
                .iter()
                .take_while(|c| c.is_ascii_whitespace())
                .count();
            self.buffer.drain(..blank);
            if self.buffer.is_empty() {
                break;
            }
            if self.buffer[0] == b')' || self.buffer[0].is_ascii_digit() {
                let Some(end) = self.buffer.iter().position(|c| *c == b'\n') else {
                    if self.buffer.len() > 64 {
                        return Err("Invalid Gemini stream prefix".into());
                    }
                    break;
                };
                let prefix = &self.buffer[..end];
                if prefix != b")]}'" && !prefix.iter().all(u8::is_ascii_digit) {
                    return Err("Invalid Gemini stream framing".into());
                }
                self.buffer.drain(..=end);
                continue;
            }
            let mut decoder =
                serde_json::Deserializer::from_slice(&self.buffer).into_iter::<Value>();
            match decoder.next() {
                Some(Ok(value)) if value.is_array() => {
                    let consumed = decoder.byte_offset();
                    values.push(value);
                    self.buffer.drain(..consumed);
                }
                Some(Err(error)) if error.is_eof() => break,
                _ => return Err("Invalid Gemini response frame".into()),
            }
        }
        Ok(values)
    }
    pub fn finish(&self) -> Result<(), String> {
        if self.buffer.iter().all(u8::is_ascii_whitespace) {
            Ok(())
        } else {
            Err("Gemini stream ended with an incomplete frame; no tools dispatched".into())
        }
    }
}

#[derive(Default)]
pub struct Reply {
    pub text: String,
    pub metadata: Option<Value>,
    candidate: Option<String>,
    completed: bool,
}
impl Reply {
    pub fn accept(&mut self, frame: Value) -> Result<(), String> {
        for row in frame.as_array().ok_or("Invalid Gemini frame")? {
            match row.get(0).and_then(Value::as_str) {
                Some("er") => {
                    return Err("Gemini rejected the request; check account access/quota".into())
                }
                Some("wrb.fr") => {}
                _ => continue,
            }
            let Some(encoded) = row.get(2).and_then(Value::as_str) else {
                continue;
            };
            let payload: Value =
                serde_json::from_str(encoded).map_err(|_| "Invalid Gemini payload")?;
            if let Some(meta) = payload.get(1).filter(|v| v.is_array()) {
                self.metadata = Some(meta.clone());
            }
            if let Some(token) = payload
                .get(25)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                let mut metadata = self.metadata.clone().unwrap_or_else(fresh_metadata);
                if let Some(items) = metadata.as_array_mut() {
                    items.resize(items.len().max(10), Value::Null);
                    items[9] = json!(token);
                }
                self.metadata = Some(metadata);
            }
            let Some(candidate) = payload
                .get(4)
                .and_then(Value::as_array)
                .and_then(|items| items.first())
            else {
                continue;
            };
            let Some(candidate_id) = candidate
                .get(0)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            if self
                .candidate
                .as_deref()
                .is_some_and(|old| old != candidate_id)
            {
                return Err("Gemini changed response candidate; no tools dispatched".into());
            }
            self.candidate = Some(candidate_id.to_owned());
            if let Some(text) = candidate.pointer("/1/0").and_then(Value::as_str) {
                self.text = text.to_owned();
            }
            if candidate.pointer("/8/0").and_then(Value::as_i64) == Some(2) {
                self.completed = true;
            }
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<(String, Value), String> {
        if !self.completed || self.text.is_empty() {
            return Err("Gemini did not confirm a complete reply; no tools dispatched".into());
        }
        let mut metadata = self
            .metadata
            .take()
            .ok_or("Gemini did not return conversation metadata")?;
        let items = metadata
            .as_array_mut()
            .ok_or("Invalid Gemini conversation metadata")?;
        if !items
            .first()
            .and_then(Value::as_str)
            .is_some_and(|v| v.starts_with("c_"))
            && !items
                .get(9)
                .and_then(Value::as_str)
                .is_some_and(|v| !v.is_empty())
        {
            return Err("Gemini conversation could not be identified; no tools dispatched".into());
        }
        if let Some(candidate) = self.candidate {
            items.resize(items.len().max(3), Value::Null);
            items[2] = json!(candidate);
        }
        Ok((self.text, metadata))
    }
}
