//! Correlate Muse chat events to one acknowledged user message.
use std::collections::{HashMap, HashSet};

use super::widgets::{quick_replies, QuickReplies, WidgetTextFilter};
use serde_json::Value;

const MAX_PENDING_EVENTS: usize = 128;
const MAX_PENDING_BYTES: usize = 256 * 1024;

#[derive(Debug, PartialEq)]
pub enum TurnEvent {
    Text(String),
    QuickReplies(Vec<QuickReplies>),
    MessageDone,
    Progress { text: String, code: Option<String> },
    Completed,
    Cancelled,
}

/// Muse repeats the entire input in display_text, payload and chat_context of
/// message.user. Correlation needs only IDs; never retain those input copies in
/// the bounded pre-ACK queues or treat them as assistant output.
pub fn compact_user_echo(event: &mut Value) {
    if event.get("event").and_then(Value::as_str) != Some("message.user") {
        return;
    }
    if let Some(payload) = event.get_mut("payload").and_then(Value::as_object_mut) {
        payload.retain(|key, _| {
            matches!(
                key.as_str(),
                "session_id" | "message_id" | "reply_to_message_id" | "parent_message_id" | "seq"
            )
        });
    }
    if let Some(object) = event.as_object_mut() {
        object.retain(|key, _| matches!(key.as_str(), "type" | "event" | "payload" | "seq"));
    }
}

/// Unlinked events are held briefly and never emitted until an explicit parent
/// chain links them to this turn. This avoids leaking another user's messages.
#[derive(Default)]
pub struct Turn {
    session_id: Option<String>,
    observed_user: bool,
    user_id: Option<String>,
    linked: HashSet<String>,
    pending: Vec<Value>,
    pending_bytes: usize,
    sent: HashMap<String, String>,
    text_filters: HashMap<String, WidgetTextFilter>,
    widgets: HashMap<String, Vec<QuickReplies>>,
    completed: HashMap<String, Option<String>>,
    visible_started: HashSet<String>,
    widget_versions: HashMap<String, Vec<QuickReplies>>,
    seen_sequences: HashSet<(String, String, u64)>,
    revision: u64,
    text_bytes: usize,
}

impl Turn {
    fn reserve_text(&mut self, bytes: usize) -> Result<(), &'static str> {
        if bytes > (2 * 1024 * 1024usize).saturating_sub(self.text_bytes) {
            return Err("Muse reply text exceeded 2 MiB");
        }
        self.text_bytes += bytes;
        Ok(())
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn has_open_messages(&self) -> bool {
        self.sent.keys().any(|id| !self.completed.contains_key(id))
    }
    pub fn completed_messages(&self) -> usize {
        self.completed.len()
    }

