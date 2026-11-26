use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::*;
use crate::{
    core::turn::TurnEvent,
    ports::{GenerationFuture, TextGenerator},
};

const KEY: &str = "responses-fixture-key";
const PATCH: &str = "*** Begin Patch\n*** Add File: fixture-result.txt\n+patched\n*** End Patch";

struct Fixture;

impl TextGenerator for Fixture {
    fn generate(&self, prompt: String) -> GenerationFuture<'_> {
        Box::pin(async move {
            let (_, after) = prompt.split_once("Copy bridge_id exactly: ").unwrap();
            let nonce = after.split_once('.').unwrap().0;
            let context: Value =
                serde_json::from_str(prompt.rsplit_once("\nCONTEXT_JSON\n").unwrap().1).unwrap();
            let contains_result = context["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| {
                    message["content"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|block| block["type"] == "tool_result")
                });
            let tool = context["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == "apply_patch");
            let block = if tool.is_some() && !contains_result {
                json!({"type":"tool_use","name":"apply_patch","input":{"input":PATCH}})
            } else {
                json!({"type":"text","text":"Fixture finished ✓"})
            };
            let (sender, receiver) = mpsc::channel(2);
            sender
                .send(Ok(TurnEvent::Text(
                    json!({"bridge_id":nonce,"content":[block]}).to_string(),
                )))
                .await
                .unwrap();
            sender.send(Ok(TurnEvent::Completed)).await.unwrap();
            Ok(receiver)
        })
    }
}

async fn start() -> (String, tokio::task::JoinHandle<()>) {
    let app = anthropic::router(AgentState {
        model: "muse",
        generator: Some(Arc::new(Fixture)),
        api_key: Some(KEY.into()),
        sessions: Default::default(),
        decision: None,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, server)
}

#[test]
fn normalizes_role_image_and_parallel_tool_history() {
    let body = json!({"model":"muse","input":[
        {"type":"message","role":"developer","content":[{"type":"input_text","text":"Developer reminder"}]},
        {"type":"message","role":"user","content":[{"type":"input_image","image_url":"data:image/png;base64,iVBORw0KGgpm"}]},
        {"type":"function_call","namespace":"mcp_tools","name":"read","call_id":"one","arguments":"{}"},
        {"type":"custom_tool_call","name":"apply_patch","call_id":"two","input":PATCH},
        {"type":"function_call_output","call_id":"one","output":[{"type":"input_text","text":"done"}]},
        {"type":"custom_tool_call_output","call_id":"two","output":"updated"}
    ],"tools":[{"type":"namespace","name":"mcp_tools","tools":[{"type":"function","name":"read"}]}, {"type":"custom","name":"apply_patch"}]});
    let (request, tools) = request::parse(body.clone(), "muse").unwrap();
    let messages = request.agent.messages;
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(request.agent.images.len(), 1);
    assert_eq!(messages[2]["content"][1]["input"]["input"], PATCH);
    assert_eq!(messages[3]["content"].as_array().unwrap().len(), 2);
    let name = messages[2]["content"][0]["name"].as_str().unwrap();
    assert_eq!(tools[name].namespace.as_deref(), Some("mcp_tools"));
    let mut previous = body;
    previous["previous_response_id"] = json!("resp_previous");
    assert!(request::parse(previous, "muse").is_err());
}

#[tokio::test]
async fn emits_sse_custom_tool_and_accepts_its_result_in_a_new_request() {
    let (url, server) = start().await;
    let endpoint = format!("{url}/responses");
    let client = reqwest::Client::new();
    let tools = json!([{"type":"custom","name":"apply_patch"}]);
    let mut body = json!({"model":"muse","input":[{"role":"user","content":"Edit"}],"stream":true,"tools":tools});
    let denied = client.post(&endpoint).json(&body).send().await.unwrap();
    assert_eq!(denied.status(), 401);
    let response = client
        .post(&endpoint)
        .bearer_auth(KEY)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let text = response.text().await.unwrap();
    let events: Vec<Value> = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|json| serde_json::from_str(json).unwrap())
        .collect();
    assert_eq!(events[0]["type"], "response.created");
    assert!(events
        .iter()
        .enumerate()
        .all(|(index, event)| event["sequence_number"] == index));
    let completed = events.last().unwrap();
    assert_eq!(completed["type"], "response.completed");
    let call = &completed["response"]["output"][0];
    assert_eq!(call["type"], "custom_tool_call");
    assert_eq!(call["input"], PATCH);
    body["input"].as_array_mut().unwrap().extend([
        call.clone(),
        json!({"type":"custom_tool_call_output","call_id":call["call_id"],"output":"patched"}),
    ]);
    body["stream"] = json!(false);
    let response = client
        .post(&endpoint)
        .bearer_auth(KEY)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let reply = response.json::<Value>().await.unwrap();
    assert_eq!(
        reply["output"][0]["content"][0]["text"],
        "Fixture finished ✓"
    );
    server.abort();
}

#[tokio::test]
#[ignore = "requires an installed Codex executable; set DINDESK_TEST_CODEX_EXE"]
async fn installed_codex_streams_and_applies_a_patch_through_the_adapter() {
    let executable =
        std::env::var_os("DINDESK_TEST_CODEX_EXE").expect("Set DINDESK_TEST_CODEX_EXE");
    let (url, server) = start().await;
    let root = std::env::temp_dir().join(id("DinDesk-codex-fixture"));
    std::fs::create_dir(&root).unwrap();
    struct FixtureDirectory(std::path::PathBuf);
    impl Drop for FixtureDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _fixture = FixtureDirectory(root.clone());
    std::fs::create_dir(root.join("codex-home")).unwrap();
    let catalog_path = root.join("models.json");
    std::fs::write(
        &catalog_path,
        include_str!("../../../codex-model-catalog.json"),
    )
    .unwrap();
    let output = tokio::task::spawn_blocking({
        let root = root.clone();
        move || {
            std::process::Command::new(executable)
                .current_dir(&root)
                .env("CODEX_HOME", root.join("codex-home"))
                .env("DINDESK_PROXY_API_KEY", KEY)
                .args([
                    "-c",
                    "model_provider=dindesk",
                    "-c",
                    "model=muse",
                    "-c",
                    "model_providers.dindesk.name=DinDesk",
                    "-c",
                    "model_providers.dindesk.env_key=DINDESK_PROXY_API_KEY",
                    "-c",
                    "web_search=disabled",
                    "-c",
                    "analytics.enabled=false",
                    "-c",
                    "model_providers.dindesk.capabilities.remote_compaction=unsupported",
                ])
                .arg("-c")
                .arg(format!("model_catalog_json={}", catalog_path.display()))
                .arg("-c")
                .arg(format!("model_providers.dindesk.base_url={url}"))
                .args([
                    "--dangerously-bypass-approvals-and-sandbox",
                    "exec",
                    "--skip-git-repo-check",
                    "--ephemeral",
                    "--json",
                    "Add the fixture-result.txt file with apply_patch.",
                ])
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap()
        }
    })
    .await
    .unwrap();
    server.abort();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Fixture finished"));
    assert!(std::fs::read_to_string(root.join("fixture-result.txt"))
        .unwrap()
        .contains("patched"));
}
