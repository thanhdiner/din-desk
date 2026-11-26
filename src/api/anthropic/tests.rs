use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::{request, router, AgentState};
use crate::{
    core::{
        agent::{infer, ToolChoice},
        turn::TurnEvent,
    },
    ports::{GenerationFuture, TextGenerator},
};

const KEY: &str = "test-key-only";

#[test]
fn desktop_routes_accept_default_thinking_only_for_the_active_backend() {
    for (model, other) in [("muse", "gemini"), ("gemini", "muse")] {
        let mut body = json!({"model":format!("claude-dindesk-{model}"), "max_tokens":32,
            "messages":[{"role":"user","content":"hello"}],"thinking":{"type":"adaptive"}});
        assert!(request::parse_for_model(body.clone(), model).is_ok());
        assert!(request::parse_for_model(body.clone(), other).is_err());
        body["model"] = json!(model);
        assert!(request::parse_for_model(body, model).is_err());
    }
}

#[test]
fn decision_quota_and_nonretryable_failures_have_explicit_http_statuses() {
    use axum::http::StatusCode;
    assert_eq!(
        super::response::failure_kind("Decision service returned HTTP 429; no tools dispatched").0,
        StatusCode::TOO_MANY_REQUESTS
    );
    for error in [
        "Decision service is uncertain about the next action; no tools dispatched",
        "Decision writer did not finish normally; no tools dispatched",
        "Decision context exceeds 512 KiB",
    ] {
        assert_eq!(
            super::response::failure_kind(error).0,
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn decision_model_is_explicit_authenticated_and_streams_without_using_muse() {
    use crate::ports::{AgentInference, InferenceFuture};
    struct Decision;
    impl AgentInference for Decision {
        fn infer<'a>(
            &'a self,
            request: &'a crate::core::agent::PreparedRequest,
        ) -> InferenceFuture<'a> {
            Box::pin(async move {
                request.parse_reply(&json!({"bridge_id":request.nonce,"content":[{"type":"text","text":"decision fixture reply"}]}).to_string())
            })
        }
    }
    let app = router(AgentState {
        model: "muse",
        generator: None,
        decision: Some(Arc::new(Decision)),
        api_key: Some(KEY.into()),
        sessions: Default::default(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let http = reqwest::Client::new();
    let mut body = base();
    body["model"] = json!(crate::adapters::decision::MODEL);
    let denied = http
        .post(format!("{url}/v1/messages"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 401);
    let count = http
        .post(format!("{url}/v1/messages/count_tokens"))
        .bearer_auth(KEY)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(count.status(), 200);
    for stream in [false, true] {
        body["stream"] = json!(stream);
        let response = http
            .post(format!("{url}/v1/messages"))
            .bearer_auth(KEY)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["x-muse-tool-mode"], "decision-router");
        let text = response.text().await.unwrap();
        assert!(text.contains("decision fixture reply"));
        assert!(text.contains("gemini-decision"));
        if stream {
            assert!(text.contains("message_stop"));
        }
    }
    body["model"] = json!("muse");
    body["stream"] = json!(false);
    let original = http
        .post(format!("{url}/v1/messages"))
        .bearer_auth(KEY)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(
        original.status(),
        503,
        "Muse must not silently use Decision when its own backend is absent"
    );
    server.abort();
}

#[tokio::test]
async fn invalid_json_repairs_once_then_stream_to_json_fallback_fails_fast_without_tools() {
    struct Invalid(std::sync::atomic::AtomicUsize);
    impl TextGenerator for Invalid {
        fn supports_context_upload(&self) -> bool {
            true
        }
        fn generate(&self, _: String) -> GenerationFuture<'_> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async {
                let (sender, receiver) = mpsc::channel(2);
                sender
                    .send(Ok(TurnEvent::Text(
                        "{\"content\":[{\"type\":\"tool_use\"".into(),
                    )))
                    .await
                    .unwrap();
                sender.send(Ok(TurnEvent::Completed)).await.unwrap();
                Ok(receiver)
            })
        }
    }
    let backend = Arc::new(Invalid(std::sync::atomic::AtomicUsize::new(0)));
    let (url, server) = start(Some(backend.clone()), Some(KEY)).await;
    let http = reqwest::Client::new();
    let session = uuid::Uuid::new_v4().to_string();
    let mut body = base();
    body["stream"] = json!(true);
    let response = http
        .post(format!("{url}/v1/messages"))
        .bearer_auth(KEY)
        .header("x-claude-code-session-id", &session)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let text = response.text().await.unwrap();
    assert!(text.contains("invalid_request_error"));
    assert!(!text.contains("content_block_start"));
    assert!(!text.contains("message_stop"));
    assert_eq!(backend.0.load(std::sync::atomic::Ordering::SeqCst), 2);
    body["stream"] = json!(false);
    for _ in 0..2 {
        let response = http
            .post(format!("{url}/v1/messages"))
            .bearer_auth(KEY)
            .header("x-claude-code-session-id", &session)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        let reply = response.json::<Value>().await.unwrap();
        assert_eq!(reply["error"]["type"], "invalid_request_error");
        assert!(reply.get("content").is_none());
    }
    assert_eq!(backend.0.load(std::sync::atomic::Ordering::SeqCst), 2);
    body["messages"][0]["content"] = json!("changed task");
    let response = http
        .post(format!("{url}/v1/messages"))
        .bearer_auth(KEY)
        .header("x-claude-code-session-id", &session)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(backend.0.load(std::sync::atomic::Ordering::SeqCst), 4);
    server.abort();
}

#[tokio::test]
async fn completed_provider_explanation_reaches_stream_and_json_as_text_without_tools() {
    struct Prose;
    impl TextGenerator for Prose {
        fn supports_context_upload(&self) -> bool {
            true
        }
        fn generate(&self, _: String) -> GenerationFuture<'_> {
            Box::pin(async {
                let (sender, receiver) = mpsc::channel(2);
                sender
                    .send(Ok(TurnEvent::Text(
                        "I cannot fulfill this application request.".into(),
                    )))
                    .await
                    .unwrap();
                sender.send(Ok(TurnEvent::Completed)).await.unwrap();
                Ok(receiver)
            })
        }
    }
    let (url, server) = start(Some(Arc::new(Prose)), Some(KEY)).await;
    for stream in [false, true] {
        let mut body = base();
        body["stream"] = json!(stream);
        let response = reqwest::Client::new()
            .post(format!("{url}/v1/messages"))
            .bearer_auth(KEY)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let text = response.text().await.unwrap();
        assert!(text.contains("I cannot fulfill this application request."));
        assert!(!text.contains("tool_use"));
        assert!(!text.contains("invalid_request_error"));
        if stream {
            assert!(text.contains("message_stop"));
        } else {
            let reply: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(reply["stop_reason"], "end_turn");
            assert_eq!(reply["content"][0]["type"], "text");
        }
    }
    server.abort();
}