    fn visible_text(&mut self, id: &str, text: String, output: &mut Vec<TurnEvent>) {
        if text.is_empty() {
            return;
        }
        if self.visible_started.insert(id.to_owned()) && self.visible_started.len() > 1 {
            output.push(TurnEvent::Text("\n\n".into()));
        }
        output.push(TurnEvent::Text(text));
    }
    pub fn scope(&mut self, session_id: &str) {
        self.session_id = Some(session_id.to_owned());
    }
    pub fn acknowledge_alias(&mut self, id: &str) -> Result<Vec<TurnEvent>, &'static str> {
        if !id.is_empty() {
            self.linked.insert(id.to_owned());
        }
        self.replay()
    }
    pub fn acknowledge(&mut self, user_id: &str) -> Result<Vec<TurnEvent>, &'static str> {
        if user_id.is_empty() || self.user_id.is_some() {
            return Err("invalid or duplicate chat acknowledgement");
        }
        self.user_id = Some(user_id.to_owned());
        self.linked.insert(user_id.to_owned());
        self.replay()
    }

    pub fn ingest(&mut self, mut event: Value) -> Result<Vec<TurnEvent>, &'static str> {
        compact_user_echo(&mut event);
        if let Some(scope) = self.session_id.as_deref() {
            if event
                .pointer("/payload/session_id")
                .and_then(Value::as_str)
                .is_some_and(|id| id != scope)
            {
                return Ok(Vec::new());
            }
        }
        tracing::debug!(keys = ?event.get("payload").and_then(|p| p.as_object()).map(|o| o.keys().collect::<Vec<_>>()), event = event.get("event").and_then(|v| v.as_str()), "Muse event fields");
        if event
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind != "event")
        {
            return Ok(Vec::new());
        }
        let name = event.get("event").and_then(Value::as_str).unwrap_or("");
        if !matches!(
            name,
            "message.user" | "delta.message_start" | "delta.text_append" | "delta.message_done"
        ) {
            return Ok(Vec::new());
        }
        let size = serde_json::to_vec(&event)
            .map_err(|_| "invalid chat event")?
            .len();
        if self.pending.len() >= MAX_PENDING_EVENTS
            || size > MAX_PENDING_BYTES.saturating_sub(self.pending_bytes)
        {
            return Err("unmatched chat event buffer exceeded");
        }
        self.pending_bytes += size;
        self.pending.push(event);
        self.replay()
    }

    fn replay(&mut self) -> Result<Vec<TurnEvent>, &'static str> {
        let mut output = Vec::new();
        loop {
            let mut remaining = Vec::new();
            let mut progressed = false;
            for event in std::mem::take(&mut self.pending) {
                let payload = event
                    .get("payload")
                    .and_then(Value::as_object)
                    .ok_or("chat event has no payload")?;
                if self.session_id.as_deref().is_some_and(|scope| {
                    payload
                        .get("session_id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| id != scope)
                }) {
                    continue;
                }
                let id = payload
                    .get("message_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let name = event.get("event").and_then(Value::as_str).unwrap_or("");
                let same_session = self.session_id.as_deref().is_some_and(|scope| {
                    payload.get("session_id").and_then(Value::as_str) == Some(scope)
                });
                if name == "message.user" && same_session && self.user_id.is_some() {
                    if self.linked.contains(id) {
                        self.observed_user = true;
                        progressed = true;
                        continue;
                    }
                    if self.observed_user {
                        return Err("another user message arrived during this chat turn");
                    }
                }
                let parent = payload
                    .get("reply_to_message_id")
                    .filter(|value| value.as_str().is_some_and(|id| !id.is_empty()))
                    .or_else(|| payload.get("parent_message_id"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                // Main-chat replies omit their user parent. Bind only a new
                // message_start in the acknowledged session after observing our
                // own message.user echo; reject concurrent user input above.
                let unparented_start = name == "delta.message_start"
                    && parent.is_empty()
                    && same_session
                    && self.observed_user;
                let belongs = !id.is_empty()
                    && (self.linked.contains(id)
                        || self.linked.contains(parent)
                        || unparented_start);
                tracing::debug!(
                    belongs,
                    has_parent = !parent.is_empty(),
                    "Muse event correlation"
                );
                if !belongs {
                    remaining.push(event);
                    continue;
                }
                progressed = true;
                self.linked.insert(id.to_owned());
                if let Some(seq) = payload
                    .get("seq")
                    .or_else(|| event.get("seq"))
                    .and_then(Value::as_u64)
                {
                    if self.seen_sequences.len() >= 32_768 {
                        return Err("too many Muse events in one turn");
                    }
                    if !self
                        .seen_sequences
                        .insert((id.to_owned(), name.to_owned(), seq))
                    {
                        continue;
                    }
                }
                if self.linked.len() > 1024 {
                    return Err("too many Muse messages in one turn");
                }
                if let Some(widgets) = payload.get("widgets") {
                    let parsed = quick_replies(widgets);
                    if !parsed.is_empty() {
                        if self.completed.contains_key(id) {
                            if self.widget_versions.get(id) != Some(&parsed) {
                                self.widget_versions.insert(id.to_owned(), parsed.clone());
                                output.push(TurnEvent::QuickReplies(parsed));
                                self.revision += 1;
                            }
                        } else {
                            self.widgets.insert(id.to_owned(), parsed);
                        }
                    }
                }
                let name = event.get("event").and_then(Value::as_str).unwrap_or("");
                match name {
                    "delta.message_start" => {
                        if !self.sent.contains_key(id) {
                            self.sent.insert(id.to_owned(), String::new());
                            self.revision += 1;
                        }
                    }
                    "delta.text_append" => {
                        if let Some(text) = payload.get("text").and_then(Value::as_str) {
                            self.reserve_text(text.len())?;
                            if self.completed.contains_key(id) {
                                return Err("Muse appended text to an already completed reply");
                            }
                            self.sent.entry(id.to_owned()).or_default().push_str(text);
                            self.revision += 1;
                            let visible = self
                                .text_filters
                                .entry(id.to_owned())
                                .or_default()
                                .push(text);
                            self.visible_text(id, visible, &mut output);
                        }
                    }
                    "delta.message_done" => {
                        let final_text = payload
                            .get("content")
                            .or_else(|| payload.get("transcript"))
                            .and_then(Value::as_str);
                        if let Some(previous) = self.completed.get(id) {
                            if let Some(text) = final_text {
                                let old = previous
                                    .as_deref()
                                    .or_else(|| self.sent.get(id).map(String::as_str))
                                    .unwrap_or("");
                                if text != old {
                                    return Err("Muse final text differs from completed reply");
                                }
                            }
                            continue;
                        }
                        if let Some(text) = final_text {
                            let previous = self.sent.get(id).map(String::as_str).unwrap_or("");
                            if let Some(rest) = text.strip_prefix(previous) {
                                if !rest.is_empty() {
                                    self.reserve_text(rest.len())?;
                                    let visible = self
                                        .text_filters
                                        .entry(id.to_owned())
                                        .or_default()
                                        .push(rest);
                                    self.sent.entry(id.to_owned()).or_default().push_str(rest);
                                    self.visible_text(id, visible, &mut output);
                                }
                            } else if !previous.is_empty() {
                                return Err("Muse final text differs from streamed text");
                            }
                        }
                        if let Some(mut filter) = self.text_filters.remove(id) {
                            let tail = filter.finish();
                            if !tail.is_empty() {
                                self.visible_text(id, tail, &mut output);
                            }
                        }
                        if let Some(widgets) = self.widgets.remove(id) {
                            self.widget_versions.insert(id.to_owned(), widgets.clone());
                            output.push(TurnEvent::QuickReplies(widgets));
                        }
                        self.completed
                            .insert(id.to_owned(), final_text.map(str::to_owned));
                        self.revision += 1;
                        output.push(TurnEvent::MessageDone);
                    }
                    _ => {}
                }
            }
            self.pending = remaining;
            self.pending_bytes = self
                .pending
                .iter()
                .map(|v| serde_json::to_vec(v).map_or(0, |b| b.len()))
                .sum();
            if !progressed {
                break;
            }
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn large_user_echo_before_ack_keeps_only_correlation_ids_and_still_detects_other_users() {
        let mut turn = Turn::default();
        turn.scope("own");
        let large = "full tools and system prompt ".repeat(30_000);
        let echo = |id: &str| json!({"type":"event","event":"message.user","payload":{"message_id":id,"session_id":"own","seq":10,"display_text":large,"chat_context":{"prompt":large},"payload":{"text":large}}});
        assert!(turn.ingest(echo("user")).unwrap().is_empty());
        assert!(turn.pending_bytes < 1024);
        assert!(turn.acknowledge("user").unwrap().is_empty());
        turn.ingest(json!({"event":"delta.message_start","payload":{"message_id":"reply","session_id":"own"}})).unwrap();
        assert_eq!(turn.ingest(json!({"event":"delta.text_append","payload":{"message_id":"reply","session_id":"own","text":"OK"}})).unwrap(),vec![TurnEvent::Text("OK".into())]);
        assert!(turn.ingest(echo("another-user")).is_err());
    }
    #[test]
    fn multiple_replies_are_separated_and_replayed_final_messages_are_deduplicated() {
        let mut turn = Turn::default();
        turn.acknowledge("user").unwrap();
        let event = |id: &str, text: &str| json!({"event":"delta.message_done","payload":{"message_id":id,"parent_message_id":"user","content":text}});
        assert_eq!(
            turn.ingest(event("a", "first")).unwrap(),
            vec![TurnEvent::Text("first".into()), TurnEvent::MessageDone]
        );
        assert!(turn.ingest(event("a", "first")).unwrap().is_empty());
        assert_eq!(
            turn.ingest(event("b", "second")).unwrap(),
            vec![
                TurnEvent::Text("\n\n".into()),
                TurnEvent::Text("second".into()),
                TurnEvent::MessageDone
            ]
        );
        assert_eq!(turn.completed_messages(), 2);
        assert!(turn.ingest(event("a", "revised")).is_err());
    }

    #[test]
    fn repeated_delta_sequence_is_ignored_but_identical_text_with_a_new_sequence_is_kept() {
        let mut turn = Turn::default();
        turn.acknowledge("user").unwrap();
        let event = |seq| json!({"event":"delta.text_append","payload":{"message_id":"reply","parent_message_id":"user","seq":seq,"text":"ha"}});
        assert_eq!(
            turn.ingest(event(1)).unwrap(),
            vec![TurnEvent::Text("ha".into())]
        );
        assert!(turn.ingest(event(1)).unwrap().is_empty());
        assert_eq!(
            turn.ingest(event(2)).unwrap(),
            vec![TurnEvent::Text("ha".into())]
        );
        assert!(turn.has_open_messages());
    }
    #[test]
    fn streams_clean_text_then_emits_option_labels_without_the_placeholder() {
        let mut turn = Turn::default();
        turn.acknowledge("user").unwrap();
        assert_eq!(turn.ingest(json!({"event":"delta.text_append","payload":{"message_id":"reply","parent_message_id":"user","text":"Chọn:\n[[hatch_"}})).unwrap(), vec![TurnEvent::Text("Chọn:\n".into())]);
        assert!(turn.ingest(json!({"event":"delta.text_append","payload":{"message_id":"reply","text":"widget:widget-demo]]"}})).unwrap().is_empty());
        let output = turn.ingest(json!({"event":"delta.message_done","payload":{"message_id":"reply","transcript":"Chọn:\n[[hatch_widget:widget-demo]]","widgets":[{"widget_id":"widget-demo","kind":"option","data":{"options":["Lên lộ trình chi tiết", "Bắt đầu bài đầu tiên"]}}]}})).unwrap();
        assert_eq!(
            output,
            vec![
                TurnEvent::QuickReplies(vec![QuickReplies {
                    id: "widget-demo".into(),
                    options: vec![
                        "Lên lộ trình chi tiết".into(),
                        "Bắt đầu bài đầu tiên".into()
                    ]
                }]),
                TurnEvent::MessageDone
            ]
        );
    }
    #[test]
    fn browser_events_bind_after_own_echo_in_acknowledged_session() {
        let mut turn = Turn::default();
        turn.scope("main");
        turn.acknowledge("user").unwrap();
        turn.ingest(
            json!({"event":"message.user","payload":{"session_id":"main","message_id":"user"}}),
        )
        .unwrap();
        turn.ingest(json!({"event":"delta.message_start","payload":{"session_id":"main","message_id":"reply","reply_to_message_id":""}})).unwrap();
        assert_eq!(turn.ingest(json!({"event":"delta.text_append","payload":{"session_id":"main","message_id":"reply","parent_message_id":"reply","reply_to_message_id":"","text":"OK"}})).unwrap(), vec![TurnEvent::Text("OK".into())]);
        assert!(turn.ingest(json!({"event":"message.user","payload":{"session_id":"main","message_id":"someone-else"}})).is_err());
    }

    #[test]
    fn buffers_until_ack_and_drops_unrelated_messages() {
        let mut turn = Turn::default();
        let own = json!({"type":"event","event":"delta.text_append","payload":{"message_id":"reply","parent_message_id":"user","text":"hi"}});
        let other = json!({"type":"event","event":"delta.text_append","payload":{"message_id":"other","parent_message_id":"other-user","text":"secret"}});
        assert!(turn.ingest(own).unwrap().is_empty());
        assert!(turn.ingest(other).unwrap().is_empty());
        assert_eq!(
            turn.acknowledge("user").unwrap(),
            vec![TurnEvent::Text("hi".into())]
        );
        let done = json!({"type":"event","event":"delta.message_done","payload":{"message_id":"reply","content":"hi!"}});
        assert_eq!(
            turn.ingest(done).unwrap(),
            vec![TurnEvent::Text("!".into()), TurnEvent::MessageDone]
        );
    }
}
