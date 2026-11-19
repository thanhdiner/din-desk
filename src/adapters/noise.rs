use snow::{params::NoiseParams, Builder, TransportState};
use std::error::Error;

const NOISE_PATTERN: &str = "Noise_NN_25519_ChaChaPoly_BLAKE2s";
pub(crate) const MAX_NOISE_MESSAGE: usize = 65_535;
type NoiseResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// Noise NN responder and per-connection transport state.
pub(crate) struct NoiseSession {
    transport: TransportState,
    buffer: Vec<u8>,
}

impl NoiseSession {
    pub(crate) fn accept(first: &[u8]) -> NoiseResult<(Self, Vec<u8>)> {
        let params: NoiseParams = NOISE_PATTERN.parse()?;
        let mut handshake = Builder::new(params).build_responder()?;
        let mut buffer = vec![0; MAX_NOISE_MESSAGE];
        if handshake.read_message(first, &mut buffer)? != 0 {
            return Err("handshake payload must be empty".into());
        }
        let len = handshake.write_message(&[], &mut buffer)?;
        let response = buffer[..len].to_vec();
        let transport = handshake.into_transport_mode()?;
        Ok((Self { transport, buffer }, response))
    }

    pub(crate) fn decrypt(&mut self, ciphertext: &[u8]) -> NoiseResult<Vec<u8>> {
        let len = self.transport.read_message(ciphertext, &mut self.buffer)?;
        Ok(self.buffer[..len].to_vec())
    }

    pub(crate) fn encrypt(&mut self, plaintext: &[u8]) -> NoiseResult<Vec<u8>> {
        let len = self.transport.write_message(plaintext, &mut self.buffer)?;
        Ok(self.buffer[..len].to_vec())
    }
}