#[tokio::test]
async fn native_session_http_resume_isolated_chats_and_compacted_history() {
    use crate::ports::SessionFuture;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Shared {
        opened: AtomicUsize,
        calls: Mutex<Vec<(usize, bool)>>,
    }
    struct Root(Arc<Shared>);
    struct Bound(Arc<Shared>, usize);
    impl TextGenerator for Root {
        fn generate(&self, _: String) -> GenerationFuture<'_> {
            panic!("expected bound session")
        }
        fn open_session(&self) -> SessionFuture<'_> {
            Box::pin(async {
                Ok(Some(Arc::new(Bound(
                    self.0.clone(),
                    self.0.opened.fetch_add(1, Ordering::SeqCst),
                )) as Arc<dyn TextGenerator>))
            })
        }
    }
    impl TextGenerator for Bound {
        fn generate(&self, prompt: String) -> GenerationFuture<'_> {
            Box::pin(async move {
                let delta = prompt.contains("CONTEXT_DELTA\n");
                self.0.calls.lock().unwrap().push((self.1, delta));
                let nonce = if delta {
                    prompt
                        .split("bridge_id exactly ")
                        .nth(1)
                        .unwrap()
                        .split_whitespace()
                        .next()
                        .unwrap()
                } else {
                    prompt
                        .split("bridge_id exactly: ")
                        .nth(1)
                        .unwrap()
                        .split('.')
                        .next()
                        .unwrap()
                };
                let (sender, receiver) = mpsc::channel(2);
                sender
                    .send(Ok(TurnEvent::Text(answer(
                        nonce,
                        json!([{"type":"text","text":"OK"}]),
                    ))))
                    .await
                    .unwrap();
                sender.send(Ok(TurnEvent::Completed)).await.unwrap();
                Ok(receiver)
            })
        }
    }
    let shared = Arc::new(Shared {
        opened: AtomicUsize::new(0),
        calls: Mutex::new(vec![]),
    });
    let (url, server) = start(Some(Arc::new(Root(shared.clone()))), Some(KEY)).await;
    let http = reqwest::Client::new();
    let native = uuid::Uuid::new_v4().to_string();
    let other = uuid::Uuid::new_v4().to_string();
    let mut history = vec![json!({"role":"user","content":"First"})];
    let mut first_reply = None;
    // Each call uses a different launcher ID. Only the native conversation ID
    // should join the first two requests; compaction must still force full load.
    for (index, id) in [&native, &native, &other, &native, &native]
        .into_iter()
        .enumerate()
    {
        let messages = if index == 3 {
            json!([{"role":"user","content":"Compacted replacement history"}])
        } else {
            json!(history)
        };
        let mut body = json!({"model":"muse","max_tokens":128,"messages":messages});
        if index % 2 == 0 {
            body["metadata"] = json!({"user_id":format!("user_account_session_{id}")});
        } else {
            body["metadata"] = json!({"user_id":json!({"session_id":id}).to_string()});
        }
        let response = http
            .post(format!("{url}/v1/messages"))
            .bearer_auth(KEY)
            .header("x-muse-client-id", uuid::Uuid::new_v4().to_string())
            .header("x-claude-code-session-id", id)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let reply = response.json::<Value>().await.unwrap();
        if index == 0 {
            first_reply = Some(reply["content"].clone());
            history.push(json!({"role":"assistant","content":reply["content"]}));
            history.push(json!({"role":"user","content":"Resume"}));
        }
    }
    assert!(first_reply.is_some());
    let calls = shared.calls.lock().unwrap();
    assert_eq!(
        *calls,
        vec![(0, false), (0, true), (1, false), (2, false), (3, false)]
    );
    // Identical history already answered is a branch/retry, not an appended turn.
    assert_eq!(shared.opened.load(Ordering::SeqCst), 4);
    server.abort();
}

