use super::*;
use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use std::sync::atomic::{AtomicUsize, Ordering};

fn wire(text: &str, cid: &str, complete: bool) -> String {
    let mut candidate = vec![Value::Null; 9];
    candidate[0] = json!("rc_fixture");
    candidate[1] = json!([text]);
    candidate[8] = json!([if complete { 2 } else { 1 }]);
    let payload = json!([null, [cid, "r_fixture"], null, null, [candidate]]);
    let frame = json!([[
        "wrb.fr",
        null,
        payload.to_string(),
        null,
        null,
        null,
        "generic"
    ]])
    .to_string();
    format!("{}\n{frame}\n", frame.encode_utf16().count())
}

#[test]
fn protocol_preserves_unicode_at_every_byte_boundary_and_requires_completion() {
    let bytes = format!(
        ")]}}'\n\n{}{}",
        wire("Đang viết 🙂", "c_one", false),
        wire("Đã sửa hoàn chỉnh 🙂", "c_one", true)
    )
    .into_bytes();
    for width in [1, 2, 7, 256] {
        let mut frames = protocol::Frames::default();
        let mut reply = protocol::Reply::default();
        for chunk in bytes.chunks(width) {
            for frame in frames.push(chunk).unwrap() {
                reply.accept(frame).unwrap();
            }
        }
        frames.finish().unwrap();
        let (text, metadata) = reply.finish().unwrap();
        assert_eq!(text, "Đã sửa hoàn chỉnh 🙂");
        assert_eq!(metadata[0], "c_one");
        assert_eq!(metadata[2], "rc_fixture");
    }
    let mut incomplete = protocol::Frames::default();
    incomplete.push(b")]}'\n42\n[[\"wrb.fr\",null,").unwrap();
    assert!(incomplete.finish().is_err());
    let mut frames = protocol::Frames::default();
    let mut reply = protocol::Reply::default();
    for frame in frames
        .push(wire("{\"content\":[]}", "c_one", false).as_bytes())
        .unwrap()
    {
        reply.accept(frame).unwrap();
    }
    assert!(reply.finish().is_err());
    assert!(protocol::Reply::default()
        .accept(json!([["er", null, null]]))
        .is_err());
}

#[test]
fn bootstrap_and_request_fields_do_not_invent_model_or_drop_context() {
    let html = r#"<script>window.WIZ_global_data={"SNlM0e":"escaped\"token","cfb2h":"boq_fixture","FdrFJe":"sid"};</script>"#;
    assert_eq!(
        protocol::bootstrap_field(html, "SNlM0e").unwrap(),
        "escaped\"token"
    );
    let body = protocol::generate_body(
        "hello",
        &json!(["c_one", "r_one", "rc_one"]),
        &[json!([["ref"], "image.png"])],
        "uuid",
    );
    let inner: Value = serde_json::from_str(body[1].as_str().unwrap()).unwrap();
    assert_eq!(inner[0][0], "hello");
    assert_eq!(inner[0][3][0][0][0], "ref");
    assert_eq!(inner[2][0], "c_one");
    assert_eq!(inner[59], "uuid");
    assert!(validate_cookie("hatch_sess=must-not-be-used-as-Gemini").is_err());
    assert!(validate_cookie("__Secure-1PSID=valid\r\nInjected: x").is_err());
}

