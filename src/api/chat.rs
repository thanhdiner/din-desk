use std::{
    convert::Infallible,
    env,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use futures_util::stream;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::{adapters::muse::session::MuseSession, core::turn::TurnEvent};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Default)]
struct ChatState {
    session: Option<Arc<MuseSession>>,
}

pub(super) fn router() -> Router {
    if std::env::var("MUSE_BACKEND").is_ok_and(|value| value == "gemini") {
        return super::gemini::router();
    }
    let session = env::var("MUSE_COOKIE")
        .ok()
        .zip(env::var("MUSE_NODE_ID").ok())
        .and_then(|(cookie, node)| MuseSession::new(&cookie, node).ok())
        .map(Arc::new);
    Router::new()
        .route("/v1/models", get(models))
        .route("/v1/models/{id}", get(model))
        .route(
            "/v1/conversations",
            post(create_conversation).get(list_conversations),
        )
        .route("/v1/conversations/{id}", get(get_conversation))
        .route("/v1/chat/completions", post(completions))
        .with_state(ChatState {
            session: session.clone(),
        })
        .merge(super::anthropic::router(super::anthropic::AgentState {
            model: "muse",
            generator: session.map(|session| {
                Arc::new(crate::adapters::muse::agent::MuseGenerator(session))
                    as Arc<dyn crate::ports::TextGenerator>
            }),
            api_key: env::var("MUSE_PROXY_API_KEY")
                .ok()
                .filter(|key| !key.trim().is_empty())
                .map(Into::into),
            sessions: Default::default(),
            decision: crate::adapters::decision::from_env(),
        }))
}

fn conversation_error(error: Box<dyn std::error::Error + Send + Sync>) -> Response {
    tracing::warn!(%error, "Conversation operation failed");
    let missing = error.is::<crate::adapters::muse::session::ConversationNotFound>()
        || error
            .downcast_ref::<crate::adapters::muse::client::ServiceError>()
            .is_some_and(|e| e.0 == 404);
    api_error(
        if missing {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_GATEWAY
        },
        if missing {
            "Conversation not found or no longer available for this Muse account"
        } else {
            "Muse conversation operation failed; see server log"
        },
    )
}

async fn create_conversation(State(state): State<ChatState>) -> Response {
    let Some(session) = state.session else {
        return api_error(StatusCode::SERVICE_UNAVAILABLE, "Muse is not configured");
    };
    match session.create_conversation().await {
        Ok(row) => (StatusCode::CREATED, Json(row.summary())).into_response(),
        Err(error) => conversation_error(error),
    }
}
async fn list_conversations(State(state): State<ChatState>) -> Response {
    let Some(session) = state.session else {
        return api_error(StatusCode::SERVICE_UNAVAILABLE, "Muse is not configured");
    };
    match session.conversations().await {
        Ok(rows) => Json(json!({"data":rows.iter().map(|row| row.summary()).collect::<Vec<_>>()}))
            .into_response(),
        Err(error) => conversation_error(error),
    }
}
async fn get_conversation(State(state): State<ChatState>, Path(id): Path<String>) -> Response {
    let Some(session) = state.session else {
        return api_error(StatusCode::SERVICE_UNAVAILABLE, "Muse is not configured");
    };
    match session.conversation(&id).await {
        Ok(row) => Json(row.summary()).into_response(),
        Err(error) => conversation_error(error),
    }
}

fn api_error(status: StatusCode, message: &str) -> Response {
    let kind = if status.is_server_error() {
        "upstream_error"
    } else {
        "invalid_request_error"
    };
    (
        status,
        Json(json!({"error":{"message":message,"type":kind}})),
    )
        .into_response()
}

fn model_object() -> Value {
    json!({"id":"muse","object":"model","created":0,"owned_by":"muse"})
}
async fn models() -> Json<Value> {
    let mut models = vec![model_object()];
    if crate::adapters::decision::configured() {
        models.push(json!({"id":crate::adapters::decision::MODEL,"object":"model","created":0,"owned_by":"google","description":"Gemini Decision action routing with Gemini 3.5 Flash-Lite argument generation and replies"}));
    }
    Json(json!({"object":"list","data":models}))
}
async fn model(Path(id): Path<String>) -> Response {
    if id == "muse" {
        Json(model_object()).into_response()
    } else if id == crate::adapters::decision::MODEL && crate::adapters::decision::configured() {
        Json(json!({"id":id,"object":"model","created":0,"owned_by":"google"})).into_response()
    } else {
        api_error(StatusCode::NOT_FOUND, "Model not found")
    }
}