#[test]
fn image_history_beyond_eight_accepts_new_turn_without_dropping_old_visuals() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    fn image(index: usize) -> Value {
        let mut bytes = b"\x89PNG\r\n\x1a\nfixture".to_vec();
        bytes.extend_from_slice(&(index as u64).to_le_bytes());
        json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":STANDARD.encode(bytes)}})
    }
    let mut value = base();
    let mut history = Vec::new();
    for index in 0..9 {
        history.push(json!({"role":"user","content":[image(index)]}));
        history.push(json!({"role":"assistant","content":"Observed image"}));
    }
    history
        .push(json!({"role":"user","content":[{"type":"text","text":"New screenshot"},image(9)]}));
    value["messages"] = json!(history);
    let prepared = request::parse(value).unwrap().agent.prepare().unwrap();
    assert_eq!(prepared.images_from(0).len(), 10);
    let delta = prepared.images_from(18);
    assert_eq!(delta.len(), 1);
    assert_eq!(delta[0].id, prepared.request.images[9].id);

    let mut value = base();
    let limit = crate::core::image::MAX_IMAGES;
    let mut blocks: Vec<_> = (0..limit).map(image).collect();
    blocks.push(image(0)); // Repeated visual must not consume another slot.
    value["messages"] = json!([{"role":"user","content":blocks}]);
    assert_eq!(
        request::parse(value.clone()).unwrap().agent.images.len(),
        limit
    );
    value["messages"][0]["content"]
        .as_array_mut()
        .unwrap()
        .push(image(limit));
    assert!(request::parse(value)
        .err()
        .unwrap()
        .contains("compact the conversation"));
}

fn base() -> Value {
    json!({"model":"muse","max_tokens":1024,"messages":[{"role":"user","content":"Read my file"}],"tools":[{"name":"Read","input_schema":{"type":"object","properties":{"file_path":{"type":"string"}},"required":["file_path"],"additionalProperties":false}}]})
}

fn prepared() -> crate::core::agent::PreparedRequest {
    request::parse(base()).unwrap().agent.prepare().unwrap()
}

fn answer(nonce: &str, blocks: Value) -> String {
    json!({"bridge_id":nonce,"content":blocks}).to_string()
}

const PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/Nl8AAAAASUVORK5CYII=";
fn image() -> Value {
    json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":PNG}})
}

