//! Completion belongs to the root task, not an individual assistant message.
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

pub const TERMINAL_QUIET: Duration = Duration::from_secs(1);
const CONFIRMED_QUIET: Duration = Duration::from_millis(250);
pub const HISTORY_QUIET: Duration = Duration::from_secs(3);
const MAX_TASKS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    Completed,
    Cancelled,
    Failed,
}

pub struct Lifecycle {
    scope: Option<String>,
    user_id: Option<String>,
    own_echo: bool,
    running: HashSet<String>,
    tasks: HashMap<String, String>,
    root: Option<String>,
    terminal: Option<Outcome>,
    changed: Instant,
    last_progress: Option<(String, Option<String>)>,
    early_running: Vec<Value>,
    active_children: usize,
    confirmed_no_children: bool,
    fast_completion: bool,
}

impl Lifecycle {
    pub fn scope(&mut self, scope: &str) {
        self.scope = Some(scope.to_owned());
    }
    pub fn new(scope: Option<&str>, now: Instant) -> Self {
        Self {
            scope: scope.map(str::to_owned),
            user_id: None,
            own_echo: false,
            running: HashSet::new(),
            tasks: HashMap::new(),
            root: None,
            terminal: None,
            changed: now,
            last_progress: None,
            early_running: Vec::new(),
            active_children: 0,
            confirmed_no_children: false,
            fast_completion: !crate::config::performance_baseline(),
        }
    }
    pub fn acknowledge(&mut self, id: &str) {
        self.user_id = Some(id.to_owned());
    }
    pub fn activity(&mut self, now: Instant) {
        self.changed = now;
    }
    pub fn has_task(&self) -> bool {
        self.root.is_some()
    }

