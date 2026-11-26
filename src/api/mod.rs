mod chat;
mod chat_request;
pub(crate) mod gemini;
mod proxy_auth;
mod responses;
mod websocket;

use crate::ports::MessageHandler;
use axum::{middleware, routing::get, Router};
use std::sync::Arc;

pub fn router(handler: Arc<dyn MessageHandler>) -> Router {
    let routes = Router::new()
        .route("/ws", get(websocket::upgrade))
        .with_state(handler)
        .merge(chat::router());
    let routes = match proxy_auth::ProxyAuth::from_env() {
        Some(auth) => routes.route_layer(middleware::from_fn_with_state(auth, proxy_auth::require_key)),
        None => routes,
    };
    routes.route("/health", get(|| async { "ok" }))
}
mod anthropic;
