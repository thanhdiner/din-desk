mod request;
#[cfg(test)]
mod tests;

use std::{
    collections::{HashMap, VecDeque},
    convert::Infallible,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{rejection::JsonRejection, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use futures_util::stream;
use serde_json::{json, Value};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::{
    api::anthropic::{self, AgentState},
    core::agent::AgentReply,
};
use request::ToolName;

pub(super) async fn handle(
    State(state): State<AgentState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    if let Err(error) = anthropic::authorize(&headers, &state) {
        return error.into_response();
    }
    let Json(value) = match body {
        Ok(body) => body,
        Err(error) => {
            return http_error(
                error.status(),
                "Expected a JSON request body of at most 4 MiB",
            )
        }
    };
    let model = state.model;
    let (request, tools) = match request::parse(value, model) {
        Ok(parsed) => parsed,
        Err(error) => return http_error(StatusCode::BAD_REQUEST, &error),
    };
    let prepared = match request.agent.prepare() {
        Ok(prepared) => prepared,
        Err(error) => return http_error(StatusCode::BAD_REQUEST, &error),
    };
    if state.generator.is_none() {
        return http_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Connect the selected backend in Din Desk",
        );
    }
    let client = ["thread-id", "session-id"]
        .iter()
        .find_map(|name| Uuid::parse_str(headers.get(*name)?.to_str().ok()?).ok())
        .map(|id| format!("responses:{id}"));
    let receiver = anthropic::spawn_inference(state, None, prepared, client);
    let response = json!({
        "id":id("resp"), "object":"response", "model":model,
        "created_at":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
        "status":"in_progress", "output":[], "usage":null, "error":null,
        "incomplete_details":null, "store":false
    });
    if request.stream {
        sse(receiver, response, tools)
    } else {
        match receive(receiver).await {
            Ok(reply) => Json(complete(response, reply, &tools)).into_response(),
            Err(error) => http_error(anthropic::response::failure_kind(&error).0, &error),
        }
    }
}

fn id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

fn http_error(status: StatusCode, message: &str) -> Response {
    let kind = if status.is_client_error() {
        "invalid_request_error"
    } else {
        "server_error"
    };
    (
        status,
        Json(json!({"error":{"type":kind,"message":message}})),
    )
        .into_response()
}

async fn receive(
    receiver: oneshot::Receiver<Result<AgentReply, String>>,
) -> Result<AgentReply, String> {
    receiver
        .await
        .unwrap_or_else(|_| Err("Proxy inference ended unexpectedly".into()))
}

fn complete(mut response: Value, reply: AgentReply, tools: &HashMap<String, ToolName>) -> Value {
    let output: Vec<_> = reply.content.into_iter().map(|block| {
        if block["type"] == "text" {
            return json!({"type":"message","role":"assistant","id":id("msg"),"status":"completed",
                "content":[{"type":"output_text","text":block["text"],"annotations":[]}]});
        }
        let name = block["name"].as_str().expect("validated tool name");
        let tool = &tools[name];
        let mut item = if tool.custom {
            json!({"type":"custom_tool_call","id":id("ctc"),"call_id":block["id"],
                "name":tool.name,"input":block["input"]["input"],"status":"completed"})
        } else {
            json!({"type":"function_call","id":id("fc"),"call_id":block["id"],
                "name":tool.name,"arguments":block["input"].to_string(),"status":"completed"})
        };
        if let Some(namespace) = &tool.namespace { item["namespace"] = json!(namespace); }
        item
    }).collect();
    response["output"] = Value::Array(output);
    response["status"] = json!("completed");
    response["usage"] = json!({"input_tokens":reply.input_tokens,"output_tokens":reply.output_tokens,
        "total_tokens":reply.input_tokens.saturating_add(reply.output_tokens),
        "input_tokens_details":{"cached_tokens":0},"output_tokens_details":{"reasoning_tokens":0}});
    response
}

fn event(sequence: &mut u64, kind: &str, mut data: Value) -> Event {
    data["type"] = json!(kind);
    data["sequence_number"] = json!(*sequence);
    *sequence += 1;
    Event::default().event(kind).data(data.to_string())
}

fn item_events(queue: &mut VecDeque<Event>, sequence: &mut u64, index: usize, item: &Value) {
    let kind = item["type"].as_str().expect("output item type");
    let mut start = item.clone();
    start["status"] = json!("in_progress");
    match kind {
        "message" => start["content"] = json!([]),
        "function_call" => start["arguments"] = json!(""),
        _ => start["input"] = json!(""),
    }
    queue.push_back(event(
        sequence,
        "response.output_item.added",
        json!({"output_index":index,"item":start}),
    ));
    if kind == "message" {
        let part = &item["content"][0];
        let output = part["text"].as_str().expect("validated output text");
        queue.push_back(event(sequence, "response.content_part.added", json!({"output_index":index,"item_id":item["id"],"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}})));
        let mut chars = output.chars().peekable();
        while chars.peek().is_some() {
            let delta: String = chars.by_ref().take(128).collect();
            queue.push_back(event(sequence, "response.output_text.delta", json!({"output_index":index,"item_id":item["id"],"content_index":0,"delta":delta,"logprobs":[]})));
        }
        queue.push_back(event(sequence, "response.output_text.done", json!({"output_index":index,"item_id":item["id"],"content_index":0,"text":output,"logprobs":[]})));
        queue.push_back(event(
            sequence,
            "response.content_part.done",
            json!({"output_index":index,"item_id":item["id"],"content_index":0,"part":part}),
        ));
    } else if kind == "function_call" {
        queue.push_back(event(
            sequence,
            "response.function_call_arguments.delta",
            json!({"output_index":index,"item_id":item["id"],"delta":item["arguments"]}),
        ));
        queue.push_back(event(sequence, "response.function_call_arguments.done", json!({"output_index":index,"item_id":item["id"],"name":item["name"],"arguments":item["arguments"]})));
    } else {
        queue.push_back(event(
            sequence,
            "response.custom_tool_call_input.delta",
            json!({"output_index":index,"item_id":item["id"],"delta":item["input"]}),
        ));
        queue.push_back(event(
            sequence,
            "response.custom_tool_call_input.done",
            json!({"output_index":index,"item_id":item["id"],"input":item["input"]}),
        ));
    }
    queue.push_back(event(
        sequence,
        "response.output_item.done",
        json!({"output_index":index,"item":item}),
    ));
}

fn sse(
    receiver: oneshot::Receiver<Result<AgentReply, String>>,
    response: Value,
    tools: HashMap<String, ToolName>,
) -> Response {
    let mut sequence = 0;
    let initial = event(
        &mut sequence,
        "response.created",
        json!({"response":response}),
    );
    let events = stream::unfold(
        (Some((receiver, response, tools)), VecDeque::from([initial])),
        move |(mut pending, mut queue)| async move {
            if let Some(item) = queue.pop_front() {
                return Some((Ok::<_, Infallible>(item), (pending, queue)));
            }
            let (receiver, response, tools) = pending.take()?;
            let mut sequence = sequence;
            match receive(receiver).await {
                Ok(reply) => {
                    let response = complete(response, reply, &tools);
                    for (index, item) in response["output"]
                        .as_array()
                        .expect("response output")
                        .iter()
                        .enumerate()
                    {
                        item_events(&mut queue, &mut sequence, index, item);
                    }
                    queue.push_back(event(
                        &mut sequence,
                        "response.completed",
                        json!({"response":response}),
                    ));
                }
                Err(message) => {
                    let mut response = response;
                    response["status"] = json!("failed");
                    response["error"] = json!({"code":"server_error","message":message});
                    queue.push_back(event(
                        &mut sequence,
                        "response.failed",
                        json!({"response":response}),
                    ));
                }
            }
            queue.pop_front().map(|item| (Ok(item), (None, queue)))
        },
    );
    Sse::new(events)
        .keep_alive(KeepAlive::default())
        .into_response()
}
