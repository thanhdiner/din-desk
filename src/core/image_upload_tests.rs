use super::*;
use crate::{core::turn::TurnEvent, ports::GenerationFuture};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Clone, Copy)]
enum Failure {
    Task,
    Size,
    Timeout,
    Disconnect,
    Cancel,
    Incomplete,
    WrongReceipt,
    Hang,
}
struct Backend {
    limit: usize,
    failure: Failure,
    calls: Mutex<Vec<Vec<String>>>,
    accepted: Mutex<Vec<String>>,
}
impl Backend {
    fn new(limit: usize, failure: Failure) -> Self {
        Self {
            limit,
            failure,
            calls: Mutex::new(vec![]),
            accepted: Mutex::new(vec![]),
        }
    }
}
impl TextGenerator for Backend {
    fn generate(&self, prompt: String) -> GenerationFuture<'_> {
        self.generate_with_images(prompt, vec![])
    }
    fn generate_with_images(
        &self,
        prompt: String,
        images: Vec<InputImage>,
    ) -> GenerationFuture<'_> {
        Box::pin(async move {
            let ids: Vec<_> = images.iter().map(|image| image.id.clone()).collect();
            if !images.is_empty() {
                self.calls.lock().unwrap().push(ids.clone());
            }
            if images.len() > self.limit {
                match self.failure {
                    Failure::Task => return Err("Muse task failed".into()),
                    Failure::Size => return Err(crate::ports::UPSTREAM_INPUT_TOO_LARGE.into()),
                    Failure::Timeout => return Err("Muse chat timed out".into()),
                    Failure::Disconnect => return Err("fixture disconnected".into()),
                    Failure::Hang => return std::future::pending().await,
                    _ => {}
                }
            }
            let ack = prompt.lines().last().unwrap().to_owned();
            let (sender, receiver) = mpsc::channel(2);
            if matches!(self.failure, Failure::Cancel) {
                sender.send(Ok(TurnEvent::Cancelled)).await.unwrap();
                return Ok(receiver);
            }
            sender
                .send(Ok(TurnEvent::Text(
                    if matches!(self.failure, Failure::WrongReceipt) {
                        "wrong receipt".into()
                    } else {
                        ack
                    },
                )))
                .await
                .unwrap();
            if !matches!(self.failure, Failure::Incomplete) {
                sender.send(Ok(TurnEvent::Completed)).await.unwrap();
                self.accepted.lock().unwrap().extend(ids);
            }
            Ok(receiver)
        })
    }
}
fn images(count: usize) -> Vec<InputImage> {
    (0..count)
        .map(|index| InputImage {
            id: format!("image_{index}"),
            media_type: "image/png".into(),
            data_base64: Arc::from("fixture-bytes"),
        })
        .collect()
}

