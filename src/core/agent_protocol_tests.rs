use super::*;
use crate::{
    core::agent::{AgentRequest, Tool, ToolChoice},
    ports::GenerationFuture,
};
use serde_json::json;
use std::{collections::VecDeque, sync::Mutex};
use tokio::sync::mpsc;

enum Step {
    Complete(String),
    Cancel,
    Incomplete,
    Disconnect,
}
struct Script {
    steps: Mutex<VecDeque<Step>>,
    calls: Mutex<Vec<(String, usize)>>,
    persistent: bool,
}
impl Script {
    fn new(steps: Vec<Step>) -> Self {
        Self {
            steps: Mutex::new(steps.into()),
            calls: Mutex::new(vec![]),
            persistent: true,
        }
    }
}
impl TextGenerator for Script {
    fn supports_context_upload(&self) -> bool {
        self.persistent
    }
    fn generate(&self, prompt: String) -> GenerationFuture<'_> {
        self.generate_with_images(prompt, vec![])
    }
    fn generate_with_images(
        &self,
        prompt: String,
        images: Vec<InputImage>,
    ) -> GenerationFuture<'_> {
        Box::pin(async move {
            self.calls.lock().unwrap().push((prompt, images.len()));
            let step = self
                .steps
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected upstream retry");
            let (sender, receiver) = mpsc::channel(2);
            match step {
                Step::Complete(text) => {
                    sender.send(Ok(TurnEvent::Text(text))).await.unwrap();
                    sender.send(Ok(TurnEvent::Completed)).await.unwrap();
                }
                Step::Cancel => {
                    sender.send(Ok(TurnEvent::Cancelled)).await.unwrap();
                }
                Step::Incomplete => {
                    sender
                        .send(Ok(TurnEvent::Text("partial".into())))
                        .await
                        .unwrap();
                }
                Step::Disconnect => return Err("fixture disconnected".into()),
            }
            Ok(receiver)
        })
    }
}
fn prepared() -> PreparedRequest {
    let schema = json!({"type":"object","properties":{"file_path":{"type":"string"}},"required":["file_path"],"additionalProperties":false});
    AgentRequest {
        system:json!([]), messages:json!([{"role":"user","content":[{"type":"text","text":"PRIVATE_TRANSCRIPT_MARKER"}]}]),
        tools:vec![Tool {name:"Read".into(),definition:json!({"name":"Read","input_schema":schema}),validator:jsonschema::validator_for(&schema).unwrap()}],
        choice:ToolChoice::Auto, parallel:false, max_tokens:512, images:vec![],
    }.prepare().unwrap()
}
fn valid(nonce: &str) -> String {
    json!({"bridge_id":nonce,"content":[{"type":"tool_use","name":"Read","input":{"file_path":"fixture.txt"}}]}).to_string()
}

#[tokio::test]
async fn repairs_completed_invalid_json_once_without_replaying_context_or_images() {
    let request = prepared();
    let backend = Script::new(vec![
        Step::Complete("Here is my answer: {broken".into()),
        Step::Complete(valid(&request.nonce)),
    ]);
    let image = InputImage {
        id: "visual".into(),
        media_type: "image/png".into(),
        data_base64: std::sync::Arc::from("private-pixels"),
    };
    let reply = generate(&backend, &request, request.prompt.clone(), vec![image])
        .await
        .unwrap();
    assert_eq!(reply.stop_reason, "tool_use");
    let calls = backend.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].1, 1);
    assert_eq!(calls[1].1, 0);
    assert!(calls[1].0.starts_with("PROTOCOL REPAIR ONLY."));
    assert!(calls[1].0.contains(&request.nonce));
    assert!(!calls[1].0.contains("PRIVATE_TRANSCRIPT_MARKER"));
    assert!(!calls[1].0.contains("Here is my answer"));
    assert!(!calls[1].0.contains("private-pixels"));
}

#[tokio::test]
async fn failed_repair_never_admits_partial_or_unvalidated_tools() {
    for second in ["{still invalid".to_owned(), valid("wrong-nonce"),
        json!({"bridge_id":"REPLACE","content":[{"type":"tool_use","name":"Read","input":{}}]}).to_string(),
        json!({"bridge_id":"REPLACE","content":[{"type":"text","text":"partial success"},{"type":"tool_use","name":"Unknown","input":{}}]}).to_string()] {
        let request=prepared();
        let second=second.replace("REPLACE",&request.nonce);
        let backend=Script::new(vec![Step::Complete("{\"content\":[{\"type\":\"tool_use\"".into()),Step::Complete(second)]);
        let error=generate(&backend,&request,request.prompt.clone(),vec![]).await.unwrap_err();
        assert!(terminal_failure(&error)); assert_eq!(backend.calls.lock().unwrap().len(),2);
    }
}