#[test]
fn accepts_user_and_tool_images_but_keeps_binary_out_of_prompts_and_old_deltas() {
    let mut value = base();
    value["messages"] = json!([{"role":"user","content":[image(),{"type":"text","text":"What is in this image?"}]}]);
    let prepared = request::parse(value.clone())
        .unwrap()
        .agent
        .prepare()
        .unwrap();
    assert_eq!(prepared.request.images.len(), 1);
    let id = prepared.request.images[0].id.clone();
    assert!(prepared.prompt.contains(&id));
    assert!(!prepared.prompt.contains(PNG));
    assert_eq!(prepared.images_from(0).len(), 1);
    assert!(prepared.input_tokens < 5000);
    value["messages"] = json!([
        {"role":"user","content":[image(),{"type":"text","text":"What is in this image?"}]},
        {"role":"assistant","content":[{"type":"text","text":"An image"}]},
        {"role":"user","content":"And its colour?"}
    ]);
    assert!(request::parse(value)
        .unwrap()
        .agent
        .prepare()
        .unwrap()
        .images_from(2)
        .is_empty());
    let mut value = base();
    value["messages"] = json!([
        {"role":"user","content":"Read the image file"},
        {"role":"assistant","content":[{"type":"tool_use","id":"toolu_read","name":"Read","input":{"file_path":"test.png"}}]},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_read","content":[{"type":"text","text":"Image file"},image()]}]}
    ]);
    let prepared = request::parse(value).unwrap().agent.prepare().unwrap();
    assert_eq!(prepared.images_from(2)[0].id, id);
    assert!(!prepared.prompt.contains(PNG));
}

#[tokio::test]
async fn http_dispatches_images_through_the_visual_port_and_rejects_invalid_sources() {
    struct Vision;
    impl TextGenerator for Vision {
        fn generate(&self, _: String) -> GenerationFuture<'_> {
            Box::pin(async { Err("Images must not use text-only generation".into()) })
        }
        fn generate_with_images(
            &self,
            prompt: String,
            images: Vec<crate::core::image::InputImage>,
        ) -> GenerationFuture<'_> {
            Box::pin(async move {
                assert_eq!(images.len(), 1);
                assert_eq!(images[0].data_base64.as_ref(), PNG);
                assert!(prompt.contains(&images[0].id));
                assert!(!prompt.contains(PNG));
                let nonce = prompt
                    .split("bridge_id exactly: ")
                    .nth(1)
                    .unwrap()
                    .split('.')
                    .next()
                    .unwrap();
                let (sender, receiver) = mpsc::channel(2);
                sender
                    .send(Ok(TurnEvent::Text(answer(
                        nonce,
                        json!([{"type":"text","text":"Visual input received"}]),
                    ))))
                    .await
                    .unwrap();
                sender.send(Ok(TurnEvent::Completed)).await.unwrap();
                Ok(receiver)
            })
        }
    }
    let (url, server) = start(Some(Arc::new(Vision)), Some(KEY)).await;
    let http = reqwest::Client::new();
    let mut value = base();
    value["messages"] =
        json!([{"role":"user","content":[image(),{"type":"text","text":"Describe it"}]}]);
    let response = http
        .post(format!("{url}/v1/messages"))
        .bearer_auth(KEY)
        .json(&value)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap()["content"][0]["text"],
        "Visual input received"
    );
    value["messages"][0]["content"][0]["source"] =
        json!({"type":"url","url":"http://127.0.0.1/private"});
    assert_eq!(
        http.post(format!("{url}/v1/messages"))
            .bearer_auth(KEY)
            .json(&value)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    server.abort();
}

