//! Reuse only an exact, successfully completed transcript prefix. Changes,
//! branches, cancellation and failures always start a fresh remote context.
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, OwnedMutexGuard};

use super::agent::{infer, infer_prompt, AgentReply, PreparedRequest, MAX_CONTEXT_BYTES};
use crate::ports::TextGenerator;

const MAX_SESSIONS: usize = 16;
const TTL: Duration = Duration::from_secs(30 * 60);
const FAILED_REQUEST_TTL: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct AgentSessions {
    slots: Mutex<Vec<Slot>>,
    failed_requests: Mutex<Vec<FailedRequest>>,
}

struct FailedRequest {
    client: String,
    digest: [u8; 32],
    at: Instant,
    error: String,
}

fn retry_digest(prepared: &PreparedRequest) -> [u8; 32] {
    // A new bridge nonce and stream/JSON fallback must not disguise the same
    // failed request. Retain all semantic controls, history and actual images.
    let mut digest = Sha256::new();
    digest.update(prepared.prompt.replace(&prepared.nonce, "<bridge_id>"));
    for image in &prepared.request.images {
        digest.update(image.id.as_bytes());
        digest.update(image.media_type.as_bytes());
        digest.update(image.data_base64.as_bytes());
    }
    digest.finalize().into()
}

struct Slot {
    client: String,
    entry: Arc<Mutex<Entry>>,
}

struct Entry {
    static_context: String,
    history: Vec<Value>,
    generator: Option<Arc<dyn TextGenerator>>,
    valid: bool,
    used: Instant,
}

impl AgentSessions {
    async fn select(
        &self,
        client: &str,
        prepared: &PreparedRequest,
    ) -> Option<OwnedMutexGuard<Entry>> {
        let mut slots = self.slots.lock().await;
        let now = Instant::now();
        slots.retain(|slot| {
            Arc::strong_count(&slot.entry) > 1
                || slot
                    .entry
                    .try_lock()
                    .is_ok_and(|entry| now.duration_since(entry.used) < TTL)
        });
        let static_context = prepared.static_context();
        let messages = prepared
            .request
            .messages
            .as_array()
            .expect("validated history");
        let mut best: Option<OwnedMutexGuard<Entry>> = None;
        for slot in slots.iter().filter(|slot| slot.client == client) {
            let Ok(entry) = slot.entry.clone().try_lock_owned() else {
                continue;
            };
            tracing::debug!(
                static_match = entry.static_context == static_context,
                cached_messages = entry.history.len(),
                request_messages = messages.len(),
                common_prefix = messages
                    .iter()
                    .zip(&entry.history)
                    .take_while(|(a, b)| a == b)
                    .count(),
                valid = entry.valid,
                "agent session prefix check"
            );
            if entry.valid
                && entry.static_context == static_context
                && !entry.history.is_empty()
                && messages.len() > entry.history.len()
                && messages.starts_with(&entry.history)
                && best
                    .as_ref()
                    .is_none_or(|best| entry.history.len() > best.history.len())
            {
                best = Some(entry);
            }
        }
        if best.is_some() {
            return best;
        }
        if slots.len() >= MAX_SESSIONS {
            let oldest = slots
                .iter()
                .enumerate()
                .filter(|(_, slot)| Arc::strong_count(&slot.entry) == 1)
                .filter_map(|(index, slot)| {
                    slot.entry.try_lock().ok().map(|entry| (index, entry.used))
                })
                .min_by_key(|(_, used)| *used)
                .map(|(index, _)| index)?;
            slots.remove(oldest);
        }
        let entry = Arc::new(Mutex::new(Entry {
            static_context,
            history: Vec::new(),
            generator: None,
            valid: false,
            used: now,
        }));
        let guard = entry
            .clone()
            .try_lock_owned()
            .expect("new entry is unlocked");
        slots.push(Slot {
            client: client.to_owned(),
            entry,
        });
        Some(guard)
    }

    pub async fn infer(
        &self,
        generator: Arc<dyn TextGenerator>,
        prepared: PreparedRequest,
        client: Option<&str>,
    ) -> Result<AgentReply, String> {
        let digest = retry_digest(&prepared);
        {
            let mut failures = self.failed_requests.lock().await;
            failures.retain(|failure| failure.at.elapsed() < FAILED_REQUEST_TTL);
            if let Some(failure) = failures
                .iter()
                .find(|failure| failure.client == client.unwrap_or("") && failure.digest == digest)
            {
                tracing::warn!("Suppressing unchanged failed agent request during cooldown");
                return Err(failure.error.clone());
            }
        }
        let result = self.infer_inner(generator, prepared, client).await;
        if let Err(error) = &result {
            if super::agent::terminal_failure(error) {
                let mut failures = self.failed_requests.lock().await;
                if failures.len() >= MAX_SESSIONS {
                    failures.remove(0);
                }
                failures.push(FailedRequest {
                    client: client.unwrap_or("").to_owned(),
                    digest,
                    at: Instant::now(),
                    error: error.clone(),
                });
            }
        }
        result
    }

