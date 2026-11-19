//! A serialized Muse session, with bounded SSE output backpressure.
use std::{
    error::Error,
    future::Future,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex, Semaphore};

use super::{
    auth::{MuseAuth, MuseCredentials},
    client::MuseClient,
    events::EventDecoder,
    protocol::{ApplicationRequest, Header},
    streams::{StreamEvent, Streams},
    transport::NoiseSocket,
};
use crate::core::image::InputImage;
use crate::core::lifecycle::{Lifecycle, Outcome};
use crate::core::turn::{Turn, TurnEvent};
use crate::{
    core::conversation::Conversation, infrastructure::conversations::FileConversationStore,
    ports::ConversationStore,
};
use std::time::Instant;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
const MAX_ACK: usize = 256 * 1024;
const TURN_TIMEOUT: Duration = Duration::from_secs(120);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

async fn prepare_client<F, Fut>(
    cached: &mut Option<MuseClient>,
    probe: bool,
    connect: F,
) -> Result<()>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<MuseClient>>,
{
    if let Some(client) = cached.as_mut().filter(|_| probe) {
        if !matches!(
            tokio::time::timeout(PROBE_TIMEOUT, client.health_check()).await,
            Ok(Ok(()))
        ) {
            tracing::info!("Muse connection is stale; reconnecting before chat submission");
            *cached = None;
        }
    }
    if cached.is_none() {
        *cached = Some(connect().await?);
    }
    Ok(())
}

/// Stable, non-secret error text for the HTTP boundary.
pub fn client_error(error: &(dyn Error + Send + Sync + 'static)) -> String {
    if let Some(gateway) = error.downcast_ref::<super::transport::GatewayError>() {
        return match gateway.0 {
            502..=504 => format!("Gateway Muse đang không khả dụng (HTTP {}); chưa gửi nội dung chat. Thử lại sau hoặc kiểm tra Muse Web.", gateway.0),
            401 | 403 => format!("Gateway Muse từ chối phiên đăng nhập (HTTP {}); chưa gửi nội dung chat. Kiểm tra tài khoản trên Muse Web và cập nhật cookie nếu cần.", gateway.0),
            429 => "Gateway Muse giới hạn kết nối (HTTP 429); chưa gửi nội dung chat. Chờ trước khi thử lại.".into(),
            status => format!("Gateway Muse từ chối mở WebSocket (HTTP {status}); chưa gửi nội dung chat."),
        };
    }
    if error.is::<tokio_tungstenite::tungstenite::Error>() {
        return "Muse da dong ket noi trong luc xu ly. Tin nhan co the da duoc gui; proxy khong tu gui lai. Luot tiep theo se ket noi lai.".into();
    }
    if let Some(auth) = error.downcast_ref::<super::auth::AuthError>() {
        return auth.to_string();
    }
    if error
        .downcast_ref::<super::client::ServiceError>()
        .is_some_and(|error| error.0 == 413)
    {
        return crate::ports::UPSTREAM_INPUT_TOO_LARGE.into();
    }
    match error.to_string().as_str() {
        "Muse queue is full" | "Muse connection pool is busy" => "Proxy Muse đang quá tải; tin nhắn chưa được gửi. Thử lại sau.".into(),
        "Muse queue wait exceeded 15 seconds" => "Đã chờ quá 15 giây để vào lượt chat; tin nhắn chưa được gửi.".into(),
        "Muse task failed" => "Muse bao task that bai. Phan noi dung da nhan co the chua day du; proxy khong tu gui lai tin nhan.".into(),
        "Muse chat timed out" => "Muse khong hoan tat phan hoi trong 120 giay. Tin nhan khong duoc tu gui lai.".into(),
        "another user message arrived during this chat turn" => "Phat hien tin nhan khac trong cung chat Muse; luot hien tai da dung de tranh nham phan hoi.".into(),
        _ => "Muse chat gap loi giao thuc. Xem log server de biet chi tiet; luot tiep theo se ket noi lai.".into(),
    }
}

pub struct MuseSession {
    credentials: MuseCredentials,
    node_id: String,
    connection: Mutex<AccountConnection>,
    conversations: Arc<dyn ConversationStore>,
    chats: super::connections::ConversationConnections,
    bootstrap_lock: Mutex<()>,
    capacity: Semaphore,
    pending: Semaphore,
    auth: MuseAuth,
    proxy: super::proxy::AccountProxy,
    pub(super) performance: super::performance::Performance,
    pub(super) warm: super::warm::WarmConnections,
}

#[derive(Default)]
pub(super) struct AccountConnection {
    client: Option<MuseClient>,
    pub(super) vm_id: Option<String>,
    healthy_at: Option<Instant>,
}

#[derive(Debug)]
pub struct ConversationNotFound;
impl std::fmt::Display for ConversationNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Conversation not found for this Muse account")
    }
}
impl Error for ConversationNotFound {}

impl MuseSession {
    pub fn new(cookie: &str, node_id: String) -> Result<Self> {
        if cookie.trim().is_empty() || node_id.trim().is_empty() {
            return Err("Muse cookie and node ID are required".into());
        }
        let proxy = super::proxy::AccountProxy::from_env()?;
        Ok(Self {
            credentials: MuseCredentials::from_cookie_header(cookie)?,
            node_id,
            connection: Mutex::new(AccountConnection::default()),
            chats: Default::default(),
            bootstrap_lock: Mutex::new(()),
            capacity: Semaphore::new(4),
            pending: Semaphore::new(64),
            auth: MuseAuth::with_proxy(&proxy)?,
            proxy,
            performance: super::performance::Performance::from_env(),
            warm: Default::default(),
            conversations: Arc::new(FileConversationStore::new(
                std::env::var_os("MUSE_CONVERSATIONS_PATH")
                    .map(Into::into)
                    .unwrap_or_else(|| std::path::PathBuf::from(".local/conversations.json")),
            )),
        })
    }

