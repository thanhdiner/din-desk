//! Explicit compatibility policy: Muse owns history; only the latest user turn
//! is submitted. Unsupported capabilities must not silently look successful.
use serde_json::Value;

pub struct ChatRequest {
    pub conversation_id: Option<String>,
    pub text: String,
    pub stream: bool,
    pub include_usage: bool,
}

pub fn parse(value: Value) -> Result<ChatRequest, String> {
    let object = value.as_object().ok_or("Request must be a JSON object")?;
    if object.get("model").and_then(Value::as_str) != Some("muse") {
        return Err("Only model muse is supported".into());
    }
    let boolean = |value: Option<&Value>, name: &str| -> Result<bool, String> {
        match value {
            None => Ok(false),
            Some(value) => value
                .as_bool()
                .ok_or_else(|| format!("{name} must be a boolean")),
        }
    };
    let stream = boolean(object.get("stream"), "stream")?;
    let include_usage = match object.get("stream_options") {
        None | Some(Value::Null) => false,
        Some(options) if options.is_object() => {
            boolean(options.get("include_usage"), "stream_options.include_usage")?
        }
        _ => return Err("stream_options must be an object".into()),
    };
    for field in ["tools", "functions", "stop"] {
        if object
            .get(field)
            .is_some_and(|v| !v.is_null() && !v.as_array().is_some_and(Vec::is_empty))
        {
            return Err(format!("{field} is not supported"));
        }
    }
    for field in ["tool_choice", "function_call"] {
        if object
            .get(field)
            .is_some_and(|v| !v.is_null() && v.as_str() != Some("none"))
        {
            return Err(format!("{field} is not supported"));
        }
    }
    if object
        .get("response_format")
        .is_some_and(|v| !v.is_null() && *v != serde_json::json!({"type":"text"}))
    {
        return Err("Only text response_format is supported".into());
    }
    if object
        .get("modalities")
        .is_some_and(|v| !v.is_null() && *v != serde_json::json!(["text"]))
    {
        return Err("Only text modalities are supported".into());
    }
    if object.get("audio").is_some_and(|v| !v.is_null()) {
        return Err("audio is not supported".into());
    }
    if object
        .get("logprobs")
        .is_some_and(|v| !v.is_null() && v.as_bool() != Some(false))
    {
        return Err("logprobs is not supported".into());
    }
    if object.get("n").is_some_and(|v| v.as_u64() != Some(1)) {
        return Err("Only n=1 is supported".into());
    }
    let conversation_id = match object.get("conversation_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(id))
            if id.starts_with("conv_")
                && id.len() == 37
                && id[5..].bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            Some(id.clone())
        }
        _ => {
            return Err(
                "conversation_id must be conv_ followed by 32 hexadecimal characters".into(),
            )
        }
    };
    let messages = object
        .get("messages")
        .and_then(Value::as_array)
        .filter(|messages| !messages.is_empty())
        .ok_or("messages must be a nonempty array")?;
    for message in messages {
        if !message.is_object()
            || !matches!(
                message.get("role").and_then(Value::as_str),
                Some("user" | "assistant" | "system" | "developer")
            )
        {
            return Err("Only user/assistant/system/developer text messages are supported".into());
        }
    }
    let last = messages
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .ok_or("messages has no user message")?;
    let text = match last.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => {
            let mut texts = Vec::new();
            for part in parts {
                if !matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("text" | "input_text")
                ) {
                    return Err("Only text content parts are supported".into());
                }
                texts.push(
                    part.get("text")
                        .and_then(Value::as_str)
                        .ok_or("Text content parts must contain text")?,
                );
            }
            texts.join("\n")
        }
        _ => return Err("The latest user content must be text or text parts".into()),
    };
    if text.trim().is_empty() || text.len() > 32_768 {
        return Err("Latest user message must contain 1 to 32768 bytes of nonempty text".into());
    }
    Ok(ChatRequest {
        conversation_id,
        text,
        stream,
        include_usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn accepts_text_parts_and_submits_only_latest_user_turn() {
        let request=parse(json!({"model":"muse","messages":[{"role":"system","content":"context"},{"role":"user","content":"old"},{"role":"assistant","content":"reply"},{"role":"user","content":[{"type":"text","text":"new"},{"type":"input_text","text":"line"}]}],"stream":true,"stream_options":{"include_usage":true}})).unwrap();
        assert_eq!(request.text, "new\nline");
        assert!(request.stream && request.include_usage);
    }
    #[test]
    fn rejects_unsupported_capabilities_and_never_falls_back_to_an_older_user_message() {
        let base = json!({"model":"muse","messages":[{"role":"user","content":"old"},{"role":"user","content":""}]});
        assert!(parse(base).is_err());
        for (field, value) in [
            ("tools", json!([{"type":"function"}])),
            ("stream", json!("true")),
            ("n", json!(2)),
            ("stop", json!(["end"])),
            ("response_format", json!({"type":"json_object"})),
        ] {
            let mut request = json!({"model":"muse","messages":[{"role":"user","content":"test"}]});
            request[field] = value;
            assert!(parse(request).is_err(), "{field}");
        }
    }
}
