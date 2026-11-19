use std::{collections::VecDeque, convert::Infallible};

use axum::{
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use futures_util::stream;
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::core::agent::AgentReply;

pub fn error(status: StatusCode, kind: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"type":"error","error":{"type":kind,"message":message}})),
    )
        .into_response()
}

pub fn failure_kind(message: &str) -> (StatusCode, &'static str) {
    if message.starts_with("Decision service returned HTTP 429") {
        (StatusCode::TOO_MANY_REQUESTS, "rate_limit_error")
    } else if message.starts_with("Decision routing")
        || message.starts_with("Decision context")
        || message.starts_with("Decision writer")
        || message.starts_with("Decision service is uncertain")
        || message.starts_with("Decision service did not select")
        || message.starts_with("Decision service selected an undeclared")
        || message.starts_with("Decision service omitted valid")
    {
        (StatusCode::BAD_REQUEST, "invalid_request_error")
    } else if message == crate::ports::UPSTREAM_INPUT_TOO_LARGE {
        (StatusCode::PAYLOAD_TOO_LARGE, "invalid_request_error")
    } else if crate::core::agent::terminal_failure(message) {
        (StatusCode::BAD_REQUEST, "invalid_request_error")
    } else {
        (StatusCode::BAD_GATEWAY, "api_error")
    }
}

pub fn inference_error(message: &str) -> Response {
    let (status, kind) = failure_kind(message);
    error(status, kind, message)
}

fn usage(input: u64, output: u64) -> Value {
    json!({"input_tokens":input,"output_tokens":output,"cache_creation_input_tokens":0,"cache_read_input_tokens":0})
}

fn message(
    id: &str,
    content: &[Value],
    stop_reason: Option<&str>,
    input: u64,
    output: u64,
    model: &str,
) -> Value {
    json!({"id":id,"type":"message","role":"assistant","model":model,"content":content,"stop_reason":stop_reason,"stop_sequence":null,"usage":usage(input, output)})
}

pub fn json_model(reply: AgentReply, id: &str, model: &str) -> Response {
    Json(message(
        id,
        &reply.content,
        Some(reply.stop_reason),
        reply.input_tokens,
        reply.output_tokens,
        model,
    ))
    .into_response()
}

fn event(kind: &str, value: Value) -> Event {
    Event::default().event(kind).data(value.to_string())
}

fn reply_events(reply: AgentReply) -> VecDeque<Event> {
    let mut events = VecDeque::new();
    for (index, block) in reply.content.into_iter().enumerate() {
        if block["type"] == "text" {
            events.push_back(event("content_block_start", json!({"type":"content_block_start","index":index,"content_block":{"type":"text","text":""}})));
            // UTF-8 safe chunking. Inference is buffered until validated; these
            // chunks provide wire compatibility, not upstream token streaming.
            let text = block["text"].as_str().expect("validated text");
            let mut chars = text.chars().peekable();
            while chars.peek().is_some() {
                let part: String = chars.by_ref().take(128).collect();
                events.push_back(event("content_block_delta", json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":part}})));
            }
        } else {
            events.push_back(event("content_block_start", json!({"type":"content_block_start","index":index,"content_block":{"type":"tool_use","id":block["id"],"name":block["name"],"input":{}}})));
            events.push_back(event("content_block_delta", json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":block["input"].to_string()}})));
        }
        events.push_back(event(
            "content_block_stop",
            json!({"type":"content_block_stop","index":index}),
        ));
    }
    events.push_back(event("message_delta", json!({"type":"message_delta","delta":{"stop_reason":reply.stop_reason,"stop_sequence":null},"usage":{"output_tokens":reply.output_tokens}})));
    events.push_back(event("message_stop", json!({"type":"message_stop"})));
    events
}

#[cfg(test)]
pub fn sse(
    receiver: oneshot::Receiver<Result<AgentReply, String>>,
    id: &str,
    input_tokens: u64,
) -> Response {
    sse_model(receiver, id, input_tokens, "muse")
}
pub fn sse_model(
    receiver: oneshot::Receiver<Result<AgentReply, String>>,
    id: &str,
    input_tokens: u64,
    model: &str,
) -> Response {
    let initial = VecDeque::from([event(
        "message_start",
        json!({"type":"message_start","message":message(id,&[],None,input_tokens,0,model)}),
    )]);
    let events = stream::unfold(
        (Some(receiver), initial),
        |(mut receiver, mut queue)| async move {
            if let Some(item) = queue.pop_front() {
                return Some((Ok::<_, Infallible>(item), (receiver, queue)));
            }
            let result = receiver
                .take()?
                .await
                .unwrap_or_else(|_| Err("Muse agent request ended unexpectedly".into()));
            queue = match result {
                Ok(reply) => reply_events(reply),
                Err(message) => VecDeque::from([event(
                    "error",
                    json!({"type":"error","error":{"type":failure_kind(&message).1,"message":message}}),
                )]),
            };
            let item = queue.pop_front()?;
            Some((Ok(item), (receiver, queue)))
        },
    );
    Sse::new(events)
        .keep_alive(KeepAlive::default())
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_context_receipt_is_not_a_retryable_gateway_error_in_json_or_sse() {
        let message = "Muse context transfer could not be confirmed. Fixture";
        assert_eq!(inference_error(message).status(), StatusCode::BAD_REQUEST);
        let (sender, receiver) = oneshot::channel();
        sender.send(Err(message.into())).unwrap();
        let bytes = axum::body::to_bytes(sse(receiver, "msg_test", 1).into_body(), 16384)
            .await
            .unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("invalid_request_error"));
        assert!(!body.contains("content_block_start"));
        assert!(!body.contains("message_stop"));
        assert_eq!(
            inference_error("Muse task failed").status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            inference_error(crate::ports::UPSTREAM_INPUT_TOO_LARGE).status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    #[tokio::test]
    async fn sse_round_trips_unicode_and_tool_input_with_anthropic_events() {
        let (sender, receiver) = oneshot::channel();
        let text = "Chào 🦀 ".repeat(60);
        sender.send(Ok(AgentReply { content:vec![json!({"type":"text","text":text}),json!({"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"F:\\test.rs"}})],stop_reason:"tool_use",input_tokens:12,output_tokens:34 })).unwrap();
        let bytes = axum::body::to_bytes(sse(receiver, "msg_1", 12).into_body(), 65536)
            .await
            .unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        let data: Vec<Value> = body
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(data[0]["type"], "message_start");
        let combined: String = data
            .iter()
            .filter_map(|value| value["delta"]["text"].as_str())
            .collect();
        assert_eq!(combined, text);
        let input = data
            .iter()
            .find_map(|value| value["delta"]["partial_json"].as_str())
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(input).unwrap()["file_path"],
            "F:\\test.rs"
        );
        assert_eq!(data[data.len() - 2]["delta"]["stop_reason"], "tool_use");
        assert_eq!(data.last().unwrap()["type"], "message_stop");
        assert!(!body.contains("[DONE]"));
    }

    #[tokio::test]
    async fn failed_validation_never_streams_a_partial_tool_or_success_stop() {
        let (sender, receiver) = oneshot::channel();
        sender.send(Err("Invalid tool input".into())).unwrap();
        let bytes = axum::body::to_bytes(sse(receiver, "msg_1", 1).into_body(), 16384)
            .await
            .unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("event: error"));
        assert!(!body.contains("content_block_start"));
        assert!(!body.contains("message_stop"));
    }
}