    async fn prepare(&self, connection: &mut AccountConnection) -> Result<()> {
        let probe = connection
            .healthy_at
            .is_none_or(|at| at.elapsed() >= self.performance.probe_after);
        let mut fresh = None;
        prepare_client(&mut connection.client, probe, || async {
            let mut connected = self.connect_account().await?;
            let client = connected.client.take().expect("connected");
            fresh = Some(connected);
            Ok(client)
        })
        .await?;
        if let Some(fresh) = fresh {
            connection.vm_id = fresh.vm_id;
        }
        connection.healthy_at = Some(Instant::now());
        Ok(())
    }

    pub(super) async fn connect_account(&self) -> Result<AccountConnection> {
        let started = Instant::now();
        let _bootstrap = self.bootstrap_lock.lock().await;
        let session = if crate::config::performance_baseline() {
            MuseAuth::with_proxy(&self.proxy)?
                .bootstrap(&self.credentials)
                .await?
        } else {
            self.auth.bootstrap(&self.credentials).await?
        };
        let socket = NoiseSocket::connect_via(session.gateway_url().as_str(), &self.proxy).await?;
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis(),
            "Authenticated Muse socket connected"
        );
        Ok(AccountConnection {
            client: Some(MuseClient::new(socket)),
            vm_id: Some(session.vm_id),
            healthy_at: Some(Instant::now()),
        })
    }

    pub async fn create_conversation(&self) -> Result<Conversation> {
        self.start_conversation(true).await
    }

    /// Stateless agent requests carry their full transcript. Do not add these
    /// sessions to the user's resumable conversation list.
    pub async fn create_inference_conversation(&self) -> Result<Conversation> {
        self.start_conversation(false).await
    }

    async fn start_conversation(&self, remember: bool) -> Result<Conversation> {
        let mut connection = self.connection.lock().await;
        self.prepare(&mut connection).await?;
        let result = connection.client.as_mut().expect("connected").request_json(post("/session/start", json!({"method":"/api/session/start","params":{"origin":"fresh","lifecycle":"persistent"}}))?).await;
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                connection.client = None;
                return Err(error);
            }
        };
        let session_id = value
            .get("session_id")
            .or_else(|| value.get("id"))
            .and_then(Value::as_str)
            .filter(|id| valid_session_id(id))
            .ok_or("Muse returned an invalid session ID")?
            .to_owned();
        let conversation = Conversation {
            id: format!("conv_{}", uuid::Uuid::new_v4().simple()),
            created: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
            session_id,
            vm_id: connection.vm_id.clone().expect("VM is known"),
        };
        if remember {
            self.conversations.insert(conversation.clone())?;
        }
        Ok(conversation)
    }

    pub async fn conversation(&self, id: &str) -> Result<Conversation> {
        let row = self.conversations.get(id)?.ok_or(ConversationNotFound)?;
        let mut connection = self.connection.lock().await;
        self.prepare(&mut connection).await?;
        if connection.vm_id.as_deref() != Some(row.vm_id.as_str()) {
            return Err(Box::new(ConversationNotFound));
        }
        let result = connection
            .client
            .as_mut()
            .expect("connected")
            .request_json(get(&format!("/api/session/list/{}", row.session_id)))
            .await;
        if let Err(error) = result {
            connection.client = None;
            return Err(error);
        }
        Ok(row)
    }

    pub fn lookup_conversation(&self, id: &str) -> Result<Conversation> {
        self.conversations
            .get(id)?
            .ok_or_else(|| Box::new(ConversationNotFound) as Box<dyn Error + Send + Sync>)
    }

    pub async fn conversations(self: &Arc<Self>) -> Result<Vec<Conversation>> {
        let mut connection = self.connection.lock().await;
        self.prepare(&mut connection).await?;
        self.schedule_warm();
        Ok(self
            .conversations
            .list()?
            .into_iter()
            .filter(|row| Some(row.vm_id.as_str()) == connection.vm_id.as_deref())
            .collect())
    }

    /// Serialize turns per conversation, with at most four active conversations.
    pub async fn run(
        self: Arc<Self>,
        text: String,
        conversation: Conversation,
        output: mpsc::Sender<std::result::Result<TurnEvent, String>>,
    ) -> Result<()> {
        self.run_with_images(text, Vec::new(), conversation, output)
            .await
    }

    pub async fn run_with_images(
        self: Arc<Self>,
        text: String,
        images: Vec<InputImage>,
        conversation: Conversation,
        output: mpsc::Sender<std::result::Result<TurnEvent, String>>,
    ) -> Result<()> {
        let _admission = self
            .pending
            .try_acquire()
            .map_err(|_| "Muse queue is full")?;
        let admitted = Instant::now();
        let deadline = tokio::time::Instant::now() + TURN_TIMEOUT;
        let slot = self.chats.get(&conversation.id).await?;
        // A busy conversation must not reserve capacity needed by another one.
        let queue_deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let mut guard = tokio::select! {
            _ = output.closed() => return Err("downstream closed".into()),
            guard = tokio::time::timeout_at(queue_deadline,slot.lock()) => guard.map_err(|_| "Muse queue wait exceeded 15 seconds")?,
        };
        let _capacity = tokio::select! {
            _ = output.closed() => return Err("downstream closed".into()),
            permit = tokio::time::timeout_at(queue_deadline,self.capacity.acquire()) => permit.map_err(|_| "Muse queue wait exceeded 15 seconds")?.map_err(|_|"Muse session is shutting down")?,
        };
        let queue_ms = admitted.elapsed().as_millis();
        let preparing = Instant::now();
        let mut warm_used = false;
        if guard.client.is_none() {
            if let Some(ready) = self.warm.ready(&conversation.vm_id).await {
                *guard = ready;
                warm_used = true;
                tracing::info!("Assigned unused warm socket to isolated Muse conversation");
            }
        }
        tokio::select! {
            _ = output.closed() => { guard.client = None; return Err("downstream closed".into()); }
            result = tokio::time::timeout_at(deadline,self.prepare(&mut guard)) => {
                match result { Ok(result) => result?, Err(_) => { guard.client = None; return Err("Muse chat timed out".into()); } }
            },
        }
        if guard.vm_id.as_deref() != Some(conversation.vm_id.as_str()) {
            return Err(Box::new(ConversationNotFound));
        }
        self.schedule_warm();
        tracing::info!(
            queue_ms,
            prepare_ms = preparing.elapsed().as_millis(),
            warm_used,
            "Muse turn preparation finished"
        );
        let result = tokio::select! {
            _ = output.closed() => { guard.client = None; return Err("downstream closed".into()); }
            result = tokio::time::timeout_at(deadline, run_turn(guard.client.as_mut().expect("connected"), &self.node_id, &text, &images, Some(&conversation.session_id), &output)) => result,
        };
        match result {
            Ok(Ok(())) => {
                guard.healthy_at = Some(Instant::now());
                Ok(())
            }
            Ok(Err(error)) => {
                guard.client = None;
                Err(error)
            }
            Err(_) => {
                guard.client = None;
                Err("Muse chat timed out".into())
            }
        }
    }
}

fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
fn get(path: &str) -> ApplicationRequest {
    ApplicationRequest {
        verb: "GET".into(),
        path: path.into(),
        headers: vec![],
        body: vec![],
        end_body: true,
    }
}
fn history_path(session_id: &str, limit: usize) -> String {
    format!("/chat/history?limit={limit}&session_id={session_id}")
}

fn post(path: &str, body: Value) -> Result<ApplicationRequest> {
    Ok(ApplicationRequest {
        verb: "POST".into(),
        path: path.into(),
        headers: vec![Header {
            key: "content-type".into(),
            value: "application/json".into(),
        }],
        body: serde_json::to_vec(&body)?,
        end_body: true,
    })
}

fn chat_body(node_id: &str, text: &str, images: &[InputImage], session_id: Option<&str>) -> Value {
    let mut items = vec![json!({"type":"text","text":text})];
    for image in images {
        items.push(json!({"type":"text","text":format!("Attached visual image_id={} ({}) referenced in the external client transcript:",image.id,image.media_type)}));
        items.push(json!({"type":"image","mime_type":image.media_type,"data_base64":image.data_base64.as_ref()}));
    }
    let mut body = json!({"items":items,"node_id":node_id,"capabilities":{}});
    if let Some(scope) = session_id {
        body["session_id"] = json!(scope);
    }
    body
}

