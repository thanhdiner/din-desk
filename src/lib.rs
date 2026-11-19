pub mod adapters;
pub mod api;
pub mod config;
pub mod core;
pub mod infrastructure;
pub mod ports;

use core::EchoService;
use std::sync::Arc;

/// Composition root: wire the application to its inbound HTTP adapter.
pub fn app() -> axum::Router {
    api::router(Arc::new(EchoService))
}
