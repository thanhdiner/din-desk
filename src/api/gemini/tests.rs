use super::*;
use crate::{core::turn::TurnEvent, ports::GenerationFuture};
use tokio::sync::mpsc;

struct Fixture;
impl TextGenerator for Fixture {
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
            let content = if context["tools"].as_array().is_some_and(|a| !a.is_empty()) {
                json!([{"type":"tool_use","name":"check","input":{"path":"fixture.txt"}}])
            } else {
                json!([{"type":"text","text":"Hello Gemini"}])
            };
            let text = json!({"bridge_id":nonce,"content":content}).to_string();
            let (tx, rx) = mpsc::channel(2);
            tx.send(Ok(TurnEvent::Text(text))).await.unwrap();
            tx.send(Ok(TurnEvent::Completed)).await.unwrap();
            Ok(rx)
        })
    }
}

#[tokio::test]
async fn active_backend_rejects_wrong_model_and_serves_anthropic_json_sse_and_openai_tools() {
    let state = AgentState {
        model: "gemini",
        generator: Some(Arc::new(Fixture)),
        api_key: Some(Arc::from("fixture-api-key")),
        sessions: Default::default(),
        decision: None,
    };
    let app = configured(state, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    let base =
        json!({"model":"gemini","max_tokens":1024,"messages":[{"role":"user","content":"Hello"}]});
    assert_eq!(
        client
            .post(format!("{url}/v1/messages"))
            .json(&base)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let reply: Value = client
        .post(format!("{url}/v1/messages"))
        .bearer_auth("fixture-api-key")
        .json(&base)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reply["model"], "gemini");
    assert_eq!(reply["content"][0]["text"], "Hello Gemini");
    let mut wrong = base.clone();
    wrong["model"] = json!("muse");
    assert_eq!(
        client
            .post(format!("{url}/v1/messages"))
            .bearer_auth("fixture-api-key")
            .json(&wrong)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    assert_eq!(
        client
            .post(format!("{url}/v1/messages/count_tokens"))
            .bearer_auth("fixture-api-key")
            .json(&wrong)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let mut stream = base.clone();
    stream["stream"] = json!(true);
    let events = client
        .post(format!("{url}/v1/messages"))
        .bearer_auth("fixture-api-key")
        .json(&stream)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(events.contains("\"model\":\"gemini\""));
    assert!(events.contains("event: message_stop"));
    assert!(!events.contains("\"model\":\"muse\""));
    let mut tool = base.clone();
    tool["tools"] = json!([{"type":"function","function":{"name":"check","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}}}]);
    let reply: Value = client
        .post(format!("{url}/v1/chat/completions"))
        .bearer_auth("fixture-api-key")
        .json(&tool)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reply["model"], "gemini");
    assert_eq!(reply["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        reply["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "check"
    );
    tool["stream"] = json!(true);
    tool["stream_options"] = json!({"include_usage":true});
    let events = client
        .post(format!("{url}/v1/chat/completions"))
        .bearer_auth("fixture-api-key")
        .json(&tool)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(events.contains("tool_calls"));
    assert!(events.contains("[DONE]"));
    assert!(events.contains("prompt_tokens"));
    let list: Value = client
        .get(format!("{url}/v1/models"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["data"][0]["id"], "gemini");
    assert_eq!(
        client
            .get(format!("{url}/v1/conversations"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    server.abort();
}

#[test]
fn openai_history_images_and_tool_results_keep_exact_ids_and_order() {
    let body = json!({"model":"gemini","messages":[{"role":"system","content":"rules"},{"role":"assistant","tool_calls":[{"id":"call_a","type":"function","function":{"name":"check","arguments":"{}"}},{"id":"call_b","type":"function","function":{"name":"check","arguments":"{}"}}]},{"role":"tool","tool_call_id":"call_a","content":"A"},{"role":"tool","tool_call_id":"call_b","content":"B"},{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/Nl8AAAAASUVORK5CYII="}}]}]});
    let (request, _, _) = openai_request(body).unwrap();
    assert_eq!(
        request["messages"][1]["content"][1]["tool_use_id"],
        "call_b"
    );
    let mut request = request;
    request["messages"]
        .as_array_mut()
        .unwrap()
        .insert(0, json!({"role":"user","content":"start"}));
    let parsed = anthropic::request::parse_for_model(request, "gemini").unwrap();
    assert_eq!(parsed.agent.images.len(), 1);
    assert!(openai_request(json!({"model":"muse","messages":[]})).is_err());
    let (ordered,_,_)=openai_request(json!({"model":"gemini","messages":[{"role":"system","content":"initial"},{"role":"user","content":"first"},{"role":"developer","content":"later rule"},{"role":"user","content":"next"}]})).unwrap();
    assert_eq!(ordered["system"][0]["text"], "initial");
    assert_eq!(ordered["messages"][1]["role"], "system");
    assert_eq!(ordered["messages"][1]["content"][0]["text"], "later rule");
}
