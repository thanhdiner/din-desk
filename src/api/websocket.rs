use crate::{
    adapters::noise::{NoiseSession, MAX_NOISE_MESSAGE},
    ports::MessageHandler,
};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use std::{error::Error, sync::Arc, time::Duration};
use tracing::{debug, warn};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
type SessionResult = Result<(), Box<dyn Error + Send + Sync>>;

pub(super) async fn upgrade(
    ws: WebSocketUpgrade,
    State(handler): State<Arc<dyn MessageHandler>>,
) -> Response {
    ws.max_message_size(MAX_NOISE_MESSAGE)
        .max_frame_size(MAX_NOISE_MESSAGE)
        .on_upgrade(move |socket| async move {
            if let Err(error) = run_session(socket, handler).await {
                debug!(%error, "websocket session ended");
            }
        })
}

async fn run_session(mut socket: WebSocket, handler: Arc<dyn MessageHandler>) -> SessionResult {
    let first = tokio::time::timeout(HANDSHAKE_TIMEOUT, socket.recv())
        .await?
        .ok_or("connection closed during handshake")??;
    let Message::Binary(first) = first else {
        return Err("expected binary Noise handshake".into());
    };
    let (mut noise, response) = NoiseSession::accept(&first)?;
    socket.send(Message::Binary(response.into())).await?;

    while let Some(message) = socket.recv().await {
        match message? {
            Message::Binary(ciphertext) => {
                let plaintext = noise.decrypt(&ciphertext)?;
                let response = handler.handle(plaintext);
                let ciphertext = noise.encrypt(&response)?;
                socket.send(Message::Binary(ciphertext.into())).await?;
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => {}
            Message::Text(_) => {
                warn!("rejecting text message on Noise websocket");
                return Err("expected binary Noise message".into());
            }
        }
    }
    Ok(())
}
