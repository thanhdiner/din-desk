//! Anthropic Messages adapter for Claude Code. Prompt-based tools are opt-in
//! through this endpoint; existing OpenAI chat semantics remain separate.
pub(super) mod request;
pub(super) mod response;
mod session;
#[cfg(test)]
mod tests;

use std::{sync::Arc, time::Duration};

use axum::{
    extract::{rejection::JsonRejection, DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::{
    core::{agent::{AgentReply, PreparedRequest, MAX_CONTEXT_BYTES}, agent_sessions::AgentSessions},
    ports::{AgentInference, TextGenerator},
};

#[derive(Clone)]
pub(super) struct AgentState {
    pub model: &'static str,
    pub generator: Option<Arc<dyn TextGenerator>>,
    pub api_key: Option<Arc<str>>,
    pub sessions: Arc<AgentSessions>,
    pub decision: Option<Arc<dyn AgentInference>>,
}

pub(super) fn router(state: AgentState) -> Router {
    Router::new()
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .route("/v1/responses", post(super::responses::handle))
        .layer(DefaultBodyLimit::max(MAX_CONTEXT_BYTES))
        .with_state(state)
}

pub(super) struct ApiError(StatusCode, &'static str, &'static str);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        response::error(self.0, self.1, self.2)
    }
}

pub(super) fn authorize(headers: &HeaderMap, state: &AgentState) -> Result<(), ApiError> {
    let Some(expected) = state.api_key.as_deref().filter(|key| !key.is_empty()) else {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "api_error",
            "Set MUSE_PROXY_API_KEY on the server to enable this adapter",
        ));
    };
    if !super::proxy_auth::matches_key(headers, expected) {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "Invalid proxy API key",
        ));
    }
    Ok(())
}

pub(super) fn authorization_error(headers: &HeaderMap, state: &AgentState) -> Option<Response> {
    authorize(headers, state)
        .err()
        .map(IntoResponse::into_response)
}

fn stamp(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        "request-id",
        format!("req_{}", uuid::Uuid::new_v4().simple())
            .parse()
            .expect("UUID header"),
    );
    if !headers.contains_key("x-muse-tool-mode") {
        headers.insert("x-muse-tool-mode", "prompt-json".parse().expect("constant"));
    }
    if !headers.contains_key("x-muse-usage") {
        headers.insert(
            "x-muse-usage",
            "estimated-text-and-images".parse().expect("constant"),
        );
    }
    response
}

fn decode(body: Result<Json<Value>, JsonRejection>) -> Result<Value, ApiError> {
    body.map(|Json(value)| value).map_err(|error| {
        ApiError(
            error.status(),
            "invalid_request_error",
            "Expected a JSON request body of at most 4 MiB",
        )
    })
}

async fn count_tokens(
    State(state): State<AgentState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    stamp(count_tokens_inner(state, headers, body).await)
}

async fn count_tokens_inner(
    state: AgentState,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    if let Err(error) = authorize(&headers, &state) {
        return error.into_response();
    }
    let mut value = match decode(body) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Some(object) = value.as_object_mut() {
        object.insert("max_tokens".into(), json!(1));
        object.insert("stream".into(), json!(false));
    }
    let model = if value["model"] == crate::adapters::decision::MODEL && state.decision.is_some() {
        crate::adapters::decision::MODEL
    } else {
        state.model
    };
    match request::parse_for_model(value, model).and_then(|request| request.agent.prepare()) {
        Ok(prepared) => (
            StatusCode::OK,
            Json(json!({"input_tokens":prepared.input_tokens})),
        )
            .into_response(),
        Err(error) => response::error(StatusCode::BAD_REQUEST, "invalid_request_error", &error),
    }
}

use axum::response::IntoResponse;

async fn messages(
    State(state): State<AgentState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    stamp(messages_inner(state, headers, body).await)
}

pub(super) async fn messages_inner(
    state: AgentState,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    if let Err(error) = authorize(&headers, &state) {
        return error.into_response();
    }
    let value = match decode(body) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let client = match session::resolve(&headers, &value) {
        Ok(client) => client,
        Err(error) => {
            return response::error(StatusCode::BAD_REQUEST, "invalid_request_error", &error)
        }
    };
    let decision = (value["model"] == crate::adapters::decision::MODEL)
        .then_some(state.decision.clone())
        .flatten();
    let model = if decision.is_some() {
        crate::adapters::decision::MODEL
    } else {
        state.model
    };
    let request = match request::parse_for_model(value, model) {
        Ok(request) => request,
        Err(error) => {
            return response::error(StatusCode::BAD_REQUEST, "invalid_request_error", &error)
        }
    };
    if state.generator.is_none() && decision.is_none() {
        return response::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "api_error",
            "The selected backend is not configured; connect its account in Muse Desk",
        );
    }
    let prepared = match request.agent.prepare() {
        Ok(prepared) => prepared,
        Err(error) => {
            return response::error(StatusCode::BAD_REQUEST, "invalid_request_error", &error)
        }
    };
    let id = format!("msg_{}", uuid::Uuid::new_v4().simple());
    let input_tokens = prepared.input_tokens;
    let decision_mode = decision.is_some();
    let receiver = spawn_inference(state, decision, prepared, client);
    let mut response = if request.stream {
        response::sse_model(receiver, &id, input_tokens, model)
    } else {
        match receiver.await {
            Ok(Ok(reply)) => response::json_model(reply, &id, model),
            Ok(Err(error)) => response::inference_error(&error),
            Err(_) => response::error(
                StatusCode::BAD_GATEWAY,
                "api_error",
                "Muse agent request ended unexpectedly",
            ),
        }
    };
    if decision_mode {
        response.headers_mut().insert(
            "x-muse-tool-mode",
            "decision-router".parse().expect("constant"),
        );
        response.headers_mut().insert(
            "x-muse-usage",
            "provider-usage-with-estimate-fallback"
                .parse()
                .expect("constant"),
        );
    }
    response
}

pub(super) fn spawn_inference(
    state: AgentState,
    decision: Option<Arc<dyn AgentInference>>,
    prepared: PreparedRequest,
    client: Option<String>,
) -> oneshot::Receiver<Result<AgentReply, String>> {
    let (mut sender, receiver) = oneshot::channel();
    tokio::spawn(async move {
        let inference = async {
            if let Some(decision) = decision {
                decision.infer(&prepared).await
            } else {
                state.sessions.infer(state.generator.expect("checked generator"), prepared, client.as_deref()).await
            }
        };
        // The response owns the receiver, so a disconnected client cancels inference.
        tokio::select! {
            _ = sender.closed() => {},
            result = tokio::time::timeout(Duration::from_secs(180), inference) => {
                let result = result.unwrap_or_else(|_| Err("Muse agent request timed out".into()));
                if result.is_err() { tracing::warn!("Muse agent inference failed; no client tools dispatched"); }
                let _ = sender.send(result);
            }
        }
    });
    receiver
}
