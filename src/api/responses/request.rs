use std::collections::HashMap;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::api::anthropic::request::{parse_for_model, Request};

pub(super) struct ToolName {
    pub name: String,
    pub namespace: Option<String>,
    pub custom: bool,
}

impl ToolName {
    fn read(value: &Value, namespace: Option<&str>, custom: bool) -> Result<Self, String> {
        let name = required_text(value, "name")?;
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err("Invalid Responses tool name".into());
        }
        let namespace = namespace
            .or_else(|| value["namespace"].as_str())
            .filter(|name| !name.is_empty());
        if namespace.is_some_and(|name| name.len() > 128) {
            return Err("Invalid Responses tool namespace".into());
        }
        Ok(Self {
            name: name.into(),
            namespace: namespace.map(str::to_owned),
            custom,
        })
    }

    fn agent_name(&self) -> String {
        match &self.namespace {
            None => self.name.clone(),
            Some(namespace) => {
                let hash = format!("{:x}", Sha256::digest(namespace));
                format!("namespace_{}_{}", &hash[..16], self.name)
            }
        }
    }
}

fn required_text<'a>(value: &'a Value, name: &str) -> Result<&'a str, String> {
    value[name]
        .as_str()
        .ok_or_else(|| format!("Expected text in {name}"))
}

fn content_blocks(value: &Value) -> Result<Vec<Value>, String> {
    if let Some(text) = value.as_str() {
        return Ok(vec![json!({"type":"text","text":text})]);
    }
    value.as_array().ok_or("Expected text or content blocks")?.iter().map(|part| {
        match part["type"].as_str() {
            Some("input_text" | "output_text" | "text") => Ok(json!({"type":"text","text":required_text(part, "text")?})),
            Some("refusal") => Ok(json!({"type":"text","text":required_text(part, "refusal")?})),
            Some("input_image") => {
                let (media_type, data) = required_text(part, "image_url")?.strip_prefix("data:")
                    .and_then(|url| url.split_once(";base64,")).ok_or("Images must use base64 data URLs")?;
                Ok(json!({"type":"image","source":{"type":"base64","media_type":media_type,"data":data}}))
            }
            _ => Err("Unsupported Responses content type".into()),
        }
    }).collect()
}

fn push_message(messages: &mut Vec<Value>, role: &str, mut blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    if let Some(message) = messages
        .last_mut()
        .filter(|message| message["role"] == role)
    {
        message["content"]
            .as_array_mut()
            .expect("normalized content")
            .append(&mut blocks);
    } else {
        messages.push(json!({"role":role,"content":blocks}));
    }
}

fn messages(input: &Value) -> Result<Vec<Value>, String> {
    let mut messages = Vec::new();
    if input.is_string() {
        push_message(&mut messages, "user", content_blocks(input)?);
        return Ok(messages);
    }
    for item in input
        .as_array()
        .ok_or("input must be a string or an array")?
    {
        let kind = item["type"].as_str().unwrap_or("message");
        let (role, blocks) = match kind {
            "message" => {
                let role = match required_text(item, "role")? {
                    "user" => "user",
                    "assistant" => "assistant",
                    "developer" | "system" => "system",
                    _ => return Err("Unsupported Responses message role".into()),
                };
                (role, content_blocks(&item["content"])?)
            }
            "function_call" | "custom_tool_call" => {
                let custom = kind == "custom_tool_call";
                let name = ToolName::read(item, None, custom)?.agent_name();
                let input = if custom {
                    json!({"input":required_text(item, "input")?})
                } else {
                    serde_json::from_str(required_text(item, "arguments")?)
                        .map_err(|_| "Invalid function call arguments")?
                };
                (
                    "assistant",
                    vec![
                        json!({"type":"tool_use","id":required_text(item, "call_id")?,"name":name,"input":input}),
                    ],
                )
            }
            "function_call_output" | "custom_tool_call_output" => {
                let output = content_blocks(&item["output"])?;
                (
                    "user",
                    vec![
                        json!({"type":"tool_result","tool_use_id":required_text(item, "call_id")?,"content":output}),
                    ],
                )
            }
            "reasoning" => continue,
            _ => return Err(format!("Unsupported Responses input type {kind}")),
        };
        push_message(&mut messages, role, blocks);
    }
    Ok(messages)
}