async fn completions(State(state): State<ChatState>, Json(value): Json<Value>) -> Response {
    let request = match super::chat_request::parse(value) {
        Ok(request) => request,
        Err(error) => return api_error(StatusCode::BAD_REQUEST, &error),
    };
    let Some(session) = state.session else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Muse is not configured; set MUSE_COOKIE and MUSE_NODE_ID",
        );
    };
    let conversation = match request.conversation_id.as_deref() {
        Some(id) => session.lookup_conversation(id),
        None => session.create_conversation().await,
    };
    let conversation = match conversation {
        Ok(row) => row,
        Err(error) => return conversation_error(error),
    };
    let conversation_id = conversation.id.clone();
    let text = request.text;
    let (sender, receiver) = mpsc::channel::<Result<TurnEvent, String>>(32);
    tokio::spawn(async move {
        if let Err(error) = session.run(text, conversation, sender.clone()).await {
            tracing::warn!(%error, "Muse chat failed");
            let _ = sender
                .send(Err(crate::adapters::muse::session::client_error(
                    error.as_ref(),
                )))
                .await;
        }
    });
    let mut response = if request.stream {
        sse_response(receiver, request.include_usage)
    } else {
        json_response(receiver).await
    };
    response.headers_mut().insert(
        "x-conversation-id",
        conversation_id
            .parse()
            .expect("generated conversation ID is a valid header"),
    );
    response
}