    async fn infer_inner(
        &self,
        generator: Arc<dyn TextGenerator>,
        prepared: PreparedRequest,
        client: Option<&str>,
    ) -> Result<AgentReply, String> {
        let Some(client) = client else {
            return infer(generator.as_ref(), prepared).await;
        };
        let Some(mut entry) = self.select(client, &prepared).await else {
            // All cached branches are active. Preserve correctness without
            // creating an unbounded cache or blocking independent branches.
            return infer(generator.as_ref(), prepared).await;
        };
        let reused = entry.valid;
        let prefix_len = if reused { entry.history.len() } else { 0 };
        let prompt = if reused {
            prepared.continuation_prompt(entry.history.len())
        } else {
            prepared.prompt.clone()
        };
        let bytes = prompt.len();
        let started = Instant::now();
        // Mark dirty BEFORE any await. Dropping this future after partial send
        // cannot leave an apparently reusable remote transcript behind.
        entry.valid = false;
        entry.used = started;
        if !reused {
            entry.generator = generator.open_session().await?;
        }
        let Some(bound) = entry.generator.clone() else {
            return infer(generator.as_ref(), prepared).await;
        };
        tracing::info!(
            mode = if reused { "delta" } else { "full" },
            upstream_bytes = bytes,
            "Muse agent context"
        );
        let mut result = infer_prompt(bound.as_ref(), &prepared, prompt, prefix_len).await;
        if super::agent::recoverable_task_failure(&result)
            && (prepared.prompt.len() > super::context_upload::DIRECT_PROMPT_BYTES
                || prepared.request.images.len() > 1)
            && bound.supports_context_upload()
        {
            // Only an explicit failed task/input rejection is recovered. Never retry a
            // timeout, disconnect, cancellation or malformed tool response.
            entry.generator = None;
            entry.generator = generator.open_session().await?;
            if let Some(fresh) = &entry.generator {
                result = super::agent::infer_uploaded(fresh.as_ref(), &prepared).await;
            }
        }
        if let Ok(reply) = &result {
            let mut history = prepared
                .request
                .messages
                .as_array()
                .expect("history")
                .clone();
            history.push(json!({"role":"assistant","content":reply.content}));
            let history_bytes = serde_json::to_vec(&history).expect("JSON").len();
            if history_bytes + entry.static_context.len() <= MAX_CONTEXT_BYTES {
                entry.history = history;
                entry.valid = true;
            } else {
                entry.history.clear();
                entry.static_context.clear();
                entry.generator = None;
            }
        }
        entry.used = Instant::now();
        tracing::info!(
            mode = if reused { "delta" } else { "full" },
            upstream_bytes = bytes,
            elapsed_ms = started.elapsed().as_millis(),
            success = result.is_ok(),
            "Muse agent inference finished"
        );
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{
            agent::{AgentRequest, Tool, ToolChoice},
            turn::TurnEvent,
        },
        ports::{GenerationFuture, SessionFuture},
    };
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex as StdMutex,
    };
    use tokio::sync::{mpsc, Notify};

    #[derive(Default)]
    struct Shared {
        sessions: AtomicUsize,
        calls: StdMutex<Vec<(usize, String)>>,
        fail: AtomicBool,
        block: AtomicBool,
        started: Notify,
    }
    struct Root(Arc<Shared>);
    struct Bound(Arc<Shared>, usize);
    impl TextGenerator for Root {
        fn generate(&self, prompt: String) -> GenerationFuture<'_> {
            Box::pin(async move { self.open_session().await?.unwrap().generate(prompt).await })
        }
        fn open_session(&self) -> SessionFuture<'_> {
            Box::pin(async move {
                let id = self.0.sessions.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(Some(
                    Arc::new(Bound(self.0.clone(), id)) as Arc<dyn TextGenerator>
                ))
            })
        }
    }
    impl TextGenerator for Bound {
        fn generate(&self, prompt: String) -> GenerationFuture<'_> {
            Box::pin(async move {
                self.0.calls.lock().unwrap().push((self.1, prompt.clone()));
                if self.0.block.swap(false, Ordering::SeqCst) {
                    self.0.started.notify_one();
                    std::future::pending::<()>().await;
                }
                let (nonce, context) = if prompt.contains("CONTEXT_DELTA\n") {
                    (
                        prompt
                            .split("bridge_id exactly ")
                            .nth(1)
                            .unwrap()
                            .split_whitespace()
                            .next()
                            .unwrap(),
                        prompt.split("CONTEXT_DELTA\n").nth(1).unwrap(),
                    )
                } else {
                    (
                        prompt
                            .split("bridge_id exactly: ")
                            .nth(1)
                            .unwrap()
                            .split('.')
                            .next()
                            .unwrap(),
                        prompt.split("CONTEXT_JSON\n").nth(1).unwrap(),
                    )
                };
                let context: Value = serde_json::from_str(context).unwrap();
                let messages = context
                    .get("new_messages")
                    .or_else(|| context.get("messages"))
                    .unwrap();
                let tool = messages
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|message| message["content"][0]["text"] == "tool please");
                let content = if tool {
                    json!([{"type":"tool_use","name":"Read","input":{"file_path":"test.txt"}}])
                } else {
                    json!([{"type":"text","text":"answer"}])
                };
                let response = if self.0.fail.swap(false, Ordering::SeqCst) {
                    "not JSON".into()
                } else {
                    json!({"bridge_id":nonce,"content":content}).to_string()
                };
                let (sender, receiver) = mpsc::channel(2);
                sender.send(Ok(TurnEvent::Text(response))).await.unwrap();
                sender.send(Ok(TurnEvent::Completed)).await.unwrap();
                Ok(receiver)
            })
        }
    }
    fn user(text: &str) -> Value {
        json!({"role":"user","content":[{"type":"text","text":text}]})
    }

    #[tokio::test]
    async fn ack_failure_cooldown_blocks_only_identical_requests_then_expires() {
        struct Failed(AtomicUsize);
        impl TextGenerator for Failed {
            fn generate(&self, _: String) -> GenerationFuture<'_> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Err(super::super::context_upload::ACK_FAILURE.into()) })
            }
        }
        let backend = Arc::new(Failed(AtomicUsize::new(0)));
        let sessions = AgentSessions::default();
        for _ in 0..3 {
            // prepare creates a fresh nonce, like Claude's stream/JSON fallback.
            assert!(sessions
                .infer(
                    backend.clone(),
                    prepare(vec![user("same")], "sys"),
                    Some("client")
                )
                .await
                .is_err());
        }
        assert_eq!(backend.0.load(Ordering::SeqCst), 1);
        assert!(sessions
            .infer(
                backend.clone(),
                prepare(vec![user("changed")], "sys"),
                Some("client")
            )
            .await
            .is_err());
        assert!(sessions
            .infer(
                backend.clone(),
                prepare(vec![user("same")], "sys"),
                Some("other-client")
            )
            .await
            .is_err());
        assert_eq!(backend.0.load(Ordering::SeqCst), 3);
        for failure in sessions.failed_requests.lock().await.iter_mut() {
            failure.at -= FAILED_REQUEST_TTL + Duration::from_secs(1);
        }
        assert!(sessions
            .infer(
                backend.clone(),
                prepare(vec![user("same")], "sys"),
                Some("client")
            )
            .await
            .is_err());
        assert_eq!(backend.0.load(Ordering::SeqCst), 4);
        assert_eq!(sessions.failed_requests.lock().await.len(), 1);
    }
    fn prepare(messages: Vec<Value>, system: &str) -> PreparedRequest {
        let schema = json!({"type":"object","properties":{"file_path":{"type":"string"}},"required":["file_path"]});
        AgentRequest {system:json!([{"type":"text","text":system}]),messages:json!(messages),
            tools:vec![Tool {name:"Read".into(),definition:json!({"name":"Read","description":"tool documentation ".repeat(3000),"input_schema":schema}),validator:jsonschema::validator_for(&schema).unwrap()}],
            choice:ToolChoice::Auto,parallel:true,max_tokens:1024,images:Vec::new()}.prepare().unwrap()
    }
    fn next(mut history: Vec<Value>, reply: &AgentReply, text: &str) -> Vec<Value> {
        history.push(json!({"role":"assistant","content":reply.content}));
        history.push(user(text));
        history
    }
    fn setup() -> (Arc<Shared>, Arc<dyn TextGenerator>, Arc<AgentSessions>) {
        let shared = Arc::new(Shared::default());
        (
            shared.clone(),
            Arc::new(Root(shared)),
            Arc::new(AgentSessions::default()),
        )
    }

    #[tokio::test]
    async fn continuation_reuses_remote_session_sends_only_delta_and_keeps_wire_tool_ids() {
        let (shared, backend, cache) = setup();
        let first_history = vec![user("tool please")];
        let reply = cache
            .infer(
                backend.clone(),
                prepare(first_history.clone(), "system"),
                Some("client-a"),
            )
            .await
            .unwrap();
        let id = reply.content[0]["id"].as_str().unwrap().to_owned();
        let mut history = first_history;
        history.push(json!({"role":"assistant","content":reply.content}));
        history.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":[{"type":"text","text":"actual file data"}],"is_error":false}]}));
        let second = cache
            .infer(backend, prepare(history, "system"), Some("client-a"))
            .await
            .unwrap();
        assert_eq!(second.stop_reason, "end_turn");
        assert_eq!(shared.sessions.load(Ordering::SeqCst), 1);
        let calls = shared.calls.lock().unwrap();
        assert_eq!(calls[0].0, calls[1].0);
        assert!(calls[1].1.len() * 10 < calls[0].1.len());
        assert!(!calls[1].1.contains("tool documentation"));
        assert!(calls[1].1.contains(&id));
        assert!(calls[1].1.contains("actual file data"));
    }

    #[tokio::test]
    async fn changed_context_branches_and_other_clients_never_share_remote_history() {
        let (shared, backend, cache) = setup();
        let first_history = vec![user("a")];
        let reply = cache
            .infer(
                backend.clone(),
                prepare(first_history.clone(), "system"),
                Some("a"),
            )
            .await
            .unwrap();
        let branch = next(first_history.clone(), &reply, "b");
        cache
            .infer(
                backend.clone(),
                prepare(branch.clone(), "system"),
                Some("a"),
            )
            .await
            .unwrap();
        cache
            .infer(
                backend.clone(),
                prepare(next(first_history, &reply, "different branch"), "system"),
                Some("a"),
            )
            .await
            .unwrap();
        cache
            .infer(
                backend.clone(),
                prepare(branch.clone(), "changed system"),
                Some("a"),
            )
            .await
            .unwrap();
        cache
            .infer(
                backend.clone(),
                prepare(branch, "system"),
                Some("another-client"),
            )
            .await
            .unwrap();
        assert_eq!(shared.sessions.load(Ordering::SeqCst), 4);
        let calls = shared.calls.lock().unwrap();
        assert_eq!(calls[0].0, calls[1].0);
        for index in 2..calls.len() {
            assert_ne!(calls[0].0, calls[index].0);
        }
    }

    #[tokio::test]
    async fn failed_or_cancelled_inference_cannot_leave_a_reusable_prefix() {
        for cancel in [false, true] {
            let (shared, backend, cache) = setup();
            let history = vec![user("a")];
            let first = cache
                .infer(
                    backend.clone(),
                    prepare(history.clone(), "system"),
                    Some("a"),
                )
                .await
                .unwrap();
            let history = next(history, &first, "b");
            if cancel {
                shared.block.store(true, Ordering::SeqCst);
                let (cache, backend, history) = (cache.clone(), backend.clone(), history.clone());
                let task = tokio::spawn(async move {
                    cache
                        .infer(backend, prepare(history, "system"), Some("a"))
                        .await
                });
                shared.started.notified().await;
                task.abort();
                let _ = task.await;
            } else {
                shared.fail.store(true, Ordering::SeqCst);
                assert!(cache
                    .infer(
                        backend.clone(),
                        prepare(history.clone(), "system"),
                        Some("a")
                    )
                    .await
                    .is_err());
                // An immediate identical retry is deliberately suppressed.
                // Expire its failure before checking that the dirty remote
                // prefix cannot be reused by a later retry.
                for failure in cache.failed_requests.lock().await.iter_mut() {
                    failure.at -= FAILED_REQUEST_TTL + Duration::from_secs(1);
                }
            }
            cache
                .infer(backend, prepare(history, "system"), Some("a"))
                .await
                .unwrap();
            assert_eq!(shared.sessions.load(Ordering::SeqCst), 2);
        }
    }

    #[tokio::test]
    async fn cache_is_bounded_expires_and_requests_without_identity_stay_stateless() {
        let (shared, backend, cache) = setup();
        for index in 0..MAX_SESSIONS + 3 {
            cache
                .infer(
                    backend.clone(),
                    prepare(vec![user("a")], "system"),
                    Some(&index.to_string()),
                )
                .await
                .unwrap();
        }
        assert_eq!(cache.slots.lock().await.len(), MAX_SESSIONS);
        let history = vec![user("a")];
        let reply = cache
            .infer(
                backend.clone(),
                prepare(history.clone(), "system"),
                Some("expire"),
            )
            .await
            .unwrap();
        {
            let slots = cache.slots.lock().await;
            let slot = slots.iter().find(|slot| slot.client == "expire").unwrap();
            slot.entry.lock().await.used = Instant::now() - TTL - Duration::from_secs(1);
        }
        let before = shared.sessions.load(Ordering::SeqCst);
        cache
            .infer(
                backend.clone(),
                prepare(next(history, &reply, "b"), "system"),
                Some("expire"),
            )
            .await
            .unwrap();
        cache
            .infer(backend.clone(), prepare(vec![user("same")], "system"), None)
            .await
            .unwrap();
        cache
            .infer(backend, prepare(vec![user("same")], "system"), None)
            .await
            .unwrap();
        assert_eq!(shared.sessions.load(Ordering::SeqCst), before + 3);
    }
}
