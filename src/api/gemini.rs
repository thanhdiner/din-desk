//! Gemini composition and OpenAI wire conversion. Muse routes stay unchanged.
use super::anthropic::{self, AgentState};
use crate::{
    adapters::gemini::GeminiGenerator, infrastructure::outbound_proxy::AccountProxy,
    ports::TextGenerator,
};
use axum::{
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::{convert::Infallible, sync::Arc};

#[derive(Clone)]
struct GeminiState {
    agent: AgentState,
    backend: Option<Arc<GeminiGenerator>>,
}

pub(super) fn router() -> Router {
    let backend = std::env::var("GEMINI_COOKIE").ok().and_then(|cookie| {
        let proxy = AccountProxy::from_env().ok()?;
        GeminiGenerator::with_options(
            &cookie,
            &proxy,
            crate::adapters::gemini::Options::from_env().ok()?,
        )
        .ok()
        .map(Arc::new)
    });
    let agent = AgentState {
        model: "gemini",
        generator: backend.clone().map(|b| b as Arc<dyn TextGenerator>),
        api_key: std::env::var("MUSE_PROXY_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .map(Into::into),
        sessions: Default::default(),
        decision: None,
    };
    configured(agent, backend)
}

fn configured(agent: AgentState, backend: Option<Arc<GeminiGenerator>>) -> Router {
    Router::new()
        .route("/v1/models", get(models))
        .route("/v1/models/{id}", get(model))
        .route("/v1/provider/check", get(check))
        .route("/v1/provider/models", get(catalog))
        .route("/v1/chat/completions", post(completions))
        .layer(DefaultBodyLimit::max(crate::core::agent::MAX_CONTEXT_BYTES))
        .with_state(GeminiState {
            agent: agent.clone(),
            backend,
        })
        .merge(anthropic::router(agent))
}

#[cfg(test)]
pub(crate) fn fixture_router(backend: Arc<GeminiGenerator>) -> Router {
    configured(
        AgentState {
            model: "gemini",
            generator: Some(backend.clone()),
            api_key: Some(Arc::from("fixture-api-key")),
            sessions: Default::default(),
            decision: None,
        },
        Some(backend),
    )
}
fn error(status: StatusCode, message: &str) -> Response {
    (status,Json(json!({"error":{"type":if status.is_client_error(){"invalid_request_error"}else{"api_error"},"message":message}}))).into_response()
}
fn model_value() -> Value {
    json!({"id":"gemini","object":"model","created":0,"owned_by":"google","description":"Gemini Web model/thinking selected in Muse Desk; Google default when no selection is configured"})
}
async fn models() -> Json<Value> {
    Json(json!({"object":"list","data":[model_value()]}))
}
async fn model(axum::extract::Path(id): axum::extract::Path<String>) -> Response {
    if id == "gemini" {
        Json(model_value()).into_response()
    } else {
        error(
            StatusCode::NOT_FOUND,
            "Model not found for the active Gemini account",
        )
    }
}
async fn check(State(state): State<GeminiState>, headers: HeaderMap) -> Response {
    if let Some(error) = anthropic::authorization_error(&headers, &state.agent) {
        return error;
    }
    let Some(backend) = state.backend else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Gemini account is not configured",
        );
    };
    match backend.check().await {
        Ok(()) => {
            Json(json!({"provider":"gemini","model":"gemini","authenticated":true,"account_index":backend.options().account_index,
                "selected_model":backend.options().model_id,"thinking":backend.options().thinking_id})).into_response()
        }
        Err(message) => error(StatusCode::BAD_GATEWAY, &message),
    }
}

async fn catalog(State(state): State<GeminiState>, headers: HeaderMap) -> Response {
    if let Some(error) = anthropic::authorization_error(&headers, &state.agent) {
        return error;
    }
    let Some(backend) = state.backend else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Gemini account is not configured",
        );
    };
    match backend.catalog(true).await {
        Ok(catalog) => Json(catalog).into_response(),
        Err(message) => error(StatusCode::BAD_GATEWAY, &message),
    }
}

