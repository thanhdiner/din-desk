use crate::ports::MessageHandler;

/// Example use case; replace or extend it when application behavior is defined.
pub struct EchoService;

impl MessageHandler for EchoService {
    fn handle(&self, message: Vec<u8>) -> Vec<u8> {
        message
    }
}