fn add_tool(
    value: &Value,
    namespace: Option<&str>,
    mapped: &mut HashMap<String, ToolName>,
    tools: &mut Vec<Value>,
) -> Result<(), String> {
    let custom = match value["type"].as_str() {
        Some("function") => false,
        Some("custom") => true,
        _ => return Err("Only client function and custom tools are supported".into()),
    };
    let wire = ToolName::read(value, namespace, custom)?;
    let name = wire.agent_name();
    let mut description = value["description"].as_str().unwrap_or_default().to_owned();
    let schema = if custom {
        if let Some(grammar) = value.pointer("/format/definition").and_then(Value::as_str) {
            description.push_str("\nInput grammar:\n");
            description.push_str(grammar);
        }
        json!({"type":"object","properties":{"input":{"type":"string"}},"required":["input"],"additionalProperties":false})
    } else if value["parameters"].is_null() {
        json!({"type":"object","properties":{},"additionalProperties":false})
    } else {
        value["parameters"].clone()
    };
    if tools.len() >= 128 || mapped.insert(name.clone(), wire).is_some() {
        return Err("Too many Responses tools or duplicate tool name".into());
    }
    tools.push(json!({"name":name,"description":description,"input_schema":schema}));
    Ok(())
}

pub(super) fn parse(
    value: Value,
    model: &str,
) -> Result<(Request, HashMap<String, ToolName>), String> {
    if !value.is_object() || value["model"] != model {
        return Err(format!("This account serves only model {model}"));
    }
    if value["store"] == true
        || !value["previous_response_id"].is_null()
        || !value["conversation"].is_null()
    {
        return Err("Send the full input history with store=false".into());
    }
    if !value["text"]["format"].is_null() && value["text"]["format"]["type"] != "text" {
        return Err("Structured output is unavailable".into());
    }
    let mut mapped = HashMap::new();
    let mut tools = Vec::new();
    if !value["tools"].is_null() {
        for tool in value["tools"].as_array().ok_or("tools must be an array")? {
            if tool["type"] == "namespace" {
                let namespace = required_text(tool, "name")?;
                for nested in tool["tools"]
                    .as_array()
                    .ok_or("Namespace must contain tools")?
                {
                    add_tool(nested, Some(namespace), &mut mapped, &mut tools)?;
                }
            } else {
                add_tool(tool, None, &mut mapped, &mut tools)?;
            }
        }
    }
    let mut choice = match value["tool_choice"].as_str() {
        Some("auto") => json!({"type":"auto"}),
        Some("none") => json!({"type":"none"}),
        Some("required") => json!({"type":"any"}),
        None if value["tool_choice"].is_null() => json!({"type":"auto"}),
        None if matches!(
            value["tool_choice"]["type"].as_str(),
            Some("function" | "custom")
        ) =>
        {
            let tool = &value["tool_choice"];
            let name = ToolName::read(tool, None, tool["type"] == "custom")?.agent_name();
            json!({"type":"tool","name":name})
        }
        _ => return Err("Unsupported Responses tool_choice".into()),
    };
    if !value["parallel_tool_calls"].is_null() {
        choice["disable_parallel_tool_use"] = json!(!value["parallel_tool_calls"]
            .as_bool()
            .ok_or("parallel_tool_calls must be boolean")?);
    }
    let request = parse_for_model(
        json!({
            "model":model,
            "system":value["instructions"].as_str().unwrap_or_default(),
            "messages":messages(&value["input"])?,
            "tools":tools,
            "tool_choice":choice,
            "max_tokens":value.get("max_output_tokens").filter(|value| !value.is_null()).cloned().unwrap_or(json!(8192)),
            "stream":value.get("stream").cloned().unwrap_or(json!(false))
        }),
        model,
    )?;
    Ok((request, mapped))
}