#[test]
fn account_policy_selects_native_model_and_thinking_without_unlocking_carbon() {
    let catalog = models::parse(&model_payload(), 5).unwrap();
    assert_eq!(catalog.account_index, 5);
    assert_eq!(catalog.models.iter().filter(|m| m.available).count(), 4);
    assert!(catalog
        .models
        .iter()
        .any(|m| m.name.ends_with("Carbon") && !m.available));
    let options = Options {
        account_index: 5,
        model_id: "argon".into(),
        thinking_id: "4".into(),
    };
    let selection = catalog.select(&options).unwrap();
    let header: Value =
        serde_json::from_str(&selection.header("fixture-instance", "boq_fixture")).unwrap();
    assert!(header[4].is_null());
    assert_eq!(header[24], "argon");
    assert_eq!(header[14], 8);
    assert_eq!(header[15], 4);
    assert_eq!(header[16], "fixture-instance");
    let mut body = protocol::generate_body(
        "unchanged user prompt",
        &protocol::fresh_metadata(),
        &[],
        "request-uuid",
    );
    selection.apply_body(&mut body).unwrap();
    let inner: Value = serde_json::from_str(body[1].as_str().unwrap()).unwrap();
    assert_eq!(inner[0][0], "unchanged user prompt");
    assert_eq!(inner[79], 8);
    assert_eq!(inner[80], 4);
    assert!(catalog
        .select(&Options {
            model_id: "95a4c0abcba24364".into(),
            ..options.clone()
        })
        .is_err());
    assert!(catalog
        .select(&Options {
            thinking_id: "100".into(),
            ..options
        })
        .is_err());
    let mut unavailable = model_payload();
    unavailable[14] = json!(1016);
    assert!(models::parse(&unavailable, 5).is_err());
    assert!(Options {
        account_index: 100,
        ..Options::default()
    }
    .validate()
    .is_err());
    assert!(Options {
        thinking_id: "2".into(),
        ..Options::default()
    }
    .validate()
    .is_err());
}

#[test]
fn legacy_catalog_and_invalid_rpc_do_not_guess_new_model_ids_or_thinking_enums() {
    let mut payload = model_payload();
    payload[24] = Value::Null;
    let legacy = models::parse(&payload, 0).unwrap();
    assert!(legacy.models.iter().all(|m| m.available));
    assert_eq!(
        legacy.models[0]
            .levels
            .iter()
            .map(|l| l.id.as_str())
            .collect::<Vec<_>>(),
        vec!["1", "2"]
    );
    let mut changed = model_payload();
    changed[24][1][0][1][0][0] = json!(100);
    assert!(models::parse(&changed, 5).is_err());
    assert!(models::parse_rpc(b")]}'\n[[\"wrb.fr\",\"otAQ7b\",\"[\",null]]", 5).is_err());
    assert!(models::parse_rpc(b")]}'\n[[\"er\",\"otAQ7b\",null]]", 5).is_err());
}

#[tokio::test]
async fn selected_model_thinking_and_google_index_reach_wire_and_locked_model_stops_before_upload()
{
    let (mut generator, fixture, server) = setup().await;
    Arc::get_mut(&mut generator.0).unwrap().options = Options {
        account_index: 5,
        model_id: "95a4c0abcba24364".into(),
        thinking_id: "".into(),
    };
    assert!(output(&generator, "must not send", vec![])
        .await
        .unwrap_err()
        .contains("locked"));
    assert_eq!(fixture.calls.lock().await.len(), 0);
    assert_eq!(fixture.uploads.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.catalog_calls.load(Ordering::SeqCst), 1);
    Arc::get_mut(&mut generator.0).unwrap().options = Options {
        account_index: 5,
        model_id: "argon".into(),
        thinking_id: "4".into(),
    };
    fixture.mode.store(6, Ordering::SeqCst);
    assert_eq!(
        output(&generator, "selected model prompt", vec![])
            .await
            .unwrap(),
        "Hello 🙂"
    );
    assert_eq!(fixture.catalog_calls.load(Ordering::SeqCst), 1); // no catalog call for every turn
    server.abort();
}

