use futures_util::{SinkExt, StreamExt};
use snow::{params::NoiseParams, Builder};
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[tokio::test]
async fn noise_handshake_and_encrypted_echo() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, muse_api::app()).await.unwrap() });
    let (mut socket, _) = connect_async(format!("ws://{address}/ws")).await.unwrap();
    let params: NoiseParams = "Noise_NN_25519_ChaChaPoly_BLAKE2s".parse().unwrap();
    let mut noise = Builder::new(params).build_initiator().unwrap();
    let mut buffer = vec![0; 65_535];
    let len = noise.write_message(&[], &mut buffer).unwrap();
    socket
        .send(Message::Binary(buffer[..len].to_vec().into()))
        .await
        .unwrap();
    let Message::Binary(response) = socket.next().await.unwrap().unwrap() else {
        panic!("expected binary handshake")
    };
    assert_eq!(noise.read_message(&response, &mut buffer).unwrap(), 0);
    let mut transport = noise.into_transport_mode().unwrap();
    let len = transport
        .write_message(b"hello Noise", &mut buffer)
        .unwrap();
    socket
        .send(Message::Binary(buffer[..len].to_vec().into()))
        .await
        .unwrap();
    let Message::Binary(response) = socket.next().await.unwrap().unwrap() else {
        panic!("expected binary response")
    };
    let len = transport.read_message(&response, &mut buffer).unwrap();
    assert_eq!(&buffer[..len], b"hello Noise");
    server.abort();
}