    /// Only events following our acknowledged user echo may control the task.
    /// This excludes initial subscription snapshots and unrelated sessions.
    pub fn ingest(
        &mut self,
        event: &Value,
        now: Instant,
    ) -> Result<Option<(String, Option<String>)>, &'static str> {
        let Some(payload) = event.get("payload") else {
            return Ok(None);
        };
        let scope = payload.get("session_id").and_then(Value::as_str);
        if self.scope.as_deref() != scope || self.scope.is_none() {
            return Ok(None);
        }
        let name = event.get("event").and_then(Value::as_str).unwrap_or("");
        if name == "message.user"
            && self.user_id.as_deref() == payload.get("message_id").and_then(Value::as_str)
        {
            self.own_echo = true;
            for event in std::mem::take(&mut self.early_running) {
                self.ingest(&event, now)?;
            }
            return Ok(None);
        }
        if !self.own_echo {
            if self.user_id.is_some()
                && name == "task.status"
                && payload.get("status").and_then(Value::as_str) == Some("running")
            {
                if self.early_running.len() >= MAX_TASKS {
                    return Err("too many Muse tasks before user echo");
                }
                self.early_running.push(event.clone());
            }
            return Ok(None);
        }
        if name == "agent.status" {
            let code = payload.get("activity_code").and_then(Value::as_str);
            if matches!(
                code,
                Some("idle" | "online" | "completed" | "failed" | "cancelled" | "canceled")
            ) {
                return Ok(None);
            }
            let text = payload
                .get("activity_text")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
                .or(code);
            if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
                let progress = (text.to_owned(), code.map(str::to_owned));
                if self.last_progress.as_ref() != Some(&progress) {
                    self.last_progress = Some(progress.clone());
                    self.activity(now);
                    return Ok(Some(progress));
                }
            }
            return Ok(None);
        }
        if name != "task.status" {
            return Ok(None);
        }
        let Some(id) = payload
            .get("task_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            return Ok(None);
        };
        let status = payload.get("status").and_then(Value::as_str).unwrap_or("");
        if !matches!(
            status,
            "running" | "completed" | "failed" | "errored" | "cancelled" | "canceled"
        ) {
            return Ok(None);
        }
        let agent = payload.get("agent_id").and_then(Value::as_str);
        let root_agent = payload.get("root_agent_id").and_then(Value::as_str);
        let is_root = agent.is_some() && agent == root_agent;
        if is_root && (status == "running" || self.root.as_deref() == Some(id)) {
            self.confirmed_no_children = status == "completed"
                && payload.get("active_subagent_count").and_then(Value::as_u64) == Some(0);
        }
        if is_root && (status == "running" || self.root.as_deref() == Some(id)) {
            if let Some(count) = payload.get("active_subagent_count").and_then(Value::as_u64) {
                self.active_children =
                    usize::try_from(count).map_err(|_| "invalid Muse subagent count")?;
            }
        }
        if status == "running" {
            if !self.tasks.contains_key(id) && self.tasks.len() >= MAX_TASKS {
                return Err("too many Muse tasks in one turn");
            }
            if is_root && self.root.as_deref() != Some(id) {
                self.root = Some(id.to_owned());
                self.terminal = None;
            }
            self.running.insert(id.to_owned());
        } else {
            // A terminal event alone cannot bind a stale/background task.
            if !self.tasks.contains_key(id) {
                return Ok(None);
            }
            self.running.remove(id);
            if self.root.as_deref() == Some(id) {
                self.terminal = Some(match status {
                    "completed" => Outcome::Completed,
                    "cancelled" | "canceled" => Outcome::Cancelled,
                    _ => Outcome::Failed,
                });
            }
        }
        if self.tasks.get(id).map(String::as_str) != Some(status) {
            self.tasks.insert(id.to_owned(), status.to_owned());
            self.activity(now);
        }
        Ok(None)
    }

    pub fn outcome(
        &self,
        now: Instant,
        open_messages: bool,
        completed_messages: usize,
    ) -> Option<Outcome> {
        let quiet = now.saturating_duration_since(self.changed);
        if let Some(terminal) = self.terminal {
            if terminal != Outcome::Completed {
                return Some(terminal);
            }
            return (self.running.is_empty()
                && self.active_children == 0
                && !open_messages
                && quiet
                    >= if self.fast_completion
                        && self.confirmed_no_children
                        && completed_messages > 0
                    {
                        CONFIRMED_QUIET
                    } else {
                        TERMINAL_QUIET
                    })
            .then_some(terminal);
        }
        // Some side chats expose only history. This is an explicitly weaker
        // fallback; once root task state is known it must never override it.
        (self.root.is_none()
            && self.running.is_empty()
            && !open_messages
            && completed_messages > 0
            && quiet >= HISTORY_QUIET)
            .then_some(Outcome::Completed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn idle_snapshots_before_ack_do_not_end_a_new_request() {
        let now = Instant::now();
        let mut life = Lifecycle::new(Some("own"), now);
        life.ingest(&task("stale", "root", "completed"), now)
            .unwrap();
        life.ingest(&task("stale", "root", "running"), now).unwrap();
        life.acknowledge("user");
        assert_eq!(life.outcome(now + Duration::from_secs(30), false, 0), None);
        assert!(!life.has_task());
    }
    #[test]
    fn declared_active_subagents_prevent_early_completion_even_without_child_events() {
        let now = Instant::now();
        let mut life = ready(now);
        life.ingest(&task("root-task", "root", "running"), now)
            .unwrap();
        let mut terminal = task("root-task", "root", "completed");
        terminal["payload"]["active_subagent_count"] = json!(1);
        life.ingest(&terminal, now).unwrap();
        assert_eq!(life.outcome(now + Duration::from_secs(10), false, 1), None);
        terminal["payload"]["active_subagent_count"] = json!(0);
        life.ingest(&terminal, now).unwrap();
        assert_eq!(
            life.outcome(now + Duration::from_secs(10), false, 1),
            Some(Outcome::Completed)
        );
    }
    fn task(id: &str, agent: &str, status: &str) -> Value {
        json!({"event":"task.status","payload":{"session_id":"own","task_id":id,"agent_id":agent,"root_agent_id":"root","status":status}})
    }
    fn ready(now: Instant) -> Lifecycle {
        let mut life = Lifecycle::new(Some("own"), now);
        life.acknowledge("user");
        life.ingest(
            &json!({"event":"message.user","payload":{"session_id":"own","message_id":"user"}}),
            now,
        )
        .unwrap();
        life
    }
    #[test]
    fn waits_for_root_child_open_messages_and_quiet_period() {
        let now = Instant::now();
        let later = now + Duration::from_secs(10);
        let mut life = ready(now);
        life.ingest(&task("root-task", "root", "running"), now)
            .unwrap();
        life.ingest(&task("child-task", "child", "running"), now)
            .unwrap();
        assert_eq!(life.outcome(later, false, 1), None); // message_done isn't task_done
        life.ingest(&task("root-task", "root", "completed"), now)
            .unwrap();
        assert_eq!(life.outcome(later, false, 1), None);
        life.ingest(&task("child-task", "child", "completed"), now)
            .unwrap();
        assert_eq!(life.outcome(later, true, 1), None);
        assert_eq!(life.outcome(now, false, 1), None);
        assert_eq!(life.outcome(later, false, 2), Some(Outcome::Completed));
    }
    #[test]
    fn fast_completion_requires_explicit_root_confirmation_and_closed_messages() {
        let now = Instant::now();
        let mut life = ready(now);
        life.fast_completion = true;
        life.ingest(&task("root-task", "root", "running"), now)
            .unwrap();
        life.ingest(&task("root-task", "root", "completed"), now)
            .unwrap();
        assert_eq!(
            life.outcome(now + Duration::from_millis(300), false, 1),
            None
        );
        let mut confirmed = task("root-task", "root", "completed");
        confirmed["payload"]["active_subagent_count"] = json!(0);
        life.ingest(&confirmed, now).unwrap();
        assert_eq!(
            life.outcome(now + Duration::from_millis(300), true, 1),
            None
        );
        assert_eq!(
            life.outcome(now + Duration::from_millis(300), false, 0),
            None
        );
        assert_eq!(
            life.outcome(now + Duration::from_millis(300), false, 1),
            Some(Outcome::Completed)
        );
        life.ingest(&task("child-task", "child", "running"), now)
            .unwrap();
        assert_eq!(life.outcome(now + Duration::from_secs(10), false, 1), None);
    }
    #[test]
    fn ignores_snapshots_foreign_sessions_and_reports_failed_or_cancelled_tasks() {
        let now = Instant::now();
        for (status, expected) in [
            ("failed", Outcome::Failed),
            ("cancelled", Outcome::Cancelled),
        ] {
            let mut life = ready(now);
            life.ingest(&task("stale", "root", "completed"), now)
                .unwrap();
            let mut foreign = task("other", "root", "running");
            foreign["payload"]["session_id"] = json!("foreign");
            life.ingest(&foreign, now).unwrap();
            assert!(!life.has_task());
            life.ingest(&task("new", "root", "running"), now).unwrap();
            life.ingest(&task("new", "root", status), now).unwrap();
            assert_eq!(life.outcome(now, true, 0), Some(expected));
        }
    }
}
