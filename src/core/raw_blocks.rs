//! Experimental nonce-framed code. The complete ordinary tool/schema validator
//! still runs after decoding; nothing is dispatched while blocks are arriving.
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub fn instruction(nonce: &str) -> String {
    format!("RAW PAYLOAD FORMAT: Return one JSON manifest optionally followed by raw UTF-8 blocks, without markdown fences. For text.text, Write.input.content, or Edit.input.old_string/new_string ONLY, you may replace a string with {{\"$muse_blob\":\"p0\"}} and append:\n@@MUSE_BLOB:{nonce}:p0@@\nraw content here\n@@MUSE_END:{nonce}:p0@@\nUse distinct short alphanumeric IDs. The newline after the opening marker and before the closing marker is framing, not content; retain intended trailing newlines inside the content. Keep bridge_id and all other JSON/schema fields unchanged. No extra prose or unused/missing blocks. The reference objects are transport placeholders resolved to strings BEFORE input_schema validation; their temporary object shape is allowed in the manifest. Each block must be referenced once. Use raw blocks for multiline Write/Edit content. Ordinary JSON strings remain valid for small values and format repair.")
}
pub fn decode(output: &str, nonce: &str) -> Result<Value, String> {
    let marker = "\n@@MUSE_BLOB:";
    let (manifest, mut remaining) = output.split_once(marker).unwrap_or((output, ""));
    let mut value: Value = serde_json::from_str(super::agent_protocol::json_body(manifest))
        .map_err(|error| {
            tracing::warn!(
                stage = "manifest",
                opening_markers = output.matches("@@MUSE_BLOB:").count(),
                closing_markers = output.matches("@@MUSE_END:").count(),
                line = error.line(),
                column = error.column(),
                "Experimental raw format rejected (content omitted)"
            );
            super::agent_protocol::INVALID_JSON.to_string()
        })?;
    if value.get("bridge_id").and_then(Value::as_str) != Some(nonce) {
        return Err("Muse agent response has a missing or mismatched bridge_id".into());
    }
    let mut blobs = HashMap::new();
    while !remaining.is_empty() {
        let (header, body) = remaining
            .split_once("@@\n")
            .ok_or(super::agent_protocol::INVALID_JSON)?;
        let (owner, id) = header
            .split_once(':')
            .ok_or(super::agent_protocol::INVALID_JSON)?;
        if owner != nonce
            || id.is_empty()
            || id.len() > 32
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || blobs.len() >= 64
        {
            return Err(super::agent_protocol::INVALID_JSON.into());
        }
        let end = format!("\n@@MUSE_END:{nonce}:{id}@@");
        let (content, tail) = body
            .split_once(&end)
            .ok_or(super::agent_protocol::INVALID_JSON)?;
        if blobs.insert(id.to_owned(), content.to_owned()).is_some() {
            return Err(super::agent_protocol::INVALID_JSON.into());
        }
        remaining = if tail.trim().is_empty() {
            ""
        } else {
            tail.strip_prefix(marker)
                .ok_or(super::agent_protocol::INVALID_JSON)?
        };
    }
    let mut used = HashSet::new();
    if let Some(blocks) = value.get_mut("content").and_then(Value::as_array_mut) {
        for block in blocks {
            if block["type"] == "text" {
                if let Some(text) = block.get_mut("text") {
                    resolve(text, &blobs, &mut used)?;
                }
            } else if block["type"] == "tool_use" {
                let fields: &[&str] = match block["name"].as_str() {
                    Some("Write") => &["content"],
                    Some("Edit") => &["old_string", "new_string"],
                    _ => &[],
                };
                for field in fields {
                    if let Some(value) = block
                        .get_mut("input")
                        .and_then(|input| input.get_mut(*field))
                    {
                        resolve(value, &blobs, &mut used)?;
                    }
                }
            }
        }
    }
    if used.len() != blobs.len() {
        return Err(super::agent_protocol::INVALID_JSON.into());
    }
    if !blobs.is_empty() {
        tracing::info!(
            blocks = blobs.len(),
            "Decoded experimental raw agent payloads before schema validation"
        );
    }
    Ok(value)
}
fn resolve(
    value: &mut Value,
    blobs: &HashMap<String, String>,
    used: &mut HashSet<String>,
) -> Result<(), String> {
    if let Some(object) = value.as_object().filter(|object| object.len() == 1) {
        if let Some(id) = object.get("$muse_blob").and_then(Value::as_str) {
            let text = blobs.get(id).ok_or(super::agent_protocol::INVALID_JSON)?;
            if !used.insert(id.to_owned()) {
                return Err(super::agent_protocol::INVALID_JSON.into());
            }
            *value = Value::String(text.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn manifest() -> String {
        json!({"bridge_id":"n","content":[{"type":"tool_use","name":"Write","input":{"file_path":"a.rs","content":{"$muse_blob":"p0"}}}]}).to_string()
    }
    fn frame(text: &str) -> String {
        format!("\n@@MUSE_BLOB:n:p0@@\n{text}\n@@MUSE_END:n:p0@@")
    }
    #[test]
    fn preserves_quotes_unicode_newlines_and_ordinary_json() {
        for text in ["", "fn main() { println!(\"🦀\"); }\n", "line\r\nend\n\n"] {
            assert_eq!(
                decode(&(manifest() + &frame(text)), "n").unwrap()["content"][0]["input"]
                    ["content"],
                text
            );
        }
        let ordinary = json!({"bridge_id":"n","content":[{"type":"text","text":"ok"}]});
        assert_eq!(decode(&ordinary.to_string(), "n").unwrap(), ordinary);
    }
    #[test]
    fn rejects_missing_duplicate_unused_foreign_and_unterminated_blocks() {
        for value in [
            manifest(),
            manifest() + &frame("x") + &frame("y"),
            manifest() + "\n@@MUSE_BLOB:n:p0@@\nincomplete",
            manifest() + &frame("x").replace("BLOB:n:", "BLOB:foreign:"),
            json!({"bridge_id":"n","content":[{"type":"text","text":"ok"}]}).to_string()
                + &frame("unused"),
        ] {
            assert!(decode(&value, "n").is_err());
        }
    }
}
