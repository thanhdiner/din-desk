//! Anthropic wire validation; normalize only supported text/client-tool blocks.
use std::collections::HashSet;

use serde_json::{json, Value};

use crate::core::agent::{
    validate_tool_history, AgentRequest, Tool, ToolChoice, MAX_CONTEXT_BYTES,
};
use crate::core::image::{InputImage, MAX_IMAGES};

pub struct Request {
    pub agent: AgentRequest,
    pub stream: bool,
}

fn text_blocks(value: &Value) -> Result<Value, String> {
    if let Some(text) = value.as_str() {
        return Ok(json!([{"type":"text","text":text}]));
    }
    let blocks = value
        .as_array()
        .ok_or("Expected text or text content blocks")?;
    let mut result = Vec::new();
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            return Err("This field only accepts text blocks".into());
        }
        let text = block
            .get("text")
            .and_then(Value::as_str)
            .ok_or("Text block must contain text")?;
        result.push(json!({"type":"text","text":text}));
    }
    Ok(json!(result))
}

fn identifier(value: Option<&Value>, field: &str) -> Result<String, String> {
    let name = value
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty() && name.len() <= 200)
        .ok_or_else(|| format!("Invalid {field}"))?;
    Ok(name.to_owned())
}

fn image_block(block: &Value, images: &mut Vec<InputImage>) -> Result<Value, String> {
    let source = block
        .get("source")
        .ok_or("Image block must have a source")?;
    if source.get("type").and_then(Value::as_str) != Some("base64") {
        return Err(
            "Image sources must be base64; URL and file_id sources are not supported".into(),
        );
    }
    let media_type = source
        .get("media_type")
        .and_then(Value::as_str)
        .ok_or("Image source must contain media_type")?;
    let data = source
        .get("data")
        .and_then(Value::as_str)
        .ok_or("Image source must contain base64 data")?;
    let image = InputImage::from_base64(media_type, data)?;
    let reference = json!({"type":"image","image_id":image.id,"media_type":image.media_type});
    if !images.iter().any(|existing| existing.id == image.id) {
        if images.len() >= MAX_IMAGES {
            return Err(format!("Conversation contains more than {MAX_IMAGES} distinct images; compact the conversation before adding more"));
        }
        images.push(image);
    }
    Ok(reference)
}

fn tool_result_content(value: &Value, images: &mut Vec<InputImage>) -> Result<Value, String> {
    if value.is_string() {
        return text_blocks(value);
    }
    let blocks = value
        .as_array()
        .ok_or("tool_result.content must be text or content blocks")?;
    let mut result = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => result.extend(
                text_blocks(&json!([block]))?
                    .as_array()
                    .expect("text array")
                    .iter()
                    .cloned(),
            ),
            Some("image") => result.push(image_block(block, images)?),
            _ => return Err("Tool results may only contain text and base64 images".into()),
        }
    }
    Ok(json!(result))
}

fn content(value: &Value, role: &str, images: &mut Vec<InputImage>) -> Result<Value, String> {
    if value.is_string() {
        return text_blocks(value);
    }
    let blocks = value
        .as_array()
        .filter(|blocks| !blocks.is_empty())
        .ok_or("Message content must not be empty")?;
    let mut result = Vec::new();
    let mut saw_text = false;
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                saw_text = true;
                let normalized = text_blocks(&json!([block]))?;
                result.extend(normalized.as_array().expect("array").iter().cloned());
            }
            Some("image") if role=="user"=>{
                saw_text=true;
                result.push(image_block(block,images)?);
            }
            Some("tool_use") if role == "assistant" => {
                let id = identifier(block.get("id"), "tool_use.id")?;
                let name = identifier(block.get("name"), "tool_use.name")?;
                let input = block.get("input").filter(|value| value.is_object()).ok_or("tool_use.input must be an object")?;
                result.push(json!({"type":"tool_use","id":id,"name":name,"input":input}));
            }
            Some("tool_result") if role == "user" => {
                if saw_text {
                    return Err("tool_result blocks must precede user text blocks".into());
                }
                let id = identifier(block.get("tool_use_id"), "tool_result.tool_use_id")?;
                let body = block.get("content").cloned().unwrap_or_else(|| json!(""));
                let body = tool_result_content(&body,images)?;
                let is_error = match block.get("is_error") {
                    None => false,
                    Some(value) => value.as_bool().ok_or("tool_result.is_error must be boolean")?,
                };
                result.push(json!({"type":"tool_result","tool_use_id":id,"content":body,"is_error":is_error}));
            }
            _ => return Err("Unsupported message block or role; supported blocks are text, base64 image and client tool_use/tool_result".into()),
        }
    }
    Ok(json!(result))
}