fn sse_response(
    receiver: mpsc::Receiver<Result<TurnEvent, String>>,
    include_usage: bool,
) -> Response {
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let id = format!(
        "chatcmpl-{}-{}",
        created,
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    );
    let events = stream::unfold(
        (receiver, id, created, 0u8),
        move |(mut receiver, id, created, state)| async move {
            let make = |delta: Value, finish: Option<&str>| {
                json!({"id":id,"object":"chat.completion.chunk","created":created,"model":"muse","choices":[{"index":0,"delta":delta,"finish_reason":finish}]}).to_string()
            };
            if state == 0 {
                return Some((
                    Ok::<_, Infallible>(
                        Event::default().data(make(json!({"role":"assistant"}), None)),
                    ),
                    (receiver, id, created, 1),
                ));
            }
            if state == 2 && include_usage {
                return Some((Ok(Event::default().data(json!({"id":id,"object":"chat.completion.chunk","created":created,"model":"muse","choices":[],"usage":null}).to_string())),(receiver,id,created,4)));
            }
            if state == 2 || state == 4 {
                return Some((
                    Ok(Event::default().data("[DONE]")),
                    (receiver, id, created, 3),
                ));
            }
            if state == 3 {
                return None;
            }
            let event = match receiver.recv().await {
            Some(Ok(TurnEvent::Text(text))) => (Event::default().data(make(json!({"content":text}), None)), 1),
            Some(Ok(TurnEvent::QuickReplies(widgets))) => (Event::default().data(make(json!({"muse_quick_replies":widgets}), None)), 1),
            Some(Ok(TurnEvent::MessageDone)) => (Event::default().data(make(json!({"muse_message_done":true}), None)), 1),
            Some(Ok(TurnEvent::Completed)) => (Event::default().data(make(json!({}), Some("stop"))), 2),
            Some(Ok(TurnEvent::Cancelled)) => {
                let mut value: Value = serde_json::from_str(&make(json!({}), Some("stop"))).expect("serialized JSON");
                value["muse_proxy"] = json!({"kind":"cancel"});
                (Event::default().data(value.to_string()), 2)
            },
            Some(Ok(TurnEvent::Progress { text, code })) => {
                let mut value: Value = serde_json::from_str(&make(json!({}), None)).expect("serialized JSON");
                value["muse_proxy"] = json!({"kind":"progress","text":text,"activity_code":code});
                (Event::default().data(value.to_string()), 1)
            },
            Some(Err(message)) => (Event::default().data(json!({"error":{"message":message,"type":"upstream_error"}}).to_string()), 4),
            None => (Event::default().data(json!({"error":{"message":"Muse stream ended unexpectedly","type":"upstream_error"}}).to_string()), 4),
        };
            Some((Ok(event.0), (receiver, id, created, event.1)))
        },
    );
    Sse::new(events)
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn json_response(mut receiver: mpsc::Receiver<Result<TurnEvent, String>>) -> Response {
    let mut text = String::new();
    let mut widgets = Vec::new();
    while let Some(event) = receiver.recv().await {
        match event {
            Ok(TurnEvent::Text(part)) => {
                if text.len() + part.len() > 2 * 1024 * 1024 {
                    return api_error(StatusCode::BAD_GATEWAY, "Muse response exceeded 2 MiB");
                }
                text.push_str(&part);
            }
            Ok(TurnEvent::QuickReplies(replies)) => widgets.extend(replies),
            Ok(TurnEvent::Progress { .. }) | Ok(TurnEvent::MessageDone) => {}
            Ok(terminal @ (TurnEvent::Completed | TurnEvent::Cancelled)) => {
                let cancelled = terminal == TurnEvent::Cancelled;
                let created = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let mut value = json!({"id":format!("chatcmpl-{}",uuid::Uuid::new_v4().simple()),"object":"chat.completion","created":created,"model":"muse","choices":[{"index":0,"message":{"role":"assistant","content":text},"finish_reason":"stop"}]});
                if !widgets.is_empty() {
                    value["choices"][0]["message"]["muse_quick_replies"] = json!(widgets);
                }
                if cancelled {
                    value["muse_proxy"] = json!({"kind":"cancel"});
                }
                return Json(value).into_response();
            }
            Err(error) => return api_error(StatusCode::BAD_GATEWAY, &error),
        }
    }
    api_error(
        StatusCode::BAD_GATEWAY,
        "Muse stream ended without a terminal task event",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn json_response_waits_for_whole_task_and_combines_replies() {
        let (sender, receiver) = mpsc::channel(8);
        let mut response = tokio::spawn(json_response(receiver));
        sender
            .send(Ok(TurnEvent::Text("first".into())))
            .await
            .unwrap();
        sender.send(Ok(TurnEvent::MessageDone)).await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), &mut response)
                .await
                .is_err()
        );
        sender
            .send(Ok(TurnEvent::Progress {
                text: "Working".into(),
                code: None,
            }))
            .await
            .unwrap();
        sender
            .send(Ok(TurnEvent::Text("\n\nsecond".into())))
            .await
            .unwrap();
        sender.send(Ok(TurnEvent::Completed)).await.unwrap();
        let response = response.await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 16384)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["choices"][0]["message"]["content"], "first\n\nsecond");
        assert!(value.get("usage").is_none());
    }

    #[tokio::test]
    async fn cancellation_and_progress_have_explicit_sse_metadata() {
        let (sender, receiver) = mpsc::channel(8);
        sender
            .send(Ok(TurnEvent::Progress {
                text: "Searching".into(),
                code: Some("researching".into()),
            }))
            .await
            .unwrap();
        sender.send(Ok(TurnEvent::Cancelled)).await.unwrap();
        drop(sender);
        let bytes = axum::body::to_bytes(sse_response(receiver, true).into_body(), 16384)
            .await
            .unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let records: Vec<_> = text
            .split("\n\n")
            .filter_map(|line| line.strip_prefix("data: "))
            .collect();
        let progress: Value = serde_json::from_str(records[1]).unwrap();
        let cancel: Value = serde_json::from_str(records[2]).unwrap();
        let usage: Value = serde_json::from_str(records[3]).unwrap();
        assert_eq!(progress["muse_proxy"]["text"], "Searching");
        assert!(progress["choices"][0]["delta"].get("content").is_none());
        assert_eq!(cancel["muse_proxy"]["kind"], "cancel");
        assert!(usage.get("usage").unwrap().is_null());
        assert_eq!(records[4], "[DONE]");
    }

    #[tokio::test]
    async fn json_error_does_not_return_success_with_partial_text() {
        let (sender, receiver) = mpsc::channel(4);
        sender
            .send(Ok(TurnEvent::Text("partial".into())))
            .await
            .unwrap();
        sender.send(Err("Muse task failed".into())).await.unwrap();
        drop(sender);
        assert_eq!(
            json_response(receiver).await.status(),
            StatusCode::BAD_GATEWAY
        );
    }

    #[tokio::test]
    async fn sse_encodes_text_finish_and_done() {
        let (sender, receiver) = mpsc::channel(4);
        sender
            .send(Ok(TurnEvent::Text("hello\nworld".into())))
            .await
            .unwrap();
        sender
            .send(Ok(TurnEvent::QuickReplies(vec![
                crate::core::widgets::QuickReplies {
                    id: "widget-1".into(),
                    options: vec!["Học React".into()],
                },
            ])))
            .await
            .unwrap();
        sender.send(Ok(TurnEvent::Completed)).await.unwrap();
        drop(sender);
        let response = sse_response(receiver, false);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        let bytes = axum::body::to_bytes(response.into_body(), 16_384)
            .await
            .unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        let records: Vec<_> = body
            .split("\n\n")
            .filter_map(|line| line.strip_prefix("data: "))
            .collect();
        assert_eq!(records.len(), 5);
        let role: Value = serde_json::from_str(records[0]).unwrap();
        let text: Value = serde_json::from_str(records[1]).unwrap();
        let suggestions: Value = serde_json::from_str(records[2]).unwrap();
        let finish: Value = serde_json::from_str(records[3]).unwrap();
        assert_eq!(role["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(text["choices"][0]["delta"]["content"], "hello\nworld");
        assert_eq!(
            suggestions["choices"][0]["delta"]["muse_quick_replies"][0]["options"][0],
            "Học React"
        );
        assert_eq!(finish["choices"][0]["finish_reason"], "stop");
        assert_eq!(records[4], "[DONE]");
    }
}