#[derive(Default)]
struct Fixture {
    calls: Mutex<Vec<Value>>,
    uploads: AtomicUsize,
    pages: AtomicUsize,
    ids: AtomicUsize,
    mode: AtomicUsize,
    read_path: Mutex<String>,
    catalog_calls: AtomicUsize,
}
fn model_payload() -> Value {
    serde_json::from_str(include_str!("fixtures/models.json")).unwrap()
}
async fn catalog_fixture(State(f): State<Arc<Fixture>>, request: Request) -> impl IntoResponse {
    assert!(request.uri().query().unwrap().contains("authuser=5"));
    assert!(request.headers()["cookie"]
        .to_str()
        .unwrap()
        .contains("fixture-gemini"));
    let bytes = axum::body::to_bytes(request.into_body(), 8192)
        .await
        .unwrap();
    let url = Url::parse(&format!(
        "http://fixture/?{}",
        std::str::from_utf8(&bytes).unwrap()
    ))
    .unwrap();
    let form: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(
        form["f.req"],
        json!([[["otAQ7b", "[1]", null, "generic"]]]).to_string()
    );
    f.catalog_calls.fetch_add(1, Ordering::SeqCst);
    format!(
        ")]}}'\n{}\n",
        json!([["wrb.fr", "otAQ7b", model_payload().to_string()]])
    )
}
async fn page(State(f): State<Arc<Fixture>>, headers: HeaderMap) -> impl IntoResponse {
    assert!(headers
        .get("cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("__Secure-1PSID=fixture-gemini"));
    f.pages.fetch_add(1, Ordering::SeqCst);
    let html = if f.mode.load(Ordering::SeqCst) == 1 {
        "<html>Please sign in</html>"
    } else {
        r#"<script>{"SNlM0e":"fixture-at","cfb2h":"boq_fixture","FdrFJe":"fixture-sid"}</script>"#
    };
    (
        [("set-cookie", "__Secure-1PSIDTS=rotated; Secure; Path=/")],
        html,
    )
}
async fn generate(State(f): State<Arc<Fixture>>, request: Request) -> axum::response::Response {
    if f.mode.load(Ordering::SeqCst) == 6 {
        assert!(request.uri().query().unwrap().contains("authuser=5"));
        assert_eq!(
            request.headers()["referer"]
                .to_str()
                .unwrap()
                .rsplit('/')
                .take(3)
                .collect::<Vec<_>>(),
            vec!["app", "5", "u"]
        );
        let selected: Value = serde_json::from_str(
            request.headers()["x-goog-ext-525001261-jspb"]
                .to_str()
                .unwrap(),
        )
        .unwrap();
        assert!(selected[4].is_null());
        assert_eq!(selected[24], "argon");
        assert_eq!(selected[14], 8);
        assert_eq!(selected[15], 4);
    }
    assert!(request
        .headers()
        .get("cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("__Secure-1PSIDTS=rotated"));
    assert!(!request.headers().contains_key("x-api-key"));
    let bytes = axum::body::to_bytes(request.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let url = Url::parse(&format!(
        "http://fixture/?{}",
        std::str::from_utf8(&bytes).unwrap()
    ))
    .unwrap();
    let fields: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(fields["at"], "fixture-at");
    let outer: Value = serde_json::from_str(&fields["f.req"]).unwrap();
    let inner: Value = serde_json::from_str(outer[1].as_str().unwrap()).unwrap();
    if f.mode.load(Ordering::SeqCst) == 6 {
        assert_eq!(inner[79], 8);
        assert_eq!(inner[80], 4);
    }
    f.calls.lock().await.push(inner.clone());
    let mode = f.mode.load(Ordering::SeqCst);
    if mode == 2 {
        return (StatusCode::TOO_MANY_REQUESTS, "private-error-body").into_response();
    }
    if mode == 4 {
        std::future::pending::<()>().await;
    }
    let cid = inner[2][0]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("c_{}", f.ids.fetch_add(1, Ordering::SeqCst)));
    let text = if mode == 5 {
        let prompt = inner[0][0].as_str().unwrap();
        let nonce = prompt
            .split("bridge_id exactly: ")
            .nth(1)
            .or_else(|| prompt.split("bridge_id exactly ").nth(1))
            .unwrap()
            .chars()
            .take_while(|c| c.is_ascii_hexdigit())
            .collect::<String>();
        let blocks = if inner[2][0].as_str().is_some_and(|s| !s.is_empty()) {
            assert!(prompt.contains("CONTEXT_DELTA"));
            assert!(prompt.contains("FIXTURE_WEB_GEMINI_OK"));
            json!([{"type":"text","text":"FIXTURE_WEB_GEMINI_OK"}])
        } else {
            json!([{"type":"tool_use","name":"Read","input":{"file_path":f.read_path.lock().await.clone()}}])
        };
        json!({"bridge_id":nonce,"content":blocks}).to_string()
    } else {
        "Hello 🙂".into()
    };
    let response = format!(
        ")]}}'\n\n{}{}",
        wire("partial", &cid, false),
        wire(&text, &cid, mode != 3)
    );
    response.into_response()
}
async fn upload(State(f): State<Arc<Fixture>>, request: Request) -> impl IntoResponse {
    assert!(!request.headers().contains_key("cookie"));
    assert!(!request.headers().contains_key("authorization"));
    assert!(!request.headers().contains_key("x-api-key"));
    assert!(request
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("multipart/form-data; boundary="));
    let bytes = axum::body::to_bytes(request.into_body(), 3 * 1024 * 1024)
        .await
        .unwrap();
    assert!(bytes.windows(6).any(|s| s == b"image_"));
    f.uploads.fetch_add(1, Ordering::SeqCst);
    "/fixture/uploaded-reference"
}
async fn setup() -> (GeminiGenerator, Arc<Fixture>, tokio::task::JoinHandle<()>) {
    let fixture = Arc::new(Fixture::default());
    let app = Router::new()
        .route("/app", get(page))
        .route("/u/5/app", get(page))
        .route("/_/BardChatUi/data/batchexecute", post(catalog_fixture))
        .route("/u/5/_/BardChatUi/data/batchexecute", post(catalog_fixture))
        .route(
            "/u/5/_/BardChatUi/data/assistant.lamda.BardFrontendService/StreamGenerate",
            post(generate),
        )
        .route(
            "/_/BardChatUi/data/assistant.lamda.BardFrontendService/StreamGenerate",
            post(generate),
        )
        .route("/upload", post(upload))
        .with_state(fixture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut generator = GeminiGenerator::new(
        "__Secure-1PSID=fixture-gemini; __Secure-1PSIDTS=initial",
        &AccountProxy::default(),
    )
    .unwrap();
    let shared = Arc::get_mut(&mut generator.0).unwrap();
    shared.origin = Url::parse(&root).unwrap();
    shared.upload = Url::parse(&format!("{root}upload")).unwrap();
    (generator, fixture, server)
}
async fn output(
    generator: &dyn TextGenerator,
    prompt: &str,
    images: Vec<InputImage>,
) -> Result<String, String> {
    let mut receiver = generator
        .generate_with_images(prompt.into(), images)
        .await?;
    let mut text = String::new();
    let mut completed = false;
    while let Some(event) = receiver.recv().await {
        match event? {
            TurnEvent::Text(value) => text.push_str(&value),
            TurnEvent::Completed => completed = true,
            _ => {}
        }
    }
    if !completed {
        return Err("not completed".into());
    }
    Ok(text)
}

#[tokio::test]
async fn independent_sessions_reuse_metadata_images_and_rotated_cookie_without_cross_talk() {
    let (generator, fixture, server) = setup().await;
    generator.check().await.unwrap();
    let a = generator.open_session().await.unwrap().unwrap();
    let b = generator.open_session().await.unwrap().unwrap();
    let image = InputImage::from_base64("image/png", &STANDARD.encode(b"\x89PNG\r\n\x1a\nfixture"))
        .unwrap();
    assert_eq!(
        output(a.as_ref(), "first A", vec![image.clone()])
            .await
            .unwrap(),
        "Hello 🙂"
    );
    output(b.as_ref(), "first B", Vec::new()).await.unwrap();
    output(a.as_ref(), "continue A", vec![image]).await.unwrap();
    let calls = fixture.calls.lock().await;
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0][2][0], "");
    assert_eq!(calls[1][2][0], "");
    assert_eq!(calls[2][2][0], "c_0");
    assert_eq!(calls[2][2][2], "rc_fixture");
    assert_eq!(fixture.ids.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.uploads.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.pages.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn invalid_cookie_never_falls_back_to_anonymous_and_quota_errors_never_retry() {
    let (generator, fixture, server) = setup().await;
    fixture.mode.store(1, Ordering::SeqCst);
    assert!(generator
        .check()
        .await
        .unwrap_err()
        .contains("not signed in"));
    assert!(fixture.calls.lock().await.is_empty());
    fixture.mode.store(2, Ordering::SeqCst);
    generator.check().await.unwrap();
    let error = output(&generator, "quota", Vec::new()).await.unwrap_err();
    assert!(error.contains("quota"));
    assert!(!error.contains("private-error"));
    assert_eq!(fixture.calls.lock().await.len(), 1);
    server.abort();
}

#[tokio::test]
async fn incomplete_reply_and_cancelled_request_do_not_emit_completion_or_retry() {
    let (generator, fixture, server) = setup().await;
    fixture.mode.store(3, Ordering::SeqCst);
    assert!(output(&generator, "partial", Vec::new())
        .await
        .unwrap_err()
        .contains("complete reply"));
    fixture.mode.store(4, Ordering::SeqCst);
    let bound = generator.open_session().await.unwrap().unwrap();
    assert!(tokio::time::timeout(
        Duration::from_millis(100),
        output(bound.as_ref(), "cancel", Vec::new())
    )
    .await
    .is_err());
    assert_eq!(fixture.calls.lock().await.len(), 2);
    assert_eq!(generator.0.slots.available_permits(), 4);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires installed Claude Code; explicit local fixture test, no real Google account"]
async fn claude_code_reads_a_real_file_through_gemini_http_protocol_and_reuses_context() {
    let (generator, fixture, upstream) = setup().await;
    fixture.mode.store(5, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!(
        "musedesk-gemini-cli-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("probe.txt");
    std::fs::write(&file, "FIXTURE_WEB_GEMINI_OK\n").unwrap();
    *fixture.read_path.lock().await = file.to_string_lossy().into_owned();
    let app = crate::api::gemini::fixture_router(Arc::new(generator));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let executable = std::path::PathBuf::from(
        std::env::var("ProgramFiles").unwrap_or_else(|_| "C:/Program Files".into()),
    )
    .join("nodejs/node_modules/@anthropic-ai/claude-code/bin/claude.exe");
    let mut command = std::process::Command::new(executable);
    command
        .current_dir(&root)
        .args([
            "-p",
            "Read probe.txt with Read and return its exact contents.",
            "--model",
            "gemini",
            "--tools",
            "Read",
            "--allowedTools",
            "Read",
            "--no-session-persistence",
            "--no-chrome",
            "--disable-slash-commands",
            "--strict-mcp-config",
            "--mcp-config",
            "{\"mcpServers\":{}}",
            "--setting-sources",
            "",
            "--max-turns",
            "3",
            "--output-format",
            "json",
            "--system-prompt",
            "Use the Read tool to read the requested file. Then answer with its exact contents.",
        ])
        .env("ANTHROPIC_BASE_URL", &url)
        .env("ANTHROPIC_AUTH_TOKEN", "fixture-api-key")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
        .env("ANTHROPIC_MODEL", "gemini")
        .env("ANTHROPIC_DEFAULT_HAIKU_MODEL", "gemini")
        .env("ANTHROPIC_DEFAULT_SONNET_MODEL", "gemini")
        .env("ANTHROPIC_DEFAULT_OPUS_MODEL", "gemini")
        .env(
            "ANTHROPIC_CUSTOM_HEADERS",
            "x-muse-client-id: gemini-fixture-cli",
        )
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
        .env("ENABLE_TOOL_SEARCH", "false")
        .env("DISABLE_PROMPT_CACHING", "1")
        .env("MAX_THINKING_TOKENS", "0")
        .env("CLAUDE_CODE_DISABLE_THINKING", "1")
        .env("CLAUDE_CODE_MAX_OUTPUT_TOKENS", "8192")
        .env_remove("CLAUDE_CODE_USE_BEDROCK")
        .env_remove("CLAUDE_CODE_USE_VERTEX")
        .env_remove("CLAUDE_CODE_USE_FOUNDRY")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().expect("installed Claude Code");
    let started = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if started.elapsed() > Duration::from_secs(90) {
            child.kill().unwrap();
            panic!("Claude fixture timed out")
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let output = child.wait_with_output().unwrap();
    server.abort();
    upstream.abort();
    std::fs::remove_file(&file).unwrap();
    // Claude may create its own empty .claude state, so do not recursively clean.
    let _ = std::fs::remove_dir(&root);
    assert!(
        output.status.success(),
        "Claude fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["subtype"], "success");
    assert!(result["result"]
        .as_str()
        .unwrap()
        .contains("FIXTURE_WEB_GEMINI_OK"));
    let calls = fixture.calls.lock().await;
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1][2][0], "c_0");
    assert_eq!(fixture.pages.load(Ordering::SeqCst), 1);
}