fn local_refs_only(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().all(|(key, value)| {
            if key == "$ref" || key == "$dynamicRef" || key == "$recursiveRef" {
                value
                    .as_str()
                    .is_some_and(|reference| reference.starts_with('#'))
            } else if key == "$schema" {
                true // Dialect identifier, not an external data fetch.
            } else {
                local_refs_only(value)
            }
        }),
        Value::Array(values) => values.iter().all(local_refs_only),
        _ => true,
    }
}

pub fn parse(value: Value) -> Result<Request, String> {
    if !value.is_object() || serde_json::to_vec(&value).expect("JSON").len() > MAX_CONTEXT_BYTES {
        return Err(
            "Request must be an object of at most 4 MiB; compact the conversation first".into(),
        );
    }
    if value.get("model").and_then(Value::as_str) != Some("muse") {
        return Err(
            "Only model muse is supported; configure all Claude Code model overrides to muse"
                .into(),
        );
    }
    let max_tokens = value
        .get("max_tokens")
        .and_then(Value::as_u64)
        .filter(|tokens| *tokens > 0 && *tokens <= 131_072)
        .ok_or("max_tokens must be 1..131072")?;
    let stream = match value.get("stream") {
        None => false,
        Some(value) => value.as_bool().ok_or("stream must be boolean")?,
    };
    if value
        .get("thinking")
        .is_some_and(|v| !v.is_null() && v.get("type").and_then(Value::as_str) != Some("disabled"))
    {
        return Err("Extended thinking is unavailable; disable thinking for Muse".into());
    }
    for field in ["stop_sequences", "mcp_servers"] {
        if value
            .get(field)
            .is_some_and(|v| !v.is_null() && !v.as_array().is_some_and(Vec::is_empty))
        {
            return Err(format!("{field} is not supported"));
        }
    }
    if value
        .get("output_config")
        .and_then(|v| v.get("format"))
        .is_some_and(|v| !v.is_null())
    {
        return Err("Structured output is unavailable".into());
    }
    if value
        .get("context_management")
        .is_some_and(|v| !v.is_null())
    {
        return Err("Server context management is unavailable; compact on the client".into());
    }
    let system = value
        .get("system")
        .map(text_blocks)
        .transpose()?
        .unwrap_or_else(|| json!([]));
    let raw = value
        .get("messages")
        .and_then(Value::as_array)
        .filter(|messages| !messages.is_empty())
        .ok_or("messages must be a nonempty array")?;
    let mut messages = Vec::new();
    let mut images = Vec::new();
    for message in raw {
        // Newer Claude clients interleave system reminders in messages.
        // Keep their exact position rather than promoting them to the top.
        if message.get("role").and_then(Value::as_str) == Some("system") {
            let blocks = text_blocks(message.get("content").ok_or("Missing system content")?)?;
            messages.push(json!({"role":"system","content":blocks}));
            continue;
        }
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .filter(|role| matches!(*role, "user" | "assistant"))
            .ok_or("Only user and assistant message roles are supported")?;
        let normalized = content(
            message.get("content").ok_or("Missing message content")?,
            role,
            &mut images,
        )?;
        messages.push(json!({"role":role,"content":normalized}));
    }
    let dialogue: Vec<_> = messages
        .iter()
        .filter(|message| message["role"] != "system")
        .collect();
    if dialogue.first().and_then(|v| v["role"].as_str()) != Some("user")
        || dialogue.last().and_then(|v| v["role"].as_str()) != Some("user")
    {
        return Err(
            "Conversation must start and end with user; assistant prefill is unsupported".into(),
        );
    }
    validate_tool_history(&messages)?;
    let mut names = HashSet::new();
    let mut tools = Vec::new();
    if let Some(raw) = value.get("tools").filter(|value| !value.is_null()) {
        let raw = raw
            .as_array()
            .filter(|tools| tools.len() <= 128)
            .ok_or("tools must be an array of at most 128 definitions")?;
        for tool in raw {
            let name = identifier(tool.get("name"), "tool name")?;
            if !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                || !names.insert(name.clone())
            {
                return Err(
                    "Tool names must be unique and contain only letters, digits, _ and -".into(),
                );
            }
            if tool
                .get("type")
                .is_some_and(|v| v.as_str() != Some("custom"))
                || tool.get("defer_loading").is_some_and(|v| v != false)
            {
                return Err("Only eagerly loaded custom client tools are supported; set ENABLE_TOOL_SEARCH=false".into());
            }
            let schema = tool
                .get("input_schema")
                .filter(|v| {
                    v.is_object() && v.get("type").and_then(Value::as_str) == Some("object")
                })
                .ok_or("Each tool needs an object input_schema")?;
            if !local_refs_only(schema) {
                return Err("Tool schemas may only reference local # definitions".into());
            }
            let validator = jsonschema::validator_for(schema)
                .map_err(|_| format!("Invalid input_schema for {name}"))?;
            let description = tool
                .get("description")
                .map(|v| v.as_str().ok_or("Tool description must be text"))
                .transpose()?
                .unwrap_or("");
            tools.push(Tool {
                name: name.clone(),
                definition: json!({"name":name,"description":description,"input_schema":schema}),
                validator,
            });
        }
    }
    let mut parallel = true;
    let choice = match value.get("tool_choice").filter(|value| !value.is_null()) {
        None => ToolChoice::Auto,
        Some(choice) => {
            if let Some(value) = choice.get("disable_parallel_tool_use") {
                parallel = !value
                    .as_bool()
                    .ok_or("disable_parallel_tool_use must be boolean")?;
            }
            match choice.get("type").and_then(Value::as_str) {
                Some("auto") => ToolChoice::Auto,
                Some("none") => ToolChoice::None,
                Some("any") if !tools.is_empty() => ToolChoice::Any,
                Some("tool") => {
                    let name = identifier(choice.get("name"), "tool_choice.name")?;
                    if !names.contains(&name) {
                        return Err("tool_choice refers to an undeclared tool".into());
                    }
                    ToolChoice::Named(name)
                }
                _ => return Err("Invalid tool_choice or no tools available".into()),
            }
        }
    };
    Ok(Request {
        agent: AgentRequest {
            system,
            messages: json!(messages),
            tools,
            choice,
            parallel,
            max_tokens,
            images,
        },
        stream,
    })
}

/// The active backend is fixed at startup. A model mismatch never falls back
/// to another provider, account, credential or remote conversation.
pub fn parse_for_model(mut value: Value, model: &str) -> Result<Request, String> {
    let requested = value.get("model").and_then(Value::as_str);
    let desktop_alias = match model {
        "muse" => Some("claude-dindesk-muse"),
        "gemini" => Some("claude-dindesk-gemini"),
        _ => None,
    };
    let is_desktop = desktop_alias.is_some() && requested == desktop_alias;
    if requested != Some(model) && !is_desktop {
        return Err(format!(
            "This account serves only model {model}; select the matching backend"
        ));
    }
    if let Some(object) = value.as_object_mut() {
        // Desktop enables extended thinking by default. These gateway routes
        // return ordinary text and tool calls without a separate thinking stream.
        if is_desktop {
            object.remove("thinking");
        }
        object.insert("model".into(), json!("muse"));
    }
    parse(value)
}
