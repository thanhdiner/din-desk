use super::*;
use crate::core::agent::{AgentRequest, Tool};
use axum::{
    extract::State,
    http::{StatusCode, Uri},
    routing::post,
    Json, Router,
};
use std::{collections::VecDeque, sync::Mutex};

struct Fixture {
    replies: Mutex<VecDeque<(StatusCode, Value)>>,
    calls: Mutex<Vec<(String, Value)>>,
}
async fn serve(
    State(f): State<Arc<Fixture>>,
    uri: Uri,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    f.calls.lock().unwrap().push((uri.path().into(), body));
    let (status, body) = f
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .expect("unexpected retry");
    (status, Json(body))
}
async fn backend(
    replies: Vec<(StatusCode, Value)>,
) -> (DecisionClient, Arc<Fixture>, tokio::task::JoinHandle<()>) {
    let f = Arc::new(Fixture {
        replies: Mutex::new(replies.into()),
        calls: Mutex::new(vec![]),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/{*path}", post(serve))
        .with_state(f.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        DecisionClient::new("fixture-key", &base).unwrap(),
        f,
        server,
    )
}
fn prepared(choice: ToolChoice) -> PreparedRequest {
    let schema = json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false});
    AgentRequest {system:json!([]),messages:json!([{"role":"user","content":[{"type":"text","text":"Research this niche"}]}]),tools:vec![Tool{name:"web_search".into(),definition:json!({"name":"web_search","description":"Search public sources","input_schema":schema}),validator:jsonschema::validator_for(&schema).unwrap()}],choice,parallel:false,max_tokens:1024,images:vec![]}.prepare().unwrap()
}
fn choice(action: &str, confidence: f64) -> Value {
    json!({"answers":{"action":{"choice":action,"confidence":confidence}}})
}
fn written(value: Value, finish: &str) -> Value {
    json!({"candidates":[{"finishReason":finish,"content":{"parts":[{"text":value.to_string()}]}}],"usageMetadata":{"promptTokenCount":23,"candidatesTokenCount":7}})
}

#[tokio::test]
async fn selected_tool_arguments_are_validated_and_results_can_finish_without_a_tool() {
    let (backend, f, server) = backend(vec![
        (StatusCode::OK, choice("tool_0", 0.95)),
        (
            StatusCode::OK,
            written(json!({"query":"small English centers"}), "STOP"),
        ),
        (
            StatusCode::OK,
            written(
                json!({"text":"Based on SAMPLE_RESULT, here is the report."}),
                "STOP",
            ),
        ),
    ])
    .await;
    let request = prepared(ToolChoice::Auto);
    let reply = backend.infer(&request).await.unwrap();
    assert_eq!(reply.stop_reason, "tool_use");
    assert_eq!(reply.content[0]["name"], "web_search");
    assert_eq!(reply.content[0]["input"]["query"], "small English centers");
    let mut next = prepared(ToolChoice::None).request;
    let mut history = request.request.messages.as_array().unwrap().clone();
    history.push(json!({"role":"assistant","content":reply.content}));
    history.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":reply.content[0]["id"],"content":[{"type":"text","text":"SAMPLE_RESULT"}]}]}));
    next.messages = json!(history);
    let reply = backend.infer(&next.prepare().unwrap()).await.unwrap();
    assert_eq!(reply.stop_reason, "end_turn");
    assert!(reply.content[0]["text"]
        .as_str()
        .unwrap()
        .contains("SAMPLE_RESULT"));
    let calls = f.calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].0, "/v1beta/decisions");
    assert!(calls[1].0.contains(WRITER));
    assert!(calls[2].1.to_string().contains("SAMPLE_RESULT"));
    assert!(calls.iter().all(|(_, b)| b.get("tools").is_none()));
    server.abort();
}