#[tokio::test]
async fn completed_prose_is_literal_text_and_cannot_dispatch_embedded_tools() {
    let request = prepared();
    let text = format!(
        "I cannot perform that request. A quoted example is {}",
        valid(&request.nonce)
    );
    let backend = Script::new(vec![
        Step::Complete("plain answer".into()),
        Step::Complete(text.clone()),
    ]);
    let reply = generate(&backend, &request, request.prompt.clone(), vec![])
        .await
        .unwrap();
    assert_eq!(reply.stop_reason, "end_turn");
    assert_eq!(reply.content, vec![json!({"type":"text","text":text})]);
    assert_eq!(backend.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn required_tools_structured_candidates_and_incomplete_repairs_never_fall_back_to_text() {
    for choice in [ToolChoice::Any, ToolChoice::Named("Read".into())] {
        let mut request = prepared();
        request.request.choice = choice;
        let backend = Script::new(vec![
            Step::Complete("plain".into()),
            Step::Complete("I cannot do that".into()),
        ]);
        assert!(generate(&backend, &request, request.prompt.clone(), vec![])
            .await
            .is_err());
    }
    for text in [
        "{\"bridge_id\":",
        "[broken",
        "```json\n{broken",
        "@@MUSE_BLOB:unfinished@@",
    ] {
        let request = prepared();
        let backend = Script::new(vec![
            Step::Complete("plain".into()),
            Step::Complete(text.into()),
        ]);
        assert!(generate(&backend, &request, request.prompt.clone(), vec![])
            .await
            .is_err());
    }
    for step in [Step::Cancel, Step::Incomplete, Step::Disconnect] {
        let request = prepared();
        let backend = Script::new(vec![Step::Complete("plain".into()), step]);
        assert!(generate(&backend, &request, request.prompt.clone(), vec![])
            .await
            .is_err());
    }
}

#[tokio::test]
async fn wrong_nonce_or_schema_in_valid_json_is_rejected_without_repair() {
    for wrong_nonce in [true, false] {
        let request = prepared();
        let text = if wrong_nonce {
            valid("wrong-nonce")
        } else {
            json!({"bridge_id":request.nonce,"content":[{"type":"tool_use","name":"Read","input":{}}]}).to_string()
        };
        let backend = Script::new(vec![Step::Complete(text)]);
        assert!(terminal_failure(
            &generate(&backend, &request, request.prompt.clone(), vec![])
                .await
                .unwrap_err()
        ));
        assert_eq!(backend.calls.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn cancelled_incomplete_disconnected_and_stateless_replies_are_not_repaired() {
    for step in [Step::Cancel, Step::Incomplete, Step::Disconnect] {
        let request = prepared();
        let backend = Script::new(vec![step]);
        assert!(generate(&backend, &request, request.prompt.clone(), vec![])
            .await
            .is_err());
        assert_eq!(backend.calls.lock().unwrap().len(), 1);
    }
    let request = prepared();
    let mut backend = Script::new(vec![Step::Complete("invalid".into())]);
    backend.persistent = false;
    assert!(generate(&backend, &request, request.prompt.clone(), vec![])
        .await
        .is_err());
    assert_eq!(backend.calls.lock().unwrap().len(), 1);
}

#[test]
fn accepts_windows_fences_but_does_not_scan_prose_for_embedded_calls() {
    let request = prepared();
    let text = valid(&request.nonce);
    assert!(request
        .parse_reply(&format!("```json\r\n{text}\r\n``` "))
        .is_ok());
    assert!(request
        .parse_reply(&format!("Some prose before {text}"))
        .is_err());
}

#[test]
fn experimental_raw_text_still_requires_valid_nonce_and_entire_tool_batch() {
    let mut request = prepared();
    request.raw_blocks = true;
    let manifest =
        json!({"bridge_id":request.nonce,"content":[{"type":"text","text":{"$muse_blob":"t"}}]});
    let frame = format!(
        "\n@@MUSE_BLOB:{}:t@@\ncode with \"quotes\" 🦀\n@@MUSE_END:{}:t@@",
        request.nonce, request.nonce
    );
    let reply = request
        .parse_reply(&(manifest.to_string() + &frame))
        .unwrap();
    assert_eq!(reply.content[0]["text"], "code with \"quotes\" 🦀");
    let invalid = json!({"bridge_id":request.nonce,"content":[{"type":"text","text":{"$muse_blob":"t"}},{"type":"tool_use","name":"Read","input":{}}]});
    assert!(request
        .parse_reply(&(invalid.to_string() + &frame))
        .is_err());
    assert!(request
        .parse_reply(&(manifest.to_string().replace(&request.nonce, "foreign") + &frame))
        .is_err());
}

#[tokio::test]
#[ignore = "requires a user-provided MUSE_COOKIE and calls real Muse"]
async fn muse_live_application_greeting_and_tool_result_roundtrip() {
    use crate::adapters::muse::{agent::MuseGenerator, session::MuseSession};
    use std::sync::Arc;
    let root = MuseGenerator(Arc::new(
        MuseSession::new(
            &std::env::var("MUSE_COOKIE").unwrap(),
            std::env::var("MUSE_NODE_ID").unwrap(),
        )
        .unwrap(),
    ));
    let bound = root.open_session().await.unwrap().unwrap();
    let make = |messages, choice| {
        let schema = json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false});
        AgentRequest {
            system: json!([{"type":"text","text":"You assist users with business research in a chat application. Be concise. Use only the application tools provided for research. Never invent a search result."}]),
            messages, tools: vec![Tool { name:"web_search".into(), definition:json!({"name":"web_search","description":"Search public web sources using the application's search service.","input_schema":schema}),validator:jsonschema::validator_for(&schema).unwrap()}],
            choice, parallel:false,max_tokens:1024,images:vec![],
        }.prepare().unwrap()
    };
    let greeting = make(
        json!([{"role":"user","content":[{"type":"text","text":"Hello team. Briefly greet me. No research needed."}]}]),
        ToolChoice::None,
    );
    let reply = tokio::time::timeout(
        Duration::from_secs(150),
        generate(bound.as_ref(), &greeting, greeting.prompt.clone(), vec![]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(reply.stop_reason, "end_turn");
    assert!(reply.content.iter().all(|b| b["type"] == "text"));

    let bound = root.open_session().await.unwrap().unwrap();
    let messages = json!([{"role":"user","content":[{"type":"text","text":"Search for small English learning centers in Vietnam. Request one application web_search call; do not search using provider tools."}]}]);
    let request = make(messages.clone(), ToolChoice::Named("web_search".into()));
    let reply = tokio::time::timeout(
        Duration::from_secs(150),
        generate(bound.as_ref(), &request, request.prompt.clone(), vec![]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(reply.stop_reason, "tool_use");
    let call = reply
        .content
        .iter()
        .find(|b| b["type"] == "tool_use")
        .unwrap();
    assert_eq!(call["name"], "web_search");
    let mut history = messages.as_array().unwrap().clone();
    history.push(json!({"role":"assistant","content":reply.content}));
    history.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":call["id"],"content":[{"type":"text","text":"Diagnostic fixture only, not a real search result: SAMPLE_RESULT_731. Explain this is test data and include its marker."}]}]}));
    let next = make(json!(history), ToolChoice::None);
    let reply = tokio::time::timeout(
        Duration::from_secs(150),
        generate(bound.as_ref(), &next, next.continuation_prompt(2), vec![]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(reply.stop_reason, "end_turn");
    assert!(reply.content.iter().any(|b| b["text"]
        .as_str()
        .is_some_and(|t| t.contains("SAMPLE_RESULT_731"))));
    println!("Real Muse: application greeting, schema-validated search request, tool result continuation; no application tools executed");
}

#[tokio::test]
#[ignore = "requires a user-provided MUSE_COOKIE and calls real Muse"]
async fn muse_live_repairs_one_forced_plain_reply_in_the_same_session() {
    use crate::adapters::muse::{agent::MuseGenerator, session::MuseSession};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    struct Fault {
        bound: Arc<dyn TextGenerator>,
        calls: AtomicUsize,
    }
    impl TextGenerator for Fault {
        fn supports_context_upload(&self) -> bool {
            true
        }
        fn generate(&self, prompt: String) -> GenerationFuture<'_> {
            self.generate_with_images(prompt, vec![])
        }
        fn generate_with_images(
            &self,
            mut prompt: String,
            images: Vec<InputImage>,
        ) -> GenerationFuture<'_> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                // Simulate a turn whose output-format wrapper was lost. The
                // real Muse conversation still receives the intended text task.
                prompt = "Diagnostic only. No tools. This is a plain-text exchange. Reply with exactly FORMAT_REPAIR_OK as plain text; do not use JSON or markdown.".into();
            }
            self.bound.generate_with_images(prompt, images)
        }
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter("muse_api=info")
        .try_init();
    let root = MuseGenerator(Arc::new(
        MuseSession::new(
            &std::env::var("MUSE_COOKIE").unwrap(),
            std::env::var("MUSE_NODE_ID").unwrap(),
        )
        .unwrap(),
    ));
    let backend = Fault {
        bound: root.open_session().await.unwrap().unwrap(),
        calls: AtomicUsize::new(0),
    };
    let mut request = prepared().request;
    request.choice = ToolChoice::None;
    request.messages = json!([{"role":"user","content":[{"type":"text","text":"Diagnostic only. Return the literal text FORMAT_REPAIR_OK. Never use tools."}]}]);
    let request = request.prepare().unwrap();
    let reply = tokio::time::timeout(
        Duration::from_secs(120),
        generate(&backend, &request, request.prompt.clone(), vec![]),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        backend.calls.load(Ordering::SeqCst),
        2,
        "Fault injection did not produce an invalid first reply"
    );
    assert_eq!(reply.stop_reason, "end_turn");
    assert!(reply.content.iter().any(|block| block["text"]
        .as_str()
        .is_some_and(|text| text.contains("FORMAT_REPAIR_OK"))));
    println!("Real Muse: forced invalid first reply, one same-session repair, validated text reply, no client tools");
}