async fn run_turn(
    client: &mut MuseClient,
    node_id: &str,
    text: &str,
    images: &[InputImage],
    session_id: Option<&str>,
    output: &mpsc::Sender<std::result::Result<TurnEvent, String>>,
) -> Result<()> {
    let baseline = if let Some(scope) = session_id {
        let history = client.request_json(get(&history_path(scope, 1))).await?;
        history
            .get("chat_events")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|row| row.get("seq").and_then(Value::as_u64))
            .max()
            .unwrap_or(0)
    } else {
        0
    };
    let mut streams = Streams::default();
    let subscription = client
        .start_request(post("/chat/subscribe", {
            let mut body = json!({"capabilities": ["chat_cancel", "delta_stream"]});
            if let Some(scope) = session_id {
                body["session_id"] = json!(scope);
            }
            body
        })?)
        .await?;
    streams.register(subscription)?;
    // Start both streams in send order: some gateways defer subscription
    // headers until the first event, so waiting for them can deadlock a turn.
    let chat_id = client
        .start_request(post(
            "/chat/stream",
            chat_body(node_id, text, images, session_id),
        )?)
        .await?;
    streams.register(chat_id)?;
    let chat = Some(chat_id);
    let mut ack = Vec::new();
    let mut decoder = EventDecoder::default();
    let mut turn = Turn::default();
    if let Some(scope) = session_id {
        turn.scope(scope);
    }
    let mut life = Lifecycle::new(session_id, Instant::now());
    let mut early_events = Vec::new();
    let mut early_bytes = 0usize;
    let mut acknowledged = false;
    let mut history_stream = None;
    let mut history_body = Vec::new();
    let mut history_busy = false;
    let mut last_realtime = Instant::now();
    let mut poll = tokio::time::interval(Duration::from_secs(1));
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let outcome = loop {
        let frame = tokio::select! {
            _ = tick.tick() => {
                if acknowledged {
                    if let Some(outcome) = life.outcome(Instant::now(), turn.has_open_messages() || history_busy, turn.completed_messages()) {
                        if outcome == Outcome::Completed && history_stream.is_some() { continue; }
                        break outcome;
                    }
                }
                continue;
            }
            frame = client.receive() => frame?,
            _ = poll.tick(), if session_id.is_some() && acknowledged && history_stream.is_none() => {
                if last_realtime.elapsed() < Duration::from_secs(2) { continue; }
                let id = client.start_request(get(&history_path(session_id.expect("scoped"), 30))).await?;
                streams.register(id)?;
                history_stream = Some(id);
                history_body.clear();
                continue;
            }
        };
        // Late events from a reset stream belong to an earlier turn.
        if frame.stream_id != subscription
            && Some(frame.stream_id) != chat
            && Some(frame.stream_id) != history_stream
        {
            continue;
        }
        for event in streams.consume(frame)? {
            match event {
                StreamEvent::Started {
                    stream_id, status, ..
                } => {
                    tracing::debug!(stream_id, status, "Muse response started");
                    if !(200..300).contains(&status) {
                        return Err(Box::new(super::client::ServiceError(status)));
                    }
                }
                StreamEvent::Data { stream_id, data } if stream_id == subscription => {
                    for mut record in decoder.push(&data)? {
                        crate::core::turn::compact_user_echo(&mut record);
                        tracing::debug!(
                            event = record.get("event").and_then(|value| value.as_str()),
                            "Muse subscription event"
                        );
                        let before = turn.revision();
                        let items = turn.ingest(record.clone())?;
                        if turn.revision() != before {
                            life.activity(Instant::now());
                            last_realtime = Instant::now();
                        }
                        emit(items, output).await?;
                        if acknowledged {
                            if let Some((text, code)) = life.ingest(&record, Instant::now())? {
                                output
                                    .send(Ok(TurnEvent::Progress { text, code }))
                                    .await
                                    .map_err(|_| "downstream closed")?;
                            }
                        } else if matches!(
                            record.get("event").and_then(Value::as_str),
                            Some("message.user" | "task.status" | "agent.status")
                        ) {
                            early_bytes += serde_json::to_vec(&record)?.len();
                            if early_events.len() >= 128 || early_bytes > 256 * 1024 {
                                return Err("too many events before Muse acknowledgement".into());
                            }
                            early_events.push(record);
                        }
                    }
                }
                StreamEvent::Data { stream_id, data } if Some(stream_id) == chat => {
                    if data.len() > MAX_ACK.saturating_sub(ack.len()) {
                        return Err("Muse acknowledgement is too large".into());
                    }
                    ack.extend(data);
                }
                StreamEvent::Data { stream_id, data } if Some(stream_id) == history_stream => {
                    if history_body.len() + data.len() > 32 * 1024 * 1024 {
                        return Err("Muse history response too large".into());
                    }
                    history_body.extend(data);
                }
                StreamEvent::End { stream_id } if Some(stream_id) == history_stream => {
                    let history: Value = serde_json::from_slice(&history_body)?;
                    let history = history.get("result").unwrap_or(&history);
                    if history.get("ok").and_then(Value::as_bool) == Some(false) {
                        return Err("Muse rejected history request".into());
                    }
                    history_busy = history
                        .get("chat_events")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .any(|row| {
                            row.get("seq").and_then(Value::as_u64).unwrap_or(0) > baseline
                                && row.get("event_name").and_then(Value::as_str)
                                    == Some("message.assistant")
                                && row.pointer("/payload/status").and_then(Value::as_str)
                                    != Some("completed")
                        });
                    for (id, text, widgets) in
                        completed_replies(history, baseline, session_id.expect("scoped"))
                    {
                        let before = turn.revision();
                        let mut items = turn.acknowledge_alias(id)?;
                        items.extend(turn.ingest(json!({"event":"delta.message_done","payload":{"session_id":session_id,"message_id":id,"content":text,"widgets":widgets}}))?);
                        if turn.revision() != before {
                            life.activity(Instant::now());
                        }
                        emit(items, output).await?;
                    }
                    history_stream = None;
                }
                StreamEvent::End { stream_id } if Some(stream_id) == chat => {
                    let value: Value = serde_json::from_slice(&ack)?;
                    tracing::debug!(keys = ?value.as_object().map(|object| object.keys().collect::<Vec<_>>()), "Muse acknowledgement received");
                    let value = value.get("result").unwrap_or(&value);
                    let id = value
                        .get("message_id")
                        .and_then(Value::as_str)
                        .ok_or("Muse acknowledgement has no message ID")?;
                    if let Some(scope) = value.get("session_id").and_then(Value::as_str) {
                        if session_id.is_some_and(|expected| expected != scope) {
                            return Err(
                                "Muse acknowledgement belongs to another conversation".into()
                            );
                        }
                        turn.scope(scope);
                        life.scope(scope);
                    }
                    let mut ack_events = turn.acknowledge(id)?;
                    if let Some(alias) = value.get("reply_to_message_id").and_then(Value::as_str) {
                        ack_events.extend(turn.acknowledge_alias(alias)?);
                    }
                    life.acknowledge(id);
                    if turn.revision() > 0 {
                        life.activity(Instant::now());
                    }
                    emit(ack_events, output).await?;
                    for record in early_events.drain(..) {
                        if let Some((text, code)) = life.ingest(&record, Instant::now())? {
                            output
                                .send(Ok(TurnEvent::Progress { text, code }))
                                .await
                                .map_err(|_| "downstream closed")?;
                        }
                    }
                    acknowledged = true;
                }
                StreamEvent::End { stream_id } if stream_id == subscription => {
                    return Err("Muse subscription ended early".into())
                }
                StreamEvent::Reset { .. } => return Err("Muse stream was reset".into()),
                _ => {}
            }
        }
    };
    client.reset(subscription).await?;
    if let Some(id) = history_stream {
        client.reset(id).await?;
    }
    if outcome == Outcome::Failed {
        return Err("Muse task failed".into());
    }
    output
        .send(Ok(if outcome == Outcome::Cancelled {
            TurnEvent::Cancelled
        } else {
            TurnEvent::Completed
        }))
        .await
        .map_err(|_| "downstream closed")?;
    Ok(())
}

async fn emit(
    items: Vec<TurnEvent>,
    output: &mpsc::Sender<std::result::Result<TurnEvent, String>>,
) -> Result<()> {
    for item in items {
        if item != TurnEvent::MessageDone {
            output
                .send(Ok(item))
                .await
                .map_err(|_| "downstream closed")?;
        }
    }
    Ok(())
}