#[test]
fn preserves_system_history_and_tool_error_without_confusing_them_with_instructions() {
    let mut value = base();
    value["system"] = json!([{"type":"text","text":"Only edit the requested file","cache_control":{"type":"ephemeral"}}]);
    value["messages"] = json!([
        {"role":"user","content":"First question"},
        {"role":"system","content":"Reminder in its original position"},
        {"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"missing.txt"}}]},
        {"role":"system","content":"Still waiting for the tool result"},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","is_error":true,"content":"File not found"},{"type":"text","text":"Try another file"}]},
        {"role":"system","content":"Trailing client reminder"}
    ]);
    let agent = request::parse(value).unwrap().agent;
    assert_eq!(agent.system[0]["text"], "Only edit the requested file");
    assert_eq!(agent.messages[0]["content"][0]["text"], "First question");
    assert_eq!(agent.messages[4]["content"][0]["is_error"], true);
    assert_eq!(agent.messages[1]["role"], "system");
    assert_eq!(
        agent.messages[5]["content"][0]["text"],
        "Trailing client reminder"
    );
    let prompt = agent.prepare().unwrap().prompt;
    assert!(prompt.contains("File not found"));
    assert!(prompt.contains("Try another file"));
}

#[test]
fn rejects_orphan_duplicate_and_unresolved_tool_results() {
    for messages in [
        json!([{"role":"user","content":[{"type":"tool_result","tool_use_id":"unknown","content":"x"}]}]),
        json!([{"role":"user","content":"x"},{"role":"assistant","content":[{"type":"tool_use","id":"id","name":"Read","input":{}}]},{"role":"user","content":"missing result"}]),
        json!([{"role":"user","content":"x"},{"role":"assistant","content":[{"type":"tool_use","id":"id","name":"Read","input":{}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"id","content":"a"},{"type":"tool_result","tool_use_id":"id","content":"b"}]}]),
    ] {
        let mut value = base();
        value["messages"] = messages;
        assert!(request::parse(value).is_err());
    }
}

#[test]
fn rejects_unsupported_capabilities_and_external_schema_fetches() {
    for (field, value) in [
        ("thinking", json!({"type":"adaptive"})),
        ("stop_sequences", json!(["stop"])),
        ("context_management", json!({"edits":[]})),
        (
            "messages",
            json!([{"role":"user","content":[{"type":"image","source":{}}]}]),
        ),
        (
            "tools",
            json!([{"name":"Read","input_schema":{"type":"object","properties":{"value":{"$ref":"https://example.com/schema"}}}}]),
        ),
        (
            "tools",
            json!([{"name":"Read","defer_loading":true,"input_schema":{"type":"object"}}]),
        ),
    ] {
        let mut request = base();
        request[field] = value;
        assert!(request::parse(request).is_err(), "{field}");
    }
}

#[test]
fn tool_batch_requires_known_names_valid_schema_and_the_per_request_nonce() {
    let prepared = prepared();
    for blocks in [
        json!([{"type":"tool_use","name":"Bash","input":{"command":"echo bad"}}]),
        json!([{"type":"tool_use","name":"Read","input":{}}]),
        json!([{"type":"tool_use","name":"Read","input":{"file_path":42}}]),
        json!([{"type":"tool_use","name":"Read","input":{"file_path":"ok"}},{"type":"tool_use","name":"Bash","input":{}}]),
    ] {
        assert!(prepared
            .parse_reply(&answer(&prepared.nonce, blocks))
            .is_err());
    }
    assert!(prepared
        .parse_reply(&answer(
            "other-request",
            json!([{"type":"text","text":"x"}])
        ))
        .is_err());
    assert!(prepared
        .parse_reply("Here is some JSON: {\"content\":[]}")
        .is_err());
    let reply = prepared
        .parse_reply(&answer(
            &prepared.nonce,
            json!([{"type":"tool_use","name":"Read","input":{"file_path":"test.txt"}}]),
        ))
        .unwrap();
    assert_eq!(reply.stop_reason, "tool_use");
    assert!(reply.content[0]["id"]
        .as_str()
        .unwrap()
        .starts_with("toolu_"));
}

#[test]
fn enforces_forced_none_and_serial_tool_choice() {
    let mut prepared = prepared();
    let call = json!({"type":"tool_use","name":"Read","input":{"file_path":"test.txt"}});
    prepared.request.choice = ToolChoice::None;
    assert!(prepared
        .parse_reply(&answer(&prepared.nonce, json!([call])))
        .is_err());
    prepared.request.choice = ToolChoice::Any;
    assert!(prepared
        .parse_reply(&answer(
            &prepared.nonce,
            json!([{"type":"text","text":"done"}])
        ))
        .is_err());
    prepared.request.parallel = false;
    assert!(prepared
        .parse_reply(&answer(&prepared.nonce, json!([call, call])))
        .is_err());
}

#[test]
fn context_limit_never_silently_drops_early_system_or_history() {
    let mut value = base();
    value["system"] = json!("x".repeat(crate::core::agent::MAX_CONTEXT_BYTES));
    assert!(request::parse(value).is_err());
}

struct ScriptedGenerator {
    prompts: Mutex<Vec<Value>>,
    cancelled: bool,
}

impl TextGenerator for ScriptedGenerator {
    fn generate(&self, prompt: String) -> GenerationFuture<'_> {
        Box::pin(async move {
            let nonce = prompt
                .split("bridge_id exactly: ")
                .nth(1)
                .unwrap()
                .split('.')
                .next()
                .unwrap();
            let context: Value =
                serde_json::from_str(prompt.split("CONTEXT_JSON\n").nth(1).unwrap()).unwrap();
            let last = context["messages"].as_array().unwrap().last().unwrap();
            let blocks = if last["content"][0]["type"] == "tool_result" {
                assert_eq!(
                    last["content"][0]["content"][0]["text"],
                    "actual-file-content"
                );
                json!([{"type":"text","text":"I read actual-file-content"}])
            } else {
                json!([{"type":"tool_use","name":"Read","input":{"file_path":"test.txt"}}])
            };
            self.prompts.lock().unwrap().push(context);
            let (sender, receiver) = mpsc::channel(4);
            sender
                .send(Ok(TurnEvent::Text(answer(nonce, blocks))))
                .await
                .unwrap();
            sender.send(Ok(TurnEvent::MessageDone)).await.unwrap();
            sender
                .send(Ok(if self.cancelled {
                    TurnEvent::Cancelled
                } else {
                    TurnEvent::Completed
                }))
                .await
                .unwrap();
            Ok(receiver)
        })
    }
}