#[cfg(test)]
fn openai_request(value: Value) -> Result<(Value, bool, bool), String> {
    openai_request_for_model(value, "gemini")
}
fn openai_request_for_model(value: Value, expected: &str) -> Result<(Value, bool, bool), String> {
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .ok_or("Missing model")?;
    if model != expected {
        return Err(format!("The active backend serves only model {expected}"));
    }
    for name in [
        "functions",
        "function_call",
        "response_format",
        "audio",
        "modalities",
        "conversation_id",
    ] {
        if value.get(name).is_some_and(|v| !v.is_null()) {
            return Err(format!("{name} is not supported on this OpenAI adapter"));
        }
    }
    if value.get("n").is_some_and(|v| v.as_u64() != Some(1)) {
        return Err("Only n=1 is supported".into());
    }
    if value
        .get("stop")
        .is_some_and(|v| !v.is_null() && !v.as_array().is_some_and(Vec::is_empty))
    {
        return Err("stop is not supported".into());
    }
    let stream = match value.get("stream") {
        None => false,
        Some(v) => v.as_bool().ok_or("stream must be boolean")?,
    };
    let usage = value
        .pointer("/stream_options/include_usage")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut system = Vec::new();
    let mut messages = Vec::new();
    for message in value
        .get("messages")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .ok_or("messages must be nonempty")?
    {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .ok_or("Missing message role")?;
        if role == "tool" {
            let id = message
                .get("tool_call_id")
                .and_then(Value::as_str)
                .ok_or("Missing tool_call_id")?;
            let text = message
                .get("content")
                .and_then(Value::as_str)
                .ok_or("Tool result must be text")?;
            let block = json!({"type":"tool_result","tool_use_id":id,"content":text});
            if messages.last().is_some_and(|m: &Value| {
                m["role"] == "user" && m["content"][0]["type"] == "tool_result"
            }) {
                messages.last_mut().unwrap()["content"]
                    .as_array_mut()
                    .unwrap()
                    .push(block);
            } else {
                messages.push(json!({"role":"user","content":[block]}));
            }
            continue;
        }
        if !matches!(role, "system" | "developer" | "user" | "assistant") {
            return Err("Unsupported message role".into());
        }
        let mut content = Vec::new();
        match message.get("content") {
            Some(Value::String(text)) => {
                if !text.is_empty() {
                    content.push(json!({"type":"text","text":text}));
                }
            }
            Some(Value::Array(parts)) => {
                for part in parts {
                    match part.get("type").and_then(Value::as_str){
                    Some("text")=>content.push(json!({"type":"text","text":part.get("text").and_then(Value::as_str).ok_or("Invalid text part")?})),
                    Some("image_url") if role=="user"=>{
                        let url=part.pointer("/image_url/url").and_then(Value::as_str).ok_or("Invalid image URL")?;
                        let (mime,data)=url.strip_prefix("data:").and_then(|v|v.split_once(";base64,")).ok_or("Only base64 data image URLs are supported")?;
                        content.push(json!({"type":"image","source":{"type":"base64","media_type":mime,"data":data}}));
                    },
                    _=>return Err("Unsupported OpenAI message content".into()),
                }
                }
            }
            None | Some(Value::Null) if role == "assistant" => {}
            _ => return Err("Unsupported OpenAI message content".into()),
        }
        if let Some(calls) = message.get("tool_calls").filter(|v| !v.is_null()) {
            if role != "assistant" {
                return Err("tool_calls require assistant role".into());
            }
            for call in calls.as_array().ok_or("tool_calls must be array")? {
                if call["type"] != "function" {
                    return Err("Only function tools supported".into());
                }
                let input: Value = serde_json::from_str(
                    call.pointer("/function/arguments")
                        .and_then(Value::as_str)
                        .ok_or("Missing tool arguments")?,
                )
                .map_err(|_| "Invalid tool arguments JSON")?;
                content.push(json!({"type":"tool_use","id":call["id"],"name":call["function"]["name"],"input":input}));
            }
        }
        if (role == "system" || role == "developer") && messages.is_empty() {
            system.extend(content);
        } else {
            messages
                .push(json!({"role":if role=="developer"{"system"}else{role},"content":content}));
        }
    }
    let mut tools = Vec::new();
    if let Some(raw) = value.get("tools").filter(|v| !v.is_null()) {
        for tool in raw.as_array().ok_or("tools must be array")? {
            if tool["type"] != "function" {
                return Err("Only function tools supported".into());
            }
            let f = &tool["function"];
            tools.push(json!({"name":f["name"],"description":f.get("description").cloned().unwrap_or(json!("")),"input_schema":f.get("parameters").cloned().unwrap_or(json!({"type":"object","properties":{}}))}));
        }
    }
    let choice = match value.get("tool_choice") {
        None | Some(Value::Null) => json!({"type":"auto"}),
        Some(Value::String(v)) => match v.as_str() {
            "auto" => json!({"type":"auto"}),
            "none" => json!({"type":"none"}),
            "required" => json!({"type":"any"}),
            _ => return Err("Invalid tool_choice".into()),
        },
        Some(v) if v["type"] == "function" => json!({"type":"tool","name":v["function"]["name"]}),
        _ => return Err("Invalid tool_choice".into()),
    };
    let mut choice = choice;
    if let Some(parallel) = value.get("parallel_tool_calls") {
        choice["disable_parallel_tool_use"] = json!(!parallel
            .as_bool()
            .ok_or("parallel_tool_calls must be boolean")?);
    }
    Ok((
        json!({"model":expected,"system":system,"messages":messages,"tools":tools,"tool_choice":choice,"max_tokens":value.get("max_completion_tokens").or_else(||value.get("max_tokens")).cloned().unwrap_or(json!(8192)),"stream":false}),
        stream,
        usage,
    ))
}