fn completed_replies<'a>(
    history: &'a Value,
    baseline: u64,
    scope: &str,
) -> Vec<(&'a str, &'a str, Option<&'a Value>)> {
    let mut replies: Vec<_> = history
        .get("chat_events")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|row| row.get("seq").and_then(Value::as_u64).unwrap_or(0) > baseline)
        .filter(|row| row.get("event_name").and_then(Value::as_str) == Some("message.assistant"))
        .filter_map(|row| {
            let payload = row.get("payload")?;
            if row
                .get("session_id")
                .or_else(|| payload.get("session_id"))
                .and_then(Value::as_str)
                .is_some_and(|id| id != scope)
            {
                return None;
            }
            if row
                .get("reply_to_message_id")
                .or_else(|| payload.get("reply_to_message_id"))
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty())
            {
                return None;
            }
            if payload.get("status").and_then(Value::as_str) != Some("completed") {
                return None;
            }
            let id = row
                .get("message_id")
                .or_else(|| payload.get("message_id"))?
                .as_str()?;
            let text = payload
                .get("display_text")
                .or_else(|| payload.get("content"))?
                .as_str()?;
            if text.is_empty() {
                return None;
            }
            Some((row.get("seq")?.as_u64()?, id, text, payload.get("widgets")))
        })
        .collect();
    replies.sort_by_key(|(seq, _, _, _)| *seq);
    replies
        .into_iter()
        .map(|(_, id, text, widgets)| (id, text, widgets))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires a user-provided Muse account; creates two empty diagnostic sessions"]
    async fn muse_live_fresh_inference_sessions_are_distinct_and_empty() {
        let session = MuseSession::new(
            &std::env::var("MUSE_COOKIE").unwrap(),
            std::env::var("MUSE_NODE_ID").unwrap(),
        )
        .unwrap();
        let first = session.create_inference_conversation().await.unwrap();
        let second = session.create_inference_conversation().await.unwrap();
        assert_ne!(
            first.session_id, second.session_id,
            "Fresh inference sessions share an upstream ID"
        );
        let mut connection = session.connection.lock().await;
        for conversation in [&first, &second] {
            let history = connection
                .client
                .as_mut()
                .unwrap()
                .request_json(get(&history_path(&conversation.session_id, 2)))
                .await
                .unwrap();
            let history = history.get("result").unwrap_or(&history);
            let count = history
                .get("chat_events")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            println!("New upstream session history count: {count}");
            assert_eq!(
                count, 0,
                "New inference session contains earlier conversation events"
            );
        }
    }
    use crate::adapters::muse::protocol::{
        service_frame::Frame, ApplicationResponse, BodyChunk, NoiseTransportFrame, ServiceFrame,
        ServiceRequest, ServiceResponse,
    };
    use futures_util::{SinkExt, StreamExt};
    use prost::Message as _;
    use snow::{params::NoiseParams, Builder, TransportState};
    use tokio::net::TcpStream;
    use tokio_tungstenite::{accept_async, tungstenite::Message, WebSocketStream};

    async fn subscription_events(
        ws: &mut WebSocketStream<TcpStream>,
        noise: &mut TransportState,
        stream_id: i64,
        records: Vec<Value>,
    ) {
        let body: Vec<u8> = records
            .into_iter()
            .flat_map(|value| {
                let mut bytes = serde_json::to_vec(&value).unwrap();
                bytes.push(b'\n');
                bytes
            })
            .collect();
        respond(
            ws,
            noise,
            ServiceFrame {
                stream_id,
                frame: Some(Frame::BodyChunk(BodyChunk {
                    data: body,
                    end_body: false,
                })),
            },
        )
        .await;
    }

    #[tokio::test]
    async fn root_task_and_child_must_finish_before_multi_reply_stream_ends() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut ws, mut noise) = accept_peer(stream).await;
            let sub = read_request(&mut ws, &mut noise).await;
            let chat = read_request(&mut ws, &mut noise).await;
            respond(
                &mut ws,
                &mut noise,
                ServiceFrame {
                    stream_id: sub.stream_id,
                    frame: Some(Frame::Response(ApplicationResponse {
                        status: 200,
                        headers: vec![],
                        body: vec![],
                        end_body: false,
                    })),
                },
            )
            .await;
            respond(
                &mut ws,
                &mut noise,
                ServiceFrame {
                    stream_id: chat.stream_id,
                    frame: Some(Frame::Response(ApplicationResponse {
                        status: 200,
                        headers: vec![],
                        body: br#"{"message_id":"user","session_id":"own"}"#.to_vec(),
                        end_body: true,
                    })),
                },
            )
            .await;
            let event = |name: &str, mut payload: Value| {
                payload["session_id"] = json!("own");
                json!({"event":name,"payload":payload})
            };
            let task = |id: &str, agent: &str, status: &str| {
                event(
                    "task.status",
                    json!({"task_id":id,"agent_id":agent,"root_agent_id":"root","status":status}),
                )
            };
            subscription_events(
                &mut ws,
                &mut noise,
                sub.stream_id,
                vec![
                    event("message.user", json!({"message_id":"user"})),
                    task("root-task", "root", "running"),
                    task("child-task", "child", "running"),
                    event(
                        "agent.status",
                        json!({"activity_code":"researching","activity_text":"Checking sources"}),
                    ),
                    event(
                        "agent.status",
                        json!({"activity_code":"researching","activity_text":"Checking sources"}),
                    ),
                    event(
                        "delta.message_done",
                        json!({"message_id":"a","parent_message_id":"user","content":"first"}),
                    ),
                    task("root-task", "root", "completed"),
                    event(
                        "delta.message_start",
                        json!({"message_id":"b","parent_message_id":"user"}),
                    ),
                ],
            )
            .await;
            // Longer than terminal settle time; the open child/message must
            // prevent the client from closing before this second reply arrives.
            tokio::time::sleep(Duration::from_millis(1200)).await;
            subscription_events(&mut ws, &mut noise, sub.stream_id, vec![
                event("delta.message_done",json!({"message_id":"a","content":"first"})),
                event("delta.message_done",json!({"message_id":"b","content":"second","widgets":[{"widget_id":"late","kind":"option","data":{"options":["Next"]}}]})),
                task("child-task","child","completed"),
            ]).await;
            assert!(matches!(
                read_request(&mut ws, &mut noise).await.frame,
                Some(Frame::Reset(_))
            ));
        });
        let mut client = MuseClient::new(
            NoiseSocket::connect(&format!("ws://{address}"))
                .await
                .unwrap(),
        );
        let (sender, mut receiver) = mpsc::channel(32);
        tokio::time::timeout(
            Duration::from_secs(8),
            run_turn(&mut client, "node", "test", &[], None, &sender),
        )
        .await
        .unwrap()
        .unwrap();
        drop(sender);
        let mut events = Vec::new();
        while let Some(item) = receiver.recv().await {
            events.push(item.unwrap());
        }
        let text: String = events
            .iter()
            .filter_map(|item| {
                if let TurnEvent::Text(text) = item {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(text, "first\n\nsecond");
        assert_eq!(
            events
                .iter()
                .filter(|item| matches!(item, TurnEvent::Progress { .. }))
                .count(),
            1
        );
        assert!(events
            .iter()
            .any(|item| matches!(item, TurnEvent::QuickReplies(_))));
        assert_eq!(events.last(), Some(&TurnEvent::Completed));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn remote_cancellation_or_failure_never_becomes_success() {
        for status in ["cancelled", "failed"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let (mut ws, mut noise) = accept_peer(stream).await;
                let sub = read_request(&mut ws, &mut noise).await;
                let chat = read_request(&mut ws, &mut noise).await;
                respond(
                    &mut ws,
                    &mut noise,
                    ServiceFrame {
                        stream_id: sub.stream_id,
                        frame: Some(Frame::Response(ApplicationResponse {
                            status: 200,
                            headers: vec![],
                            body: vec![],
                            end_body: false,
                        })),
                    },
                )
                .await;
                respond(
                    &mut ws,
                    &mut noise,
                    ServiceFrame {
                        stream_id: chat.stream_id,
                        frame: Some(Frame::Response(ApplicationResponse {
                            status: 200,
                            headers: vec![],
                            body: br#"{"message_id":"user","session_id":"own"}"#.to_vec(),
                            end_body: true,
                        })),
                    },
                )
                .await;
                subscription_events(&mut ws,&mut noise,sub.stream_id,vec![
                    json!({"event":"message.user","payload":{"session_id":"own","message_id":"user"}}),
                    json!({"event":"task.status","payload":{"session_id":"own","task_id":"task","agent_id":"root","root_agent_id":"root","status":"running"}}),
                    json!({"event":"task.status","payload":{"session_id":"own","task_id":"task","agent_id":"root","root_agent_id":"root","status":status}}),
                ]).await;
                assert!(matches!(
                    read_request(&mut ws, &mut noise).await.frame,
                    Some(Frame::Reset(_))
                ));
            });
            let mut client = MuseClient::new(
                NoiseSocket::connect(&format!("ws://{address}"))
                    .await
                    .unwrap(),
            );
            let (sender, mut receiver) = mpsc::channel(8);
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                run_turn(&mut client, "node", "test", &[], None, &sender),
            )
            .await
            .unwrap();
            drop(sender);
            if status == "failed" {
                assert_eq!(result.unwrap_err().to_string(), "Muse task failed");
                assert!(receiver.recv().await.is_none());
            } else {
                result.unwrap();
                assert_eq!(
                    receiver.recv().await.unwrap().unwrap(),
                    TurnEvent::Cancelled
                );
            }
            server.await.unwrap();
        }
    }

    #[test]
    fn history_fallback_excludes_old_proactive_and_other_session_events() {
        let history = json!({"chat_events":[
            {"seq":10,"event_name":"message.assistant","message_id":"old","payload":{"status":"completed","display_text":"old"}},
            {"seq":11,"event_name":"message.assistant","message_id":"foreign","session_id":"another","payload":{"status":"completed","display_text":"wrong"}},
            {"seq":12,"event_name":"message.assistant","message_id":"push","reply_to_message_id":"push","payload":{"status":"completed","display_text":"background"}},
            {"seq":14,"event_name":"message.assistant","message_id":"later","session_id":"own","payload":{"status":"completed","display_text":"later"}},
            {"seq":13,"event_name":"message.assistant","message_id":"reply","session_id":"own","payload":{"status":"completed","display_text":"correct"}}
        ]});
        assert_eq!(
            completed_replies(&history, 10, "own"),
            vec![("reply", "correct", None), ("later", "later", None)]
        );
    }

    #[tokio::test]
    async fn scoped_chat_uses_history_when_subscription_has_no_deltas() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (mut ws, mut noise) = accept_peer(socket).await;
            let baseline = read_request(&mut ws, &mut noise).await;
            assert!(
                matches!(&baseline.frame, Some(Frame::Request(r)) if r.path == "/chat/history?limit=1&session_id=own")
            );
            respond(
                &mut ws,
                &mut noise,
                ServiceFrame {
                    stream_id: baseline.stream_id,
                    frame: Some(Frame::Response(ApplicationResponse {
                        status: 200,
                        headers: vec![],
                        body: br#"{"chat_events":[{"seq":10}]}"#.to_vec(),
                        end_body: true,
                    })),
                },
            )
            .await;
            let subscribe = read_request(&mut ws, &mut noise).await;
            let chat = read_request(&mut ws, &mut noise).await;
            for frame in [&subscribe, &chat] {
                let Some(Frame::Request(request)) = &frame.frame else {
                    panic!("expected request")
                };
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(body["session_id"], "own");
            }
            respond(
                &mut ws,
                &mut noise,
                ServiceFrame {
                    stream_id: subscribe.stream_id,
                    frame: Some(Frame::Response(ApplicationResponse {
                        status: 200,
                        headers: vec![],
                        body: vec![],
                        end_body: false,
                    })),
                },
            )
            .await;
            respond(
                &mut ws,
                &mut noise,
                ServiceFrame {
                    stream_id: chat.stream_id,
                    frame: Some(Frame::Response(ApplicationResponse {
                        status: 200,
                        headers: vec![],
                        body: br#"{"message_id":"user","session_id":"own"}"#.to_vec(),
                        end_body: true,
                    })),
                },
            )
            .await;
            let poll = read_request(&mut ws, &mut noise).await;
            assert!(
                matches!(&poll.frame, Some(Frame::Request(r)) if r.path == "/chat/history?limit=30&session_id=own")
            );
            let body = json!({"chat_events":[{"seq":11,"event_name":"message.assistant","message_id":"reply","session_id":"own","payload":{"status":"completed","display_text":"scoped reply[[hatch_widget:widget-1]]","widgets":[{"widget_id":"widget-1","kind":"option","data":{"options":["Next step"]}}]}}]}).to_string().into_bytes();
            respond(
                &mut ws,
                &mut noise,
                ServiceFrame {
                    stream_id: poll.stream_id,
                    frame: Some(Frame::Response(ApplicationResponse {
                        status: 200,
                        headers: vec![],
                        body,
                        end_body: true,
                    })),
                },
            )
            .await;
            loop {
                let request = read_request(&mut ws, &mut noise).await;
                if matches!(request.frame, Some(Frame::Reset(_))) {
                    break;
                }
                assert!(
                    matches!(&request.frame, Some(Frame::Request(r)) if r.path == "/chat/history?limit=30&session_id=own")
                );
                let body = json!({"chat_events":[{"seq":11,"event_name":"message.assistant","message_id":"reply","session_id":"own","payload":{"status":"completed","display_text":"scoped reply[[hatch_widget:widget-1]]","widgets":[{"widget_id":"widget-1","kind":"option","data":{"options":["Next step"]}}]}}]}).to_string().into_bytes();
                respond(
                    &mut ws,
                    &mut noise,
                    ServiceFrame {
                        stream_id: request.stream_id,
                        frame: Some(Frame::Response(ApplicationResponse {
                            status: 200,
                            headers: vec![],
                            body,
                            end_body: true,
                        })),
                    },
                )
                .await;
            }
        });
        let mut client = MuseClient::new(
            NoiseSocket::connect(&format!("ws://{address}"))
                .await
                .unwrap(),
        );
        let (tx, mut rx) = mpsc::channel(8);
        run_turn(&mut client, "node", "test", &[], Some("own"), &tx)
            .await
            .unwrap();
        drop(tx);
        assert_eq!(
            rx.recv().await.unwrap().unwrap(),
            TurnEvent::Text("scoped reply".into())
        );
        assert_eq!(
            rx.recv().await.unwrap().unwrap(),
            TurnEvent::QuickReplies(vec![crate::core::widgets::QuickReplies {
                id: "widget-1".into(),
                options: vec!["Next step".into()]
            }])
        );
        assert_eq!(rx.recv().await.unwrap().unwrap(), TurnEvent::Completed);
        server.await.unwrap();
    }

    async fn read_request(
        ws: &mut WebSocketStream<TcpStream>,
        noise: &mut TransportState,
    ) -> ServiceFrame {
        let mut assembler = super::super::protocol::Reassembler::default();
        loop {
            let Message::Binary(cipher) = ws.next().await.unwrap().unwrap() else {
                panic!("expected binary")
            };
            assert!(cipher.len() <= 65_535);
            let mut bytes = vec![0; 65_535];
            let size = noise.read_message(&cipher, &mut bytes).unwrap();
            let transport = NoiseTransportFrame::decode(&bytes[..size]).unwrap();
            if let Some(payload) = assembler.push(transport).unwrap() {
                let request = ServiceRequest::decode(payload.as_slice()).unwrap();
                return ServiceFrame::decode(request.payload.as_slice()).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn large_outgoing_chat_survives_noise_then_reset_and_next_stream_still_work() {
        let text =
            "Vietnamese: Tiếng Việt 🦀, tool schema and conversation history. ".repeat(75_000);
        let expected = text.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (mut ws, mut noise) = accept_peer(socket).await;
            let first = read_request(&mut ws, &mut noise).await;
            assert_eq!(first.stream_id, 1);
            let Some(Frame::Request(request)) = first.frame else {
                panic!("expected request")
            };
            assert_eq!(request.path, "/chat/stream");
            assert!(request.end_body);
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["items"][0]["text"], expected);
            let reset = read_request(&mut ws, &mut noise).await;
            assert_eq!(reset.stream_id, 1);
            assert!(matches!(reset.frame, Some(Frame::Reset(_))));
            let next = read_request(&mut ws, &mut noise).await;
            assert_eq!(next.stream_id, 2);
            assert!(
                matches!(next.frame,Some(Frame::Request(request)) if request.path == "/api/ping")
            );
        });
        let mut client = MuseClient::new(
            NoiseSocket::connect(&format!("ws://{address}"))
                .await
                .unwrap(),
        );
        let request = post(
            "/chat/stream",
            json!({"items":[{"type":"text","text":text}],"node_id":"node"}),
        )
        .unwrap();
        assert!(request.body.len() > crate::core::agent::MAX_CONTEXT_BYTES);
        let id = client.start_request(request).await.unwrap();
        client.reset(id).await.unwrap();
        client
            .start_request(post("/api/ping", json!({})).unwrap())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .unwrap()
            .unwrap();
    }

    async fn respond(
        ws: &mut WebSocketStream<TcpStream>,
        noise: &mut TransportState,
        frame: ServiceFrame,
    ) {
        let response = ServiceResponse {
            payload: frame.encode_to_vec(),
        };
        let transport = NoiseTransportFrame {
            chunk_id: frame.stream_id,
            chunk_index: 0,
            total_chunks: 1,
            payload: response.encode_to_vec(),
        };
        let mut bytes = vec![0; 65_535];
        let size = noise
            .write_message(&transport.encode_to_vec(), &mut bytes)
            .unwrap();
        ws.send(Message::Binary(bytes[..size].to_vec().into()))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn cached_closed_connection_reconnects_before_chat_and_healthy_socket_is_reused() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (closed, closed_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (ws, _) = accept_peer(socket).await;
            drop(ws); // Reproduce a remote host closing an idle connection.
            closed.send(()).unwrap();
            let (socket, _) = listener.accept().await.unwrap();
            let (mut ws, mut noise) = accept_peer(socket).await;
            let ping = read_request(&mut ws, &mut noise).await;
            assert!(
                matches!(&ping.frame, Some(Frame::Request(request)) if request.path == "/api/ping")
            );
            respond(
                &mut ws,
                &mut noise,
                ServiceFrame {
                    stream_id: ping.stream_id,
                    frame: Some(Frame::Response(ApplicationResponse {
                        status: 200,
                        headers: vec![],
                        body: vec![],
                        end_body: true,
                    })),
                },
            )
            .await;
        });
        let url = format!("ws://{address}");
        let mut cached = Some(MuseClient::new(NoiseSocket::connect(&url).await.unwrap()));
        closed_rx.await.unwrap();
        let connects = AtomicUsize::new(0);
        prepare_client(&mut cached, true, || async {
            connects.fetch_add(1, Ordering::SeqCst);
            Ok(MuseClient::new(NoiseSocket::connect(&url).await?))
        })
        .await
        .unwrap();
        assert_eq!(connects.load(Ordering::SeqCst), 1);
        prepare_client(&mut cached, true, || async {
            panic!("a healthy connection must not reconnect")
        })
        .await
        .unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn oversized_chat_status_reaches_the_adaptive_input_failure_classifier() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (mut ws, mut noise) = accept_peer(socket).await;
            let subscription = read_request(&mut ws, &mut noise).await;
            let chat = read_request(&mut ws, &mut noise).await;
            for (id, status) in [(subscription.stream_id, 200), (chat.stream_id, 413)] {
                respond(
                    &mut ws,
                    &mut noise,
                    ServiceFrame {
                        stream_id: id,
                        frame: Some(Frame::Response(ApplicationResponse {
                            status,
                            headers: vec![],
                            body: vec![],
                            end_body: status != 200,
                        })),
                    },
                )
                .await;
            }
        });
        let mut client = MuseClient::new(
            NoiseSocket::connect(&format!("ws://{address}"))
                .await
                .unwrap(),
        );
        let (sender, _receiver) = mpsc::channel(4);
        let error = run_turn(&mut client, "node", "oversized fixture", &[], None, &sender)
            .await
            .unwrap_err();
        let error = client_error(error.as_ref());
        assert_eq!(error, crate::ports::UPSTREAM_INPUT_TOO_LARGE);
        assert!(crate::core::agent::recoverable_task_failure(&Err(error)));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn hot_connection_skips_probe_but_idle_connection_still_checks_liveness() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (mut ws, mut noise) = accept_peer(socket).await;
            // There must be exactly one ping: the explicit idle check below.
            let ping = read_request(&mut ws, &mut noise).await;
            assert!(
                matches!(&ping.frame,Some(Frame::Request(request)) if request.path=="/api/ping")
            );
            respond(
                &mut ws,
                &mut noise,
                ServiceFrame {
                    stream_id: ping.stream_id,
                    frame: Some(Frame::Response(ApplicationResponse {
                        status: 200,
                        headers: vec![],
                        body: vec![],
                        end_body: true,
                    })),
                },
            )
            .await;
        });
        let mut cached = Some(MuseClient::new(
            NoiseSocket::connect(&format!("ws://{address}"))
                .await
                .unwrap(),
        ));
        prepare_client(&mut cached, false, || async {
            panic!("hot socket must be reused")
        })
        .await
        .unwrap();
        prepare_client(&mut cached, true, || async {
            panic!("healthy idle socket must not reconnect")
        })
        .await
        .unwrap();
        server.await.unwrap();
    }

    async fn accept_peer(socket: TcpStream) -> (WebSocketStream<TcpStream>, TransportState) {
        let mut ws = accept_async(socket).await.unwrap();
        let params: NoiseParams = "Noise_XX_25519_AESGCM_SHA256".parse().unwrap();
        let builder = Builder::new(params);
        let key = builder.generate_keypair().unwrap();
        let mut handshake = builder
            .local_private_key(&key.private)
            .unwrap()
            .build_responder()
            .unwrap();
        let mut buffer = vec![0; 65_535];
        let Message::Binary(first) = ws.next().await.unwrap().unwrap() else {
            panic!("expected handshake")
        };
        handshake.read_message(&first, &mut buffer).unwrap();
        let size = handshake.write_message(&[], &mut buffer).unwrap();
        ws.send(Message::Binary(buffer[..size].to_vec().into()))
            .await
            .unwrap();
        let Message::Binary(third) = ws.next().await.unwrap().unwrap() else {
            panic!("expected handshake")
        };
        handshake.read_message(&third, &mut buffer).unwrap();
        (ws, handshake.into_transport_mode().unwrap())
    }

    #[tokio::test]
    async fn routes_early_subscription_events_and_ignores_unrelated_messages() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (mut ws, mut noise) = accept_peer(socket).await;
            let subscribe = read_request(&mut ws, &mut noise).await;
            assert!(
                matches!(&subscribe.frame, Some(Frame::Request(request)) if request.path == "/chat/subscribe")
            );
            respond(
                &mut ws,
                &mut noise,
                ServiceFrame {
                    stream_id: subscribe.stream_id,
                    frame: Some(Frame::Response(ApplicationResponse {
                        status: 200,
                        headers: vec![],
                        body: vec![],
                        end_body: false,
                    })),
                },
            )
            .await;
            let chat = read_request(&mut ws, &mut noise).await;
            assert!(
                matches!(&chat.frame, Some(Frame::Request(request)) if request.path == "/chat/stream")
            );
            let events = concat!("{\"type\":\"event\",\"event\":\"delta.text_append\",\"payload\":{\"message_id\":\"other\",\"parent_message_id\":\"other-user\",\"text\":\"secret\"}}\n", "{\"type\":\"event\",\"event\":\"delta.text_append\",\"payload\":{\"message_id\":\"reply\",\"parent_message_id\":\"user-1\",\"text\":\"hello\"}}\n", "{\"type\":\"event\",\"event\":\"delta.message_done\",\"payload\":{\"message_id\":\"reply\",\"content\":\"hello!\"}}\n");
            respond(
                &mut ws,
                &mut noise,
                ServiceFrame {
                    stream_id: subscribe.stream_id,
                    frame: Some(Frame::BodyChunk(BodyChunk {
                        data: events.as_bytes().to_vec(),
                        end_body: false,
                    })),
                },
            )
            .await;
            respond(
                &mut ws,
                &mut noise,
                ServiceFrame {
                    stream_id: chat.stream_id,
                    frame: Some(Frame::Response(ApplicationResponse {
                        status: 200,
                        headers: vec![],
                        body: br#"{"message_id":"user-1"}"#.to_vec(),
                        end_body: true,
                    })),
                },
            )
            .await;
            let reset = read_request(&mut ws, &mut noise).await;
            assert!(matches!(reset.frame, Some(Frame::Reset(_))));
        });
        let socket = NoiseSocket::connect(&format!("ws://{address}"))
            .await
            .unwrap();
        let mut client = MuseClient::new(socket);
        let (tx, mut rx) = mpsc::channel(8);
        run_turn(&mut client, "test-node", "hi", &[], None, &tx)
            .await
            .unwrap();
        drop(tx);
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event.unwrap());
        }
        assert_eq!(
            events,
            vec![
                TurnEvent::Text("hello".into()),
                TurnEvent::Text("!".into()),
                TurnEvent::Completed
            ]
        );
        server.await.unwrap();
    }
}