#[tokio::test]
async fn full_tool_loop_preserves_actual_result_and_does_not_dispatch_on_cancel() {
    let generator = ScriptedGenerator {
        prompts: Default::default(),
        cancelled: false,
    };
    let first = infer(&generator, prepared()).await.unwrap();
    let mut next = base();
    next["messages"] = json!([
        {"role":"user","content":"Read my file"},
        {"role":"assistant","content":first.content},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":first.content[0]["id"],"content":"actual-file-content"}]}
    ]);
    let second = infer(
        &generator,
        request::parse(next).unwrap().agent.prepare().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(second.stop_reason, "end_turn");
    assert_eq!(second.content[0]["text"], "I read actual-file-content");
    assert_eq!(generator.prompts.lock().unwrap().len(), 2);
    let cancelled = ScriptedGenerator {
        prompts: Default::default(),
        cancelled: true,
    };
    assert!(infer(&cancelled, prepared()).await.is_err());
}

async fn start(
    generator: Option<Arc<dyn TextGenerator>>,
    key: Option<&str>,
) -> (String, tokio::task::JoinHandle<()>) {
    let app = router(AgentState {
        model: "muse",
        generator,
        api_key: key.map(Into::into),
        sessions: Default::default(),
        decision: None,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, server)
}

#[tokio::test]
async fn http_auth_count_json_sse_and_unconfigured_muse() {
    let generator = Arc::new(ScriptedGenerator {
        prompts: Default::default(),
        cancelled: false,
    });
    let (url, server) = start(Some(generator), Some(KEY)).await;
    let http = reqwest::Client::new();
    let response = http
        .post(format!("{url}/v1/messages"))
        .json(&base())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(response.json::<Value>().await.unwrap()["type"], "error");
    let response = http
        .post(format!("{url}/v1/messages?beta=true"))
        .bearer_auth(KEY)
        .json(&base())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["x-muse-usage"],
        "estimated-text-and-images"
    );
    assert_eq!(
        response.json::<Value>().await.unwrap()["stop_reason"],
        "tool_use"
    );
    let mut stream = base();
    stream["stream"] = json!(true);
    let response = http
        .post(format!("{url}/v1/messages"))
        .header("x-api-key", KEY)
        .json(&stream)
        .send()
        .await
        .unwrap();
    assert!(response
        .text()
        .await
        .unwrap()
        .contains("event: message_stop"));
    // Real HTTP admission above the former 512 KiB cap, not just a parser test.
    let mut large = base();
    large["messages"][0]["content"] = json!("x".repeat(1024 * 1024));
    let response = http
        .post(format!("{url}/v1/messages/count_tokens"))
        .bearer_auth(KEY)
        .json(&large)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response.json::<Value>().await.unwrap()["input_tokens"]
            .as_u64()
            .unwrap()
            > 256_000
    );
    let response = http
        .post(format!("{url}/v1/messages"))
        .bearer_auth(KEY)
        .json(&large)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap()["stop_reason"],
        "tool_use"
    );
    large["messages"][0]["content"] = json!("x".repeat(crate::core::agent::MAX_CONTEXT_BYTES));
    let response = http
        .post(format!("{url}/v1/messages"))
        .bearer_auth(KEY)
        .json(&large)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 413);
    assert_eq!(response.json::<Value>().await.unwrap()["type"], "error");
    let mut count = base();
    count.as_object_mut().unwrap().remove("max_tokens");
    let response = http
        .post(format!("{url}/v1/messages/count_tokens"))
        .bearer_auth(KEY)
        .json(&count)
        .send()
        .await
        .unwrap();
    assert!(
        response.json::<Value>().await.unwrap()["input_tokens"]
            .as_u64()
            .unwrap()
            > 0
    );
    let response = http
        .post(format!("{url}/v1/messages"))
        .bearer_auth(KEY)
        .header("content-type", "application/json")
        .body("{")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    server.abort();

    let (url, server) = start(None, Some(KEY)).await;
    let response = http
        .post(format!("{url}/v1/messages"))
        .bearer_auth(KEY)
        .json(&base())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    server.abort();
    let (url, server) = start(None, None).await;
    let response = http
        .post(format!("{url}/v1/messages"))
        .json(&base())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    server.abort();
}

