//! Muse generators: either fresh inference or an isolated reusable session.
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;

use super::session::{client_error, MuseSession};
use crate::{
    core::{conversation::Conversation, image::InputImage},
    ports::{GenerationFuture, SessionFuture, TextGenerator},
};

pub struct MuseGenerator(pub Arc<MuseSession>);

impl TextGenerator for MuseGenerator {
    fn generate(&self, prompt: String) -> GenerationFuture<'_> {
        Box::pin(async move {
            let bound = self.open_session().await?.expect("Muse supports sessions");
            bound.generate(prompt).await
        })
    }

    fn generate_with_images(
        &self,
        prompt: String,
        images: Vec<InputImage>,
    ) -> GenerationFuture<'_> {
        Box::pin(async move {
            let bound = self.open_session().await?.expect("Muse supports sessions");
            bound.generate_with_images(prompt, images).await
        })
    }

    fn open_session(&self) -> SessionFuture<'_> {
        Box::pin(async move {
            let conversation = tokio::time::timeout(
                Duration::from_secs(40),
                self.0.create_inference_conversation(),
            )
            .await
            .map_err(|_| "Muse session creation timed out".to_string())?
            .map_err(|error| {
                tracing::warn!(%error, "Muse session creation failed before chat submission");
                client_error(error.as_ref())
            })?;
            Ok(Some(Arc::new(MuseConversation {
                session: self.0.clone(),
                conversation,
            }) as Arc<dyn TextGenerator>))
        })
    }
}

struct MuseConversation {
    session: Arc<MuseSession>,
    conversation: Conversation,
}

impl TextGenerator for MuseConversation {
    fn supports_context_upload(&self) -> bool {
        true
    }

    fn generate(&self, prompt: String) -> GenerationFuture<'_> {
        self.generate_with_images(prompt, Vec::new())
    }

    fn generate_with_images(
        &self,
        prompt: String,
        images: Vec<InputImage>,
    ) -> GenerationFuture<'_> {
        Box::pin(async move {
            let session = self.session.clone();
            let conversation = self.conversation.clone();
            let (sender, receiver) = mpsc::channel(32);
            tokio::spawn(async move {
                if let Err(error) = session
                    .run_with_images(prompt, images, conversation, sender.clone())
                    .await
                {
                    tracing::warn!(%error,"Muse agent inference failed");
                    let _ = sender.send(Err(client_error(error.as_ref()))).await;
                }
            });
            Ok(receiver)
        })
    }
}