async fn completions(
    State(state): State<GeminiState>,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Response {
    openai_completions(state.agent, headers, value).await
}
pub(super) async fn openai_completions(
    agent: AgentState,
    headers: HeaderMap,
    value: Value,
) -> Response {
    let model = agent.model;
    if let Some(error) = anthropic::authorization_error(&headers, &agent) {
        return error;
    }
    let (body, stream, include_usage) = match openai_request_for_model(value, model) {
        Ok(v) => v,
        Err(message) => return error(StatusCode::BAD_REQUEST, &message),
    };
    let response = anthropic::messages_inner(agent, headers, Ok(Json(body))).await;
    if !response.status().is_success() {
        return response;
    }
    let bytes = match axum::body::to_bytes(
        response.into_body(),
        crate::core::agent::MAX_REPLY_BYTES * 2,
    )
    .await
    {
        Ok(v) => v,
        Err(_) => {
            return error(
                StatusCode::BAD_GATEWAY,
                "Could not read validated upstream reply",
            )
        }
    };
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return error(StatusCode::BAD_GATEWAY, "Invalid upstream adapter reply"),
    };
    let mut text = String::new();
    let mut calls = Vec::new();
    for block in body["content"].as_array().expect("validated content") {
        if block["type"] == "text" {
            text.push_str(block["text"].as_str().unwrap());
        } else {
            calls.push(json!({"id":block["id"],"type":"function","function":{"name":block["name"],"arguments":block["input"].to_string()}}));
        }
    }
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let reason = if calls.is_empty() {
        "stop"
    } else {
        "tool_calls"
    };
    let usage = json!({"prompt_tokens":body["usage"]["input_tokens"],"completion_tokens":body["usage"]["output_tokens"],"total_tokens":body["usage"]["input_tokens"].as_u64().unwrap_or(0)+body["usage"]["output_tokens"].as_u64().unwrap_or(0)});
    let mut message =
        json!({"role":"assistant","content":if text.is_empty(){Value::Null}else{json!(text)}});
    if !calls.is_empty() {
        message["tool_calls"] = json!(calls);
    }
    if !stream {
        return Json(json!({"id":id,"object":"chat.completion","created":created,"model":model,"choices":[{"index":0,"message":message,"finish_reason":reason}],"usage":usage})).into_response();
    }
    let mut delta = message;
    if let Some(calls) = delta.get_mut("tool_calls").and_then(Value::as_array_mut) {
        for (index, call) in calls.iter_mut().enumerate() {
            call["index"] = json!(index);
        }
    }
    let mut events = vec![
        json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[{"index":0,"delta":delta,"finish_reason":null}]}),
        json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[{"index":0,"delta":{},"finish_reason":reason}]}),
    ];
    if include_usage {
        events.push(json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[],"usage":usage}));
    }
    let mut events: Vec<Result<Event, Infallible>> = events
        .into_iter()
        .map(|v| Ok(Event::default().data(v.to_string())))
        .collect();
    events.push(Ok(Event::default().data("[DONE]")));
    Sse::new(futures_util::stream::iter(events)).into_response()
}

#[cfg(test)]
mod tests;