// Runs the installed Claude Code against a deterministic model adapter, inside
// an isolated temp folder. Proves the client protocol, NOT Muse's capabilities.
#[cfg(windows)]
#[tokio::test]
#[ignore = "requires installed Claude Code; run explicitly with --ignored --nocapture"]
async fn claude_code_reads_and_writes_a_fixture_through_real_tools() {
    struct CodingGenerator(Mutex<Vec<String>>);
    struct Fixture(std::path::PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            for name in ["input.txt", "output.txt", "context-padding.txt"] {
                let _ = std::fs::remove_file(self.0.join(name));
            }
            let _ = std::fs::remove_dir(&self.0);
        }
    }
    impl TextGenerator for CodingGenerator {
        fn generate(&self, prompt: String) -> GenerationFuture<'_> {
            Box::pin(async move {
                let nonce = prompt
                    .split("bridge_id exactly: ")
                    .nth(1)
                    .unwrap()
                    .split('.')
                    .next()
                    .unwrap();
                let context: Value =
                    serde_json::from_str(prompt.split("CONTEXT_JSON\n").nth(1).unwrap()).unwrap();
                if std::env::var_os("MUSE_TEST_CONTEXT_PADDING").is_some() {
                    assert_eq!(
                        context["max_output_tokens_target"], 8192,
                        "launcher must leave sufficient input room in its context window"
                    );
                }
                let last = context["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .rev()
                    .find(|message| message["role"] != "system")
                    .unwrap();
                let results: Vec<_> = last["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|block| block["type"] == "tool_result")
                    .collect();
                let blocks = if let Some(result) = results.first() {
                    let text = result["content"].to_string();
                    self.0.lock().unwrap().push(text.clone());
                    if let Some(value) = text.split("PROBE_VALUE=").nth(1) {
                        let value: String = value
                            .chars()
                            .take_while(|c| c.is_ascii_hexdigit() || *c == '-')
                            .collect();
                        json!([{"type":"tool_use","name":"Write","input":{"file_path":std::env::temp_dir().join("unused").to_string_lossy(),"content":value}}])
                    } else {
                        json!([{"type":"text","text":"BRIDGE_SMOKE_OK"}])
                    }
                } else {
                    json!([{"type":"tool_use","name":"Read","input":{"file_path":"input.txt"}}])
                };
                // Test-only: use absolute paths from the caller's explicit task.
                let mut blocks = blocks;
                for block in blocks.as_array_mut().unwrap() {
                    if block["type"] == "tool_use" {
                        let user = context["messages"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter(|message| message["role"] == "user")
                            .flat_map(|message| message["content"].as_array().unwrap())
                            .filter_map(|block| block.get("text").and_then(Value::as_str))
                            .find(|text| text.contains("INPUT=") && text.contains("OUTPUT="))
                            .unwrap();
                        let path = if block["name"] == "Read" {
                            user.split("INPUT=").nth(1)
                        } else {
                            user.split("OUTPUT=").nth(1)
                        }
                        .unwrap()
                        .split('\n')
                        .next()
                        .unwrap();
                        block["input"]["file_path"] = json!(path);
                    }
                }
                let (sender, receiver) = mpsc::channel(2);
                sender
                    .send(Ok(TurnEvent::Text(answer(nonce, blocks))))
                    .await
                    .unwrap();
                sender.send(Ok(TurnEvent::Completed)).await.unwrap();
                Ok(receiver)
            })
        }
    }
    let root = std::env::temp_dir().join(format!(
        "muse-claude-smoke-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir(&root).unwrap();
    let _fixture = Fixture(root.clone());
    let secret = uuid::Uuid::new_v4().to_string();
    std::fs::write(root.join("input.txt"), format!("PROBE_VALUE={secret}\n")).unwrap();
    if std::env::var_os("MUSE_TEST_CONTEXT_PADDING").is_some() {
        std::fs::write(
            root.join("context-padding.txt"),
            "Context sizing fixture: this line supplies no additional instructions.\n".repeat(850),
        )
        .unwrap();
    }
    struct LiveGenerator(String);
    impl TextGenerator for LiveGenerator {
        fn generate(&self, prompt: String) -> GenerationFuture<'_> {
            Box::pin(async move {
                let response = reqwest::Client::new()
                    .post(format!("{}/v1/chat/completions",self.0.trim_end_matches('/')))
                    .timeout(std::time::Duration::from_secs(160))
                    .json(&json!({"model":"muse","stream":false,"messages":[{"role":"user","content":prompt}]}))
                    .send().await.map_err(|_| "Live Muse HTTP request failed")?;
                let value: Value = response
                    .json()
                    .await
                    .map_err(|_| "Invalid live Muse response")?;
                let text = value["choices"][0]["message"]["content"]
                    .as_str()
                    .ok_or_else(|| {
                        format!(
                            "Live Muse failed: {}",
                            value.get("error").unwrap_or(&Value::Null)
                        )
                    })?;
                let (sender, receiver) = mpsc::channel(2);
                sender
                    .send(Ok(TurnEvent::Text(text.to_owned())))
                    .await
                    .unwrap();
                sender.send(Ok(TurnEvent::Completed)).await.unwrap();
                Ok(receiver)
            })
        }
    }
    let generator = Arc::new(CodingGenerator(Default::default()));
    let live = std::env::var("MUSE_TEST_UPSTREAM_URL").ok();
    // Direct production-gateway mode covers the real Muse transport as well as
    // the Anthropic adapter. Combine with ALL_TOOLS to exceed one Noise packet.
    let gateway = std::env::var("MUSE_TEST_GATEWAY_URL").ok();
    let backend: Arc<dyn TextGenerator> = if let Some(url) = &live {
        Arc::new(LiveGenerator(url.clone()))
    } else {
        generator.clone()
    };
    let (url, server) = if let Some(url) = &gateway {
        (url.clone(), None)
    } else {
        let (url, server) = start(Some(backend), Some(KEY)).await;
        (url, Some(server))
    };
    let key = if gateway.is_some() {
        std::env::var("MUSE_PROXY_API_KEY").expect("Set MUSE_PROXY_API_KEY for the real gateway")
    } else {
        KEY.to_owned()
    };
    let run_root = root.clone();
    let output = tokio::task::spawn_blocking(move || {
        let executable = std::env::var_os("MUSE_TEST_CLAUDE_EXE").expect("Set MUSE_TEST_CLAUDE_EXE to installed claude.exe");
        let mut command = if std::env::var_os("MUSE_TEST_LAUNCHER").is_some() {
            let mut command = std::process::Command::new("powershell.exe");
            command.args(["-NoProfile","-File"]).arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("claude-muse.ps1"));
            command
        } else {
            std::process::Command::new(executable)
        };
        command.current_dir(&run_root)
            .env("MUSE_PROXY_URL",&url).env("MUSE_PROXY_API_KEY",&key)
            .env("ANTHROPIC_BASE_URL",url).env("ANTHROPIC_AUTH_TOKEN",&key)
            .env_remove("ANTHROPIC_API_KEY").env_remove("CLAUDE_CODE_OAUTH_TOKEN")
            .env("ANTHROPIC_MODEL","muse").env("ANTHROPIC_DEFAULT_SONNET_MODEL","muse")
            .env("ANTHROPIC_DEFAULT_OPUS_MODEL","muse").env("ANTHROPIC_DEFAULT_HAIKU_MODEL","muse")
            .env("CLAUDE_CODE_SUBAGENT_MODEL","muse").env("ENABLE_TOOL_SEARCH","false")
            .env("MAX_THINKING_TOKENS","0").env("CLAUDE_CODE_DISABLE_THINKING","1")
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC","1").env("DISABLE_PROMPT_CACHING","1")
            .args(["-p","--model","muse","--allowedTools","Read,Write","--max-turns","4","--setting-sources=","--no-session-persistence","--output-format","json"]);
        if std::env::var_os("MUSE_TEST_ALL_TOOLS").is_none() { command.args(["--tools","Read,Write"]); }
        if std::env::var_os("MUSE_TEST_CONTEXT_PADDING").is_some() {
            command.arg("--append-system-prompt-file").arg(run_root.join("context-padding.txt"));
        }
        let mut child = command
            .arg(format!("Read INPUT, copy only its PROBE_VALUE to OUTPUT, then say BRIDGE_SMOKE_OK.\nINPUT={}\nOUTPUT={}\n",run_root.join("input.txt").display(),run_root.join("output.txt").display()))
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
            .spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(240);
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline { let _ = child.kill(); break; }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        child.wait_with_output().unwrap()
    }).await.unwrap();
    if let Some(server) = server {
        server.abort();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("Claude Code stdout: {stdout}\nstderr: {stderr}");
    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string(root.join("output.txt")).unwrap(),
        secret
    );
    assert!(
        live.is_some()
            || gateway.is_some()
            || generator
                .0
                .lock()
                .unwrap()
                .iter()
                .any(|result| result.contains(&secret))
    );
    assert!(stdout.contains("BRIDGE_SMOKE_OK"));
}
