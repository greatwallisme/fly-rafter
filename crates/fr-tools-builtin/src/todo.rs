// Rust guideline compliant 2026-10-30
//! The todo list domain (dsh `tool-todo`, 212-line twin): the whole-list
//! replacement tool body, the `todos` projection fold, and the tool's
//! presentation faces.
//!
//! Each call appends a `todo/write` snapshot to the owning session; replay
//! is last-write-wins, cleared by the next `turn/start` (`turn/end` keeps
//! the finished checklist visible). The baked `allow-parallel-in-progress`
//! judgment mirrors the headless bundle row (`true`; the fr-schema
//! `fr-tools-builtin` declaration carries the same face for assembly-side
//! validation — the dual-source drift pin rides the fr-schema tests).

use serde_json::{Value, json};

use fr_wire::event::event_type;

use crate::tools::BodyError;

/// The durable change event (dsh `todo/write`; the vocabulary declaration
/// lands at activation).
pub const CHANGE_EVENT: &str = event_type::TODO_WRITE;

/// The projection key (dsh `todos`).
pub const PROJECTION_KEY: &str = "todos";

/// The projection format version (dsh `stateVersion: 2`).
pub const STATE_VERSION: u64 = 2;

/// The valid statuses (dsh `STATUSES`; the schema enum freezes the same
/// order).
pub const STATUSES: [&str; 3] = ["pending", "in_progress", "completed"];

/// The session-append seam: one durable record onto the owning session
/// (the dsh `agent.session.append` face).
pub trait SessionAppend: Send + Sync {
    /// Appends one durable event record.
    ///
    /// # Errors
    ///
    /// The persistence refusal.
    fn append(&self, session: &str, event_type: &str, data: &Value) -> Result<(), String>;
}

/// One canonical list entry (the dsh `TodoItem`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItem {
    /// The trimmed non-empty task line.
    pub content: String,
    /// The lifecycle status (`pending` / `in_progress` / `completed`).
    pub status: String,
}

/// The todo domain (dsh `apply`: the registered tool body over the baked
/// policy and the owning sessions).
pub struct TodoLists {
    allow_parallel: bool,
    log: Box<dyn SessionAppend>,
}

impl std::fmt::Debug for TodoLists {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TodoLists")
            .field("allow_parallel", &self.allow_parallel)
            .finish_non_exhaustive()
    }
}

impl TodoLists {
    /// Assembles the domain over the baked policy and the session-append
    /// seam.
    #[must_use]
    pub fn new(allow_parallel: bool, log: Box<dyn SessionAppend>) -> Self {
        Self {
            allow_parallel,
            log,
        }
    }

    /// Validates the value constraints the parameter schema cannot express
    /// and builds the canonical list (dsh `toTodoList`, call-for-call):
    /// trimmed non-empty unique content, and at most one `in_progress`
    /// item unless the deployment allows parallel work.
    ///
    /// # Errors
    ///
    /// The dsh refusal texts, verbatim.
    pub fn to_todo_list(&self, raw: &Value) -> Result<Vec<TodoItem>, BodyError> {
        let Some(items) = raw.as_array() else {
            return Err(BodyError::plain(
                "invalid todos: expected an array of todo items",
            ));
        };
        let mut todos = Vec::with_capacity(items.len());
        let mut seen = std::collections::HashSet::new();
        let mut active = 0_usize;
        for item in items {
            let content = item
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned();
            if content.is_empty() {
                return Err(BodyError::plain(
                    "invalid todo: `content` must be a non-empty string",
                ));
            }
            if seen.contains(&content) {
                return Err(BodyError::plain(format!(
                    "invalid todos: duplicate content {}",
                    Value::String(content)
                )));
            }
            seen.insert(content.clone());
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if status == "in_progress" {
                active += 1;
            }
            todos.push(TodoItem {
                content,
                status: status.to_owned(),
            });
        }
        if !self.allow_parallel && active > 1 {
            return Err(BodyError::plain(format!(
                "invalid todos: at most one task may be in_progress (got {active})"
            )));
        }
        Ok(todos)
    }

    /// The tool body (dsh `execute`, call-for-call): validates first, then
    /// rejects a caller without an owning session, appends the durable
    /// snapshot, and returns the list plus the status counts.
    ///
    /// # Errors
    ///
    /// The validation refusals above or the persistence failure.
    pub fn execute(&self, arguments: &Value, agent: Option<&str>) -> Result<Value, BodyError> {
        let todos = self.to_todo_list(arguments.get("todos").unwrap_or(&Value::Null))?;
        let Some(agent) = agent else {
            return Err(BodyError::plain(
                "todo_write requires an owning agent session",
            ));
        };
        let data = json!({ "todos": todos.iter().map(|item| json!({
            "content": item.content,
            "status": item.status,
        })).collect::<Vec<_>>() });
        self.log
            .append(agent, CHANGE_EVENT, &data)
            .map_err(BodyError::plain)?;
        let count = |status: &str| todos.iter().filter(|item| item.status == status).count();
        Ok(json!({
            "todos": todos.iter().map(|item| json!({
                "content": item.content,
                "status": item.status,
            })).collect::<Vec<_>>(),
            "counts": {
                "pending": count("pending"),
                "inProgress": count("in_progress"),
                "completed": count("completed"),
            },
        }))
    }
}

