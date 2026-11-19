/// Inbound port for processing an already decrypted message.
pub trait MessageHandler: Send + Sync + 'static {
    fn handle(&self, message: Vec<u8>) -> Vec<u8>;
}

/// Durable conversation metadata; prompts and credentials are not stored here.
pub trait ConversationStore: Send + Sync {
    fn insert(&self, conversation: crate::core::conversation::Conversation) -> std::io::Result<()>;
    fn get(&self, id: &str) -> std::io::Result<Option<crate::core::conversation::Conversation>>;
    fn list(&self) -> std::io::Result<Vec<crate::core::conversation::Conversation>>;
}
/// Outbound text-event port. Root generators create fresh inference contexts;
/// open_session can provide a generator bound to one isolated conversation.
pub type Generation =
    Result<tokio::sync::mpsc::Receiver<Result<crate::core::turn::TurnEvent, String>>, String>;
pub const UPSTREAM_INPUT_TOO_LARGE: &str = "Muse rejected an oversized input request";
pub type GenerationFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Generation> + Send + 'a>>;
pub type SessionFuture<'a> = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<Option<std::sync::Arc<dyn TextGenerator>>, String>>
            + Send
            + 'a,
    >,
>;

pub trait TextGenerator: Send + Sync {
    fn generate(&self, prompt: String) -> GenerationFuture<'_>;

    /// True only for a generator bound to a persistent context that accepts
    /// staged text uploads. Stateless generators must never opt in.
    fn supports_context_upload(&self) -> bool {
        false
    }

    /// Image-capable backends override this. Never silently discard visual data.
    fn generate_with_images(
        &self,
        prompt: String,
        images: Vec<crate::core::image::InputImage>,
    ) -> GenerationFuture<'_> {
        if images.is_empty() {
            self.generate(prompt)
        } else {
            Box::pin(async { Err("This backend does not support image input".into()) })
        }
    }

    /// Return a generator bound to one isolated remote conversation. Backends
    /// without this capability keep the original stateless behavior.
    fn open_session(&self) -> SessionFuture<'_> {
        Box::pin(async { Ok(None) })
    }
}

/// A backend that returns an entire validated assistant turn, including tool
/// requests. The application remains responsible for executing those tools.
pub type InferenceFuture<'a> = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<crate::core::agent::AgentReply, String>>
            + Send
            + 'a,
    >,
>;
pub trait AgentInference: Send + Sync {
    fn infer<'a>(&'a self, request: &'a crate::core::agent::PreparedRequest)
        -> InferenceFuture<'a>;
}