#[tokio::test]
async fn invalid_decisions_do_not_reach_the_writer() {
    for selected in [
        choice("unknown", 0.99),
        json!({"answers":{"action":{"choice":"tool_0"}}}),
    ] {
        let (backend, f, server) = backend(vec![(StatusCode::OK, selected)]).await;
        assert!(backend.infer(&prepared(ToolChoice::Auto)).await.is_err());
        assert_eq!(f.calls.lock().unwrap().len(), 1);
        server.abort();
    }
}

#[tokio::test]
async fn uncertainty_defers_to_planning_but_never_bypasses_schema_or_tool_limits() {
    for (blocks, valid) in [
        (
            json!([{"type":"tool_use","name":"web_search","input":{"query":"fixture"}}]),
            true,
        ),
        (
            json!([{"type":"text","text":"Please clarify the task"}]),
            true,
        ),
        (
            json!([{"type":"tool_use","name":"Unknown","input":{}}]),
            false,
        ),
        (
            json!([{"type":"tool_use","name":"web_search","input":{"query":42}}]),
            false,
        ),
        (
            json!([{"type":"tool_use","name":"web_search","input":{"query":"a"}},{"type":"tool_use","name":"web_search","input":{"query":"b"}}]),
            false,
        ),
    ] {
        let (backend, f, server) = backend(vec![
            (StatusCode::OK, choice("tool_0", 0.59)),
            (StatusCode::OK, written(json!({"content":blocks}), "STOP")),
        ])
        .await;
        assert_eq!(
            backend.infer(&prepared(ToolChoice::Auto)).await.is_ok(),
            valid
        );
        assert_eq!(f.calls.lock().unwrap().len(), 2);
        server.abort();
    }
}

#[tokio::test]
async fn malformed_arguments_truncation_and_rate_limits_fail_without_retry_or_dispatch() {
    for response in [
        written(json!({"query":42}), "STOP"),
        written(json!({"query":"ok","extra":true}), "STOP"),
        written(json!({"query":"ok"}), "MAX_TOKENS"),
        json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"functionCall":{"name":"web_search","args":{"query":"bad"}}}]}}]}),
    ] {
        let (backend, f, server) = backend(vec![(StatusCode::OK, response)]).await;
        assert!(backend
            .infer(&prepared(ToolChoice::Named("web_search".into())))
            .await
            .is_err());
        assert_eq!(f.calls.lock().unwrap().len(), 1);
        server.abort();
    }
    let (backend, f, server) = backend(vec![(
        StatusCode::TOO_MANY_REQUESTS,
        json!({"error":{"message":"fixture-secret-do-not-echo"}}),
    )])
    .await;
    let error = backend
        .infer(&prepared(ToolChoice::Auto))
        .await
        .unwrap_err();
    assert!(error.contains("429"));
    assert!(!error.contains("fixture-secret"));
    assert_eq!(f.calls.lock().unwrap().len(), 1);
    server.abort();
}

#[tokio::test]
async fn explicit_none_cannot_select_tools_and_oversized_or_visual_input_is_not_truncated() {
    let (backend, f, server) = backend(vec![(
        StatusCode::OK,
        written(json!({"text":"hello"}), "STOP"),
    )])
    .await;
    let reply = backend.infer(&prepared(ToolChoice::None)).await.unwrap();
    assert_eq!(reply.stop_reason, "end_turn");
    assert_eq!(f.calls.lock().unwrap().len(), 1);
    let mut request = prepared(ToolChoice::Auto);
    request.request.messages =
        json!([{"role":"user","content":[{"type":"text","text":"x".repeat(MAX_CONTEXT)}]}]);
    assert!(backend
        .infer(&request)
        .await
        .unwrap_err()
        .contains("512 KiB"));
    request = prepared(ToolChoice::Auto);
    request.request.images.push(crate::core::image::InputImage {
        id: "fixture".into(),
        media_type: "image/png".into(),
        data_base64: Arc::from("AA=="),
    });
    assert!(backend
        .infer(&request)
        .await
        .unwrap_err()
        .contains("text-only"));
    assert_eq!(f.calls.lock().unwrap().len(), 1);
    server.abort();
}