/// Projects the model-facing text (dsh `output.render`).
#[must_use]
pub fn render(_args: &Value, value: &Value) -> Vec<Value> {
    let counts = value.get("counts");
    let count = |key: &str| {
        counts
            .and_then(|counts| counts.get(key))
            .and_then(Value::as_i64)
            .unwrap_or_default()
    };
    let text = format!(
        "Updated todo list: {} pending, {} in progress, {} completed.",
        count("pending"),
        count("inProgress"),
        count("completed"),
    );
    vec![json!({ "type": "text", "text": text })]
}

/// Projects the pending-call presentation (dsh `presentCall`).
#[must_use]
pub fn present_call(args: &Value) -> Value {
    json!({
        "card": "generic",
        "title": "Update todo list",
        "kind": "other",
        "rawInput": args.get("todos").cloned().unwrap_or(Value::Null),
    })
}

/// The projection fold (dsh `apply`): a `todo/write` snapshot replaces the
/// state, a `turn/start` clears it, every other event keeps it.
#[must_use]
pub fn fold(events: &Value) -> Value {
    let mut state = Value::Null;
    let Value::Array(items) = events else {
        return state;
    };
    for event in items {
        match event.get("type").and_then(Value::as_str) {
            Some(name) if name == CHANGE_EVENT => {
                state = event.pointer("/data/todos").cloned().unwrap_or(Value::Null);
            }
            Some(name) if name == event_type::TURN_START => state = Value::Null,
            _ => {}
        }
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One recording log seam.
    #[derive(Debug, Default)]
    struct RecordingLog {
        records: std::sync::Mutex<Vec<(String, String, Value)>>,
    }

    impl SessionAppend for RecordingLog {
        fn append(&self, session: &str, event_type: &str, data: &Value) -> Result<(), String> {
            self.records.lock().expect("lock").push((
                session.to_owned(),
                event_type.to_owned(),
                data.clone(),
            ));
            Ok(())
        }
    }

    fn domain(allow_parallel: bool) -> TodoLists {
        TodoLists::new(allow_parallel, Box::new(RecordingLog::default()))
    }

    fn todos_value(items: &[(&str, &str)]) -> Value {
        Value::Array(
            items
                .iter()
                .map(|(content, status)| json!({ "content": content, "status": status }))
                .collect(),
        )
    }

    #[test]
    fn body_trims_and_rejects_the_invalid_lists() {
        let lists = domain(true);
        // The valid list trims content.
        let canonical = lists
            .to_todo_list(&todos_value(&[("  write spec  ", "pending")]))
            .expect("valid");
        assert_eq!(canonical[0].content, "write spec");
        // Empty (after trim) content refuses with the dsh text.
        assert_eq!(
            lists
                .to_todo_list(&todos_value(&[("   ", "pending")]))
                .unwrap_err()
                .message,
            "invalid todo: `content` must be a non-empty string"
        );
        // Duplicate content refuses with the JSON-quoted text.
        assert_eq!(
            lists
                .to_todo_list(&todos_value(&[("same", "pending"), ("same", "completed")]))
                .unwrap_err()
                .message,
            "invalid todos: duplicate content \"same\""
        );
    }

    #[test]
    fn body_pins_the_single_active_arms() {
        // Parallel allowed: several in_progress items pass.
        let lists = domain(true);
        assert!(
            lists
                .to_todo_list(&todos_value(&[("a", "in_progress"), ("b", "in_progress")]))
                .is_ok(),
            "the parallel deployment accepts several active items"
        );
        // Single-active: the second active item refuses with the count.
        let strict = domain(false);
        assert_eq!(
            strict
                .to_todo_list(&todos_value(&[("a", "in_progress"), ("b", "in_progress")]))
                .unwrap_err()
                .message,
            "invalid todos: at most one task may be in_progress (got 2)"
        );
        assert!(
            strict
                .to_todo_list(&todos_value(&[("a", "in_progress"), ("b", "completed")]))
                .is_ok(),
            "one active item passes the strict discipline"
        );
    }

    #[test]
    fn execute_appends_and_counts_and_refuses_without_an_agent() {
        let log = std::sync::Arc::new(RecordingLog::default());
        let owned_log = std::sync::Arc::clone(&log);
        let lists = TodoLists::new(true, Box::new(LogRelay { inner: owned_log }));
        let value = lists
            .execute(
                &serde_json::json!({ "todos": [
                    {"content": "a", "status": "completed"},
                    {"content": "b", "status": "in_progress"},
                    {"content": "c", "status": "pending"},
                ]}),
                Some("s1"),
            )
            .expect("valid call");
        // The durable snapshot landed on the owning session.
        let record = {
            let records = log.records.lock().expect("lock");
            assert_eq!(records.len(), 1);
            records[0].clone()
        };
        let (session, event_type, data) = (record.0.as_str(), record.1.as_str(), &record.2);
        assert_eq!(session, "s1");
        assert_eq!(event_type, "todo/write");
        assert_eq!(
            data,
            &serde_json::json!({ "todos": [
                {"content": "a", "status": "completed"},
                {"content": "b", "status": "in_progress"},
                {"content": "c", "status": "pending"},
            ]})
        );
        // The counts ride the wire spelling (the protocol exemption).
        assert_eq!(
            value,
            serde_json::json!({
                "todos": [
                    {"content": "a", "status": "completed"},
                    {"content": "b", "status": "in_progress"},
                    {"content": "c", "status": "pending"},
                ],
                "counts": {"pending": 1, "inProgress": 1, "completed": 1},
            })
        );
        // A caller without an owning session refuses (after validation —
        // the dsh order).
        assert_eq!(
            lists
                .execute(
                    &serde_json::json!({ "todos": [{"content": "a", "status": "pending"}]}),
                    None
                )
                .unwrap_err()
                .message,
            "todo_write requires an owning agent session"
        );
        // The illegal-flow family refuses through the same face: an
        // empty-after-trim content never appends.
        assert_eq!(
            lists
                .execute(
                    &serde_json::json!({ "todos": [{"content": "   ", "status": "pending"}]}),
                    Some("s1")
                )
                .unwrap_err()
                .message,
            "invalid todo: `content` must be a non-empty string"
        );
        // The policy arms ride the execute face too: the parallel
        // deployment accepts several active items on the owning session,
        // and the single-active discipline rejects the second one before
        // anything appends.
        let parallel_marked = lists
            .execute(
                &serde_json::json!({ "todos": [
                    {"content": "a", "status": "in_progress"},
                    {"content": "b", "status": "in_progress"},
                ]}),
                Some("s1"),
            )
            .expect("the parallel deployment accepts several active items");
        assert_eq!(
            parallel_marked["counts"]["inProgress"],
            serde_json::json!(2)
        );
        let strict_lists = TodoLists::new(false, Box::new(LogRelay { inner: log }));
        assert_eq!(
            strict_lists
                .execute(
                    &serde_json::json!({ "todos": [
                        {"content": "a", "status": "in_progress"},
                        {"content": "b", "status": "in_progress"},
                    ]}),
                    Some("s1")
                )
                .unwrap_err()
                .message,
            "invalid todos: at most one task may be in_progress (got 2)"
        );
    }

    /// Relays one boxed seam into a shared recorder (the test bridge).
    struct LogRelay {
        inner: std::sync::Arc<RecordingLog>,
    }

    impl SessionAppend for LogRelay {
        fn append(&self, session: &str, event_type: &str, data: &Value) -> Result<(), String> {
            self.inner.append(session, event_type, data)
        }
    }

    #[test]
    fn render_and_presentation_pin_the_faces() {
        let value = serde_json::json!({
            "counts": {"pending": 2, "inProgress": 1, "completed": 3},
        });
        assert_eq!(
            render(&Value::Null, &value)[0],
            json!({ "type": "text", "text": "Updated todo list: 2 pending, 1 in progress, 3 completed." })
        );
        let args = serde_json::json!({ "todos": [{"content": "a", "status": "pending"}] });
        assert_eq!(
            present_call(&args),
            json!({
                "card": "generic",
                "title": "Update todo list",
                "kind": "other",
                "rawInput": [{"content": "a", "status": "pending"}],
            })
        );
    }

    #[test]
    fn fold_replaces_clears_and_keeps() {
        let events = serde_json::json!([
            {"type": "todo/write", "data": {"todos": [{"content": "a", "status": "pending"}]}},
            {"type": "step/start", "data": {}},
            {"type": "todo/write", "data": {"todos": [{"content": "b", "status": "completed"}]}},
            {"type": "turn/start", "data": {"turn": 2}},
            {"type": "step/start", "data": {}},
        ]);
        // Last write wins; the later turn/start clears; the trailing
        // unrelated event keeps the cleared state.
        assert_eq!(fold(&events), Value::Null);
        let prefix = serde_json::json!([
            {"type": "todo/write", "data": {"todos": [{"content": "a", "status": "pending"}]}},
            {"type": "other/event", "data": {"todos": "not a write"}},
        ]);
        assert_eq!(
            fold(&prefix),
            serde_json::json!([{ "content": "a", "status": "pending" }])
        );
        assert_eq!(fold(&serde_json::json!([])), Value::Null);
    }
}
