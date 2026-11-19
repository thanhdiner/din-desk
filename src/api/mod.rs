mod chat;
mod chat_request;
pub(crate) mod gemini;
mod websocket;

use crate::ports::MessageHandler;
use axum::{routing::get, Router};
use std::sync::Arc;

pub fn router(handler: Arc<dyn MessageHandler>) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/ws", get(websocket::upgrade))
        .with_state(handler)
        .merge(chat::router())
}
mod anthropic;