#[tokio::test]
async fn all_eleven_images_use_one_batch_and_one_receipt_when_accepted() {
    let backend = Backend::new(11, Failure::Task);
    let images = images(11);
    upload(&backend, "nonce", &images).await.unwrap();
    assert_eq!(
        backend
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        vec![11]
    );
    assert_eq!(
        *backend.accepted.lock().unwrap(),
        images
            .iter()
            .map(|image| image.id.clone())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn odd_batches_bisect_recursively_and_never_replay_successful_children() {
    for failure in [Failure::Task, Failure::Size] {
        let backend = Backend::new(3, failure);
        let images = images(11);
        upload(&backend, "nonce", &images).await.unwrap();
        assert_eq!(
            backend
                .calls
                .lock()
                .unwrap()
                .iter()
                .map(Vec::len)
                .collect::<Vec<_>>(),
            vec![11, 5, 2, 3, 6, 3, 3]
        );
        assert_eq!(
            *backend.accepted.lock().unwrap(),
            images
                .iter()
                .map(|image| image.id.clone())
                .collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn singleton_failure_stops_and_uncertain_or_wrong_receipts_are_never_split() {
    let backend = Backend::new(0, Failure::Size);
    let error = upload(&backend, "nonce", &images(1)).await.unwrap_err();
    assert!(crate::core::agent::terminal_failure(&error));
    assert_eq!(backend.calls.lock().unwrap().len(), 1);
    for failure in [
        Failure::Timeout,
        Failure::Disconnect,
        Failure::Cancel,
        Failure::Incomplete,
        Failure::WrongReceipt,
    ] {
        let backend = Backend::new(0, failure);
        let error = upload(&backend, "nonce", &images(11)).await.unwrap_err();
        assert!(crate::core::agent::terminal_failure(&error));
        assert_eq!(backend.calls.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn empty_input_and_expired_budget_do_not_send_any_images() {
    let backend = Backend::new(11, Failure::Task);
    upload(&backend, "nonce", &[]).await.unwrap();
    assert!(upload_before(
        &backend,
        "nonce",
        &images(11),
        tokio::time::Instant::now() - Duration::from_secs(1)
    )
    .await
    .is_err());
    assert!(backend.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn worst_case_is_finite_and_inflight_timeout_never_starts_smaller_batches() {
    let backend = Backend::new(1, Failure::Size);
    let input = images(128);
    upload(&backend, "nonce", &input).await.unwrap();
    assert_eq!(backend.calls.lock().unwrap().len(), 255);
    assert_eq!(backend.accepted.lock().unwrap().len(), 128);
    let backend = Backend::new(0, Failure::Hang);
    let error = upload_before(
        &backend,
        "nonce",
        &images(11),
        tokio::time::Instant::now() + Duration::from_millis(20),
    )
    .await
    .unwrap_err();
    assert!(crate::core::agent::terminal_failure(&error));
    assert_eq!(backend.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
#[ignore = "requires MUSE_COOKIE and a generated synthetic image fixture; calls real Muse"]
async fn muse_live_twelve_images_share_one_receipt_and_preserve_every_visual() {
    use crate::{
        adapters::muse::{agent::MuseGenerator, session::MuseSession},
        core::agent::{infer_uploaded, AgentRequest, ToolChoice},
    };
    use serde_json::json;
    let _ = tracing_subscriber::fmt()
        .with_env_filter("muse_api=info")
        .try_init();
    let fixture: serde_json::Value = serde_json::from_slice(
        &std::fs::read(std::env::var("MUSE_TEST_IMAGE_FIXTURE").unwrap()).unwrap(),
    )
    .unwrap();
    let images: Vec<_> = fixture["images"]
        .as_array()
        .unwrap()
        .iter()
        .map(|image| {
            InputImage::from_base64(
                image["media_type"].as_str().unwrap(),
                image["data"].as_str().unwrap(),
            )
            .unwrap()
        })
        .collect();
    assert_eq!(images.len(), 12);
    let mut blocks = vec![
        json!({"type":"text","text":"Diagnostic only. Read the large CARD label from EVERY attached image. Return all labels in attachment order. Do not use tools. Read the actual pixels, not the hashes or filenames."}),
    ];
    blocks.extend(
        images
            .iter()
            .map(|image| json!({"type":"image","image_id":image.id})),
    );
    let prepared = AgentRequest {
        system: json!([]),
        messages: json!([{"role":"user","content":blocks}]),
        tools: vec![],
        choice: ToolChoice::None,
        parallel: false,
        max_tokens: 1024,
        images,
    }
    .prepare()
    .unwrap();
    struct Counted {
        bound: Arc<dyn TextGenerator>,
        calls: Mutex<Vec<usize>>,
        limit: usize,
    }
    impl TextGenerator for Counted {
        fn supports_context_upload(&self) -> bool {
            true
        }
        fn generate(&self, prompt: String) -> GenerationFuture<'_> {
            self.bound.generate(prompt)
        }
        fn generate_with_images(
            &self,
            prompt: String,
            images: Vec<InputImage>,
        ) -> GenerationFuture<'_> {
            if !images.is_empty() {
                self.calls.lock().unwrap().push(images.len());
            }
            if images.len() > self.limit {
                return Box::pin(async { Err(crate::ports::UPSTREAM_INPUT_TOO_LARGE.into()) });
            }
            self.bound.generate_with_images(prompt, images)
        }
    }
    let root = MuseGenerator(Arc::new(
        MuseSession::new(
            &std::env::var("MUSE_COOKIE").unwrap(),
            std::env::var("MUSE_NODE_ID").unwrap(),
        )
        .unwrap(),
    ));
    let backend = Counted {
        bound: root.open_session().await.unwrap().unwrap(),
        calls: Mutex::new(vec![]),
        limit: std::env::var("MUSE_TEST_IMAGE_BATCH_LIMIT")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(128),
    };
    let reply = tokio::time::timeout(
        Duration::from_secs(180),
        infer_uploaded(&backend, &prepared),
    )
    .await
    .unwrap()
    .unwrap();
    let text: String = reply
        .content
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect();
    let mut offset = 0;
    for label in fixture["labels"].as_array().unwrap() {
        let label = label.as_str().unwrap();
        offset += text[offset..]
            .find(label)
            .expect("A visual label was not retained in input order")
            + label.len();
    }
    let batches = backend.calls.lock().unwrap();
    assert_eq!(
        *batches,
        if backend.limit == 3 {
            vec![12, 6, 3, 3, 6, 3, 3]
        } else {
            vec![12]
        },
        "Real Muse needed smaller batches; inspect logs before changing expectations"
    );
    println!("Real Muse: images=12, image_batches={batches:?}, all_visual_labels_matched=true, client_tools_dispatched=false");
}
